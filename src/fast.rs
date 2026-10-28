//! Faster page switches for slow-to-write devices (maddog-performance-optimization).
//!
//! Measured on a Maddog GK150K: the device needs ~30 ms per key image whatever its size (15 images
//! of 15 KB took 442 ms, of 30 KB 475 ms) and ~40 ms per refresh (STP), and holds uploaded images
//! until a refresh; any other command takes ~0.5 ms. So the number of images is what costs, and
//! this module:
//!
//! - skips images identical to what the key already shows (OpenDeck redraws every key on a switch);
//! - turns clears into black images, so they join the batch instead of blocking ~40 ms each;
//! - ignores OpenDeck's "clear the whole screen" on a page switch, so the screen never goes black;
//! - keeps the controller (the nav key) and the side screen out of animations, and draws the rest of
//!   a new page in waves, a refresh after each, in the order of its animation;
//! - lets a page switch overtake background key updates that are being sent.
//!
//! Animations are set in `~/Library/Application Support/opendeck/akp153-animation`; see [config].

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

/// JPEG quality for key images; mirajazz uses 90. Size barely matters on this device, so stay sharp.
pub const JPEG_QUALITY: u8 = 85;

/// Quiet window before a flush; upstream uses 50 ms.
pub const DEBOUNCE: Duration = Duration::from_millis(15);

/// After a page switch, how long to wait for the new page's keys before blanking the ones that never came.
pub const PAGE_TIMEOUT: Duration = Duration::from_millis(300);

/// Hash of the black image used for cleared keys.
const BLACK: u64 = 0;

/// Grid of OpenDeck positions: 3 rows of 6, the side keys being the last column.
const COLUMNS: i32 = 6;

/// Key order of the `snake` animation (OpenDeck positions); `rsnake` runs it backwards.
const SNAKE: [u8; 15] = [0, 6, 12, 13, 7, 1, 2, 8, 14, 3, 9, 15, 16, 10, 4];

#[derive(Default)]
pub struct DeviceState {
    /// What each key (OpenDeck position) shows on the hardware, as a hash of the image OpenDeck sent.
    shown: HashMap<u8, u64>,
    /// The last key pressed and when, to tell what a page switch was.
    press: Option<(Instant, u8)>,
    /// Keys a page switch is still waiting for, and when the switch began.
    page: Option<(Instant, HashSet<u8>)>,
    /// Counts page switches, so a background batch can notice one started and step aside.
    switches: u64,
    /// JPEGs waiting to be sent, by OpenDeck position.
    pending: HashMap<u8, Vec<u8>>,
    /// The kind of the page switch whose images are coming in.
    transition: Option<Kind>,
    /// How many folders are open: the nav key closes one while there are any.
    folders: usize,
}

/// What a page switch is: the nav key on a page (`page`), another key that switched pages
/// (`open`, a folder), or the nav key inside a folder (`close`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Page,
    Open,
    Close,
}

pub static STATE: LazyLock<Mutex<HashMap<String, DeviceState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Held for every write sequence: an image is several reports, and a command slipped between them
/// would be read as image data.
pub static WIRE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// `~/Library/Application Support/opendeck/akp153-animation`, read on every page switch so a change
/// applies to the next one:
///
/// ```text
/// page  wave       # the nav key switching pages: classic, wave or snake
/// open  rsnake     # a key that opened another page (a folder); it closes with this backwards
/// nav   16         # the nav key; without it every switch is a `page`
/// ```
///
/// A file of a single line without these names (the old format, e.g. `wave`) sets the page animation.
fn config() -> HashMap<String, String> {
    let text = std::env::var_os("HOME")
        .map(|home| {
            std::path::Path::new(&home).join("Library/Application Support/opendeck/akp153-animation")
        })
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let mut config = HashMap::new();
    for line in text.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        match (words.next(), words.next()) {
            (Some(name @ ("page" | "open" | "nav")), Some(value)) => {
                config.insert(name.to_string(), value.to_string());
            }
            (Some(value), _) => {
                config.insert("page".to_string(), value.to_string()); // old format
            }
            _ => {}
        }
    }
    config
}

/// The animation for a kind of page switch. A folder closes with its opening played backwards.
fn animation(kind: Kind) -> &'static str {
    let config = config();
    let name = |key: &str, default| match config.get(key).map(String::as_str) {
        Some("classic" | "batch") => "classic",
        Some("wave" | "diagonal") => "wave",
        Some("rwave") => "rwave",
        Some("snake") => "snake",
        Some("rsnake") => "rsnake",
        _ => default,
    };
    match kind {
        Kind::Page => name("page", "wave"),
        Kind::Open => name("open", "rsnake"),
        Kind::Close => match name("open", "rsnake") {
            "snake" => "rsnake",
            "rsnake" => "snake",
            "wave" => "rwave",
            "rwave" => "wave",
            other => other,
        },
    }
}

fn nav_key() -> Option<u8> {
    config().get("nav")?.parse().ok()
}

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

