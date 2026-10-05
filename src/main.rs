//! onIfy: a native Spotify client that stays smooth and light.

mod api;
mod audio;
mod covers;
mod discord;
mod images;
mod lastfm;
mod local;
mod memory;
#[cfg(not(target_os = "linux"))]
mod media_controls;
#[cfg(target_os = "linux")]
mod mpris;
mod rt;
mod settings;
mod spotify;
mod ui;

use adw::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    memory::init();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,onify=info"))
        .init();
    // Tag the audio stream so PipeWire treats it as music.
    #[cfg(target_os = "linux")]
    unsafe {
        std::env::set_var("PULSE_PROP_media.role", "music");
        std::env::set_var("PULSE_PROP_application.icon_name", ui::APP_ID);
    }

    gio::resources_register_include!("onify.gresource").expect("register resources");
    glib::set_application_name("onIfy");

    let app = adw::Application::builder()
        .application_id(ui::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(ui::startup);
    app.connect_activate(ui::activate);
    // `onify spotify:album:…` or an open.spotify.com link opens that page.
    app.connect_open(ui::open_links);
    app.run()
}
