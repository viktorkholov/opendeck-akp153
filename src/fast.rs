//! Faster page switches for slow-to-write devices (maddog-performance-optimization).
//!
//! Measured on a Mad Dog GK150K: the device takes about 3.8 ms per 512-byte image report, while a
//! command report takes 0.5 ms. Every byte of JPEG on a page switch is visible latency, so this module:
//!
//! - skips images identical to what the key already shows (OpenDeck redraws every key on a switch);
//! - encodes at a lower JPEG quality (90 in mirajazz), which sends about a third fewer bytes;
//! - turns clears into black images, so they join the batched flush instead of blocking one by one;
//! - ignores OpenDeck's "clear the whole screen" on a page switch, so the old page stays up instead of
//!   going dark, and draws the new page as a wave starting at the key that was pressed: the device
//!   needs ~30 ms per image whatever its size, so a whole page takes ~0.45 s, and showing it in rings
//!   (a refresh after each) answers the press at once instead of after the last image.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ColorType, DynamicImage};
use mirajazz::device::Device;
use mirajazz::error::MirajazzError;
use mirajazz::types::{ImageFormat, ImageMirroring, ImageMode, ImageRotation};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// JPEG quality for key images; mirajazz uses 90.
pub const JPEG_QUALITY: u8 = 75;

/// Quiet window before a flush; upstream uses 50 ms.
pub const DEBOUNCE: Duration = Duration::from_millis(20);

/// After a page switch, how long to wait for the new page's keys before blanking the ones that never came.
pub const PAGE_TIMEOUT: Duration = Duration::from_millis(300);

/// Hash of the black image used for cleared keys.
const BLACK: u64 = 0;

#[derive(Default)]
pub struct DeviceState {
    /// What each key (OpenDeck position) shows on the hardware, as a hash of the image OpenDeck sent.
    shown: HashMap<u8, u64>,
    /// Keys a page switch is still waiting for, and when the switch began.
    page: Option<(Instant, HashSet<u8>)>,
    /// JPEGs waiting to be sent, by OpenDeck position.
    pending: HashMap<u8, Vec<u8>>,
    /// The last key pressed (OpenDeck position) and when, where a page wave starts.
    press: Option<(Instant, u8)>,
}

/// Grid of OpenDeck positions: 3 rows of 6, the side keys being the last column.
const COLUMNS: i32 = 6;

pub static STATE: LazyLock<Mutex<HashMap<String, DeviceState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish() | 1 // never collides with BLACK
}

/// What to do with an image for a key.
pub enum Plan {
    Skip,
    Send,
}

/// Records that `position` will show `image_hash` (BLACK for a clear) and says whether it needs sending.
pub async fn plan(id: &str, position: u8, image_hash: u64) -> Plan {
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    if let Some((_, waiting)) = state.page.as_mut() {
        waiting.remove(&position);
    }
    if state.shown.get(&position) == Some(&image_hash) {
        return Plan::Skip;
    }
    state.shown.insert(position, image_hash);
    Plan::Send
}

pub async fn enqueue(id: &str, position: u8, jpeg: Vec<u8>) {
    let mut states = STATE.lock().await;
    states.entry(id.to_string()).or_default().pending.insert(position, jpeg);
}

pub async fn pressed(id: &str, position: u8) {
    let mut states = STATE.lock().await;
    states.entry(id.to_string()).or_default().press = Some((Instant::now(), position));
}

/// OpenDeck cleared the whole screen: a page switch. Start waiting for all `keys` positions.
pub async fn page_switch(id: &str, keys: usize) {
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    state.page = Some((Instant::now(), (0..keys as u8).collect()));
}

/// How a page appears, read from `~/Library/Application Support/opendeck/akp153-animation` on every
/// flush so it can be compared live: `wave` (rings from the pressed key, the default), `sweep`
/// (column by column from the pressed key's side), `rows` (top to bottom), `keys` (one key at a time,
/// in rings) or `batch` (all at once, like upstream).
fn animation() -> String {
    std::env::var_os("HOME")
        .map(|home| {
            std::path::Path::new(&home).join("Library/Application Support/opendeck/akp153-animation")
        })
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "wave".to_string())
}

