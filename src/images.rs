//! Cover art loading.
//!
//! Images are fetched (or read from the disk cache), decoded and resized to
//! their exact on-screen pixel size on background threads, then handed to the
//! UI as ready-to-upload textures. Requests are served newest first and are
//! dropped if their widget has scrolled away or been reused before the fetch
//! starts, so flinging through a long list never builds a backlog.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use gtk::prelude::*;
use gtk::{gdk, glib};
use librespot_core::http_client::HttpClient;

use crate::rt;

/// Decoded covers kept for instant reuse, by pixel bytes.
const CACHE_BYTES: usize = 64 << 20;
/// Low memory mode keeps this much instead.
const CACHE_BYTES_LOW: usize = 16 << 20;
static CACHE_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(CACHE_BYTES);

/// Low memory mode: keep fewer decoded covers around.
pub fn set_low_memory(low: bool) {
    let limit = if low { CACHE_BYTES_LOW } else { CACHE_BYTES };
    CACHE_LIMIT.store(limit, std::sync::atomic::Ordering::Relaxed);
    LOADER.with_borrow_mut(|l| {
        while l.bytes > limit {
            let Some(oldest) = l.textures.iter().min_by_key(|e| e.1.1).map(|e| e.0.clone()) else { break };
            if let Some((t, _)) = l.textures.remove(&oldest) {
                l.bytes -= t.width() as usize * t.height() as usize * 4;
            }
        }
    });
    crate::memory::trim_soon();
}
const MAX_IN_FLIGHT: usize = 6;
const DISK_MAX_AGE: Duration = Duration::from_secs(60 * 60 * 24 * 30);
/// Pass as `px` to get a heavily blurred backdrop version of the image.
pub const BACKDROP: u32 = 0;
/// The cover is blurred at this size, then smoothly enlarged to BACKDROP_OUT.
const BACKDROP_PX: u32 = 64;
const BACKDROP_OUT: u32 = 256;

/// An RGB colour in 0..1.
pub type Rgb = [f32; 3];
type Decoded = (Vec<u8>, u32, u32, Option<Rgb>);

type Key = (String, u32);

struct Waiter {
    wanted: Box<dyn Fn() -> bool>,
    done: Box<dyn FnOnce(&gdk::Texture)>,
}

#[derive(Default)]
struct Loader {
    textures: HashMap<Key, (gdk::Texture, u64)>,
    bytes: usize,
    clock: u64,
    waiting: HashMap<Key, Vec<Waiter>>,
    /// Pending fetches; the newest request is at the end and goes first.
    queue: Vec<Key>,
    in_flight: usize,
}

thread_local! {
    static LOADER: RefCell<Loader> = RefCell::default();
    static SCALE: Cell<i32> = const { Cell::new(0) };
    static ACCENTS: RefCell<HashMap<String, Rgb>> = RefCell::default();
}

/// A bright, saturated colour picked from a backdrop image, once it's loaded.
pub fn accent(url: &str) -> Option<Rgb> {
    ACCENTS.with_borrow(|a| a.get(url).copied())
}

static HTTP: LazyLock<HttpClient> = LazyLock::new(|| HttpClient::new(None));

fn disk_dir() -> PathBuf {
    crate::spotify::cache_dir().join("images")
}

/// The display's scale factor, so images are decoded at device resolution.
pub fn scale() -> i32 {
    SCALE.with(|s| {
        if s.get() == 0 {
            let max = gdk::Display::default()
                .map(|d| {
                    d.monitors()
                        .iter::<gdk::Monitor>()
                        .flatten()
                        .map(|m| m.scale().ceil() as i32)
                        .max()
                        .unwrap_or(1)
                })
                .unwrap_or(1);
            s.set(max.max(1));
        }
        s.get()
    })
}

pub fn cached(url: &str, px: u32) -> Option<gdk::Texture> {
    LOADER.with_borrow_mut(|l| {
        l.clock += 1;
        let clock = l.clock;
        l.textures.get_mut(&(url.to_owned(), px)).map(|(t, used)| {
            *used = clock;
            t.clone()
        })
    })
}

/// Loads `url` at `px`×`px`. `done` runs on the UI thread, and only if
/// `wanted` still holds when the fetch starts and when it finishes.
pub fn request(
    url: &str,
    px: u32,
    wanted: impl Fn() -> bool + 'static,
    done: impl FnOnce(&gdk::Texture) + 'static,
) {
    let key = (url.to_owned(), px);
    LOADER.with_borrow_mut(|l| {
        let waiters = l.waiting.entry(key.clone()).or_default();
        waiters.push(Waiter {
            wanted: Box::new(wanted),
            done: Box::new(done),
        });
        if waiters.len() == 1 {
            l.queue.push(key);
        } else if let Some(i) = l.queue.iter().position(|k| *k == key) {
            // Asked for again: it's on screen now, move it to the front.
            let k = l.queue.remove(i);
            l.queue.push(k);
        }
    });
    pump();
}

fn pump() {
    loop {
        let next = LOADER.with_borrow_mut(|l| {
            while l.in_flight < MAX_IN_FLIGHT {
                let key = l.queue.pop()?;
                let waiters = l.waiting.get_mut(&key)?;
                waiters.retain(|w| (w.wanted)());
                if waiters.is_empty() {
                    l.waiting.remove(&key);
                    continue;
                }
                l.in_flight += 1;
                return Some(key);
            }
            None
        });
        let Some(key) = next else { return };
        glib::spawn_future_local(async move {
            let (url, px) = key.clone();
            let decoded = rt::spawn(fetch(url, px)).await;
            finish(key, decoded);
        });
    }
}

