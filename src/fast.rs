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
//! - draws a new page in waves, a refresh after each, in the order of the chosen animation;
//!   optionally dims the screen meanwhile;
//! - lets a page switch overtake background key updates that are being sent.
//!
//! The look is set per kind of switch (a page, a folder opening, a folder closing) in
//! `~/Library/Application Support/opendeck/akp153-animation`; see [config].

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

/// Brightness while a page is being drawn, and the steps back up afterwards.
const DIM: u8 = 12;
const RAMP_STEPS: u8 = 4;
const RAMP_STEP: Duration = Duration::from_millis(25);


/// Hash of the black image used for cleared keys.
const BLACK: u64 = 0;

/// Grid of OpenDeck positions: 3 rows of 6, the side keys being the last column.
const COLUMNS: i32 = 6;

/// Key order of the `snake` animation (OpenDeck positions).
const SNAKE: [u8; 15] = [0, 6, 12, 13, 7, 1, 2, 8, 14, 3, 9, 15, 16, 10, 4];

#[derive(Default)]
pub struct DeviceState {
    /// What each key (OpenDeck position) shows on the hardware, as a hash of the image OpenDeck sent.
    shown: HashMap<u8, u64>,
    /// The last key pressed and when: where `rings` and `sweep` start.
    press: Option<(Instant, u8)>,
    /// Keys a page switch is still waiting for, and when the switch began.
    page: Option<(Instant, HashSet<u8>)>,
    /// Counts page switches, so a background batch can notice one started and step aside.
    switches: u64,
    /// The screen is dimmed for a page switch.
    dimmed: bool,
    /// Brightness OpenDeck asked for.
    brightness: Option<u8>,
    /// JPEGs waiting to be sent, by OpenDeck position.
    pending: HashMap<u8, Vec<u8>>,
    /// The kind of the page switch in progress and the key it started from, for its animation.
    transition: Option<(Kind, Option<u8>)>,
    /// Folder keys opened and not yet closed, innermost last.
    folders: Vec<u8>,
    /// A folder is opening: its content keys go dark first, then the folder draws.
    vanish: bool,
}

/// What a page switch is: the nav key on a page (`page`), another key that switched pages
/// (`open`, a folder), or the nav key inside a folder (`close`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Page,
    Open,
    Close,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Page => "page",
            Kind::Open => "open",
            Kind::Close => "close",
        }
    }
}

pub static STATE: LazyLock<Mutex<HashMap<String, DeviceState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Held for every write sequence: an image is several reports, and a command slipped between them
/// would be read as image data.
pub static WIRE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub struct Look {
    pub order: String,
    pub dim: bool,
    /// Darken all content keys in one wave before drawing (only for a folder opening).
    pub vanish: bool,
    /// Send a refresh (STP, ~40 ms) after every wave. The GK150K holds uploaded images until a
    /// refresh, so without one per wave the page appears all at once at the end (~240 ms sooner).
    pub refresh: bool,
}

/// `~/Library/Application Support/opendeck/akp153-animation`, read on every use so changes apply on
/// the next page switch. One line per setting:
///
/// ```text
/// page  wave       # the nav key switching pages
/// open  rsnake     # a key that opened another page: a folder; it closes with the mirror of this
/// nav   16         # the nav key; without it every switch is a `page`
/// ```
///
/// A file of a single line without these names (the old format, e.g. `wave`) sets the page look.
fn config() -> HashMap<String, Vec<String>> {
    let text = std::env::var_os("HOME")
        .map(|home| {
            std::path::Path::new(&home).join("Library/Application Support/opendeck/akp153-animation")
        })
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let names = ["page", "open", "nav"];
    let mut config: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut words = line.split_whitespace().map(str::to_string);
        let Some(first) = words.next() else { continue };
        if names.contains(&first.as_str()) {
            config.insert(first, words.collect());
        } else {
            // old format: the page look
            config.insert("page".to_string(), std::iter::once(first).chain(words).collect());
        }
    }
    config
}

/// Friendlier names: `wave` is the diagonal, `classic` is everything at once like upstream.
fn alias(w: &str) -> &str {
    match w {
        "wave" => "diagonal",
        "classic" => "batch",
        other => other,
    }
}

