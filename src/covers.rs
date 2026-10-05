//! Cover art for local files: the picture embedded in the file, saved small
//! in the cache so the player, the backdrop and the system's media controls
//! can show it like any other cover. Discord only takes links, so for Rich
//! Presence the cover is uploaded to a temporary host that deletes it within
//! an hour, the way Music Presence does it.

use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use bytes::Bytes;
use librespot_core::http_client::HttpClient;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardVisualKey};
use symphonia::core::probe::Hint;

/// Saved covers are at most this wide; Discord and the player show them smaller.
const SIZE: u32 = 640;
/// Temporary host for Discord. Files expire after `KEEP`.
const UPLOAD: &str = "https://litterbox.catbox.moe/resources/internals/api.php";
pub const KEEP: Duration = Duration::from_secs(60 * 60);

static HTTP: LazyLock<HttpClient> = LazyLock::new(|| HttpClient::new(None));

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

/// Uploads a saved cover (a `file://` URL from `embedded`) and returns its
/// public link.
pub async fn upload(file_url: &str) -> Result<String, String> {
    let path = file_url.strip_prefix("file://").ok_or("not a local cover")?;
    let data = tokio::fs::read(path).await.map_err(|e| e.to_string())?;

    let boundary = format!("onify{:016x}", rand_u64());
    let mut body = Vec::with_capacity(data.len() + 512);
    for (name, value) in [("reqtype", "fileupload"), ("time", "1h")] {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"fileToUpload\"; filename=\"cover.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let request = http::Request::post(UPLOAD)
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .body(Bytes::from(body))
        .map_err(|e| e.to_string())?;
    let response = tokio::time::timeout(Duration::from_secs(30), HTTP.request_body(request))
        .await
        .map_err(|_| "cover upload timed out".to_owned())?
        .map_err(|e| e.to_string())?;
    let link = String::from_utf8_lossy(&response).trim().to_owned();
    if link.starts_with("https://") {
        Ok(link)
    } else {
        Err(format!("cover upload failed: {link}"))
    }
}

fn rand_u64() -> u64 {
    let mut hasher = DefaultHasher::new();
    (std::time::SystemTime::now(), std::process::id()).hash(&mut hasher);
    hasher.finish()
}
