//! Discord Rich Presence: "Listening to …" with the cover, artist and a
//! progress bar, from onify itself. Speaks Discord's local IPC protocol, so it
//! works with the official client and with arRPC-based clients alike.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// onify's own Discord application (discord.com/developers, named "onify",
/// with an "onify" art asset), so presence needs no setting up.
pub const ONIFY_APP: &str = "1558162192652701707";
/// Where "Get onify" on the presence card leads.
const HOME: &str = "https://github.com/orqz/onify";

/// What the status line under your name shows while listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusShows {
    /// "Listening to <song>"
    Song,
    /// "Listening to <artist>"
    Artist,
    /// "Listening to onify", like Spotify's own "Listening to Spotify"
    #[default]
    App,
}

impl StatusShows {
    pub const ALL: [StatusShows; 3] = [StatusShows::App, StatusShows::Song, StatusShows::Artist];

    pub fn name(self) -> &'static str {
        match self {
            StatusShows::Song => "song",
            StatusShows::Artist => "artist",
            StatusShows::App => "app",
        }
    }

    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|s| s.name() == name).unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            StatusShows::Song => "Song",
            StatusShows::Artist => "Artist",
            StatusShows::App => "onify",
        }
    }

    /// Discord's `status_display_type`: 0 name, 1 state, 2 details.
    fn display_type(self) -> u8 {
        match self {
            StatusShows::Song => 2,
            StatusShows::Artist => 1,
            StatusShows::App => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Presence {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// A cover link; local files have none Discord can show.
    pub cover: Option<String>,
    pub track_url: String,
    pub artist_url: Option<String>,
    pub duration_ms: u32,
    pub position_ms: u32,
    pub playing: bool,
    pub shows: StatusShows,
}

enum Message {
    Set(Option<Presence>),
    ClientId(String),
}

#[derive(Clone)]
pub struct Discord {
    tx: Sender<Message>,
}

impl Discord {
    pub fn start(client_id: String) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("onify-discord".into())
            .spawn(move || run(rx, client_id))
            .expect("spawn discord thread");
        Self { tx }
    }

    pub fn set(&self, presence: Option<Presence>) {
        let _ = self.tx.send(Message::Set(presence));
    }

    /// Empty turns presence off.
    pub fn set_client_id(&self, client_id: String) {
        let _ = self.tx.send(Message::ClientId(client_id));
    }
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::fs::File;

#[cfg(unix)]
fn connect() -> Option<Stream> {
    let mut dirs: Vec<std::path::PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(Into::into))
        .collect();
    dirs.push("/tmp".into());
    // Flatpak and Snap builds of Discord put the socket in a subdirectory;
    // other Flatpak'd clients keep theirs in their own xdg-run folder.
    let mut subdirs: Vec<std::path::PathBuf> =
        ["", "app/com.discordapp.Discord", "app/com.discordapp.DiscordCanary", "snap.discord"].map(Into::into).into();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let flatpaks = std::path::Path::new(&runtime).join(".flatpak");
        if let Ok(apps) = std::fs::read_dir(flatpaks) {
            subdirs.extend(apps.flatten().map(|app| std::path::Path::new(".flatpak").join(app.file_name()).join("xdg-run")));
        }
    }
    for dir in &dirs {
        for sub in &subdirs {
            for i in 0..10 {
                let path = dir.join(sub).join(format!("discord-ipc-{i}"));
                if let Ok(stream) = Stream::connect(&path) {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    return Some(stream);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn connect() -> Option<Stream> {
    (0..10).find_map(|i| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\.\pipe\discord-ipc-{i}"))
            .ok()
    })
}

fn send(stream: &mut Stream, op: u32, payload: &Value) -> std::io::Result<()> {
    let body = payload.to_string();
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(body.as_bytes());
    stream.write_all(&frame)
}

fn receive(stream: &mut Stream) -> std::io::Result<(u32, Value)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header)?;
    let op = u32::from_le_bytes(header[..4].try_into().unwrap());
    let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
    let mut body = vec![0u8; len.min(1 << 20)];
    stream.read_exact(&mut body)?;
    Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

fn handshake(client_id: &str) -> Option<Stream> {
    let mut stream = connect()?;
    send(&mut stream, 0, &json!({ "v": 1, "client_id": client_id })).ok()?;
    let (op, reply) = receive(&mut stream).ok()?;
    if op == 1 && reply["evt"] == "READY" {
        Some(stream)
    } else {
        log::warn!("Discord refused the connection: {reply}");
        None
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn activity(p: &Presence) -> Value {
    let mut activity = json!({
        "type": 2, // Listening
        "status_display_type": p.shows.display_type(),
        "details": clip(&p.title),
        "state": clip(&p.artist),
        "assets": {},
    });
    if !p.album.is_empty() {
        activity["assets"]["large_text"] = json!(clip(&p.album));
    }
    // The title and cover open the song on Spotify (local files have no
    // link, and Discord rejects empty URLs).
    if !p.track_url.is_empty() {
        activity["details_url"] = json!(p.track_url);
        activity["assets"]["large_url"] = json!(p.track_url);
    }
    // The cover with onify's logo in its corner, like Spotify's own badge,
    // which leads to onify; local files (no cover link) show the logo itself.
    match &p.cover {
        Some(cover) => {
            activity["assets"]["large_image"] = json!(cover);
            activity["assets"]["small_image"] = json!("onify");
            activity["assets"]["small_text"] = json!("Listening with onify");
            activity["assets"]["small_url"] = json!(HOME);
        }
        None => {
            activity["assets"]["large_image"] = json!("onify");
            activity["assets"]["large_url"] = json!(HOME);
        }
    }
    if let Some(url) = &p.artist_url {
        activity["state_url"] = json!(url);
    }
    if p.playing && p.duration_ms > 0 {
        let start = now_ms().saturating_sub(p.position_ms as u64);
        activity["timestamps"] = json!({ "start": start, "end": start + p.duration_ms as u64 });
    } else {
        activity["assets"]["small_text"] = json!("Paused");
    }
    activity
}

/// Discord rejects fields shorter than 2 or longer than 128 characters.
fn clip(s: &str) -> String {
    let mut s: String = s.chars().take(128).collect();
    while s.chars().count() < 2 {
        s.push(' ');
    }
    s
}

fn run(rx: Receiver<Message>, mut client_id: String) {
    let mut stream: Option<Stream> = None;
    let mut wanted: Option<Presence> = None;
    let mut dirty = false;
    let mut nonce = 0u64;
    loop {
        // Wake up for changes, and every so often to (re)connect.
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Message::Set(p)) => {
                wanted = p;
                dirty = true;
            }
            Ok(Message::ClientId(id)) => {
                client_id = id;
                stream = None;
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Collapse bursts (seeking, skipping) into one update.
        while let Ok(message) = rx.try_recv() {
            match message {
                Message::Set(p) => wanted = p,
                Message::ClientId(id) => {
                    client_id = id;
                    stream = None;
                }
            }
            dirty = true;
        }
        if client_id.trim().is_empty() {
            stream = None;
            continue;
        }
        if stream.is_none() {
            stream = handshake(client_id.trim());
            dirty = stream.is_some();
        }
        if !dirty {
            continue;
        }
        let Some(s) = stream.as_mut() else { continue };
        nonce += 1;
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": wanted.as_ref().map(activity) },
            "nonce": nonce.to_string(),
        });
        let ok = send(s, 1, &payload).is_ok() && receive(s).is_ok();
        if ok {
            dirty = false;
        } else {
            stream = None;
        }
    }
}
