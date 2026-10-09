//! The system's media controls follow playback: MPRIS on Linux, SMTC (and
//! the tray icon) on Windows, Now Playing on macOS. Discord gets onify's own
//! presence when it's switched on (Spotify's own Discord connection works
//! too, through Spotify). Last.fm hears of plays through Spotify's own
//! connection, so onify needs nothing for it.

use std::cell::RefCell;

#[cfg(target_os = "linux")]
use gtk::glib;

use crate::api::{id_of, join_names};
use crate::discord::{Discord, Presence, StatusShows};
use crate::spotify::NowPlaying;
use super::ctx;

thread_local! {
    static DISCORD: RefCell<Option<Discord>> = const { RefCell::new(None) };
}

/// Call after the Discord settings change.
pub fn discord_settings_changed() {
    let client_id = ctx().settings.borrow().discord_id();
    DISCORD.with_borrow(|d| {
        if let Some(d) = d {
            d.set_client_id(client_id);
        }
    });
    update_discord();
}

fn update_discord() {
    let ctx = ctx();
    let shows = StatusShows::from_name(&ctx.settings.borrow().discord_status);
    let presence = ctx.now.borrow().as_ref().filter(|_| ctx.bar.is_playing()).map(|now| Presence {
        title: now.name.clone(),
        artist: join_names(&now.artists),
        album: now.album.clone(),
        // A local file's cover is a file on this computer; Discord can't show it.
        cover: now.cover(300).filter(|c| c.starts_with("https://")).map(str::to_owned),
        track_url: if now.uri.starts_with("spotify:track:") {
            format!("https://open.spotify.com/track/{}", id_of(&now.uri))
        } else {
            String::new()
        },
        artist_url: now
            .artists
            .first()
            .filter(|a| a.uri.starts_with("spotify:artist:"))
            .map(|a| format!("https://open.spotify.com/artist/{}", id_of(&a.uri))),
        duration_ms: now.duration_ms,
        position_ms: ctx.bar.position_ms(),
        playing: true,
        shows,
    });
    DISCORD.with_borrow(|d| {
        if let Some(d) = d {
            d.set(presence);
        }
    });
}

pub fn start(window: &gtk::Window) {
    let client_id = ctx().settings.borrow().discord_id();
    DISCORD.with_borrow_mut(|d| *d = Some(Discord::start(client_id)));

    #[cfg(target_os = "linux")]
    {
        let _ = window;
        glib::spawn_future_local(async {
            if let Some(player) = crate::mpris::start().await {
                super::ctx().mpris.replace(Some(player));
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::start(window);
}

#[cfg(target_os = "linux")]
fn is_local(uri: &str) -> bool {
    uri.starts_with("spotify:local:")
}

pub fn track_changed(now: &NowPlaying) {
    #[cfg(target_os = "linux")]
    mpris_track(now);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_track(now);
    #[cfg(windows)]
    super::tray::set_track(now);
    update_discord();
}

pub fn playing(position_ms: u32) {
    #[cfg(target_os = "linux")]
    mpris_status(true, position_ms);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(true, position_ms);
    #[cfg(windows)]
    super::tray::set_playing(true);
    update_discord();
}

pub fn paused(position_ms: u32) {
    #[cfg(target_os = "linux")]
    mpris_status(false, position_ms);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(false, position_ms);
    #[cfg(windows)]
    super::tray::set_playing(false);
    update_discord();
}

pub fn seeked(position_ms: u32) {
    #[cfg(target_os = "linux")]
    {
        use mpris_server::Time;
        super::with_mpris(move |p| async move {
            let time = Time::from_millis(position_ms as i64);
            p.set_position(time);
            let _ = p.seeked(time).await;
        });
    }
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(ctx().bar.is_playing(), position_ms);
    update_discord();
}

#[cfg(target_os = "linux")]
fn mpris_track(now: &NowPlaying) {
    use mpris_server::{Metadata, Time, TrackId};
    let mut metadata = Metadata::builder()
        .title(now.name.clone())
        .artist(now.artists.iter().map(|a| a.name.clone()))
        .album(now.album.clone())
        .length(Time::from_millis(now.duration_ms as i64));
    if !is_local(&now.uri) {
        metadata = metadata.url(format!("https://open.spotify.com/track/{}", id_of(&now.uri)));
    }
    let path_id: String = id_of(&now.uri).chars().filter(char::is_ascii_alphanumeric).collect();
    if let Ok(id) = TrackId::try_from(format!("/io/github/orqz/onIfy/track/t{path_id}")) {
        metadata = metadata.trackid(id);
    }
    if let Some(cover) = now.cover(640) {
        metadata = metadata.art_url(cover.to_owned());
    }
    let metadata = metadata.build();
    super::with_mpris(|p| async move {
        let _ = p.set_metadata(metadata).await;
    });
}

#[cfg(target_os = "linux")]
fn mpris_status(playing: bool, position_ms: u32) {
    use mpris_server::{PlaybackStatus, Time};
    super::with_mpris(move |p| async move {
        p.set_position(Time::from_millis(position_ms as i64));
        let status = if playing { PlaybackStatus::Playing } else { PlaybackStatus::Paused };
        let _ = p.set_playback_status(status).await;
    });
}

pub fn shuffle(shuffle: bool) {
    #[cfg(target_os = "linux")]
    super::with_mpris(move |p| async move {
        let _ = p.set_shuffle(shuffle).await;
    });
    #[cfg(not(target_os = "linux"))]
    let _ = shuffle;
}

pub fn repeat(context: bool, track: bool) {
    #[cfg(target_os = "linux")]
    {
        use mpris_server::LoopStatus;
        let status = match (context, track) {
            (_, true) => LoopStatus::Track,
            (true, false) => LoopStatus::Playlist,
            _ => LoopStatus::None,
        };
        super::with_mpris(move |p| async move {
            let _ = p.set_loop_status(status).await;
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (context, track);
}

pub fn volume(volume: u16) {
    #[cfg(target_os = "linux")]
    super::with_mpris(move |p| async move {
        let _ = p.set_volume(volume as f64 / u16::MAX as f64).await;
    });
    #[cfg(not(target_os = "linux"))]
    let _ = volume;
}
