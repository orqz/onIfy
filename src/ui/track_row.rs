//! A song row, shared by every track list.

use std::cell::{OnceCell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use super::cover::Cover;
use crate::api::Track;

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
    /// Shown on hover (always once liked): save to or remove from Liked Songs.
    like: gtk::Button,
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
    /// Whether songs are in Liked Songs, as far as onify knows, by URI.
    static LIKED: RefCell<std::collections::HashMap<String, bool>> = RefCell::default();
    /// Songs whose liked state is being asked for.
    static ASKING_LIKED: RefCell<std::collections::HashSet<String>> = RefCell::default();
}

/// Notes that songs are (or aren't) in Liked Songs and updates their rows.
pub fn set_known_liked(uris: impl IntoIterator<Item = String>, liked: bool) {
    let uris: Vec<String> = uris.into_iter().collect();
    LIKED.with_borrow_mut(|known| {
        for uri in &uris {
            known.insert(uri.clone(), liked);
        }
    });
    ROWS.with_borrow(|rows| {
        for row in rows.iter().filter_map(|r| r.upgrade()) {
            if row.track().is_some_and(|t| uris.contains(&t.uri)) {
                row.refresh_like();
            }
        }
    });
}

/// Asks Spotify whether a song is liked, once, when its row is first hovered.
fn want_liked(uri: &str) {
    if !uri.starts_with("spotify:track:") || LIKED.with_borrow(|l| l.contains_key(uri)) {
        return;
    }
    if !ASKING_LIKED.with_borrow_mut(|asking| asking.insert(uri.to_owned())) {
        return;
    }
    let Some(api) = super::ctx().api() else { return };
    let uri = uri.to_owned();
    glib::spawn_future_local(async move {
        let asked = uri.clone();
        let liked = crate::rt::spawn(async move { api.is_liked(&asked).await }).await;
        ASKING_LIKED.with_borrow_mut(|asking| asking.remove(&uri));
        if let Ok(liked) = liked {
            set_known_liked([uri], liked);
        }
    });
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
                format!("<a href=\"{}\">{name}</a>", glib::markup_escape_text(&web_link(&a.uri)))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The open.spotify.com address of a `spotify:kind:id` uri. Links carry
/// these, so GTK's "Copy Link Address" gives something you can share.
pub fn web_link(uri: &str) -> String {
    match uri.strip_prefix("spotify:").and_then(|rest| rest.split_once(':')) {
        Some((kind, id)) => format!("https://open.spotify.com/{kind}/{id}"),
        None => uri.to_owned(),
    }
}

/// Clicking a link in `label` opens that page in onify.
pub fn open_links(label: &gtk::Label) {
    label.connect_activate_link(|_, link| {
        if let Some(uri) = super::uri_of_link(link) {
            super::open_uri(&uri);
        }
        glib::Propagation::Stop
    });
}

/// A song's menu, wherever it's right-clicked. Without the album's uri,
/// "Go to Album" looks it up from the song.
pub fn song_menu(uri: &str, album: Option<&str>, artists: &[crate::api::Named], queue: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    let add = |label: &str, action: &str, target: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
        menu.append_item(&item);
    };
    let spotify_track = uri.starts_with("spotify:track:");
    if queue {
        add("Add to Queue", "app.queue", uri);
    }
    match album.filter(|a| !a.is_empty()) {
        Some(album) => add("Go to Album", "app.open", album),
        None if spotify_track => add("Go to Album", "app.album-of", uri),
        None => {}
    }
    for artist in artists.iter().filter(|a| !a.uri.is_empty()).take(3) {
        add(&format!("Go to {}", artist.name), "app.open", &artist.uri);
    }
    if spotify_track {
        add("Copy Song Link", "app.copy-text", &web_link(uri));
    }
    menu
}

/// Opens `menu` from `popover`, pointing at (x, y) in its parent.
pub fn popup_menu(popover: &gtk::PopoverMenu, menu: &gio::Menu, x: f64, y: f64) {
    popover.set_menu_model(Some(menu));
    popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.popup();
}

