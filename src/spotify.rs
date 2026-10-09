//! The playback engine: a librespot session, player and Spotify Connect device.

use std::path::PathBuf;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use librespot_connect::{
    ClusterInfo, ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack,
    Spirc,
};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::session::Session;
use librespot_core::Error;
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_oauth::OAuthClientBuilder;
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::mixer::NoOpVolume;
use librespot_playback::player::{Player, PlayerEvent};
use tokio::sync::mpsc::UnboundedSender;

use crate::api::Named;
use crate::audio::Output;

const OAUTH_REDIRECT: &str = "http://127.0.0.1:8898/login";
const OAUTH_SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-email",
    "user-library-read",
    "user-library-modify",
    "user-read-recently-played",
    "user-top-read",
    "user-follow-read",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "app-remote-control",
];

pub fn config_dir() -> PathBuf {
    gtk::glib::user_config_dir().join("onify")
}

pub fn cache_dir() -> PathBuf {
    gtk::glib::user_cache_dir().join("onify")
}

/// The config folder holds the Spotify login (a reusable credential) and the
/// cache holds the library: both are for this account only, whatever the
/// umask made them. Windows and macOS already keep these folders per user.
#[cfg(unix)]
pub fn lock_down_dirs() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    for dir in [config_dir(), cache_dir()] {
        let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let credentials = config_dir().join("credentials.json");
    let _ = std::fs::set_permissions(credentials, std::fs::Permissions::from_mode(0o600));
}

fn cache() -> Result<Cache, Error> {
    let config = config_dir();
    Cache::new(
        Some(config.clone()),
        Some(config),
        Some(cache_dir().join("audio")),
        Some(1 << 30),
    )
}

pub fn cached_credentials() -> Option<Credentials> {
    cache().ok()?.credentials()
}

pub fn forget_credentials() {
    let _ = std::fs::remove_file(config_dir().join("credentials.json"));
}

/// Runs the browser OAuth flow. Blocks until the browser redirects back.
pub fn login_in_browser() -> Result<Credentials, Error> {
    let client_id = SessionConfig::default().client_id;
    let token = OAuthClientBuilder::new(&client_id, OAUTH_REDIRECT, OAUTH_SCOPES.to_vec())
        .open_in_browser()
        .with_custom_message("onify is logged in. You can close this tab.")
        .build()
        .and_then(|client| client.get_access_token())
        .map_err(Error::unauthenticated)?;
    Ok(Credentials::with_access_token(token.access_token))
}

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub uri: String,
    pub name: String,
    pub artists: Vec<Named>,
    pub album: String,
    pub duration_ms: u32,
    /// Cover URLs, smallest first.
    pub covers: Vec<(String, i32)>,
}

impl NowPlaying {
    fn from_item(item: AudioItem) -> Self {
        // Untagged local files go by their file name, as in the Local Files
        // list; their cover is the picture embedded in the file.
        let mut file_name = None;
        let mut embedded_cover = None;
        let (artists, album) = match item.unique_fields {
            UniqueFields::Track { artists, album, .. } => (
                artists
                    .iter()
                    .map(|a| Named {
                        name: a.name.clone(),
                        uri: a.id.to_uri().unwrap_or_default(),
                    })
                    .collect(),
                album,
            ),
            UniqueFields::Episode { show_name, .. } => (
                vec![Named {
                    name: show_name,
                    uri: String::new(),
                }],
                String::new(),
            ),
            UniqueFields::Local { artists, album, path, .. } => {
                file_name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                embedded_cover = crate::covers::embedded(&path);
                (
                    artists
                        .map(|name| Named {
                            name,
                            uri: String::new(),
                        })
                        .into_iter()
                        .collect(),
                    album.unwrap_or_default(),
                )
            }
        };
        let mut covers: Vec<(String, i32)> =
            item.covers.into_iter().map(|c| (c.url, c.width)).collect();
        covers.extend(embedded_cover.map(|url| (url, 640)));
        covers.sort_by_key(|c| c.1);
        // Untagged local files carry the name taken from their file name in
        // the URI (see local.rs); without tags, that's the best there is.
        let mut artists = artists;
        let mut name = item.name;
        if let Some(named) = crate::api::Track::from_local_uri(&item.uri) {
            if artists.is_empty() {
                artists = named.artists;
            }
            if name.is_empty() {
                name = named.name;
            }
        }
        if name.is_empty() {
            name = file_name.unwrap_or_default();
        }
        Self {
            uri: item.uri,
            name,
            artists,
            album,
            duration_ms: item.duration_ms,
            covers,
        }
    }