/// Takes the queued images in waves, each shown as soon as it is in. With nothing pressed recently,
/// or only a few images, everything is one wave.
pub async fn take_waves(id: &str) -> Vec<Vec<(u8, Vec<u8>)>> {
    let mut states = STATE.lock().await;
    let Some(state) = states.get_mut(id) else {
        return vec![];
    };
    let mut images: Vec<(u8, Vec<u8>)> = state.pending.drain().collect();
    let origin = state
        .press
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(2))
        .map(|(_, p)| p as i32);
    let mode = animation();
    let Some(origin) = origin.filter(|_| images.len() > 3 && mode != "batch") else {
        return if images.is_empty() { vec![] } else { vec![images] };
    };
    let (or, oc) = (origin / COLUMNS, origin % COLUMNS);
    let ring = |p: u8| {
        let (r, c) = (p as i32 / COLUMNS, p as i32 % COLUMNS);
        (r - or).abs().max((c - oc).abs())
    };
    // (wave number, order inside it)
    let order = |p: u8| -> (i32, i32) {
        let (r, c) = (p as i32 / COLUMNS, p as i32 % COLUMNS);
        match mode.as_str() {
            "sweep" => ((c - oc).abs(), r),
            "rows" => (r, c),
            "keys" => (ring(p) * 100 + p as i32, 0),
            _ => (ring(p), p as i32),
        }
    };
    images.sort_by_key(|(p, _)| order(*p));
    let mut waves: Vec<Vec<(u8, Vec<u8>)>> = vec![];
    let mut last = i32::MIN;
    for (p, jpeg) in images {
        if order(p).0 != last {
            waves.push(vec![]);
            last = order(p).0;
        }
        waves.last_mut().unwrap().push((p, jpeg));
    }
    waves
}

/// Sends one JPEG to a device key the way mirajazz does: a BAT header, then the data in reports.
pub async fn send_image(
    device: &Device,
    packet_size: usize,
    key: u8,
    jpeg: &[u8],
) -> Result<(), MirajazzError> {
    let mut header = vec![
        0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x42, 0x41, 0x54, 0x00, 0x00,
        (jpeg.len() >> 8) as u8,
        jpeg.len() as u8,
        key + 1,
    ];
    device.write_extended_data(&mut header).await?;
    for chunk in jpeg.chunks(packet_size) {
        let mut report = Vec::with_capacity(packet_size + 1);
        report.push(0x00);
        report.extend_from_slice(chunk);
        report.resize(packet_size + 1, 0);
        device.write_data(&report).await?;
    }
    Ok(())
}

/// Shows what has been sent so far.
pub async fn refresh(device: &Device) -> Result<(), MirajazzError> {
    let mut stp = vec![0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x53, 0x54, 0x50];
    device.write_extended_data(&mut stp).await
}

/// The hardware was cleared for real (e.g. on connect): nothing is known to be shown.
pub async fn forget(id: &str) {
    STATE.lock().await.remove(id);
}

/// Before a flush: whether a page switch is still waiting for keys (and for how much longer),
/// or the positions that never arrived and must be blanked now.
pub enum PageWait {
    Ready,
    Wait(Duration),
    Blank(Vec<u8>),
}

pub async fn page_wait(id: &str) -> PageWait {
    let mut states = STATE.lock().await;
    let Some(state) = states.get_mut(id) else {
        return PageWait::Ready;
    };
    let Some((since, waiting)) = state.page.as_ref() else {
        return PageWait::Ready;
    };
    if waiting.is_empty() {
        state.page = None;
        return PageWait::Ready;
    }
    let elapsed = since.elapsed();
    if elapsed < PAGE_TIMEOUT {
        return PageWait::Wait(PAGE_TIMEOUT - elapsed);
    }
    let missing: Vec<u8> = waiting.iter().copied().collect();
    state.page = None;
    PageWait::Blank(missing)
}

pub fn black_hash() -> u64 {
    BLACK
}

/// mirajazz's conversion (resize, rotate, mirror, encode) with our JPEG quality.
pub fn convert(format: ImageFormat, image: DynamicImage) -> Result<Vec<u8>, image::ImageError> {
    let (w, h) = format.size;
    let image = if image.width() == w as u32 && image.height() == h as u32 {
        image
    } else {
        image.resize_exact(w as u32, h as u32, FilterType::Lanczos3)
    };
    let image = match format.rotation {
        ImageRotation::Rot0 => image,
        ImageRotation::Rot90 => image.rotate90(),
        ImageRotation::Rot180 => image.rotate180(),
        ImageRotation::Rot270 => image.rotate270(),
    };
    let image = match format.mirror {
        ImageMirroring::None => image,
        ImageMirroring::X => image.fliph(),
        ImageMirroring::Y => image.flipv(),
        ImageMirroring::Both => image.fliph().flipv(),
    };
    let data = image.into_rgb8().to_vec();
    match format.mode {
        ImageMode::JPEG => {
            let mut buf = Vec::new();
            JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY).encode(
                &data,
                w as u32,
                h as u32,
                ColorType::Rgb8.into(),
            )?;
            Ok(buf)
        }
        // BMP and raw devices keep the upstream path
        _ => Err(image::ImageError::Unsupported(
            image::error::UnsupportedError::from_format_and_kind(
                image::error::ImageFormatHint::Unknown,
                image::error::UnsupportedErrorKind::GenericFeature("non-JPEG device".into()),
            ),
        )),
    }
}

pub fn black(format: &ImageFormat) -> DynamicImage {
    DynamicImage::new_rgb8(format.size.0 as u32, format.size.1 as u32)
}
