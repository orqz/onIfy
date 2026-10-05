//! The always-visible playback bar.
//!
//! The progress bar is interpolated locally from the last position the player
//! reported. Instead of redrawing every frame it wakes exactly when the knob
//! reaches the next pixel (or the clock the next second), so it moves as
//! smoothly as the screen can show while costing almost no CPU.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::cover::Cover;
use super::glass::Glass;
use super::ctx;
use super::track_row::format_duration;
use crate::spotify::NowPlaying;

#[derive(Default)]
struct State {
    has_track: bool,
    track_uri: String,
    playing: bool,
    /// Position at `at` (monotonic µs).
    position_ms: u32,
    at: i64,
    duration_ms: u32,
    shuffle: bool,
    repeat_context: bool,
    repeat_track: bool,
    liked: bool,
    volume: f64,
    muted_from: Option<f64>,
    seek_to: Option<u32>,
}

pub struct PlayerBar {
    pub widget: Glass,
    pub side_end: gtk::Box,
    pub like: gtk::Button,
    cover: Cover,
    title: gtk::Label,
    artists: gtk::Label,
    shuffle: gtk::Button,
    play: gtk::Button,
    repeat: gtk::Button,
    elapsed: gtk::Label,
    progress: gtk::Scale,
    total: gtk::Label,
    volume_button: gtk::Button,
    volume: gtk::Scale,
    state: RefCell<State>,
    tick: RefCell<Option<glib::SourceId>>,
    seek_debounce: RefCell<Option<glib::SourceId>>,
    dragging: Cell<bool>,
}

/// The cover fills the capsule's height inside its padding (see style.css).
const COVER: i32 = 46;

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_valign(gtk::Align::Center);
    button
}

fn now_us() -> i64 {
    glib::monotonic_time()
}

impl PlayerBar {
    pub fn new() -> Rc<Self> {
        // Three equal columns, like Spotify: the controls stay centred no
        // matter how long the song title is. Corners are concentric: the
        // capsule's 21px radius minus its 8px padding is the cover's 13px.
        let widget = Glass::new(gtk::Orientation::Horizontal, 12, 21.0);
        widget.set_homogeneous(true);
        widget.add_css_class("player-bar");

        // Left: what's playing.
        let side_start = gtk::Box::builder().spacing(12).build();
        let cover = Cover::new(COVER, 13.0);
        cover.set_cursor_from_name(Some("pointer"));
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(2)
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        title.add_css_class("now-title");
        let artists = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        artists.add_css_class("now-artists");
        text.append(&title);
        text.append(&artists);
        let like = icon_button("onify-heart-symbolic", "Save to Liked Songs");
        like.set_visible(false);
        side_start.append(&cover);
        side_start.append(&text);
        side_start.append(&like);

        // Center: transport and progress.
        let center = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        let shuffle = icon_button("onify-media-playlist-shuffle-symbolic", "Shuffle");
        let prev = icon_button("onify-media-skip-backward-symbolic", "Previous");
        let play = gtk::Button::from_icon_name("onify-media-playback-start-symbolic");
        play.set_tooltip_text(Some("Play"));
        play.add_css_class("play-button");
        play.add_css_class("circular");
        play.set_valign(gtk::Align::Center);
        let next = icon_button("onify-media-skip-forward-symbolic", "Next");
        let repeat = icon_button("onify-media-playlist-repeat-symbolic", "Repeat");
        for b in [&shuffle, &prev, &play, &next, &repeat] {
            buttons.append(b);
        }
        let timeline = gtk::Box::builder().spacing(8).build();
        let elapsed = gtk::Label::builder().label("0:00").width_chars(5).xalign(1.0).build();
        let total = gtk::Label::builder().label("0:00").width_chars(5).xalign(0.0).build();
        for l in [&elapsed, &total] {
            l.add_css_class("numeric");
            l.add_css_class("caption");
            l.add_css_class("dim-label");
        }
        let progress = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1000.0);
        progress.set_hexpand(true);
        progress.add_css_class("progress");
        progress.set_sensitive(false);
        timeline.append(&elapsed);
        timeline.append(&progress);
        timeline.append(&total);
        center.append(&buttons);
        center.append(&timeline);