    pub fn cover(&self, px: i32) -> Option<&str> {
        self.covers
            .iter()
            .find(|c| c.1 >= px)
            .or(self.covers.last())
            .map(|c| c.0.as_str())
    }
}

#[derive(Debug)]
pub enum Event {
    Track(NowPlaying),
    Playing { position_ms: u32 },
    Paused { position_ms: u32 },
    Position { position_ms: u32 },
    Loading,
    Stopped,
    Shuffle(bool),
    Repeat { context: bool, track: bool },
    Volume(u16),
    Unavailable,
    /// The engine with this generation lost its connection.
    Disconnected(u64),
    /// The account behind the engine with this generation isn't Premium,
    /// which Spotify needs to stream to other apps.
    NotPremium(u64),
    /// The account's devices, and what plays on the active one.
    Devices(Arc<ClusterInfo>),
    /// What another device is playing, looked up for showing it here.
    RemoteTrack(NowPlaying),
}

/// Streaming quality. Spotify offers librespot Ogg Vorbis up to 320 kbps; its
/// lossless FLAC files aren't offered (librespot_playback logs the formats
/// each song comes in, at debug level).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Normal,
    High,
    VeryHigh,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Normal, Quality::High, Quality::VeryHigh];

    fn bitrate(self) -> Bitrate {
        match self {
            Quality::Normal => Bitrate::Bitrate96,
            Quality::High => Bitrate::Bitrate160,
            Quality::VeryHigh => Bitrate::Bitrate320,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Quality::Normal => "normal",
            Quality::High => "high",
            Quality::VeryHigh => "very_high",
        }
    }

    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|q| q.name() == name).unwrap_or(Quality::VeryHigh)
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::Normal => "Normal (96 kbps)",
            Quality::High => "High (160 kbps)",
            Quality::VeryHigh => "Very high (320 kbps)",
        }
    }
}

/// What a song was played from, so it can be played the same way again
/// (see crate::resume): a playlist, album, artist or collection, or a loose
/// list of songs.
#[derive(Debug, Clone, PartialEq)]
pub enum Context {
    Uri(String),
    Tracks(Vec<String>),
}

/// Loose lists longer than this aren't remembered (Local Files can hold
/// thousands); the song alone is.
const REMEMBERED_LIST: usize = 500;

pub struct Engine {
    pub session: Session,
    /// What the last play request played from.
    context: std::sync::Mutex<Option<Context>>,
    spirc: Spirc,
    /// Kept to preload songs ahead of a click.
    player: Arc<Player>,
    /// Set when the user skips, so the old track's queued tail is dropped.
    flush_on_load: Arc<AtomicBool>,
    /// Between asking to play something and it loading (see forward_events).
    loading: Arc<AtomicBool>,
    output: Arc<Output>,
    shuffle: AtomicBool,
    repeat: AtomicBool,
}

