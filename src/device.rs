use data_url::DataUrl;
use image::load_from_memory_with_format;
use mirajazz::{device::Device, error::MirajazzError, state::DeviceStateUpdate, types::ImageMode};
use openaction::{OUTBOUND_EVENT_MANAGER, SetImageEvent};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::{
    DEVICES, FLUSH_NOTIFY, TOKENS, fast,
    inputs::opendeck_to_device,
    mappings::{
        COL_COUNT, CandidateDevice, ENCODER_COUNT, KEY_COUNT, Kind, ROW_COUNT,
        get_image_format_for_key,
    },
};

/// Initializes a device and listens for events
pub async fn device_task(candidate: CandidateDevice, token: CancellationToken) {
    log::info!("Running device task for {:?}", candidate);

    // Wrap in a closure so we can use `?` operator
    let device = async || -> Result<Device, MirajazzError> {
        let device = connect(&candidate).await?;

        device.set_brightness(50).await?;
        device.clear_all_button_images().await?;
        device.flush().await?;
        fast::forget(&candidate.id).await;

        Ok(device)
    }()
    .await;

    let device: Device = match device {
        Ok(device) => device,
        Err(err) => {
            handle_error(&candidate.id, err).await;

            log::error!(
                "Had error during device init, finishing device task: {:?}",
                candidate
            );

            return;
        }
    };

    log::info!("Registering device {}", candidate.id);
    if let Some(outbound) = OUTBOUND_EVENT_MANAGER.lock().await.as_mut() {
        outbound
            .register_device(
                candidate.id.clone(),
                candidate.kind.human_name(),
                ROW_COUNT as u8,
                COL_COUNT as u8,
                ENCODER_COUNT as u8,
                0,
            )
            .await
            .unwrap();
    }

    DEVICES.write().await.insert(candidate.id.clone(), device);

    let flush_notify = Arc::new(Notify::new());
    FLUSH_NOTIFY
        .write()
        .await
        .insert(candidate.id.clone(), flush_notify.clone());

    tokio::select! {
        _ = device_events_task(&candidate) => {},
        _ = device_flush_task(&candidate.id, flush_notify, token.clone()) => {},
        _ = token.cancelled() => {}
    };

    FLUSH_NOTIFY.write().await.remove(&candidate.id);

    log::info!("Shutting down device {:?}", candidate);

    if let Some(device) = DEVICES.read().await.get(&candidate.id) {
        device.shutdown().await.ok();
    }

    log::info!("Device task finished for {:?}", candidate);
}

/// Handles errors, returning true if should continue, returning false if an error is fatal
pub async fn handle_error(id: &String, err: MirajazzError) -> bool {
    log::error!("Device {} error: {}", id, err);

    // Some errors are not critical and can be ignored without sending disconnected event
    if matches!(err, MirajazzError::ImageError(_) | MirajazzError::BadData) {
        return true;
    }

    log::info!("Deregistering device {}", id);
    if let Some(outbound) = OUTBOUND_EVENT_MANAGER.lock().await.as_mut() {
        outbound.deregister_device(id.clone()).await.unwrap();
    }

    log::info!("Cancelling tasks for device {}", id);
    if let Some(token) = TOKENS.read().await.get(id) {
        token.cancel();
    }

    log::info!("Removing device {} from the list", id);
    DEVICES.write().await.remove(id);

    log::info!("Finished clean-up for {}", id);

    false
}

pub async fn connect(candidate: &CandidateDevice) -> Result<Device, MirajazzError> {
    let result = Device::connect(
        &candidate.dev,
        candidate.kind.protocol_version(),
        KEY_COUNT,
        ENCODER_COUNT,
    )
    .await;

    match result {
        Ok(device) => Ok(device),
        Err(e) => {
            log::error!("Error while connecting to device: {e}");

            Err(e)
        }
    }
}

