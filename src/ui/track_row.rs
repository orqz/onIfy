//! A song row, shared by every track list.

use std::cell::{OnceCell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use super::cover::Cover;
use crate::api::{Track, id_of};

const LEAD_WIDTH: i32 = 32;
const COVER: i32 = 44;

/// The item in a track list model that stands for the page's header, so the
/// header scrolls with the songs while the list stays virtualized.
pub struct HeaderSlot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowMode {
    /// Position, cover, title, album, duration.
    #[default]
    Playlist,
    /// Track number, title, duration.
    Album,
    /// Cover, title, duration.
    Compact,
}

pub struct Parts {
    number: gtk::Label,
    playing: gtk::Image,
    cover: Option<Cover>,
    title: gtk::Label,
    explicit: gtk::Label,
    artists: gtk::Label,
    album: gtk::Label,
    plays: gtk::Label,
    duration: gtk::Label,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TrackRow {
        pub parts: OnceCell<Parts>,
        pub item: RefCell<Option<glib::BoxedAnyObject>>,
        pub menu: OnceCell<gtk::PopoverMenu>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TrackRow {
        const NAME: &'static str = "OnifyTrackRow";
        type Type = super::TrackRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for TrackRow {
        fn dispose(&self) {
            if let Some(menu) = self.menu.get() {
                menu.unparent();
            }
        }
    }
    impl WidgetImpl for TrackRow {}
    impl BoxImpl for TrackRow {}
}

glib::wrapper! {
    pub struct TrackRow(ObjectSubclass<imp::TrackRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

/// How long the pointer rests on a song before it's preloaded.
const PRELOAD_AFTER: std::time::Duration = std::time::Duration::from_millis(300);

thread_local! {
    static ROWS: RefCell<Vec<glib::WeakRef<TrackRow>>> = const { RefCell::new(Vec::new()) };
    static HOVER: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
    static PRELOADED: RefCell<String> = const { RefCell::new(String::new()) };
    static NOW_PLAYING: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Highlights the playing song in every visible list.
pub fn set_now_playing(uri: &str) {
    NOW_PLAYING.with_borrow_mut(|now| {
        now.clear();
        now.push_str(uri);
    });
    ROWS.with_borrow_mut(|rows| {
        rows.retain(|weak| match weak.upgrade() {
            Some(row) => {
                row.refresh_playing();
                true
            }
            None => false,
        })
    });
}

fn dim(label: &gtk::Label) -> &gtk::Label {
    label.add_css_class("dim-label");
    label
}

fn ellipsized(class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .build();
    label.add_css_class(class);
    label
}

impl TrackRow {
    pub fn new(mode: RowMode) -> Self {
        let row: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", 16)
            .build();
        row.add_css_class("track-row");

        let lead = gtk::Box::builder().width_request(LEAD_WIDTH).hexpand(false).build();
        let number = gtk::Label::builder().xalign(1.0).hexpand(true).build();
        number.add_css_class("numeric");
        dim(&number);
        let playing = gtk::Image::from_icon_name("onify-playing-symbolic");
        playing.set_hexpand(true);
        playing.set_halign(gtk::Align::End);
        playing.set_visible(false);
        playing.add_css_class("accent");
        lead.append(&number);
        lead.append(&playing);
        row.append(&lead);

        let cover = (mode != RowMode::Album).then(|| {
            let cover = Cover::new(COVER, 8.0);
            row.append(&cover);
            cover
        });

        let columns = gtk::Box::builder().homogeneous(true).spacing(24).hexpand(true).build();
        let title_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .spacing(2)
            .build();
        let title_line = gtk::Box::builder().spacing(6).build();
        let title = ellipsized("track-title");
        let explicit = gtk::Label::new(Some("E"));
        explicit.add_css_class("explicit");
        explicit.set_valign(gtk::Align::Center);
        title_line.append(&title);
        title_line.append(&explicit);
        let artists = ellipsized("track-artists");
        dim(&artists);
        title_box.append(&title_line);
        title_box.append(&artists);
        columns.append(&title_box);

        let album = ellipsized("track-album");
        dim(&album);
        album.set_visible(mode == RowMode::Playlist);
        columns.append(&album);
        row.append(&columns);

        // Fixed width (up to "9,999,999,999") so columns line up row to row.
        let plays = gtk::Label::builder().xalign(1.0).width_chars(13).visible(false).build();
        plays.add_css_class("numeric");
        plays.add_css_class("track-plays");
        dim(&plays);
        row.append(&plays);

        let duration = gtk::Label::builder().width_chars(6).xalign(1.0).build();
        duration.add_css_class("numeric");
        dim(&duration);
        row.append(&duration);

        let _ = row.imp().parts.set(Parts {
            number,
            playing,
            cover,
            title,
            explicit,
            artists,
            album,
            plays,
            duration,
        });

        // Rest the pointer on a song and it starts loading, so a click plays
        // it at once instead of waiting on Spotify.
        let hover = gtk::EventControllerMotion::new();
        hover.connect_enter(glib::clone!(
            #[weak]
            row,
            move |_, _, _| {
                let weak = row.downgrade();
                let id = glib::timeout_add_local_once(PRELOAD_AFTER, move || {
                    HOVER.with_borrow_mut(|h| h.take());
                    let Some(track) = weak.upgrade().and_then(|r| r.track()) else { return };
                    let playing = NOW_PLAYING.with_borrow(|now| *now == track.uri);
                    let fresh = PRELOADED.with_borrow(|last| *last != track.uri);
                    if track.playable && !playing && fresh {
                        PRELOADED.with_borrow_mut(|last| *last = track.uri.clone());
                        super::ctx().with_engine(|e| e.preload(&track.uri));
                    }
                });
                if let Some(old) = HOVER.with_borrow_mut(|h| h.replace(id)) {
                    old.remove();
                }
            }
        ));
        hover.connect_leave(|_| {
            if let Some(id) = HOVER.with_borrow_mut(|h| h.take()) {
                id.remove();
            }
        });
        row.add_controller(hover);

        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        click.connect_pressed(glib::clone!(
            #[weak]
            row,
            move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                row.show_menu(x, y);
            }
        ));
        row.add_controller(click);

        ROWS.with_borrow_mut(|rows| rows.push(row.downgrade()));
        row
    }

    pub fn bind(&self, item: &glib::BoxedAnyObject, number: u32) {
        let parts = self.imp().parts.get().unwrap();
        {
            let track = item.borrow::<Track>();
            parts.number.set_label(&number.to_string());
            parts.title.set_label(&track.name);
            parts.explicit.set_visible(track.explicit);
            parts.artists.set_label(&track.artist_names());
            parts.album.set_label(&track.album.name);
            parts.duration.set_label(&format_duration(track.duration_ms));
            parts.plays.set_visible(track.plays > 0);
            parts.plays.set_label(&group_digits(track.plays));
            if let Some(cover) = &parts.cover {
                cover.set_url(track.images.pick(64));
            }
            self.set_opacity(if track.playable { 1.0 } else { 0.45 });
        }
        self.imp().item.replace(Some(item.clone()));
        self.refresh_playing();
    }

    pub fn track(&self) -> Option<Track> {
        self.imp().item.borrow().as_ref().map(|i| i.borrow::<Track>().clone())
    }

    fn refresh_playing(&self) {
        let parts = self.imp().parts.get().unwrap();
        let playing = self.imp().item.borrow().as_ref().is_some_and(|item| {
            NOW_PLAYING.with_borrow(|now| !now.is_empty() && item.borrow::<Track>().uri == *now)
        });
        parts.number.set_visible(!playing);
        parts.playing.set_visible(playing);
        if playing {
            self.add_css_class("playing");
        } else {
            self.remove_css_class("playing");
        }
    }

    fn show_menu(&self, x: f64, y: f64) {
        let Some(track) = self.track() else { return };
        let menu = gio::Menu::new();
        let add = |label: &str, action: &str, target: &str| {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
            menu.append_item(&item);
        };
        add("Add to Queue", "app.queue", &track.uri);
        if !track.album.uri.is_empty() {
            add("Go to Album", "app.open", &track.album.uri);
        }
        for artist in track.artists.iter().filter(|a| !a.uri.is_empty()).take(3) {
            add(&format!("Go to {}", artist.name), "app.open", &artist.uri);
        }
        if track.uri.starts_with("spotify:track:") {
            add(
                "Copy Song Link",
                "app.copy-text",
                &format!("https://open.spotify.com/track/{}", id_of(&track.uri)),
            );
        }

        let popover = self.imp().menu.get_or_init(|| {
            let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
            popover.set_parent(self);
            popover.set_has_arrow(false);
            popover.set_halign(gtk::Align::Start);
            popover
        });
        popover.set_menu_model(Some(&menu));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }
}

/// 1234567 → "1,234,567".
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn format_duration(ms: u32) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Column titles lined up with the rows below them.
pub fn column_header(mode: RowMode) -> gtk::Box {
    let header = gtk::Box::builder().spacing(16).build();
    header.add_css_class("track-columns");
    let caption = |text: &str, xalign: f32| {
        let label = gtk::Label::builder().label(text).xalign(xalign).build();
        label.add_css_class("eyebrow");
        label
    };
    let lead = caption("#", 1.0);
    lead.set_width_request(LEAD_WIDTH);
    header.append(&lead);
    let columns = gtk::Box::builder().homogeneous(true).spacing(24).hexpand(true).build();
    let title = caption("Title", 0.0);
    if mode != RowMode::Album {
        title.set_margin_start(COVER + 16);
    }
    columns.append(&title);
    if mode == RowMode::Playlist {
        columns.append(&caption("Album", 0.0));
    }
    header.append(&columns);
    let time = caption("Time", 1.0);
    time.set_width_chars(6);
    header.append(&time);
    header
}

/// A recycling factory: only the rows on screen exist, however long the list.
/// The item at position 0 is a [`HeaderSlot`] that shows `header`.
pub fn factory(mode: RowMode, header: gtk::Widget) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.append(&TrackRow::new(mode));
        item.set_child(Some(&slot));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let (Some(slot), Some(object)) = (
            item.child().and_downcast::<gtk::Box>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let Some(row) = slot.last_child().and_downcast::<TrackRow>() else { return };
        let is_header = object.try_borrow::<HeaderSlot>().is_ok();
        item.set_activatable(!is_header);
        item.set_selectable(!is_header);
        if is_header {
            row.set_visible(false);
            if header.parent().as_ref() != Some(slot.upcast_ref()) {
                if let Some(old) = header.parent().and_downcast::<gtk::Box>() {
                    old.remove(&header);
                }
                slot.prepend(&header);
            }
            return;
        }
        if header.parent().as_ref() == Some(slot.upcast_ref()) {
            slot.remove(&header);
        }
        row.set_visible(true);
        let number = if mode == RowMode::Album {
            object.borrow::<Track>().number
        } else {
            item.position()
        };
        row.bind(&object, number);
    });
    factory
}

/// Wraps tracks as list model items.
pub fn objects(tracks: Vec<Track>) -> Vec<glib::BoxedAnyObject> {
    tracks.into_iter().map(glib::BoxedAnyObject::new).collect()
}

/// A short, non-virtualized list of songs (search results, top tracks).
pub fn short_list(tracks: &[Track], mode: RowMode, on_activate: impl Fn(usize) + 'static) -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("tracks");
    list.set_activate_on_single_click(false);
    list.set_selection_mode(gtk::SelectionMode::Single);
    for (i, track) in tracks.iter().enumerate() {
        let row = TrackRow::new(mode);
        row.bind(&glib::BoxedAnyObject::new(track.clone()), i as u32 + 1);
        list.append(&row);
    }
    list.connect_row_activated(move |_, row| on_activate(row.index() as usize));
    list
}
