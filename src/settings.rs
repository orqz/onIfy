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
    /// Ask when a new version is out (see crate::update).
    pub check_updates: bool,
    /// Streaming quality, see spotify::Quality.
    pub quality: String,
    /// Windows: closing the window keeps onify in the tray.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub close_to_tray: bool,
    /// How big everything is drawn, 1.0 for normal (Ctrl + / Ctrl -).
    pub zoom: f64,
    /// The sidebar shows only icons.
    pub sidebar_collapsed: bool,
    /// Seconds songs blend into each other; 0 for off.
    pub crossfade: u32,
    /// Discord: show what's playing on your profile through onify itself.
    pub discord: bool,
    /// A Discord application to show presence as instead of onify's own
    /// (only set by hand in settings.json).
    pub discord_app_id: String,
    /// What Discord's status line shows: "song", "artist" or "app".
    pub discord_status: String,
    /// Performance switches.
    pub animations: bool,
    pub cover_background: bool,
    pub hover_preload: bool,
    pub low_memory: bool,
}

pub const ZOOM_MIN: f64 = 0.3;
pub const ZOOM_MAX: f64 = 2.0;

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
            check_updates: v["check_updates"].as_bool().unwrap_or(true),
            quality: v["quality"].as_str().unwrap_or("very_high").to_owned(),
            close_to_tray: v["close_to_tray"].as_bool().unwrap_or(true),
            zoom: v["zoom"].as_f64().unwrap_or(1.0).clamp(ZOOM_MIN, ZOOM_MAX),
            sidebar_collapsed: v["sidebar_collapsed"].as_bool().unwrap_or(false),
            crossfade: v["crossfade"].as_u64().unwrap_or(0).min(12) as u32,
            discord: v["discord"].as_bool().unwrap_or(true),
            discord_app_id: text("discord_app_id"),
            discord_status: text("discord_status"),
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
            "check_updates": self.check_updates,
            "quality": self.quality,
            "close_to_tray": self.close_to_tray,
            "zoom": self.zoom,
            "sidebar_collapsed": self.sidebar_collapsed,
            "crossfade": self.crossfade,
            "discord": self.discord,
            "discord_app_id": self.discord_app_id,
            "discord_status": self.discord_status,
            "animations": self.animations,
            "cover_background": self.cover_background,
            "hover_preload": self.hover_preload,
            "low_memory": self.low_memory,
        });
        let _ = std::fs::create_dir_all(crate::spotify::config_dir());
        let _ = std::fs::write(path(), v.to_string());
    }

    /// The Discord application to show presence as, or empty for none.
    pub fn discord_id(&self) -> String {
        let custom = self.discord_app_id.trim();
        match (self.discord, custom.is_empty()) {
            (false, _) => String::new(),
            (true, false) => custom.to_owned(),
            (true, true) => crate::discord::ONIFY_APP.to_owned(),
        }
    }

    /// Animations, cover background, hover preload, low memory.
    pub fn clone_switches(&self) -> (bool, bool, bool, bool) {
        (self.animations, self.cover_background, self.hover_preload, self.low_memory)
    }
}
