//! Media keys and the system's now-playing controls on Windows (SMTC) and
//! macOS (Now Playing). Linux uses MPRIS instead, see `mpris.rs`.

use std::cell::RefCell;
use std::time::Duration;

use gtk::glib;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

use crate::spotify::NowPlaying;
use crate::ui::{self, ctx};

thread_local! {
    static CONTROLS: RefCell<Option<MediaControls>> = const { RefCell::new(None) };
}

#[cfg(windows)]
fn window_handle(window: &gtk::Window) -> Option<*mut std::ffi::c_void> {
    use gtk::prelude::*;
    let surface = window.surface()?;
    let surface = surface.downcast::<gdk4_win32::Win32Surface>().ok()?;
    Some(surface.handle().0 as *mut std::ffi::c_void)
}

pub fn start(window: &gtk::Window) {
    #[cfg(windows)]
    let hwnd = window_handle(window);
    #[cfg(not(windows))]
    let hwnd = {
        let _ = window;
        None
    };
    let config = PlatformConfig {
        dbus_name: "onify",
        display_name: "onify",
        hwnd,
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("media controls unavailable: {e:?}");
            return;
        }
    };
    let attached = controls.attach(|event| {
        // Events arrive on a system thread; handle them on the UI thread.
        glib::MainContext::default().invoke(move || handle(event));
    });
    if let Err(e) = attached {
        log::warn!("media controls unavailable: {e:?}");
        return;
    }
    CONTROLS.with_borrow_mut(|c| *c = Some(controls));
}

fn handle(event: MediaControlEvent) {
    let bar = ctx().bar.clone();
    match event {
        MediaControlEvent::Toggle => ui::play_pause(),
        MediaControlEvent::Play if !bar.is_playing() => ui::play_pause(),
        MediaControlEvent::Pause | MediaControlEvent::Stop if bar.is_playing() => ui::play_pause(),
        MediaControlEvent::Next => ui::next(),
        MediaControlEvent::Previous => ui::prev(),
        MediaControlEvent::Seek(direction) | MediaControlEvent::SeekBy(direction, _) => {
            let by = match event {
                MediaControlEvent::SeekBy(_, by) => by.as_millis() as i64,
                _ => 10_000,
            };
            let delta = if matches!(direction, SeekDirection::Forward) { by } else { -by };
            ui::seek_to(bar.position_ms() as i64 + delta);
        }
        MediaControlEvent::SetPosition(MediaPosition(at)) => ui::seek_to(at.as_millis() as i64),
        MediaControlEvent::Raise => ui::raise(),
        MediaControlEvent::Quit => ui::quit(),
        _ => {}
    }
}

pub fn set_track(now: &NowPlaying) {
    let artists = crate::api::join_names(&now.artists);
    CONTROLS.with_borrow_mut(|controls| {
        if let Some(controls) = controls {
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(&now.name),
                artist: Some(&artists),
                album: Some(&now.album),
                cover_url: now.cover(640),
                duration: Some(Duration::from_millis(now.duration_ms as u64)),
            });
        }
    });
}

pub fn set_playing(playing: bool, position_ms: u32) {
    let progress = Some(MediaPosition(Duration::from_millis(position_ms as u64)));
    CONTROLS.with_borrow_mut(|controls| {
        if let Some(controls) = controls {
            let _ = controls.set_playback(if playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            });
        }
    });
}
