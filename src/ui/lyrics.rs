//! Lyrics, like Spotify's: big bold lines, the current one lit and kept in the
//! middle as the song plays. Click a line to jump to it.
//!
//! It only wakes when the next line is due (or after a seek, pause or song
//! change), so it costs nothing between lines.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{glib, graphene};

use super::ctx;
use crate::api::{Lyrics, id_of, join_names};
use crate::rt;

pub const TAG: &str = "lyrics";
/// Leave the scroll position alone this long after the user scrolls.
const HANDS_OFF_US: i64 = 3_000_000;

struct View {
    page: adw::NavigationPage,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    status: adw::StatusPage,
    scroller: gtk::ScrolledWindow,
    lines: gtk::Box,
    labels: RefCell<Vec<gtk::Label>>,
    lyrics: RefCell<Option<Lyrics>>,
    /// The song whose lyrics are shown or loading.
    track: RefCell<String>,
    current: Cell<Option<usize>>,
    tick: RefCell<Option<glib::SourceId>>,
    scroll: RefCell<Option<adw::TimedAnimation>>,
    user_scrolled_at: Cell<i64>,
}

thread_local! {
    static VIEW: RefCell<Option<Rc<View>>> = const { RefCell::new(None) };
}

fn view() -> Rc<View> {
    if let Some(view) = VIEW.with_borrow(|v| v.clone()) {
        return view;
    }
    let view = Rc::new(build());
    VIEW.with_borrow_mut(|v| *v = Some(view.clone()));
    view
}

fn build() -> View {
    let lines = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    lines.add_css_class("lyrics");
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&lines)
        .build();
    // Ahead of the wheel glide, which takes the scroll for itself.
    let scrolled = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scrolled.set_propagation_phase(gtk::PropagationPhase::Capture);
    scrolled.connect_scroll(|_, _, _| {
        view().user_scrolled_at.set(glib::monotonic_time());
        glib::Propagation::Proceed
    });
    scroller.add_controller(scrolled);
    super::pages::glide_wheel(&scroller);

    let status = adw::StatusPage::builder()
        .icon_name("onify-lyrics-symbolic")
        .vexpand(true)
        .build();
    let loading = adw::Spinner::new();
    loading.set_size_request(36, 36);
    loading.set_halign(gtk::Align::Center);
    loading.set_valign(gtk::Align::Center);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(180)
        .build();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&status, Some("none"));
    stack.add_named(&scroller, Some("lines"));

    let title = adw::WindowTitle::new("Lyrics", "");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    let page = super::pages::page("Lyrics", TAG, &stack, &header);
    page.connect_shown(|_| {
        let track = ctx().now.borrow().as_ref().map(|n| n.uri.clone()).unwrap_or_default();
        load(&track);
    });
    page.connect_hidden(|_| stop_tick(&view()));

    View {
        page,
        title,
        stack,
        status,
        scroller,
        lines,
        labels: RefCell::default(),
        lyrics: RefCell::default(),
        track: RefCell::default(),
        current: Cell::new(None),
        tick: RefCell::default(),
        scroll: RefCell::default(),
        user_scrolled_at: Cell::new(0),
    }
}

/// The lyrics page, ready to be pushed.
pub fn page() -> adw::NavigationPage {
    view().page.clone()
}

fn is_open() -> bool {
    VIEW.with_borrow(|v| v.as_ref().is_some_and(|v| v.page.is_mapped()))
}

pub fn track_changed(uri: &str) {
    if is_open() {
        load(uri);
    }
}

/// Re-check the current line after a seek, pause or resume.
pub fn resync() {
    if is_open() {
        sync(&view());
    }
}

fn show_status(view: &View, title: &str, description: &str) {
    view.status.set_title(title);
    view.status.set_description(Some(description));
    view.stack.set_visible_child_name("none");
}

fn load(uri: &str) {
    let view = view();
    if let Some(now) = ctx().now.borrow().as_ref().filter(|n| n.uri == uri) {
        view.title.set_title(&now.name);
        view.title.set_subtitle(&join_names(&now.artists));
    }
    if *view.track.borrow() == uri && view.lyrics.borrow().is_some() {
        sync(&view);
        return;
    }
    view.track.replace(uri.to_owned());
    view.lyrics.take();
    stop_tick(&view);
    if uri.is_empty() {
        show_status(&view, "Nothing playing", "Play a song to see its lyrics here.");
        return;
    }
    if !uri.starts_with("spotify:track:") {
        show_status(&view, "No lyrics", "Lyrics are only available for songs on Spotify.");
        return;
    }
    let Some(api) = ctx().api() else { return };
    view.stack.set_visible_child_name("loading");
    let wanted = uri.to_owned();
    glib::spawn_future_local(async move {
        let track = wanted.clone();
        let result = rt::spawn(async move { api.lyrics(&track).await }).await;
        let view = self::view();
        if *view.track.borrow() != wanted {
            return;
        }
        match result {
            Ok(Some(lyrics)) => fill(&view, lyrics),
            Ok(None) => show_status(&view, "No lyrics", "Spotify doesn't have lyrics for this song."),
            Err(e) => {
                log::warn!("lyrics for {} failed: {e}", id_of(&wanted));
                show_status(&view, "Couldn't load lyrics", "Check your connection and try again.");
            }
        }
    });
}