pub fn look(kind: Kind) -> Look {
    let config = config();
    // A folder closes with its opening played backwards, so it needs no setting of its own.
    let source = if kind == Kind::Close { "open" } else { kind.name() };
    let words: Vec<&str> = config
        .get(source)
        .map(|w| w.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let default = if kind == Kind::Page { "wave" } else { "rsnake" };
    let mut order = words
        .iter()
        .map(|w| alias(w))
        .find(|w| ORDERS.contains(w))
        .unwrap_or(alias(default));
    if kind == Kind::Close {
        order = mirror(order);
    }
    Look {
        order: order.to_string(),
        dim: words.contains(&"dim"),
        vanish: kind == Kind::Open && words.contains(&"vanish"),
        refresh: !words.contains(&"once"),
    }
}

const ORDERS: [&str; 12] = [
    "diagonal", "rwave", "snake", "rsnake", "bloom", "fold", "bands", "rings", "sweep", "rows", "keys", "batch",
];

/// The same animation played backwards.
fn mirror(order: &str) -> &str {
    match order {
        "snake" => "rsnake",
        "rsnake" => "snake",
        "diagonal" => "rwave",
        "rwave" => "diagonal",
        "bloom" => "fold",
        "fold" => "bloom",
        other => other,
    }
}

fn nav_key() -> Option<u8> {
    config().get("nav")?.first()?.parse().ok()
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

pub async fn pressed(id: &str, position: u8) {
    let mut states = STATE.lock().await;
    states.entry(id.to_string()).or_default().press = Some((Instant::now(), position));
}

pub async fn enqueue(id: &str, position: u8, jpeg: Vec<u8>) {
    let mut states = STATE.lock().await;
    states.entry(id.to_string()).or_default().pending.insert(position, jpeg);
}

/// OpenDeck cleared the whole screen: a page switch. Start waiting for all `keys` positions.
pub async fn page_switch(id: &str, keys: usize) -> bool {
    let nav = nav_key();
    let mut states = STATE.lock().await;
    let state = states.entry(id.to_string()).or_default();
    state.page = Some((Instant::now(), (0..keys as u8).collect()));
    state.switches += 1;
    let press = state
        .press
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(2))
        .map(|(_, p)| p);
    let transition = match (press, nav) {
        (Some(p), Some(n)) if p == n => match state.folders.pop() {
            Some(folder) => (Kind::Close, Some(folder)), // fold back into the folder key
            None => (Kind::Page, Some(p)),
        },
        (Some(p), Some(_)) => {
            state.folders.push(p);
            (Kind::Open, Some(p))
        }
        (p, _) => (Kind::Page, p),
    };
    log::info!("Page switch: {:?} from key {:?}", transition.0, transition.1);
    state.transition = Some(transition);
    if transition.0 == Kind::Open && look(Kind::Open).vanish {
        // Content keys go dark before the folder draws, so every one of them has to be sent again;
        // the controller and the side screen stay as they are.
        state.shown.retain(|p, _| chrome(*p, nav));
        state.vanish = true;
    }
    let dim = !state.dimmed && look(transition.0).dim;
    state.dimmed |= dim;
    dim
}

pub async fn switches(id: &str) -> u64 {
    STATE.lock().await.get(id).map(|s| s.switches).unwrap_or(0)
}

/// After a page is in: whether the screen was dimmed for it, and the brightness to go back to.
pub async fn undim(id: &str) -> Option<u8> {
    let mut states = STATE.lock().await;
    let state = states.get_mut(id)?;
    if !state.dimmed || state.page.is_some() || !state.pending.is_empty() {
        return None;
    }
    state.dimmed = false;
    Some(state.brightness.unwrap_or(50))
}

pub async fn set_brightness(id: &str, percent: u8) {
    STATE.lock().await.entry(id.to_string()).or_default().brightness = Some(percent);
}

/// Whether the screen is dimmed for a page switch, so OpenDeck's brightness waits for the ramp.
pub async fn dimmed(id: &str) -> bool {
    STATE.lock().await.get(id).is_some_and(|s| s.dimmed)
}

/// The hardware was cleared for real (e.g. on connect): nothing is known to be shown.
pub async fn forget(id: &str) {
    let mut states = STATE.lock().await;
    let brightness = states.get(id).and_then(|s| s.brightness);
    states.insert(id.to_string(), DeviceState { brightness, ..Default::default() });
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

/// The images to send in waves (each shown as soon as it is in), and whether to refresh after every wave.
pub struct Batch {
    pub waves: Vec<Vec<(u8, Vec<u8>)>>,
    pub refresh_each: bool,
}

/// The deck's fixed layers, which page switches never blank or animate: the controller (the nav key:
/// pages on a desktop, back in a folder) and the side screen (the last column). The rest is content.
pub fn chrome(position: u8, nav: Option<u8>) -> bool {
    Some(position) == nav || position as i32 % COLUMNS == COLUMNS - 1
}

/// A page switch is animated by its kind's look; other updates go out as one wave.
///
/// A folder opening first turns every content key dark in one wave (`dark` is a black key image),
/// leaving the controller and the side screen lit; its empty tiles (an image shared by three or more
/// keys) are drawn straight away in that wave, since they are the dark state. Then the folder's
/// own keys appear in the chosen order.
pub async fn take_waves(id: &str, dark: &[u8]) -> Batch {
    let nav = nav_key();
    let mut states = STATE.lock().await;
    let Some(state) = states.get_mut(id) else {
        return Batch { waves: vec![], refresh_each: true };
    };
    let images: Vec<(u8, Vec<u8>)> = state.pending.drain().collect();
    let one = |images: Vec<(u8, Vec<u8>)>| if images.is_empty() { vec![] } else { vec![images] };
    let Some((kind, origin)) = state.transition.take() else {
        return Batch { waves: one(images), refresh_each: true };
    };
    let look = look(kind);
    let order = look.order;
    // Keys that share one image with several others are the page's backdrop (empty tiles).
    let mut copies: HashMap<u64, usize> = HashMap::new();
    for (_, jpeg) in &images {
        *copies.entry(hash(jpeg)).or_default() += 1;
    }
    let backdrop: HashSet<u8> = images
        .iter()
        .filter(|(_, jpeg)| copies[&hash(jpeg)] >= 3)
        .map(|(p, _)| *p)
        .collect();
    if std::mem::take(&mut state.vanish) {
        let mut dark_wave = vec![];
        let mut content = vec![];
        for (p, jpeg) in images {
            if chrome(p, nav) || backdrop.contains(&p) {
                dark_wave.push((p, jpeg));
            } else {
                dark_wave.push((p, dark.to_vec()));
                content.push((p, jpeg));
            }
        }
        let mut waves = one(dark_wave);
        waves.extend(place_waves(content, &order, origin, &HashSet::new()));
        return Batch { waves, refresh_each: true }; // the dark wave must show before the folder
    }
    if images.len() <= 3 {
        return Batch { waves: one(images), refresh_each: look.refresh };
    }
    // The controller and the side screen change at once; only content animates.
    let (fixed, content): (Vec<_>, Vec<_>) = images.into_iter().partition(|(p, _)| chrome(*p, nav));
    let mut waves = one(fixed);
    waves.extend(place_waves(content, &order, origin, &backdrop));
    Batch {
        waves,
        refresh_each: look.refresh,
    }
}

/// Sorts images into waves for an animation `order` around the key `origin`.
fn place_waves(
    mut images: Vec<(u8, Vec<u8>)>,
    order: &str,
    origin: Option<u8>,
    backdrop: &HashSet<u8>,
) -> Vec<Vec<(u8, Vec<u8>)>> {
    if images.is_empty() {
        return vec![];
    }
    if order == "batch" {
        return vec![images];
    }
    let origin = origin.map(|p| p as i32).unwrap_or(0);
    let (or, oc) = (origin / COLUMNS, origin % COLUMNS);
    // (wave number, order inside it)
    let place = |p: u8| -> (i32, i32) {
        let (r, c) = (p as i32 / COLUMNS, p as i32 % COLUMNS);
        let ring = (r - or).abs().max((c - oc).abs());
        match order {
            "rings" => (ring, p as i32),
            // bloom: the backdrop at once, then the folder's keys in rings out of the pressed key
            "bloom" if backdrop.contains(&p) => (0, p as i32),
            "bloom" => (1 + ring, p as i32),
            // fold: rings closing in on the folder key, which lights up last
            "fold" => (COLUMNS - ring, p as i32),
            "sweep" => ((c - oc).abs(), r),
            "rows" => (r, c),
            "keys" => (p as i32, 0),
            // bands: the diagonal in three waves, ~160 ms faster since every wave costs a refresh
            "bands" => ((r + c) * 3 / (COLUMNS + 2), r + c),
            // snake: one key at a time down and up the columns, in this order; the side column last
            "snake" => (
                SNAKE.iter().position(|&k| k == p).map_or(100 + p as i32, |i| i as i32),
                0,
            ),
            // rsnake: the snake backwards, from the far end; each key goes straight to its final image
            "rsnake" => (
                SNAKE.iter().rev().position(|&k| k == p).map_or(100 + p as i32, |i| i as i32),
                0,
            ),
            "rwave" => (COLUMNS + 1 - (r + c), -r), // the diagonal backwards, from the bottom right
            _ => (r + c, r), // diagonal: left to right and top to bottom at once
        }
    };
    images.sort_by_key(|(p, _)| place(*p));
    let mut waves: Vec<Vec<(u8, Vec<u8>)>> = vec![];
    let mut last = i32::MIN;
    for (p, jpeg) in images {
        if place(p).0 != last {
            waves.push(vec![]);
            last = place(p).0;
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

/// Brightness straight away, without waiting for a page. The caller holds [WIRE].
pub async fn brightness(device: &Device, percent: u8) -> Result<(), MirajazzError> {
    device.set_brightness(percent).await
}

/// Back from the dim level to `target` in a few quick steps.
pub async fn ramp_up(device: &Device, target: u8) -> Result<(), MirajazzError> {
    for step in 1..=RAMP_STEPS {
        let level = DIM as u32 + (target.saturating_sub(DIM) as u32 * step as u32) / RAMP_STEPS as u32;
        {
            let _wire = WIRE.lock().await;
            brightness(device, level as u8).await?;
        }
        if step < RAMP_STEPS {
            tokio::time::sleep(RAMP_STEP).await;
        }
    }
    Ok(())
}

pub fn dim_level() -> u8 {
    DIM
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