/// Flushes queued images after a 50ms quiet window following the last set_button_image call.
/// Sleeps entirely when the device is idle — no polling.
async fn device_flush_task(id: &String, notify: Arc<Notify>, token: CancellationToken) {
    loop {
        tokio::select! {
            _ = notify.notified() => {}
            _ = token.cancelled() => break,
        }

        // Keep resetting the window while more images arrive; fire when quiet for fast::DEBOUNCE.
        // During a page switch keep waiting until every key of the new page has arrived, so the page
        // changes in one flush.
        loop {
            loop {
                if fast::page_complete(id).await {
                    break; // every key of the new page is in: no need to wait for quiet
                }
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(fast::DEBOUNCE) => break,
                }
            }
            match fast::page_wait(id).await {
                fast::PageWait::Ready => break,
                fast::PageWait::Wait(left) => {
                    tokio::select! {
                        _ = notify.notified() => {}
                        _ = token.cancelled() => return,
                        _ = tokio::time::sleep(left) => {}
                    }
                }
                fast::PageWait::Blank(missing) => {
                    if let Some(device) = DEVICES.read().await.get(id) {
                        for position in missing {
                            queue_black(device, id, position).await.ok();
                        }
                    }
                    break;
                }
            }
        }

        let flush_result = {
            let guard = DEVICES.read().await;
            if let Some(device) = guard.get(id) {
                send_waves(device, id).await
            } else {
                Ok(())
            }
        };
        if let Err(err) = flush_result {
            handle_error(id, err).await;
            break;
        }
    }
}

/// Handles events from device to OpenDeck
async fn device_events_task(candidate: &CandidateDevice) -> Result<(), MirajazzError> {
    log::info!("Connecting to {} for incoming events", candidate.id);

    let devices_lock = DEVICES.read().await;
    let reader = match devices_lock.get(&candidate.id) {
        Some(device) => device.get_reader(crate::inputs::process_input),
        None => return Ok(()),
    };
    drop(devices_lock);

    log::info!("Connected to {} for incoming events", candidate.id);

    log::info!("Reader is ready for {}", candidate.id);

    loop {
        log::info!("Reading updates...");

        let updates = match reader.read(None).await {
            Ok(updates) => updates,
            Err(e) => {
                if !handle_error(&candidate.id, e).await {
                    break;
                }

                continue;
            }
        };

        for update in updates {
            log::info!("New update: {:#?}", update);

            let id = candidate.id.clone();

            if let Some(outbound) = OUTBOUND_EVENT_MANAGER.lock().await.as_mut() {
                match update {
                    DeviceStateUpdate::ButtonDown(key) => {
                        fast::pressed(&id, key).await;
                        outbound.key_down(id, key).await.unwrap()
                    }
                    DeviceStateUpdate::ButtonUp(key) => outbound.key_up(id, key).await.unwrap(),
                    DeviceStateUpdate::EncoderDown(encoder) => {
                        outbound.encoder_down(id, encoder).await.unwrap();
                    }
                    DeviceStateUpdate::EncoderUp(encoder) => {
                        outbound.encoder_up(id, encoder).await.unwrap();
                    }
                    DeviceStateUpdate::EncoderTwist(encoder, val) => {
                        outbound
                            .encoder_change(id, encoder, val as i16)
                            .await
                            .unwrap();
                    }
                }
            }
        }
    }

    Ok(())
}

