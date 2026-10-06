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
    /// Number, playing indicator or ▶, crossfading between them.
    lead: gtk::Stack,
    /// Album pages, artist pages and search show play counts in their own
    /// column; playlists show the selected song's after its artists, so their
    /// columns never shift.
    plays_column: bool,
    /// Album pages keep the column on every row, under its heading.
    plays_always: bool,
    number: gtk::Label,
    cover: Option<Cover>,
    title: gtk::Label,
    explicit: gtk::Label,
    artists: gtk::Label,
    inline_plays: gtk::Label,
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
        /// Position in a list view, for its play button.
        pub position: std::cell::Cell<u32>,
        pub hovered: std::cell::Cell<bool>,
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
const PRELOAD_AFTER: std::time::Duration = std::time::Duration::from_millis(600);
/// At most one hover preload this often: each one asks Spotify for the song's
/// key, and too many key requests get refused for a while.
const PRELOAD_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

thread_local! {
    /// Album column labels and headings, hidden when the window is compact.
    static ALBUM_COLUMNS: RefCell<Vec<glib::WeakRef<gtk::Widget>>> = const { RefCell::new(Vec::new()) };
    static ALBUM_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    static ROWS: RefCell<Vec<glib::WeakRef<TrackRow>>> = const { RefCell::new(Vec::new()) };
    static HOVER: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
    static PRELOADED: RefCell<String> = const { RefCell::new(String::new()) };
    static LAST_PRELOAD: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
    static NOW_PLAYING: RefCell<String> = const { RefCell::new(String::new()) };
    /// Play counts fetched for selected songs, by URI.
    static PLAYS: RefCell<std::collections::HashMap<String, u64>> = RefCell::default();
}