/// A small round button on a song row, shown while it's hovered.
fn row_action(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("row-action");
    button.add_css_class("flat");
    button.set_size_request(28, 28);
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(tooltip));
    button
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

        let like = row_action("onify-heart-symbolic", "Save to Liked Songs");
        row.append(&like);
        let duration = duration_label();
        row.append(&duration);
        let more = row_action("onify-view-more-symbolic", "More");
        row.append(&more);

        play.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| row.play()
        ));
        like.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| row.toggle_like()
        ));
        more.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |more| {
                // Under the button, as if it had been right-clicked there.
                if let Some(point) = more.compute_point(&row, &gtk::graphene::Point::new(more.width() as f32 / 2.0, more.height() as f32)) {
                    row.show_menu(point.x() as f64, point.y() as f64);
                }
            }
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
            like,
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
                if let Some(track) = row.track() {
                    want_liked(&track.uri);
                }
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

        // Before the row's labels: an artist link would open GTK's own text
        // menu (cut, copy, paste) instead of the song's.
        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
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
        self.refresh_like();
    }

    /// The heart: filled and lit once liked; only Spotify songs have one.
    fn refresh_like(&self) {
        let parts = self.imp().parts.get().unwrap();
        let Some(uri) = self.track().map(|t| t.uri) else { return };
        let spotify = uri.starts_with("spotify:track:");
        let liked = LIKED.with_borrow(|l| l.get(&uri).copied().unwrap_or(false));
        parts.like.set_sensitive(spotify);
        parts.like.set_opacity(if spotify { 1.0 } else { 0.0 });
        parts.like.set_icon_name(if liked { "onify-heart-filled-symbolic" } else { "onify-heart-symbolic" });
        parts.like.set_tooltip_text(Some(if liked { "Remove from Liked Songs" } else { "Save to Liked Songs" }));
        if liked {
            parts.like.add_css_class("liked");
        } else {
            parts.like.remove_css_class("liked");
        }
    }

    fn toggle_like(&self) {
        let Some(uri) = self.track().map(|t| t.uri) else { return };
        let liked = !LIKED.with_borrow(|l| l.get(&uri).copied().unwrap_or(false));
        super::set_liked(&uri, liked);
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
        let menu = song_menu(&track.uri, Some(&track.album.uri), &track.artists, true);
        let popover = self.imp().menu.get_or_init(|| {
            let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
            popover.set_parent(self);
            popover.set_has_arrow(false);
            popover.set_halign(gtk::Align::Start);
            popover
        });
        popup_menu(popover, &menu, x, y);
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
    // Room for the rows' heart and menu buttons on either side of Time.
    header.append(&gtk::Box::builder().width_request(28).build());
    // As wide as a row's duration: same label, same font, never shown. (The
    // caption's own smaller font made it narrower, shifting the Album column.)
    let time = gtk::Stack::new();
    let sizer = duration_label();
    time.add_child(&sizer);
    let heading = caption("Time", 1.0);
    time.add_child(&heading);
    time.set_visible_child(&heading);
    header.append(&time);
    header.append(&gtk::Box::builder().width_request(28).build());
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
        let number = match mode {
            RowMode::Album => object.borrow::<Track>().number,
            _ => object.borrow::<Track>().position,
        };
        row.bind(&object, number);
    });
    factory
}

/// Wraps tracks as list model items, numbered on from `after` (the songs
/// already listed), so they keep their numbers while a find narrows the list.
pub fn objects(tracks: Vec<Track>, after: u32) -> Vec<glib::BoxedAnyObject> {
    tracks
        .into_iter()
        .zip(after + 1..)
        .map(|(mut track, position)| {
            track.position = position;
            glib::BoxedAnyObject::new(track)
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::web_link;
    use crate::ui::uri_of_link;

    #[test]
    fn links() {
        assert_eq!(web_link("spotify:track:4uLU6hMCjMI75M1A2tKUQC"), "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(web_link("spotify:artist:0hCNtLu0JehylgoiP8L4Gh"), "https://open.spotify.com/artist/0hCNtLu0JehylgoiP8L4Gh");
        for uri in ["spotify:artist:0hCNtLu0JehylgoiP8L4Gh", "spotify:album:2noRn2Aes5aoNVsU6iWThc"] {
            assert_eq!(uri_of_link(&web_link(uri)).as_deref(), Some(uri));
        }
        assert_eq!(uri_of_link("https://open.spotify.com/intl-de/track/abc?si=x").as_deref(), Some("spotify:track:abc"));
    }
}
