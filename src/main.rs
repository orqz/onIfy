//! onify: a native Spotify client that stays smooth and light.

// Release builds on Windows open no console window next to onify's own.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod audio;
mod covers;
mod images;
mod local;
mod memory;
#[cfg(not(target_os = "linux"))]
mod media_controls;
#[cfg(target_os = "linux")]
mod mpris;
mod resume;
mod rt;
mod settings;
mod spotify;
mod ui;
mod update;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    #[cfg(target_os = "macos")]
    use_bundled_gtk();
    #[cfg(unix)]
    spotify::lock_down_dirs();
    memory::init();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,onify=info"))
        .format_timestamp_millis()
        .init();
    // Tag the audio stream so PipeWire treats it as music.
    #[cfg(target_os = "linux")]
    unsafe {
        std::env::set_var("PULSE_PROP_media.role", "music");
        std::env::set_var("PULSE_PROP_application.icon_name", ui::APP_ID);
    }
    // GTK only follows the main screen's scaling by default, so Windows
    // stretched onify (blurry) on any monitor scaled differently.
    #[cfg(windows)]
    unsafe {
        std::env::set_var("GDK_WIN32_PER_MONITOR_HIDPI", "1");
    }

    gio::resources_register_include!("onify.gresource").expect("register resources");
    glib::set_application_name("onify");

    // A development copy (ONIFY_DEV) gets its own id, so it runs next to an
    // installed onify instead of handing over to it.
    let id = match std::env::var_os("ONIFY_DEV") {
        Some(_) => format!("{}.Devel", ui::APP_ID),
        None => ui::APP_ID.to_owned(),
    };
    // Windows: a second start brings the running onify forward (it may be
    // in the tray) instead of opening another.
    #[cfg(windows)]
    if ui::hand_over_to_running(&id) {
        return glib::ExitCode::SUCCESS;
    }
    let app = adw::Application::builder()
        .application_id(id)
        .resource_base_path("/io/github/orqz/onIfy")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(ui::startup);
    app.connect_activate(ui::activate);
    // `onify spotify:album:…` or an open.spotify.com link opens that page.
    app.connect_open(ui::open_links);
    app.run()
}

/// Inside onify.app, GTK's icons, settings schemas and image loaders ship in
/// Contents/Resources rather than Homebrew's prefix (packaging/macos). Windows
/// needs nothing like this: GTK finds bin/../share there by itself.
#[cfg(target_os = "macos")]
fn use_bundled_gtk() {
    let Some(resources) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("Resources")))
        .filter(|r| r.join("share").is_dir())
    else {
        return;
    };
    let pixbuf = resources.join("lib/gdk-pixbuf-2.0/2.10.0");
    // The loader list names each loader by full path, which depends on where
    // the app was put, so it's written out fresh on every start.
    let cache = glib::user_cache_dir().join("onify").join("loaders.cache");
    let loaders = pixbuf.join("loaders");
    let listed = std::fs::read_to_string(pixbuf.join("loaders.cache.in"))
        .map(|list| list.replace("@LOADERS@", &loaders.to_string_lossy()));
    let written = listed.is_ok_and(|list| {
        std::fs::create_dir_all(cache.parent().unwrap()).is_ok() && std::fs::write(&cache, list).is_ok()
    });
    // Nothing else is running yet.
    unsafe {
        std::env::set_var("XDG_DATA_DIRS", resources.join("share"));
        std::env::set_var("GSETTINGS_SCHEMA_DIR", resources.join("share/glib-2.0/schemas"));
        if written {
            std::env::set_var("GDK_PIXBUF_MODULE_FILE", &cache);
        }
    }
}