fn finish(key: Key, decoded: Option<Decoded>) {
    let waiters = LOADER.with_borrow_mut(|l| {
        l.in_flight -= 1;
        l.waiting.remove(&key).unwrap_or_default()
    });
    if let Some((pixels, w, h, accent)) = decoded {
        if let Some(accent) = accent {
            ACCENTS.with_borrow_mut(|a| a.insert(key.0.clone(), accent));
        }
        let texture = gdk::MemoryTexture::new(
            w as i32,
            h as i32,
            // Covers are opaque, so this is exact, and it's the layout the
            // renderer uploads as-is, with no conversion pass on the UI thread.
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &glib::Bytes::from_owned(pixels),
            w as usize * 4,
        )
        .upcast::<gdk::Texture>();
        LOADER.with_borrow_mut(|l| {
            l.clock += 1;
            let clock = l.clock;
            let size = |t: &gdk::Texture| t.width() as usize * t.height() as usize * 4;
            l.bytes += size(&texture);
            while l.bytes > CACHE_LIMIT.load(std::sync::atomic::Ordering::Relaxed) {
                let Some(oldest) = l.textures.iter().min_by_key(|e| e.1.1).map(|e| e.0.clone()) else { break };
                if let Some((t, _)) = l.textures.remove(&oldest) {
                    l.bytes -= size(&t);
                }
            }
            l.textures.insert(key, (texture.clone(), clock));
        });
        for w in waiters {
            if (w.wanted)() {
                (w.done)(&texture);
            }
        }
    }
    pump();
    if LOADER.with_borrow(|l| l.in_flight == 0) {
        crate::memory::trim_soon();
    }
}

async fn fetch(url: String, px: u32) -> Option<Decoded> {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let path = disk_dir().join(format!("{:016x}", hasher.finish()));

    // Local covers are already files; read them straight from disk.
    let local = url.strip_prefix("file://").map(PathBuf::from);
    let bytes = match tokio::fs::read(local.as_ref().unwrap_or(&path)).await {
        Ok(b) => Bytes::from(b),
        Err(_) if local.is_some() => return None,
        Err(_) => {
            let request = http::Request::get(&url).body(Bytes::new()).ok()?;
            let bytes = HTTP.request_body(request).await.ok()?;
            let _ = tokio::fs::create_dir_all(disk_dir()).await;
            let _ = tokio::fs::write(&path, &bytes).await;
            bytes
        }
    };
    tokio::task::spawn_blocking(move || decode(&bytes, px)).await.ok()?
}

fn decode(bytes: &[u8], px: u32) -> Option<Decoded> {
    let image = image::load_from_memory(bytes).ok()?;
    if px == BACKDROP {
        return Some(backdrop(&image));
    }
    let image = if image.width() == px && image.height() == px {
        image
    } else {
        image.resize_to_fill(px, px, image::imageops::FilterType::Triangle)
    };
    let rgba = image.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h, None))
}

/// A soft field of the cover's colours, slightly more saturated. Blurred hard
/// at a tiny size, then enlarged with a smooth filter so the GPU only has to
/// stretch it a few times over: no blotches, steps or texture. This happens
/// once per song, not every frame.
fn backdrop(image: &image::DynamicImage) -> Decoded {
    use image::imageops::{FilterType, blur, resize};
    let small = image.resize_to_fill(BACKDROP_PX, BACKDROP_PX, FilterType::Triangle).into_rgba8();
    let accent = accent_of(&small);
    let mut blurred = blur(&small, 9.0);
    for p in blurred.pixels_mut() {
        let [r, g, b, _] = p.0.map(|c| c as f32);
        let luma = 0.299 * r + 0.587 * g + 0.114 * b;
        let saturate = |c: f32| (luma + (c - luma) * 1.35).clamp(0.0, 255.0) as u8;
        p.0 = [saturate(r), saturate(g), saturate(b), 255];
    }
    let smooth = resize(&blurred, BACKDROP_OUT, BACKDROP_OUT, FilterType::CatmullRom);
    let (w, h) = smooth.dimensions();
    (smooth.into_raw(), w, h, Some(accent))
}

/// Averages the vivid, bright pixels, then lifts the result so it reads as an
/// accent on a dark background.
fn accent_of(image: &image::RgbaImage) -> Rgb {
    let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
    for p in image.pixels() {
        let [r, g, b] = [p[0], p[1], p[2]].map(|c| c as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
        let w = saturation * saturation * max;
        sum[0] += r * w;
        sum[1] += g * w;
        sum[2] += b * w;
        weight += w;
    }
    if weight < 1.0 {
        return [0.92, 0.92, 0.92];
    }
    let rgb = sum.map(|c| c / weight);
    // Keep the hue, set a fixed lightness and a floor on saturation.
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let (h, s) = {
        let d = max - min;
        if d < 1e-5 {
            (0.0, 0.0)
        } else {
            let h = if max == rgb[0] {
                ((rgb[1] - rgb[2]) / d).rem_euclid(6.0)
            } else if max == rgb[1] {
                (rgb[2] - rgb[0]) / d + 2.0
            } else {
                (rgb[0] - rgb[1]) / d + 4.0
            };
            (h * 60.0, d / (1.0 - (max + min - 1.0).abs()).max(1e-5))
        }
    };
    hsl(h, s.clamp(0.55, 0.9), 0.68)
}

fn hsl(h: f32, s: f32, l: f32) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..60 => (c, x, 0.0),
        60..120 => (x, c, 0.0),
        120..180 => (0.0, c, x),
        180..240 => (0.0, x, c),
        240..300 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Deletes cached image files that haven't been written in a month.
pub fn prune_disk_cache() {
    rt::handle().spawn_blocking(|| {
        let Ok(entries) = std::fs::read_dir(disk_dir()) else { return };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > DISK_MAX_AGE);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
}