impl Engine {
    pub async fn start(
        credentials: Credentials,
        device_id: String,
        quality: Quality,
        local_folders: Vec<PathBuf>,
        output: Arc<Output>,
        events: UnboundedSender<Event>,
        generation: u64,
    ) -> Result<Arc<Self>, Error> {
        let session_config = SessionConfig {
            device_id,
            ..SessionConfig::default()
        };
        let session = Session::new(session_config, Some(cache()?));

        let player_config = PlayerConfig {
            bitrate: quality.bitrate(),
            normalisation: true,
            position_update_interval: None,
            local_file_directories: local_folders,
            ..PlayerConfig::default()
        };
        let sink_output = output.clone();
        let player = Player::new(player_config, session.clone(), Box::new(NoOpVolume), move || {
            sink_output.sink()
        });
        let player_events = player.get_player_event_channel();

        let mixer = output.mixer();
        let connect_config = ConnectConfig {
            name: "onify".into(),
            device_type: DeviceType::Computer,
            initial_volume: mixer.volume(),
            ..ConnectConfig::default()
        };
        let (spirc, spirc_task) =
            Spirc::new(connect_config, session.clone(), credentials, player.clone(), mixer).await?;

        let flush_on_load = Arc::new(AtomicBool::new(false));
        let loading = Arc::new(AtomicBool::new(false));
        let engine = Arc::new(Self {
            session,
            context: std::sync::Mutex::new(None),
            spirc,
            player,
            flush_on_load: flush_on_load.clone(),
            loading: loading.clone(),
            output: output.clone(),
            shuffle: AtomicBool::new(false),
            repeat: AtomicBool::new(false),
        });

        let done = events.clone();
        tokio::spawn(async move {
            spirc_task.await;
            let _ = done.send(Event::Disconnected(generation));
        });
        // Other devices and what they play, for the device picker.
        let mut clusters = engine.spirc.clusters();
        let devices = events.clone();
        tokio::spawn(async move {
            while clusters.changed().await.is_ok() {
                let info = clusters.borrow_and_update().clone();
                if let Some(info) = info {
                    if devices.send(Event::Devices(info)).is_err() {
                        break;
                    }
                }
            }
        });
        // Spotify says which plan the account is on shortly after connecting.
        let session = engine.session.clone();
        let refused = events.clone();
        tokio::spawn(async move {
            for _ in 0..100 {
                if let Some(kind) = session.get_user_attribute("type") {
                    if kind != "premium" {
                        log::warn!("{kind} account; onify needs Premium");
                        let _ = refused.send(Event::NotPremium(generation));
                    }
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        tokio::spawn(forward_events(player_events, events, output, flush_on_load, loading));
        Ok(engine)
    }

    fn options(&self, playing_track: Option<PlayingTrack>, shuffle: Option<bool>) -> LoadRequestOptions {
        LoadRequestOptions {
            start_playing: true,
            seek_to: 0,
            context_options: Some(LoadContextOptions::Options(Options {
                shuffle: shuffle.unwrap_or(self.shuffle.load(Ordering::Relaxed)),
                repeat: self.repeat.load(Ordering::Relaxed),
                repeat_track: false,
            })),
            playing_track,
        }
    }

    fn load(&self, request: LoadRequest) {
        // Timestamps for how long a song takes to start (see "now playing").
        log::info!("play requested");
        self.flush_on_load.store(true, Ordering::Relaxed);
        self.loading.store(true, Ordering::Relaxed);
        let _ = self.spirc.activate();
        let _ = self.spirc.load(request);
    }

    /// Plays a playlist, album, artist or collection, optionally from a track.
    pub fn play_context(&self, context: &str, track: Option<&str>, shuffle: Option<bool>) {
        *self.context.lock().unwrap() = Some(Context::Uri(context.to_owned()));
        let track = track.map(|t| PlayingTrack::Uri(t.to_owned()));
        self.load(LoadRequest::from_context_uri(
            context.to_owned(),
            self.options(track, shuffle),
        ));
    }

    /// Plays a loose list of tracks (search results), starting at `index`.
    pub fn play_tracks(&self, uris: Vec<String>, index: usize) {
        self.play_list(uris, Some(index), None);
    }

    /// Plays a loose list of tracks (local files) from `index`, or from the top.
    pub fn play_list(&self, uris: Vec<String>, index: Option<usize>, shuffle: Option<bool>) {
        let remembered = (uris.len() <= REMEMBERED_LIST).then(|| Context::Tracks(uris.clone()));
        *self.context.lock().unwrap() = remembered;
        let track = index.map(|i| PlayingTrack::Index(i as u32));
        self.load(LoadRequest::from_tracks(uris, self.options(track, shuffle)));
    }

    /// What the last play request played from, if onify knows.
    pub fn context(&self) -> Option<Context> {
        self.context.lock().unwrap().clone()
    }

    /// Plays `track` from `position_ms` in its context, the way it was
    /// playing last time (or on its own, when its list wasn't kept).
    pub fn resume(&self, track: &str, context: Option<&Context>, position_ms: u32) {
        let mut options = self.options(Some(PlayingTrack::Uri(track.to_owned())), None);
        options.seek_to = position_ms;
        *self.context.lock().unwrap() = context.cloned();
        let request = match context {
            Some(Context::Uri(uri)) => LoadRequest::from_context_uri(uri.clone(), options),
            Some(Context::Tracks(uris)) if uris.iter().any(|u| u == track) => {
                LoadRequest::from_tracks(uris.clone(), options)
            }
            _ => LoadRequest::from_tracks(vec![track.to_owned()], options),
        };
        self.load(request);
    }

    pub fn play(&self) {
        let _ = self.spirc.play();
    }

    /// Starts loading a song likely to be played next (e.g. the one under the
    /// pointer), so it starts at once if it is.
    pub fn preload(&self, uri: &str) {
        if let Ok(uri) = librespot_core::SpotifyUri::from_uri(uri) {
            self.player.preload(uri);
        }
    }

    pub fn pause(&self) {
        let _ = self.spirc.pause();
    }

    /// Pulls the user's current or most recent session onto this device.
    pub fn take_over(&self) {
        let _ = self.spirc.transfer(None);
    }

    pub fn next(&self) {
        log::info!("skip requested");
        self.output.mark_skip();
        self.flush_on_load.store(true, Ordering::Relaxed);
        let _ = self.spirc.next();
        // Skipping plays, even from pause (librespot kept the pause).
        let _ = self.spirc.play();
    }

    pub fn prev(&self) {
        self.flush_on_load.store(true, Ordering::Relaxed);
        let _ = self.spirc.prev();
        let _ = self.spirc.play();
    }

    pub fn seek(&self, position_ms: u32) {
        let _ = self.spirc.set_position_ms(position_ms);
    }

    pub fn set_volume(&self, volume: u16) {
        let _ = self.spirc.set_volume(volume);
    }

    pub fn set_shuffle(&self, shuffle: bool) {
        self.shuffle.store(shuffle, Ordering::Relaxed);
        let _ = self.spirc.shuffle(shuffle);
    }

    pub fn set_repeat(&self, context: bool, track: bool) {
        self.repeat.store(context, Ordering::Relaxed);
        let _ = self.spirc.repeat(context);
        let _ = self.spirc.repeat_track(track);
    }

    /// Songs blend into each other over this many seconds (0: off).
    pub fn set_crossfade(&self, seconds: u32) {
        self.player.set_crossfade(seconds * 1000);
    }

    pub fn note_shuffle(&self, shuffle: bool) {
        self.shuffle.store(shuffle, Ordering::Relaxed);
    }

    pub fn note_repeat(&self, repeat: bool) {
        self.repeat.store(repeat, Ordering::Relaxed);
    }

    pub fn shutdown(&self) {
        let _ = self.spirc.shutdown();
    }

    /// This device's Spotify Connect id.
    pub fn device_id(&self) -> String {
        self.session.device_id().to_owned()
    }

    /// Looks up a song another device is playing (Event::RemoteTrack).
    pub fn look_up(&self, uri: &str, events: UnboundedSender<Event>) {
        let Ok(id) = librespot_core::SpotifyUri::from_uri(uri) else { return };
        let session = self.session.clone();
        tokio::spawn(async move {
            match AudioItem::get_file(&session, id).await {
                Ok(item) => {
                    let _ = events.send(Event::RemoteTrack(NowPlaying::from_item(item)));
                }
                Err(e) => log::warn!("couldn't look up the other device's song: {e}"),
            }
        });
    }
}

async fn forward_events(
    mut player_events: librespot_playback::player::PlayerEventChannel,
    events: UnboundedSender<Event>,
    output: Arc<Output>,
    flush_on_load: Arc<AtomicBool>,
    loading: Arc<AtomicBool>,
) {
    // librespot announces a new request before it reports the old one as
    // stopped, so events can arrive for a song that's already been replaced.
    // Acting on those left the play button showing ▶ while music played.
    let mut current = None;
    while let Some(event) = player_events.recv().await {
        if let PlayerEvent::PlayRequestIdChanged { play_request_id } = event {
            current = Some(play_request_id);
            if flush_on_load.swap(false, Ordering::Relaxed) {
                output.flush();
            }
            continue;
        }
        if event.get_play_request_id().is_some_and(|id| current.is_some_and(|c| c != id)) {
            continue;
        }
        // Starting playback activates this device first, and librespot then
        // announces its settings as they were before the request: shuffle and
        // repeat off. The request's own shuffle and repeat apply right after,
        // unannounced, so believing these turned shuffle off for the next one.
        match event {
            PlayerEvent::ShuffleChanged { .. } | PlayerEvent::RepeatChanged { .. }
                if loading.load(Ordering::Relaxed) =>
            {
                continue;
            }
            PlayerEvent::Loading { .. }
            | PlayerEvent::Playing { .. }
            | PlayerEvent::Paused { .. }
            | PlayerEvent::Stopped { .. } => loading.store(false, Ordering::Relaxed),
            _ => {}
        }
        let event = match event {
            PlayerEvent::Seeked { position_ms, .. } => {
                output.flush();
                Event::Position { position_ms }
            }
            PlayerEvent::PositionCorrection { position_ms, .. } => Event::Position { position_ms },
            PlayerEvent::Playing { position_ms, .. } => {
                log::info!("now playing");
                Event::Playing { position_ms }
            }
            PlayerEvent::Paused { position_ms, .. } => Event::Paused { position_ms },
            PlayerEvent::Loading { .. } => Event::Loading,
            PlayerEvent::Stopped { .. } => {
                output.flush();
                Event::Stopped
            }
            PlayerEvent::TrackChanged { audio_item } => {
                Event::Track(NowPlaying::from_item(*audio_item))
            }
            PlayerEvent::ShuffleChanged { shuffle } => Event::Shuffle(shuffle),
            PlayerEvent::RepeatChanged { context, track } => Event::Repeat { context, track },
            PlayerEvent::VolumeChanged { volume } => Event::Volume(volume),
            PlayerEvent::Unavailable { .. } => Event::Unavailable,
            _ => continue,
        };
        if events.send(event).is_err() {
            break;
        }
    }
}
