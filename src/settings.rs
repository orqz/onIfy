//! Preferences, stored as JSON next to the Spotify credentials.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::lastfm;

pub struct Settings {
    /// Stable Spotify Connect device id, so this computer keeps one identity.
    pub device_id: String,
    pub volume: u16,
    pub local_folders: Vec<PathBuf>,
    pub discord_enabled: bool,
    pub discord_client_id: String,
    pub lastfm: lastfm::Account,
    /// Scrobble Spotify songs too, not just local files.
    pub scrobble_spotify: bool,
}

fn path() -> PathBuf {
    crate::spotify::config_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let v: Value = std::fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let text = |key: &str| v[key].as_str().unwrap_or_default().to_owned();
        let settings = Self {
            device_id: v["device_id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| gtk::glib::uuid_string_random().replace('-', "")),
            volume: v["volume"].as_u64().map_or(u16::MAX / 10 * 7, |v| v as u16),
            local_folders: match v["local_folders"].as_array() {
                Some(folders) => folders.iter().filter_map(|f| f.as_str()).map(PathBuf::from).collect(),
                None => crate::local::default_folders(),
            },
            discord_enabled: v["discord_enabled"].as_bool().unwrap_or(true),
            discord_client_id: text("discord_client_id"),
            lastfm: lastfm::Account {
                api_key: text("lastfm_api_key"),
                secret: text("lastfm_secret"),
                session: text("lastfm_session"),
                username: text("lastfm_username"),
            },
            // onIfy plays aren't reported to Spotify, so Spotify's own Last.fm
            // link never sees them: scrobble everything by default.
            scrobble_spotify: v["scrobble_spotify"].as_bool().unwrap_or(true),
        };
        if v["device_id"].is_null() {
            settings.save();
        }
        settings
    }

    pub fn save(&self) {
        let v = json!({
            "device_id": self.device_id,
            "volume": self.volume,
            "local_folders": self.local_folders,
            "discord_enabled": self.discord_enabled,
            "discord_client_id": self.discord_client_id,
            "lastfm_api_key": self.lastfm.api_key,
            "lastfm_secret": self.lastfm.secret,
            "lastfm_session": self.lastfm.session,
            "lastfm_username": self.lastfm.username,
            "scrobble_spotify": self.scrobble_spotify,
        });
        let _ = std::fs::create_dir_all(crate::spotify::config_dir());
        let _ = std::fs::write(path(), v.to_string());
    }

    /// The Discord application to show presence as, if presence is on.
    pub fn discord_id(&self) -> String {
        if self.discord_enabled { self.discord_client_id.trim().to_owned() } else { String::new() }
    }
}