        // Right: volume.
        let side_end = gtk::Box::builder().spacing(4).halign(gtk::Align::End).build();
        let volume_button = icon_button("onify-audio-volume-high-symbolic", "Mute");
        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        volume.set_width_request(96);
        volume.add_css_class("progress");
        side_end.append(&volume_button);
        side_end.append(&volume);

        widget.append(&side_start);
        widget.append(&center);
        widget.append(&side_end);

        let bar = Rc::new(Self {
            widget,
            side_end,
            like,
            cover,
            title,
            artists,
            shuffle,
            play,
            repeat,
            elapsed,
            progress,
            total,
            volume_button,
            volume,
            state: RefCell::default(),
            tick: RefCell::default(),
            seek_debounce: RefCell::default(),
            dragging: Cell::new(false),
        });
        bar.connect(&prev, &next);
        bar
    }

    fn connect(self: &Rc<Self>, prev: &gtk::Button, next: &gtk::Button) {
        let weak = Rc::downgrade(self);
        let with = move |f: fn(&PlayerBar)| {
            let weak = weak.clone();
            move |_: &gtk::Button| {
                if let Some(bar) = weak.upgrade() {
                    f(&bar);
                }
            }
        };
        self.play.connect_clicked(with(|bar| bar.toggle_play()));
        prev.connect_clicked(with(|_| ctx().with_engine(|e| e.prev())));
        next.connect_clicked(with(|_| ctx().with_engine(|e| e.next())));
        self.shuffle.connect_clicked(with(|bar| {
            let shuffle = !bar.state.borrow().shuffle;
            bar.set_shuffle(shuffle);
            ctx().with_engine(|e| e.set_shuffle(shuffle));
        }));
        self.repeat.connect_clicked(with(|bar| {
            let (context, track) = {
                let st = bar.state.borrow();
                (st.repeat_context, st.repeat_track)
            };
            // Off → repeat all → repeat one → off.
            let (context, track) = match (context, track) {
                (_, true) => (false, false),
                (true, false) => (true, true),
                (false, false) => (true, false),
            };
            bar.set_repeat(context, track);
            ctx().with_engine(|e| e.set_repeat(context, track));
        }));
        self.like.connect_clicked(with(|bar| bar.toggle_like()));
        self.volume_button.connect_clicked(with(|bar| bar.toggle_mute()));

        let open_album = |weak: Weak<Self>| {
            move || {
                if let Some(bar) = weak.upgrade() {
                    let uri = bar.state.borrow().track_uri.clone();
                    if !uri.is_empty() {
                        super::open_album_of(&uri);
                    }
                }
            }
        };
        let click = gtk::GestureClick::new();
        let open = open_album(Rc::downgrade(self));
        click.connect_released(move |_, _, _, _| open());
        self.cover.add_controller(click);
        let open = open_album(Rc::downgrade(self));
        self.title.connect_activate_link(move |_, _| {
            open();
            glib::Propagation::Stop
        });
        self.artists.connect_activate_link(|_, uri| {
            super::open_uri(uri);
            glib::Propagation::Stop
        });

        // Seeking: follow the knob while it's held, seek once on release.
        let legacy = gtk::EventControllerLegacy::new();
        legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        legacy.connect_event(move |_, event| {
            let Some(bar) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match event.event_type() {
                gdk::EventType::ButtonPress | gdk::EventType::TouchBegin => {
                    bar.dragging.set(true);
                    bar.stop_tick();
                }
                gdk::EventType::ButtonRelease | gdk::EventType::TouchEnd => {
                    bar.dragging.set(false);
                    bar.commit_seek();
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        self.progress.add_controller(legacy);

        let weak = Rc::downgrade(self);
        self.progress.connect_change_value(move |_, _, value| {
            let Some(bar) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let duration = bar.state.borrow().duration_ms as f64;
            let ms = value.clamp(0.0, duration) as u32;
            bar.state.borrow_mut().seek_to = Some(ms);
            bar.elapsed.set_label(&format_duration(ms));
            if !bar.dragging.get() {
                // Keyboard or scroll: settle briefly, then seek.
                if let Some(id) = bar.seek_debounce.take() {
                    id.remove();
                }
                let weak = Rc::downgrade(&bar);
                let id = glib::timeout_add_local_once(Duration::from_millis(180), move || {
                    if let Some(bar) = weak.upgrade() {
                        bar.seek_debounce.take();
                        bar.commit_seek();
                    }
                });
                bar.seek_debounce.replace(Some(id));
            }
            glib::Propagation::Proceed
        });

        let weak = Rc::downgrade(self);
        self.volume.connect_change_value(move |_, _, value| {
            if let Some(bar) = weak.upgrade() {
                let value = value.clamp(0.0, 1.0);
                bar.state.borrow_mut().muted_from = None;
                bar.apply_volume(value);
            }
            glib::Propagation::Proceed
        });
    }

    pub fn toggle_play(&self) {
        let (has_track, playing) = {
            let st = self.state.borrow();
            (st.has_track, st.playing)
        };
        if !has_track {
            ctx().with_engine(|e| e.take_over());
            return;
        }
        // Answer the click immediately; the player confirms a moment later.
        self.set_playing(!playing, self.position_ms());
        ctx().with_engine(|e| if playing { e.pause() } else { e.play() });
    }

    fn commit_seek(&self) {
        let Some(ms) = self.state.borrow_mut().seek_to.take() else {
            self.schedule();
            return;
        };
        {
            let mut st = self.state.borrow_mut();
            st.position_ms = ms;
            st.at = now_us();
        }
        ctx().with_engine(|e| e.seek(ms));
        self.schedule();
    }

    fn apply_volume(&self, value: f64) {
        self.state.borrow_mut().volume = value;
        self.volume_button.set_icon_name(match value {
            v if v <= 0.0 => "onify-audio-volume-muted-symbolic",
            v if v < 0.34 => "onify-audio-volume-low-symbolic",
            v if v < 0.67 => "onify-audio-volume-medium-symbolic",
            _ => "onify-audio-volume-high-symbolic",
        });
        ctx().with_engine(|e| e.set_volume((value * u16::MAX as f64).round() as u16));
    }

    fn toggle_mute(&self) {
        let (volume, muted_from) = {
            let st = self.state.borrow();
            (st.volume, st.muted_from)
        };
        let target = match muted_from {
            Some(previous) => previous,
            None if volume > 0.0 => 0.0,
            None => 0.5,
        };
        self.state.borrow_mut().muted_from = (target == 0.0).then_some(volume);
        self.volume.set_value(target);
        self.apply_volume(target);
    }

    fn toggle_like(&self) {
        let (uri, liked) = {
            let st = self.state.borrow();
            (st.track_uri.clone(), !st.liked)
        };
        if uri.is_empty() {
            return;
        }
        self.set_liked(liked);
        super::set_liked(&uri, liked);
    }

    pub fn set_liked(&self, liked: bool) {
        self.state.borrow_mut().liked = liked;
        self.like.set_icon_name(if liked {
            "onify-heart-filled-symbolic"
        } else {
            "onify-heart-symbolic"
        });
        self.like.set_tooltip_text(Some(if liked {
            "Remove from Liked Songs"
        } else {
            "Save to Liked Songs"
        }));
        if liked {
            self.like.add_css_class("active");
        } else {
            self.like.remove_css_class("active");
        }
    }

    pub fn track_uri(&self) -> String {
        self.state.borrow().track_uri.clone()
    }

    pub fn is_playing(&self) -> bool {
        self.state.borrow().playing
    }

    pub fn set_track(&self, now: &NowPlaying) {
        {
            let mut st = self.state.borrow_mut();
            st.has_track = true;
            st.track_uri = now.uri.clone();
            st.duration_ms = now.duration_ms;
            st.position_ms = 0;
            st.at = now_us();
        }
        self.cover.set_url(now.cover(112));
        self.title.set_markup(&format!(
            "<a href=\"album\">{}</a>",
            glib::markup_escape_text(&now.name)
        ));
        self.title.set_tooltip_text(Some(&now.name));
        let artists: Vec<String> = now
            .artists
            .iter()
            .map(|a| {
                let name = glib::markup_escape_text(&a.name);
                if a.uri.is_empty() {
                    name.to_string()
                } else {
                    format!("<a href=\"{}\">{name}</a>", glib::markup_escape_text(&a.uri))
                }
            })
            .collect();
        self.artists.set_markup(&artists.join(", "));
        self.total.set_label(&format_duration(now.duration_ms));
        self.progress.set_range(0.0, now.duration_ms.max(1) as f64);
        self.progress.set_sensitive(true);
        self.like.set_visible(now.uri.starts_with("spotify:track:"));
        self.set_liked(false);
        let uri = now.uri.clone();
        if self.like.is_visible() {
            super::check_liked(&uri);
        }
        self.schedule();
    }

    pub fn set_playing(&self, playing: bool, position_ms: u32) {
        {
            let mut st = self.state.borrow_mut();
            st.playing = playing;
            st.position_ms = position_ms;
            st.at = now_us();
        }
        self.play.set_icon_name(if playing {
            "onify-media-playback-pause-symbolic"
        } else {
            "onify-media-playback-start-symbolic"
        });
        self.play.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
        self.schedule();
    }

    pub fn set_position(&self, position_ms: u32) {
        {
            let mut st = self.state.borrow_mut();
            st.position_ms = position_ms;
            st.at = now_us();
        }
        self.schedule();
    }

    /// Playback ended or moved to another device; Play takes it back.
    pub fn stopped(&self) {
        let position = self.position_ms();
        self.state.borrow_mut().has_track = false;
        self.set_playing(false, position);
    }

    pub fn set_shuffle(&self, shuffle: bool) {
        self.state.borrow_mut().shuffle = shuffle;
        if shuffle {
            self.shuffle.add_css_class("active");
        } else {
            self.shuffle.remove_css_class("active");
        }
    }

    pub fn set_repeat(&self, context: bool, track: bool) {
        {
            let mut st = self.state.borrow_mut();
            st.repeat_context = context;
            st.repeat_track = track;
        }
        self.repeat.set_icon_name(if track {
            "onify-media-playlist-repeat-song-symbolic"
        } else {
            "onify-media-playlist-repeat-symbolic"
        });
        if context || track {
            self.repeat.add_css_class("active");
        } else {
            self.repeat.remove_css_class("active");
        }
    }

    pub fn set_volume(&self, volume: u16) {
        let value = volume as f64 / u16::MAX as f64;
        self.state.borrow_mut().volume = value;
        if !self.volume.has_focus() {
            self.volume.set_value(value);
        }
        self.volume_button.set_icon_name(match value {
            v if v <= 0.0 => "onify-audio-volume-muted-symbolic",
            v if v < 0.34 => "onify-audio-volume-low-symbolic",
            v if v < 0.67 => "onify-audio-volume-medium-symbolic",
            _ => "onify-audio-volume-high-symbolic",
        });
    }

    pub fn position_ms(&self) -> u32 {
        let st = self.state.borrow();
        let mut ms = st.position_ms as i64;
        if st.playing {
            ms += (now_us() - st.at) / 1000;
        }
        ms.clamp(0, st.duration_ms as i64) as u32
    }

    fn stop_tick(&self) {
        if let Some(id) = self.tick.take() {
            id.remove();
        }
    }

    /// Draws the current position and sleeps until it next visibly changes.
    fn schedule(&self) {
        self.stop_tick();
        if self.dragging.get() || self.seek_debounce.borrow().is_some() {
            return;
        }
        let position = self.position_ms();
        let (playing, duration) = {
            let st = self.state.borrow();
            (st.playing, st.duration_ms)
        };
        self.progress.set_value(position as f64);
        self.elapsed.set_label(&format_duration(position));
        if !playing || duration == 0 {
            return;
        }

        let width = self.progress.width().max(1) as f64 * self.progress.scale_factor() as f64;
        let ms_per_px = duration as f64 / width;
        let until_px = ms_per_px - (position as f64 % ms_per_px);
        let until_second = 1000.0 - (position % 1000) as f64;
        let delay = until_px.min(until_second).clamp(4.0, 1000.0) + 1.0;

        let bar = ctx().bar.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(delay as u64), move || {
            bar.tick.take();
            bar.schedule();
        });
        self.tick.replace(Some(id));
    }
}

impl Drop for PlayerBar {
    fn drop(&mut self) {
        self.stop_tick();
    }
}