fn fill(view: &Rc<View>, lyrics: Lyrics) {
    while let Some(child) = view.lines.first_child() {
        view.lines.remove(&child);
    }
    let mut labels = Vec::with_capacity(lyrics.lines.len());
    for (start, words) in &lyrics.lines {
        let label = gtk::Label::builder()
            .label(if words.trim().is_empty() { "♪" } else { words.as_str() })
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        label.add_css_class("lyric-line");
        if lyrics.synced {
            let start = *start;
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                view_seek(start);
            });
            label.add_controller(click);
            label.set_cursor_from_name(Some("pointer"));
        } else {
            label.add_css_class("unsynced");
        }
        view.lines.append(&label);
        labels.push(label);
    }
    if !lyrics.provider.is_empty() {
        let credit = gtk::Label::builder()
            .label(format!("Lyrics provided by {}", lyrics.provider))
            .xalign(0.0)
            .build();
        credit.add_css_class("lyrics-credit");
        view.lines.append(&credit);
    }
    view.labels.replace(labels);
    view.lyrics.replace(Some(lyrics));
    view.current.set(None);
    view.scroller.vadjustment().set_value(0.0);
    view.stack.set_visible_child_name("lines");
    sync(view);
}

fn view_seek(ms: u32) {
    let view = view();
    // A click is a request to follow along again.
    view.user_scrolled_at.set(0);
    super::seek_to(ms as i64);
}

fn stop_tick(view: &View) {
    if let Some(id) = view.tick.take() {
        id.remove();
    }
}

/// Lights the line being sung and schedules the next check for when the
/// following line starts.
fn sync(view: &Rc<View>) {
    stop_tick(view);
    let lyrics = view.lyrics.borrow();
    let Some(lyrics) = lyrics.as_ref().filter(|l| l.synced) else { return };
    let bar = ctx().bar.clone();
    let position = bar.position_ms();
    let index = lyrics.lines.partition_point(|(start, _)| *start <= position).checked_sub(1);

    if index != view.current.get() {
        view.current.set(index);
        for (i, label) in view.labels.borrow().iter().enumerate() {
            label.remove_css_class("current");
            label.remove_css_class("past");
            match index {
                Some(at) if i == at => label.add_css_class("current"),
                Some(at) if i < at => label.add_css_class("past"),
                _ => {}
            }
        }
        if let Some(at) = index {
            scroll_to(view, at);
        }
    }

    if bar.is_playing() {
        let next = lyrics.lines.get(index.map_or(0, |i| i + 1)).map(|l| l.0);
        // Wake for the next line; check again within a second regardless, in
        // case the clock and the player drift apart.
        let wait = next.map_or(1000, |n| n.saturating_sub(position).clamp(20, 1000));
        let id = glib::timeout_add_local_once(Duration::from_millis(wait as u64), || {
            let view = self::view();
            view.tick.take();
            sync(&view);
        });
        view.tick.replace(Some(id));
    }
}

/// Glides the scroll position so line `index` sits a little above centre.
fn scroll_to(view: &Rc<View>, index: usize) {
    if glib::monotonic_time() - view.user_scrolled_at.get() < HANDS_OFF_US {
        return;
    }
    let Some(label) = view.labels.borrow().get(index).cloned() else { return };
    let Some(top) = label.compute_point(&view.lines, &graphene::Point::new(0.0, 0.0)) else { return };
    let adjustment = view.scroller.vadjustment();
    let middle = top.y() as f64 + label.height() as f64 / 2.0;
    let target = (middle - adjustment.page_size() * 0.42).clamp(0.0, (adjustment.upper() - adjustment.page_size()).max(0.0));
    if let Some(animation) = view.scroll.take() {
        animation.pause();
    }
    let target_fn = adw::CallbackAnimationTarget::new(glib::clone!(
        #[weak]
        adjustment,
        move |value| adjustment.set_value(value)
    ));
    let animation = adw::TimedAnimation::new(&view.scroller, adjustment.value(), target, 450, target_fn);
    animation.set_easing(adw::Easing::EaseOutCubic);
    animation.play();
    view.scroll.replace(Some(animation));
}
