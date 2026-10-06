//! Preferences, stored as JSON next to the Spotify credentials.

use std::path::PathBuf;

use serde_json::{Value, json};

pub struct Settings {
    /// Stable Spotify Connect device id, so this computer keeps one identity.
    pub device_id: String,
    pub volume: u16,
    pub local_folders: Vec<PathBuf>,
    /// How dim the blurred cover is: "bright", "normal" or "dark".
    pub background: String,
    /// The look: "vinyl" (no glass, a turning record) or "glass".
    pub style: String,
    /// Performance switches.
    pub animations: bool,
    pub cover_background: bool,
    pub hover_preload: bool,
    pub low_memory: bool,
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
            background: text("background"),
            style: v["style"].as_str().unwrap_or("vinyl").to_owned(),
            animations: v["animations"].as_bool().unwrap_or(true),
            cover_background: v["cover_background"].as_bool().unwrap_or(true),
            hover_preload: v["hover_preload"].as_bool().unwrap_or(true),
            low_memory: v["low_memory"].as_bool().unwrap_or(false),
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
            "background": self.background,
            "style": self.style,
            "animations": self.animations,
            "cover_background": self.cover_background,
            "hover_preload": self.hover_preload,
            "low_memory": self.low_memory,
        });
        let _ = std::fs::create_dir_all(crate::spotify::config_dir());
        let _ = std::fs::write(path(), v.to_string());
    }

    /// Animations, cover background, hover preload, low memory.
    pub fn clone_switches(&self) -> (bool, bool, bool, bool) {
        (self.animations, self.cover_background, self.hover_preload, self.low_memory)
    }
}
