//! Local files, the way Spotify does them: songs in your music folders get
//! `spotify:local:` URIs from their tags, so they play on their own and also
//! wherever they appear in your Spotify playlists.
//!
//! librespot plays them from the same folders; this module lists them for the
//! Local Files page, deriving URIs exactly the way librespot does so the two
//! always agree.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;

use crate::api::{Images, Named, Track};

/// What librespot will play (it matches Spotify's own list).
const EXTENSIONS: &[&str] = &["mp3", "mp4", "m4p", "flac"];

/// The user's music folder, the default local files source.
pub fn default_folders() -> Vec<PathBuf> {
    gtk::glib::user_special_dir(gtk::glib::UserDirectory::Music)
        .into_iter()
        .collect()
}

/// Every playable local song under `folders`, sorted by artist, album, track.
pub fn scan(folders: &[PathBuf]) -> Vec<Track> {
    let mut tracks = Vec::new();
    for folder in folders {
        visit(folder, &mut tracks);
    }
    tracks.sort_by(|a, b| {
        let key = |t: &Track| (t.artist_names().to_lowercase(), t.album.name.to_lowercase(), t.number, t.name.to_lowercase());
        key(a).cmp(&key(b))
    });
    tracks
}

fn visit(dir: &Path, tracks: &mut Vec<Track>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, tracks);
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
        if EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
            if let Some(track) = probe(&path, ext) {
                tracks.push(track);
            }
        }
    }
}

fn probe(path: &Path, ext: &str) -> Option<Track> {
    let source = MediaSourceStream::new(Box::new(File::open(path).ok()?), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(ext);
    let mut probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;

    // Same as librespot: container tags, else tags found while probing. Files
    // with no tags at all are skipped, because librespot can't play them.
    let tags = {
        let mut metadata = probed.format.metadata();
        if metadata.current().is_none() {
            if let Some(probe_metadata) = probed.metadata.get() {
                metadata = probe_metadata;
            }
        }
        metadata.skip_to_latest();
        metadata.current()?.tags().to_vec()
    };
    let (mut artist, mut album, mut title, mut number) = (None, None, None, 0);
    for tag in tags {
        match tag.std_key {
            Some(StandardTagKey::Artist) => artist = Some(tag.value.to_string()),
            Some(StandardTagKey::Album) => album = Some(tag.value.to_string()),
            Some(StandardTagKey::TrackTitle) => title = Some(tag.value.to_string()),
            Some(StandardTagKey::TrackNumber) => {
                number = tag.value.to_string().split('/').next()?.trim().parse().unwrap_or(0)
            }
            _ => {}
        }
    }

    let track = probed.format.default_track()?;
    let time = track.codec_params.time_base?.calc_time(track.codec_params.n_frames?);
    let part = |s: &Option<String>| -> String {
        s.as_deref()
            .map(|s| url::form_urlencoded::byte_serialize(s.as_bytes()).collect())
            .unwrap_or_default()
    };
    let uri = format!("spotify:local:{}:{}:{}:{}", part(&artist), part(&album), part(&title), time.seconds);

    let name = title.unwrap_or_else(|| {
        path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    });
    Some(Track {
        uri,
        name,
        artists: artist
            .map(|name| Named { name, uri: String::new() })
            .into_iter()
            .collect(),
        album: Named {
            name: album.unwrap_or_default(),
            uri: String::new(),
        },
        images: Images::default(),
        duration_ms: (time.seconds * 1000 + (time.frac * 1000.0) as u64) as u32,
        explicit: false,
        playable: true,
        number,
    })
}