/// Handles different combinations of "set image" event, including clearing the specific buttons and whole device
pub async fn handle_set_image(
    device: &Device,
    id: &str,
    evt: SetImageEvent,
) -> Result<(), MirajazzError> {
    let kind = Kind::from_vid_pid(device.vid, device.pid).unwrap(); // Safe to unwrap here, because device is already filtered

    match (evt.position, evt.image) {
        (Some(position), Some(image)) => {
            // OpenDeck sends image as a data url, so parse it using a library
            let url = DataUrl::process(image.as_str()).unwrap(); // Isn't expected to fail, so unwrap it is
            let (body, _fragment) = url.decode_to_vec().unwrap(); // Same here

            // Allow only image/jpeg mime for now
            if url.mime_type().subtype != "jpeg" {
                log::error!("Incorrect mime type: {}", url.mime_type());

                return Ok(()); // Not a fatal error, enough to just log it
            }

            let image_hash = fast::hash(&body);
            if let fast::Plan::Skip = fast::plan(id, position, image_hash).await {
                log::info!("Button {} already shows this image", position);
                notify_flush(id).await; // a page switch may be waiting for this key
                return Ok(());
            }

            log::info!("Setting image for button {}", position);

            let image = load_from_memory_with_format(body.as_slice(), image::ImageFormat::Jpeg)?;
            let format = get_image_format_for_key(&kind, position);

            if matches!(format.mode, ImageMode::JPEG) {
                fast::enqueue(id, position, fast::convert(format, image)?).await;
            } else {
                device
                    .set_button_image(opendeck_to_device(position), format, image)
                    .await?;
            }

            notify_flush(id).await;
        }
        (Some(position), None) => {
            // A black image instead of the device's clear command: it joins the batched flush,
            // while each clear command blocks for about 40 ms.
            if let fast::Plan::Send = fast::plan(id, position, fast::black_hash()).await {
                queue_black(device, id, position).await?;
            }
            notify_flush(id).await;
        }
        (None, None) => {
            // OpenDeck clears the whole screen before drawing a new page. Keep the old page up instead;
            // the flush task waits for the new page's keys and swaps them in at once.
            // Dimming answers the press at once and hides the keys changing one wave at a time.
            if fast::page_switch(id, KEY_COUNT).await {
                let _wire = fast::WIRE.lock().await;
                fast::brightness(device, fast::dim_level()).await?;
            }
            notify_flush(id).await;
        }
        _ => {}
    }

    Ok(())
}

async fn notify_flush(id: &str) {
    if let Some(notify) = FLUSH_NOTIFY.read().await.get(id) {
        notify.notify_one();
    }
}

async fn queue_black(device: &Device, id: &str, position: u8) -> Result<(), MirajazzError> {
    let kind = Kind::from_vid_pid(device.vid, device.pid).unwrap();
    let format = get_image_format_for_key(&kind, position);
    match fast::convert(format, fast::black(&format)) {
        Ok(data) => {
            fast::enqueue(id, position, data).await;
            Ok(())
        }
        Err(_) => device.clear_button_image(opendeck_to_device(position)).await,
    }
}

/// Sends queued images wave by wave, showing each wave as soon as it is in. A page switch that starts
/// meanwhile takes over: the rest of this batch goes back in the queue behind the new page.
async fn send_waves(device: &Device, id: &str) -> Result<(), MirajazzError> {
    let kind = Kind::from_vid_pid(device.vid, device.pid).unwrap();
    let packet_size = if kind.protocol_version() >= 2 { 1024 } else { 512 };
    let started = std::time::Instant::now();
    let switches = fast::switches(id).await;
    let format = get_image_format_for_key(&kind, 0);
    let dark = fast::convert(format, fast::black(&format)).unwrap_or_default();
    let batch = fast::take_waves(id, &dark).await;
    let (mut images, mut bytes, mut shown_waves) = (0, 0, 0);
    let refresh_each = batch.refresh_each;
    let mut waves = batch.waves.into_iter().peekable();
    while let Some(wave) = waves.next() {
        let mut wave = wave.into_iter();
        while let Some((position, jpeg)) = wave.next() {
            if fast::switches(id).await != switches {
                let rest: Vec<_> = std::iter::once((position, jpeg))
                    .chain(wave)
                    .chain(waves.flatten())
                    .collect();
                log::info!("Page switch: {} background images wait", rest.len());
                fast::requeue(id, rest).await;
                let _wire = fast::WIRE.lock().await;
                return fast::refresh(device).await;
            }
            let _wire = fast::WIRE.lock().await;
            fast::send_image(device, packet_size, opendeck_to_device(position), &jpeg).await?;
            images += 1;
            bytes += jpeg.len();
        }
        shown_waves += 1;
        if refresh_each || waves.peek().is_none() {
            let _wire = fast::WIRE.lock().await;
            fast::refresh(device).await?;
        }
    }
    device.flush().await?; // images of BMP-mode devices still go through mirajazz's cache
    if images > 0 {
        log::info!(
            "Flushed {} images, {} bytes in {} waves, {} ms",
            images,
            bytes,
            shown_waves,
            started.elapsed().as_millis()
        );
    }
    if let Some(target) = fast::undim(id).await {
        fast::ramp_up(device, target).await?;
    }
    Ok(())
}
