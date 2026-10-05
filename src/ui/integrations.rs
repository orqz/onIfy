//! Everything outside the window that follows playback: system media controls
//! (MPRIS on Linux, SMTC on Windows, Now Playing on macOS), Discord Rich
//! Presence and Last.fm.

use std::cell::RefCell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk::glib;

use super::ctx;
use crate::api::{id_of, join_names};
use crate::discord::{Discord, Presence};
use crate::lastfm::{self, PlayCounter, Song};
use crate::rt;
use crate::spotify::NowPlaying;

thread_local! {
    static DISCORD: RefCell<Option<Discord>> = const { RefCell::new(None) };
    static SCROBBLE: RefCell<PlayCounter> = RefCell::default();
    static SCROBBLE_TIMER: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

pub fn start(window: &gtk::Window) {
    let client_id = ctx().settings.borrow().discord_id();
    DISCORD.with_borrow_mut(|d| *d = Some(Discord::start(client_id)));

    #[cfg(target_os = "linux")]
    {
        let _ = window;
        glib::spawn_future_local(async {
            if let Some(player) = crate::mpris::start().await {
                ctx().mpris.replace(Some(player));
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::start(window);
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

fn is_local(uri: &str) -> bool {
    uri.starts_with("spotify:local:")
}

fn update_discord() {
    let ctx = ctx();
    let presence = ctx.now.borrow().as_ref().filter(|_| ctx.bar.is_playing()).map(|now| Presence {
        title: now.name.clone(),
        artist: join_names(&now.artists),
        album: now.album.clone(),
        cover: now.cover(300).map(str::to_owned),
        track_url: if is_local(&now.uri) {
            String::new()
        } else {
            format!("https://open.spotify.com/track/{}", id_of(&now.uri))
        },
        artist_url: now
            .artists
            .first()
            .filter(|a| a.uri.starts_with("spotify:artist:"))
            .map(|a| format!("https://open.spotify.com/artist/{}", id_of(&a.uri))),
        duration_ms: now.duration_ms,
        position_ms: ctx.bar.position_ms(),
        playing: true,
    });
    DISCORD.with_borrow(|d| {
        if let Some(d) = d {
            d.set(presence);
        }
    });
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub fn track_changed(now: &NowPlaying) {
    #[cfg(target_os = "linux")]
    mpris_track(now);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_track(now);

    let ctx = ctx();
    let settings = ctx.settings.borrow();
    // Spotify scrobbles its own songs; doing it here too would count them twice.
    let scrobble = settings.lastfm.is_connected() && is_local(&now.uri);
    let song = Song {
        artist: now.artists.first().map(|a| a.name.clone()).unwrap_or_default(),
        title: now.name.clone(),
        album: now.album.clone(),
        duration_ms: now.duration_ms,
        started: unix_now(),
    };
    SCROBBLE.with_borrow_mut(|counter| {
        if scrobble && !song.artist.is_empty() {
            counter.start(song.clone());
            if ctx.bar.is_playing() {
                counter.resume();
            }
            rt::handle().spawn(lastfm::now_playing(settings.lastfm.clone(), song));
        } else {
            *counter = PlayCounter::default();
        }
    });
    drop(settings);
    update_discord();
}

pub fn playing(position_ms: u32) {
    #[cfg(target_os = "linux")]
    mpris_status(true, position_ms);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(true, position_ms);

    update_discord();
    SCROBBLE.with_borrow_mut(PlayCounter::resume);
    // Check every few seconds whether the song has earned its scrobble.
    SCROBBLE_TIMER.with_borrow_mut(|timer| {
        if timer.is_none() {
            *timer = Some(glib::timeout_add_seconds_local(5, || {
                if let Some(song) = SCROBBLE.with_borrow_mut(PlayCounter::due) {
                    let account = ctx().settings.borrow().lastfm.clone();
                    rt::handle().spawn(lastfm::scrobble(account, song));
                }
                glib::ControlFlow::Continue
            }));
        }
    });
}

pub fn paused(position_ms: u32) {
    #[cfg(target_os = "linux")]
    mpris_status(false, position_ms);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(false, position_ms);

    update_discord();
    SCROBBLE.with_borrow_mut(PlayCounter::pause);
    SCROBBLE_TIMER.with_borrow_mut(|timer| {
        if let Some(id) = timer.take() {
            id.remove();
        }
    });
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

    // Discord's progress bar is anchored to a start time; move it.
    let discord_delay = Duration::from_millis(300);
    glib::timeout_add_local_once(discord_delay, update_discord);
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
    if let Ok(id) = TrackId::try_from(format!("/dev/orqz/onIfy/track/t{path_id}")) {
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