/// Shows a selected song's play count, asking Spotify for it if its list
/// didn't come with one.
pub fn want_plays(track: &Track) {
    if track.plays > 0 || !track.uri.starts_with("spotify:track:") {
        return;
    }
    if PLAYS.with_borrow(|p| p.contains_key(&track.uri)) {
        return;
    }
    let Some(api) = super::ctx().api() else { return };
    let uri = track.uri.clone();
    glib::spawn_future_local(async move {
        let asked = uri.clone();
        let Ok(plays) = crate::rt::spawn(async move { api.plays(&asked).await }).await else { return };
        PLAYS.with_borrow_mut(|p| p.insert(uri.clone(), plays));
        ROWS.with_borrow(|rows| {
            for row in rows.iter().filter_map(|r| r.upgrade()) {
                if row.track().is_some_and(|t| t.uri == uri) {
                    row.show_plays(plays);
                }
            }
        });
    });
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

/// Artist names as markup, each linking to its page (local files' artists
/// have none and stay plain). Pair with [`open_links`].
pub fn artist_links(artists: &[crate::api::Named]) -> String {
    artists
        .iter()
        .map(|a| {
            let name = glib::markup_escape_text(&a.name);
            if a.uri.is_empty() {
                name.to_string()
            } else {
                format!("<a href=\"{}\">{name}</a>", glib::markup_escape_text(&a.uri))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Clicking a link in `label` opens that page in onIfy.
pub fn open_links(label: &gtk::Label) {
    label.connect_activate_link(|_, uri| {
        super::open_uri(uri);
        glib::Propagation::Stop
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

        let lead = gtk::Stack::builder()
            .width_request(LEAD_WIDTH)
            .hexpand(false)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(140)
            .build();
        lead.set_overflow(gtk::Overflow::Visible);
        let number = gtk::Label::builder().xalign(1.0).hexpand(true).build();
        number.add_css_class("numeric");
        dim(&number);
        let playing = gtk::Image::from_icon_name("onify-playing-symbolic");
        playing.set_hexpand(true);
        playing.set_halign(gtk::Align::End);
        playing.add_css_class("accent");
        // Rows don't play on a click (that only selects); this button does.
        let play = gtk::Button::from_icon_name("onify-media-playback-start-symbolic");
        play.add_css_class("row-play");
        play.add_css_class("flat");
        // A fixed circle that fits the number's slot (theme "circular"
        // buttons are wider than the slot and got cut off).
        play.set_size_request(28, 28);
        play.set_tooltip_text(Some("Play"));
        play.set_hexpand(true);
        play.set_halign(gtk::Align::End);
        play.set_valign(gtk::Align::Center);
        lead.add_named(&number, Some("number"));
        lead.add_named(&playing, Some("playing"));
        lead.add_named(&play, Some("play"));
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
        open_links(&artists);
        let inline_plays = gtk::Label::builder().visible(false).build();
        inline_plays.add_css_class("track-artists");
        inline_plays.add_css_class("track-plays-inline");
        inline_plays.add_css_class("numeric");
        dim(&inline_plays);
        let artist_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        artist_line.append(&artists);
        artist_line.append(&inline_plays);
        title_box.append(&title_line);
        title_box.append(&artist_line);
        columns.append(&title_box);

        let album = ellipsized("track-album");
        dim(&album);
        if mode == RowMode::Playlist {
            album_column(&album);
        } else {
            album.set_visible(false);
        }
        columns.append(&album);
        row.append(&columns);

        let plays = plays_label();
        row.append(&plays);

        let duration = duration_label();
        row.append(&duration);

        play.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| row.play()
        ));
        let _ = row.imp().parts.set(Parts {
            lead,
            plays_column: mode != RowMode::Playlist,
            plays_always: mode == RowMode::Album,
            number,
            cover,
            title,
            explicit,
            artists,
            inline_plays,
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
                row.imp().hovered.set(true);
                row.refresh_playing();
                let weak = row.downgrade();
                let id = glib::timeout_add_local_once(PRELOAD_AFTER, move || {
                    HOVER.with_borrow_mut(|h| h.take());
                    let Some(track) = weak.upgrade().and_then(|r| r.track()) else { return };
                    let playing = NOW_PLAYING.with_borrow(|now| *now == track.uri);
                    let fresh = PRELOADED.with_borrow(|last| *last != track.uri);
                    let wanted = super::ctx().settings.borrow().hover_preload;
                    let rested = LAST_PRELOAD.get().is_none_or(|t| t.elapsed() >= PRELOAD_EVERY);
                    if track.playable && !playing && fresh && wanted && rested {
                        LAST_PRELOAD.set(Some(std::time::Instant::now()));
                        PRELOADED.with_borrow_mut(|last| *last = track.uri.clone());
                        super::ctx().with_engine(|e| e.preload(&track.uri));
                    }
                });
                if let Some(old) = HOVER.with_borrow_mut(|h| h.replace(id)) {
                    old.remove();
                }
            }
        ));
        hover.connect_leave(glib::clone!(
            #[weak]
            row,
            move |_| {
                row.imp().hovered.set(false);
                row.refresh_playing();
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
            parts.artists.set_markup(&artist_links(&track.artists));
            parts.album.set_label(&track.album.name);
            parts.duration.set_label(&format_duration(track.duration_ms));
            if let Some(cover) = &parts.cover {
                cover.set_url(track.images.pick(64));
            }
            self.set_opacity(if track.playable { 1.0 } else { 0.45 });
        }
        self.imp().item.replace(Some(item.clone()));
        let plays = {
            let track = item.borrow::<Track>();
            match track.plays {
                0 => PLAYS.with_borrow(|p| p.get(&track.uri).copied()).unwrap_or(0),
                n => n,
            }
        };
        self.show_plays(plays);
        self.refresh_playing();
    }

    /// Fills in the play count (0 for none); CSS shows it for the selected row.
    fn show_plays(&self, plays: u64) {
        let parts = self.imp().parts.get().unwrap();
        let known = plays > 0;
        parts.plays.set_visible(parts.plays_column && (known || parts.plays_always));
        parts.plays.set_label(&if known { group_digits(plays) } else { String::new() });
        parts.inline_plays.set_visible(!parts.plays_column && known);
        parts.inline_plays.set_label(&format!("  ·  {} plays", group_digits(plays)));
    }

    pub fn set_position(&self, position: u32) {
        self.imp().position.set(position);
    }

    /// Plays this row's song, in its list's context, as a double-click would.
    fn play(&self) {
        if let Some(list_row) = self.parent().and_downcast::<gtk::ListBoxRow>() {
            list_row.activate();
        } else {
            let _ = self.activate_action("list.activate-item", Some(&self.imp().position.get().to_variant()));
        }
    }

    pub fn track(&self) -> Option<Track> {
        self.imp().item.borrow().as_ref().map(|i| i.borrow::<Track>().clone())
    }

    fn refresh_playing(&self) {
        let parts = self.imp().parts.get().unwrap();
        let playing = self.imp().item.borrow().as_ref().is_some_and(|item| {
            NOW_PLAYING.with_borrow(|now| !now.is_empty() && item.borrow::<Track>().uri == *now)
        });
        let hovered = self.imp().hovered.get();
        parts.lead.set_visible_child_name(match (hovered, playing) {
            (true, _) => "play",
            (false, true) => "playing",
            (false, false) => "number",
        });
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

fn album_column(widget: &impl IsA<gtk::Widget>) {
    widget.set_visible(ALBUM_SHOWN.get());
    ALBUM_COLUMNS.with_borrow_mut(|c| c.push(widget.upcast_ref::<gtk::Widget>().downgrade()));
}

/// Shows or hides the album column in every playlist.
pub fn set_album_column(shown: bool) {
    ALBUM_SHOWN.set(shown);
    ALBUM_COLUMNS.with_borrow_mut(|columns| {
        columns.retain(|c| c.upgrade().is_some());
        for column in columns.iter().filter_map(|c| c.upgrade()) {
            column.set_visible(shown);
        }
    });
}

/// Fixed width (up to "9,999,999,999") so columns line up row to row.
fn plays_label() -> gtk::Label {
    let plays = gtk::Label::builder().xalign(1.0).width_chars(13).visible(false).build();
    plays.add_css_class("numeric");
    plays.add_css_class("track-plays");
    dim(&plays);
    plays
}

fn duration_label() -> gtk::Label {
    let duration = gtk::Label::builder().width_chars(6).xalign(1.0).build();
    duration.add_css_class("numeric");
    dim(&duration);
    duration
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
    // Same pieces as a row, so the columns line up: a cover-sized spacer,
    // then the evenly split title and album columns.
    if mode != RowMode::Album {
        header.append(&gtk::Box::builder().width_request(COVER).build());
    }
    let columns = gtk::Box::builder().homogeneous(true).spacing(24).hexpand(true).build();
    let title = caption("Title", 0.0);
    let plays = (mode == RowMode::Album).then(|| {
        // As wide as the rows' play counts, sized the same way as Time below.
        let slot = gtk::Stack::new();
        let sizer = plays_label();
        sizer.set_visible(true);
        slot.add_child(&sizer);
        let heading = caption("Plays", 1.0);
        heading.set_margin_end(24);
        slot.add_child(&heading);
        slot.set_visible_child(&heading);
        slot
    });
    columns.append(&title);
    if mode == RowMode::Playlist {
        let heading = caption("Album", 0.0);
        album_column(&heading);
        columns.append(&heading);
    }
    header.append(&columns);
    if let Some(plays) = &plays {
        header.append(plays);
    }
    // As wide as a row's duration: same label, same font, never shown. (The
    // caption's own smaller font made it narrower, shifting the Album column.)
    let time = gtk::Stack::new();
    let sizer = duration_label();
    time.add_child(&sizer);
    let heading = caption("Time", 1.0);
    time.add_child(&heading);
    time.set_visible_child(&heading);
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
        row.set_position(item.position());
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
    list.connect_row_selected(|_, row| {
        if let Some(track) = row.and_then(|r| r.child()).and_downcast::<TrackRow>().and_then(|r| r.track()) {
            want_plays(&track);
        }
    });
    list
}
