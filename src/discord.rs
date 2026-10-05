//! Discord Rich Presence: "Listening to …" with the cover, artist and a
//! progress bar. Speaks Discord's local IPC protocol, so it works with the
//! official client and with arRPC-based clients (Vesktop, onCord) alike.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct Presence {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub cover: Option<String>,
    pub track_url: String,
    pub artist_url: Option<String>,
    pub duration_ms: u32,
    pub position_ms: u32,
    pub playing: bool,
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
    // Flatpak and Snap builds of Discord put the socket in a subdirectory.
    let subdirs = ["", "app/com.discordapp.Discord", "app/com.discordapp.DiscordCanary", "snap.discord", ".flatpak/dev.vencord.Vesktop/xdg-run"];
    for dir in &dirs {
        for sub in subdirs {
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
        "status_display_type": 2, // show the song, not the app name
        "details": clip(&p.title),
        "state": clip(&p.artist),
        "assets": {},
    });
    if !p.album.is_empty() {
        activity["assets"]["large_text"] = json!(clip(&p.album));
    }
    // Local files have no link, and Discord rejects empty URLs.
    if !p.track_url.is_empty() {
        activity["details_url"] = json!(p.track_url);
        activity["buttons"] = json!([{ "label": "Open in Spotify", "url": p.track_url }]);
    }
    if let Some(cover) = &p.cover {
        activity["assets"]["large_image"] = json!(cover);
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
