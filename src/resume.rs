//! Where you left off: the last song, what it played from and how far in,
//! saved so the next start shows it in the player, ready to play from there.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::api::Named;
use crate::spotify::{Context, NowPlaying};

pub struct LastPlayed {
    pub now: NowPlaying,
    pub position_ms: u32,
    pub context: Option<Context>,
}

fn path() -> PathBuf {
    crate::spotify::config_dir().join("last-played.json")
}

pub fn load() -> Option<LastPlayed> {
    let v: Value = serde_json::from_slice(&std::fs::read(path()).ok()?).ok()?;
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let uri = text(&v["uri"]);
    if uri.is_empty() {
        return None;
    }
    let now = NowPlaying {
        uri,
        name: text(&v["name"]),
        artists: v["artists"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| Named { name: text(&a["name"]), uri: text(&a["uri"]) })
            .collect(),
        album: text(&v["album"]),
        duration_ms: v["duration_ms"].as_u64().unwrap_or(0) as u32,
        covers: v["covers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| Some((c[0].as_str()?.to_owned(), c[1].as_i64()? as i32)))
            .collect(),
    };
    let context = match (&v["context"], v["tracks"].as_array()) {
        (Value::String(uri), _) => Some(Context::Uri(uri.clone())),
        (_, Some(tracks)) => Some(Context::Tracks(tracks.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())),
        _ => None,
    };
    let position_ms = (v["position_ms"].as_u64().unwrap_or(0) as u32).min(now.duration_ms);
    Some(LastPlayed { now, position_ms, context })
}

pub fn save(last: &LastPlayed) {
    let now = &last.now;
    let mut v = json!({
        "uri": now.uri,
        "name": now.name,
        "artists": now.artists.iter().map(|a| json!({ "name": a.name, "uri": a.uri })).collect::<Vec<_>>(),
        "album": now.album,
        "duration_ms": now.duration_ms,
        "covers": now.covers.iter().map(|(url, width)| json!([url, width])).collect::<Vec<_>>(),
        "position_ms": last.position_ms,
    });
    match &last.context {
        Some(Context::Uri(uri)) => v["context"] = json!(uri),
        Some(Context::Tracks(tracks)) => v["tracks"] = json!(tracks),
        None => {}
    }
    let _ = std::fs::create_dir_all(crate::spotify::config_dir());
    let _ = std::fs::write(path(), v.to_string());
}
