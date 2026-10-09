//! The system's media controls follow playback: MPRIS on Linux, SMTC (and
//! the tray icon) on Windows, Now Playing on macOS. (Discord and Last.fm hear of plays through
//! Spotify's own connections, so onify needs nothing for them.)

#[cfg(target_os = "linux")]
use gtk::glib;

#[cfg(target_os = "linux")]
use crate::api::id_of;
use crate::spotify::NowPlaying;
#[cfg(not(target_os = "linux"))]
use super::ctx;

pub fn start(window: &gtk::Window) {
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
    {
        super::tray::set_track(now);
        super::thumbbar::set_likeable(now.uri.starts_with("spotify:track:"));
    }
}

pub fn playing(position_ms: u32) {
    #[cfg(target_os = "linux")]
    mpris_status(true, position_ms);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(true, position_ms);
    #[cfg(windows)]
    {
        super::tray::set_playing(true);
        super::thumbbar::set_playing(true);
    }
}

pub fn paused(position_ms: u32) {
    #[cfg(target_os = "linux")]
    mpris_status(false, position_ms);
    #[cfg(not(target_os = "linux"))]
    crate::media_controls::set_playing(false, position_ms);
    #[cfg(windows)]
    {
        super::tray::set_playing(false);
        super::thumbbar::set_playing(false);
    }
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