/// Records that `position` will show the image with `image_hash` (BLACK for a clear) and says
/// whether it needs sending.
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

/// A key was pressed. Besides remembering it, forget what it showed: some devices blank a key on a
/// press and rely on OpenDeck sending the same image again, which must then not be skipped.
pub async fn pressed(id: &str, position: u8) {
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    state.press = Some((Instant::now(), position));
    state.shown.remove(&position);
}

pub async fn enqueue(id: &str, position: u8, jpeg: Vec<u8>) {
    let mut states = STATE.lock().await;
    states.entry(id.to_string()).or_default().pending.insert(position, jpeg);
}

/// OpenDeck cleared the whole screen: a page switch. Tell its kind from the key pressed just before
/// and start waiting for all `keys` positions.
pub async fn page_switch(id: &str, keys: usize) {
    let nav = nav_key();
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    state.page = Some((Instant::now(), (0..keys as u8).collect()));
    state.switches += 1;
    let press = state
        .press
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(2))
        .map(|(_, p)| p);
    let kind = match (press, nav) {
        (Some(p), Some(n)) if p == n && state.folders > 0 => {
            state.folders -= 1;
            Kind::Close
        }
        (Some(p), Some(n)) if p != n => {
            state.folders += 1;
            Kind::Open
        }
        _ => Kind::Page,
    };
    log::info!("Page switch: {:?}", kind);
    state.transition = Some(kind);
}

/// Milliseconds since the last key press, for the log.
pub async fn since_press(id: &str) -> Option<u128> {
    STATE.lock().await.get(id)?.press.map(|(at, _)| at.elapsed().as_millis())
}

pub async fn switches(id: &str) -> u64 {
    STATE.lock().await.get(id).map(|s| s.switches).unwrap_or(0)
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

/// Whether a page switch has all its keys, so the flush need not wait out the quiet window.
pub async fn page_complete(id: &str) -> bool {
    STATE
        .lock()
        .await
        .get(id)
        .and_then(|s| s.page.as_ref())
        .is_some_and(|(_, waiting)| waiting.is_empty())
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

/// The deck's fixed layers, which page switches never animate: the controller (the nav key: pages on
/// a desktop, back in a folder) and the side screen (the last column). The rest is content.
fn chrome(position: u8, nav: Option<u8>) -> bool {
    Some(position) == nav || position as i32 % COLUMNS == COLUMNS - 1
}

/// Takes the queued images in waves, each shown as soon as it is in. After a page switch the
/// controller and the side screen go first, then the content in the order of the switch's
/// animation, each key straight to its final image. Other updates go out as one wave.
pub async fn take_waves(id: &str) -> Vec<Vec<(u8, Vec<u8>)>> {
    let nav = nav_key();
    let mut states = STATE.lock().await;
    let Some(state) = states.get_mut(id) else {
        return vec![];
    };
    let images: Vec<(u8, Vec<u8>)> = state.pending.drain().collect();
    if images.is_empty() {
        return vec![];
    }
    let Some(kind) = state.transition.take() else {
        return vec![images];
    };
    let (fixed, mut content): (Vec<_>, Vec<_>) = images.into_iter().partition(|(p, _)| chrome(*p, nav));
    let mut waves: Vec<Vec<(u8, Vec<u8>)>> = if fixed.is_empty() { vec![] } else { vec![fixed] };
    let order = animation(kind);
    if order == "classic" {
        if !content.is_empty() {
            waves.push(content);
        }
        return waves;
    }
    // Wave number of each key.
    let wave = |p: u8| -> i32 {
        let (r, c) = (p as i32 / COLUMNS, p as i32 % COLUMNS);
        let snake = || SNAKE.iter().position(|&k| k == p).map_or(100, |i| i as i32);
        match order {
            "snake" => snake(),
            "rsnake" => SNAKE.len() as i32 - snake(),
            "rwave" => COLUMNS + 1 - (r + c), // the diagonal backwards, from the bottom right
            _ => r + c,                        // wave: the diagonal from the top left
        }
    };
    content.sort_by_key(|(p, _)| (wave(*p), *p));
    let mut last = i32::MIN;
    for (p, jpeg) in content {
        if wave(p) != last {
            waves.push(vec![]);
            last = wave(p);
        }
        waves.last_mut().unwrap().push((p, jpeg));
    }
    waves
}

/// Puts images back in the queue after a page switch interrupted their batch, unless newer ones came.
pub async fn requeue(id: &str, images: Vec<(u8, Vec<u8>)>) {
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    for (p, jpeg) in images {
        state.pending.entry(p).or_insert(jpeg);
    }
}

/// Sends one JPEG to a device key the way mirajazz does: a BAT header, then the data in reports.
/// The caller holds [WIRE].
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

/// Shows what has been sent so far. The caller holds [WIRE].
pub async fn refresh(device: &Device) -> Result<(), MirajazzError> {
    let mut stp = vec![0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x53, 0x54, 0x50];
    device.write_extended_data(&mut stp).await
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
