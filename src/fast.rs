//! Faster page switches for slow-to-write devices (maddog-performance-optimization).
//!
//! Measured on a Mad Dog GK150K: the device takes about 3.8 ms per 512-byte image report, while a
//! command report takes 0.5 ms. Every byte of JPEG on a page switch is visible latency, so this module:
//!
//! - skips images identical to what the key already shows (OpenDeck redraws every key on a switch);
//! - encodes at a lower JPEG quality (90 in mirajazz), which sends about a third fewer bytes;
//! - turns clears into black images, so they join the batched flush instead of blocking one by one;
//! - holds OpenDeck's "clear the whole screen" on a page switch until the new page has arrived, so the
//!   old page stays up and the new one replaces it in one flush, with no blank screen in between.

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ColorType, DynamicImage};
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
    /// Images and bytes queued since the last flush, for the log.
    pub queued: (usize, usize),
}

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

pub async fn queued(id: &str, bytes: usize) {
    let mut states = STATE.lock().await;
    let q = &mut states.entry(id.to_string()).or_default().queued;
    q.0 += 1;
    q.1 += bytes;
}

/// OpenDeck cleared the whole screen: a page switch. Start waiting for all `keys` positions.
pub async fn page_switch(id: &str, keys: usize) {
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    state.page = Some((Instant::now(), (0..keys as u8).collect()));
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

pub async fn take_queued(id: &str) -> (usize, usize) {
    let mut states = STATE.lock().await;
    states
        .get_mut(id)
        .map(|s| std::mem::take(&mut s.queued))
        .unwrap_or_default()
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
