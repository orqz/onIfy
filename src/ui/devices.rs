//! Spotify Connect devices: the picker in the player, and playback on
//! another device ("Playing on Kitchen Speaker"), which the player then shows
//! and controls the way Spotify's apps do.

use std::cell::RefCell;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use librespot_connect::{ClusterInfo, ConnectDevice};
use serde_json::{Value, json};

use super::{ctx, rt, toast};
use crate::spotify::NowPlaying;

thread_local! {
    /// The account's devices and the active one's playback, as last reported.
    static CLUSTER: RefCell<Option<Arc<ClusterInfo>>> = const { RefCell::new(None) };
    /// The other device playing, while one is.
    static REMOTE: RefCell<Option<ConnectDevice>> = const { RefCell::new(None) };
    /// The other device's song that was last looked up.
    static LOOKED_UP: RefCell<String> = const { RefCell::new(String::new()) };
    /// The picker's list, rebuilt while it's open.
    static LIST: RefCell<Option<glib::WeakRef<gtk::ListBox>>> = const { RefCell::new(None) };
}

/// The other device that's playing, if one is.
pub fn remote() -> Option<ConnectDevice> {
    REMOTE.with_borrow(|r| r.clone())
}

pub fn is_remote() -> bool {
    REMOTE.with_borrow(|r| r.is_some())
}

/// The account's devices or their playback changed.
pub fn changed(info: Arc<ClusterInfo>) {
    let ctx = ctx();
    let mine = ctx.engine.borrow().as_ref().map(|e| e.device_id()).unwrap_or_default();
    CLUSTER.with_borrow_mut(|c| *c = Some(info.clone()));
    refresh_list();
    let active = &info.active_device_id;
    let other = (!active.is_empty() && *active != mine && !info.track_uri.is_empty())
        .then(|| info.devices.iter().find(|d| d.id == *active).cloned())
        .flatten();
    let Some(device) = other else {
        if REMOTE.with_borrow_mut(|r| r.take()).is_some() {
            ctx.bar.set_remote(None);
            // Nothing plays anywhere now: the player keeps showing the song,
            // paused, and Play picks the session up here.
            if active.is_empty() {
                ctx.bar.stopped();
            }
        }
        return;
    };
    REMOTE.with_borrow_mut(|r| *r = Some(device.clone()));
    ctx.bar.set_remote(Some(&device.name));
    ctx.bar.set_volume(device.volume.min(u16::MAX as u32) as u16);
    ctx.bar.set_shuffle(info.shuffle);
    ctx.bar.set_repeat(info.repeat_context, info.repeat_track);
    if ctx.bar.track_uri() == info.track_uri {
        ctx.bar.set_playing(info.playing, info.position_ms.max(0) as u32);
        return;
    }
    let fresh = LOOKED_UP.with_borrow_mut(|last| std::mem::replace(last, info.track_uri.clone()) != info.track_uri);
    if !fresh {
        return;
    }
    match crate::api::Track::from_local_uri(&info.track_uri) {
        Some(track) => show_remote_track(NowPlaying {
            uri: track.uri,
            name: track.name,
            artists: track.artists,
            album: track.album.name,
            duration_ms: track.duration_ms,
            covers: Vec::new(),
        }),
        None => {
            let events = ctx.events.clone();
            let uri = info.track_uri.clone();
            if let Some(engine) = ctx.engine.borrow().clone() {
                rt::handle().spawn(async move { engine.look_up(&uri, events) });
            }
        }
    }
}

/// The other device's song, looked up: the player shows it.
pub fn show_remote_track(now: NowPlaying) {
    let Some(info) = CLUSTER.with_borrow(|c| c.clone()) else { return };
    if !is_remote() || info.track_uri != now.uri {
        return;
    }
    let ctx = ctx();
    super::track_row::set_now_playing(&now.uri);
    ctx.bar.set_track(&now);
    ctx.bar.set_playing(info.playing, info.position_ms.max(0) as u32);
    ctx.backdrop.set_cover(now.cover(300));
    ctx.now.replace(Some(now.clone()));
    super::integrations::track_changed(&now);
    if info.playing {
        super::integrations::playing(info.position_ms.max(0) as u32);
    } else {
        super::integrations::paused(info.position_ms.max(0) as u32);
    }
}

/// Sends a Spotify Connect command to the device that's playing.
pub fn command(endpoint: &'static str, extra: Value) {
    let (Some(device), Some(api)) = (remote(), ctx().api()) else { return };
    glib::spawn_future_local(async move {
        let sent = rt::spawn(async move { api.remote(&device.id, endpoint, extra).await }).await;
        if let Err(e) = sent {
            toast(&format!("Couldn't reach {}: {e}", device.name));
        }
    });
}

/// Sets the volume of the device that's playing.
pub fn set_volume(volume: u16) {
    let (Some(device), Some(api)) = (remote(), ctx().api()) else { return };
    glib::spawn_future_local(async move {
        let _ = rt::spawn(async move { api.remote_volume(&device.id, volume).await }).await;
    });
}

