//! Last.fm scrobbling for local files. Spotify songs never go through here:
//! Spotify's own Last.fm link already scrobbles them from onIfy's playback.
//!
//! Follows Last.fm's rules: "now playing" when a song starts, a scrobble once
//! it has played for half its length or four minutes, whichever comes first,
//! and nothing for songs shorter than 30 seconds. Scrobbles that can't be sent
//! are kept on disk and retried.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use bytes::Bytes;
use librespot_core::http_client::HttpClient;
use serde_json::{Value, json};

const API: &str = "https://ws.audioscrobbler.com/2.0/";

static HTTP: LazyLock<HttpClient> = LazyLock::new(|| HttpClient::new(None));
static PENDING: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Default)]
pub struct Account {
    pub api_key: String,
    pub secret: String,
    /// Session key from a completed login.
    pub session: String,
    pub username: String,
}

impl Account {
    pub fn can_authenticate(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.secret.trim().is_empty()
    }

    pub fn is_connected(&self) -> bool {
        self.can_authenticate() && !self.session.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Song {
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_ms: u32,
    /// When playback started, in Unix seconds.
    pub started: u64,
}

/// Last.fm signs each call: md5 of the sorted parameters plus the secret.
fn sign(params: &BTreeMap<String, String>, secret: &str) -> String {
    let mut base = String::new();
    for (k, v) in params {
        if k != "format" {
            base.push_str(k);
            base.push_str(v);
        }
    }
    base.push_str(secret);
    format!("{:x}", md5::compute(base.as_bytes()))
}

async fn call(account: &Account, method: &str, mut params: BTreeMap<String, String>) -> Result<Value, String> {
    params.insert("method".into(), method.to_owned());
    params.insert("api_key".into(), account.api_key.trim().to_owned());
    if !account.session.is_empty() {
        params.insert("sk".into(), account.session.clone());
    }
    let signature = sign(&params, account.secret.trim());
    params.insert("api_sig".into(), signature);
    params.insert("format".into(), "json".to_owned());
    let body: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter())
        .finish();
    let request = http::Request::post(API)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .map_err(|e| e.to_string())?;
    let response = tokio::time::timeout(Duration::from_secs(15), HTTP.request_body(request))
        .await
        .map_err(|_| "Last.fm timed out".to_owned())?
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&response).map_err(|e| e.to_string())?;
    if let Some(message) = v["message"].as_str().filter(|_| v["error"].is_number()) {
        return Err(message.to_owned());
    }
    Ok(v)
}

/// Step one of logging in: a token the user approves in their browser.
pub async fn request_token(account: &Account) -> Result<(String, String), String> {
    let anonymous = Account { session: String::new(), ..account.clone() };
    let v = call(&anonymous, "auth.getToken", BTreeMap::new()).await?;
    let token = v["token"].as_str().ok_or("Last.fm sent no token")?.to_owned();
    let url = format!("https://www.last.fm/api/auth/?api_key={}&token={token}", account.api_key.trim());
    Ok((token, url))
}

/// Step two: waits (up to two minutes) for the user to approve the token.
pub async fn finish_login(account: &Account, token: &str) -> Result<(String, String), String> {
    let anonymous = Account { session: String::new(), ..account.clone() };
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let params = BTreeMap::from([("token".to_owned(), token.to_owned())]);
        if let Ok(v) = call(&anonymous, "auth.getSession", params).await {
            let key = v["session"]["key"].as_str().unwrap_or_default().to_owned();
            let name = v["session"]["name"].as_str().unwrap_or_default().to_owned();
            if !key.is_empty() {
                return Ok((key, name));
            }
        }
    }
    Err("Last.fm login wasn't approved in time".into())
}

fn song_params(song: &Song) -> BTreeMap<String, String> {
    let mut p = BTreeMap::from([
        ("artist".to_owned(), song.artist.clone()),
        ("track".to_owned(), song.title.clone()),
        ("duration".to_owned(), (song.duration_ms / 1000).to_string()),
    ]);
    if !song.album.is_empty() {
        p.insert("album".to_owned(), song.album.clone());
    }
    p
}

pub async fn now_playing(account: Account, song: Song) {
    if let Err(e) = call(&account, "track.updateNowPlaying", song_params(&song)).await {
        log::warn!("Last.fm now playing failed: {e}");
    }
}

fn pending_path() -> PathBuf {
    crate::spotify::cache_dir().join("scrobbles.json")
}

fn load_pending() -> Vec<Value> {
    std::fs::read(pending_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Scrobbles `song`, along with any earlier scrobbles that failed to send.
pub async fn scrobble(account: Account, song: Song) {
    let mut queue = {
        let _guard = PENDING.lock().unwrap();
        let mut queue = load_pending();
        queue.push(json!({
            "artist": song.artist, "track": song.title, "album": song.album,
            "duration": song.duration_ms / 1000, "timestamp": song.started,
        }));
        queue
    };
    // Last.fm takes up to 50 scrobbles per call.
    while !queue.is_empty() {
        let batch: Vec<Value> = queue.iter().take(50).cloned().collect();
        let mut params = BTreeMap::new();
        for (i, s) in batch.iter().enumerate() {
            for key in ["artist", "track", "album", "duration", "timestamp"] {
                let value = match &s[key] {
                    Value::String(v) => v.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => continue,
                };
                if !value.is_empty() {
                    params.insert(format!("{key}[{i}]"), value);
                }
            }
        }
        match call(&account, "track.scrobble", params).await {
            Ok(_) => {
                queue.drain(..batch.len());
            }
            Err(e) => {
                log::warn!("Last.fm scrobble failed, will retry: {e}");
                break;
            }
        }
    }
    let _guard = PENDING.lock().unwrap();
    let _ = std::fs::create_dir_all(crate::spotify::cache_dir());
    let _ = std::fs::write(pending_path(), serde_json::to_vec(&queue).unwrap_or_default());
}

/// Tracks one play of a song and decides when it has earned a scrobble.
#[derive(Debug, Default)]
pub struct PlayCounter {
    song: Option<Song>,
    played_ms: u64,
    since: Option<std::time::Instant>,
    scrobbled: bool,
}

impl PlayCounter {
    pub fn start(&mut self, song: Song) {
        *self = Self { song: Some(song), ..Self::default() };
    }

    pub fn resume(&mut self) {
        self.since.get_or_insert_with(std::time::Instant::now);
    }

    pub fn pause(&mut self) {
        if let Some(since) = self.since.take() {
            self.played_ms += since.elapsed().as_millis() as u64;
        }
    }

    /// The song, once, as soon as it qualifies for a scrobble.
    pub fn due(&mut self) -> Option<Song> {
        let song = self.song.as_ref()?;
        if self.scrobbled || song.duration_ms < 30_000 {
            return None;
        }
        let played = self.played_ms + self.since.map_or(0, |s| s.elapsed().as_millis() as u64);
        let needed = (song.duration_ms as u64 / 2).min(240_000);
        if played >= needed {
            self.scrobbled = true;
            return Some(song.clone());
        }
        None
    }
}
