//! Cover art for local files: the picture embedded in the file, saved small
//! in the cache so the player, the backdrop and the system's media controls
//! can show it like any other cover.

use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardVisualKey};
use symphonia::core::probe::Hint;

/// Saved covers are at most this wide; the player shows them smaller.
const SIZE: u32 = 640;

fn dir() -> PathBuf {
    crate::spotify::cache_dir().join("covers")
}

/// The embedded cover of the song at `path` as a `file://` URL, extracting it
/// on first use. `None` if the file has no picture.
pub fn embedded(path: &Path) -> Option<String> {
    // Keyed by path, size and modification time, so edited tags re-extract.
    let meta = std::fs::metadata(path).ok()?;
    let mut hasher = DefaultHasher::new();
    (path, meta.len(), meta.modified().ok()).hash(&mut hasher);
    let cached = dir().join(format!("{:016x}.jpg", hasher.finish()));
    if !cached.exists() {
        let picture = read_picture(path)?;
        let image = image::load_from_memory(&picture).ok()?;
        let image = if image.width() > SIZE || image.height() > SIZE {
            image.resize(SIZE, SIZE, image::imageops::FilterType::CatmullRom)
        } else {
            image
        };
        std::fs::create_dir_all(dir()).ok()?;
        image.into_rgb8().save_with_format(&cached, image::ImageFormat::Jpeg).ok()?;
    }
    Some(format!("file://{}", cached.display()))
}

/// The front cover (or else the first picture) in the file's tags.
fn read_picture(path: &Path) -> Option<Box<[u8]>> {
    let source = MediaSourceStream::new(Box::new(File::open(path).ok()?), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    let mut pictures = Vec::new();
    if let Some(revision) = probed.format.metadata().skip_to_latest() {
        pictures.extend(revision.visuals().iter().cloned());
    }
    if let Some(mut metadata) = probed.metadata.get() {
        if let Some(revision) = metadata.skip_to_latest() {
            pictures.extend(revision.visuals().iter().cloned());
        }
    }
    let front = pictures.iter().position(|v| v.usage == Some(StandardVisualKey::FrontCover));
    let picture = pictures.into_iter().nth(front.unwrap_or(0))?;
    Some(picture.data)
}