pub fn seek(position_ms: u32) {
    command("seek_to", json!({ "value": position_ms, "position": position_ms }));
}

/// Moves playback to `device`: this computer takes it over itself.
fn play_on(device: &ConnectDevice) {
    let ctx = ctx();
    let mine = ctx.engine.borrow().as_ref().map(|e| e.device_id()).unwrap_or_default();
    if device.id == mine {
        ctx.with_engine(|e| e.take_over());
        return;
    }
    let Some(api) = ctx.api() else { return };
    let active = CLUSTER.with_borrow(|c| c.as_ref().map(|c| c.active_device_id.clone())).unwrap_or_default();
    let from = if active.is_empty() { mine } else { active };
    let device = device.clone();
    glib::spawn_future_local(async move {
        let to = device.id.clone();
        if let Err(e) = rt::spawn(async move { api.transfer(&from, &to).await }).await {
            toast(&format!("Couldn't play on {}: {e}", device.name));
        }
    });
}

/// Developer aid: the devices as last reported, for the log.
pub fn describe() -> String {
    CLUSTER.with_borrow(|c| match c {
        Some(info) => format!(
            "active {:?}, playing {} ({}), devices: {}",
            info.active_device_id,
            info.track_uri,
            if info.playing { "playing" } else { "paused" },
            info.devices.iter().map(|d| format!("{} [{}] {}", d.name, d.kind, d.id)).collect::<Vec<_>>().join("; ")
        ),
        None => "no device list yet".into(),
    })
}

fn icon_for(kind: &str) -> &'static str {
    match kind {
        "computer" | "chromebook" => "onify-computer-symbolic",
        "smartphone" | "tablet" | "smartwatch" => "onify-phone-symbolic",
        "tv" | "stb" | "cast_video" | "game_console" => "onify-tv-symbolic",
        _ => "onify-speaker-symbolic",
    }
}

/// The picker: a popover listing the account's devices, the playing one lit.
pub fn picker() -> gtk::Popover {
    let list = gtk::ListBox::new();
    list.add_css_class("devices");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.connect_row_activated(|_, row| {
        let id = row.widget_name();
        let device = CLUSTER.with_borrow(|c| c.as_ref().and_then(|c| c.devices.iter().find(|d| d.id == id).cloned()));
        if let Some(device) = device {
            play_on(&device);
        }
        if let Some(popover) = row.ancestor(gtk::Popover::static_type()).and_downcast::<gtk::Popover>() {
            popover.popdown();
        }
    });
    let heading = gtk::Label::builder().label("Connect to a device").xalign(0.0).build();
    heading.add_css_class("devices-heading");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.append(&heading);
    content.append(&list);
    let popover = gtk::Popover::builder().child(&content).position(gtk::PositionType::Top).has_arrow(false).build();
    popover.add_css_class("devices-pop");
    LIST.with_borrow_mut(|l| *l = Some(list.downgrade()));
    popover.connect_show(|_| refresh_list());
    popover
}

fn refresh_list() {
    let Some(list) = LIST.with_borrow(|l| l.as_ref().and_then(|l| l.upgrade())) else { return };
    if !list.is_mapped() {
        return;
    }
    list.remove_all();
    let info = CLUSTER.with_borrow(|c| c.clone()).unwrap_or_default();
    let mine = ctx().engine.borrow().as_ref().map(|e| e.device_id()).unwrap_or_default();
    // This computer first, then the rest by name.
    let mut devices: Vec<&ConnectDevice> = info.devices.iter().collect();
    devices.sort_by_key(|d| d.id != mine);
    for device in devices {
        let active = device.id == info.active_device_id;
        let row = gtk::ListBoxRow::builder().name(device.id.as_str()).build();
        row.add_css_class("device");
        if active {
            row.add_css_class("active");
        }
        let content = gtk::Box::builder().spacing(14).build();
        let icon = gtk::Image::from_icon_name(icon_for(&device.kind));
        icon.set_pixel_size(22);
        content.append(&icon);
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).build();
        let name = gtk::Label::builder()
            .label(&device.name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .build();
        name.add_css_class("device-name");
        text.append(&name);
        let detail = match (device.id == mine, active) {
            (true, true) => Some("This computer · Playing"),
            (true, false) => Some("This computer"),
            (false, true) => Some("Playing"),
            (false, false) => None,
        };
        if let Some(detail) = detail {
            let detail = gtk::Label::builder().label(detail).xalign(0.0).build();
            detail.add_css_class("device-detail");
            text.append(&detail);
        }
        content.append(&text);
        row.set_child(Some(&content));
        list.append(&row);
    }
    if info.devices.len() <= 1 {
        let hint = gtk::Label::builder()
            .label("No other devices. Open Spotify on your phone, a speaker or a TV with the same account.")
            .wrap(true)
            .max_width_chars(30)
            .xalign(0.0)
            .build();
        hint.add_css_class("device-detail");
        let row = gtk::ListBoxRow::builder().child(&hint).activatable(false).build();
        list.append(&row);
    }
}
