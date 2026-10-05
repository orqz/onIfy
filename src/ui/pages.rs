//! The content pages: home, search, track lists and artists.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::cover::Cover;
use super::track_row::{HeaderSlot, RowMode, column_header, factory, objects, short_list};
use super::{ctx, open_card};
use crate::api::{Card, Chunk, Header, Home, Images, Kind, SearchResults, Track, id_of};
use crate::rt;

/// Horizontal page margin; everything lines up on it.
const GUTTER: i32 = 36;
/// Room under each page so its end can scroll clear of the floating player
/// (capsule plus its margins; see `glass.player-bar` in style.css).
const BAR_CLEARANCE: i32 = 116;

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn page(title: &str, tag: &str, content: &impl IsA<gtk::Widget>, header: &adw::HeaderBar) -> adw::NavigationPage {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(header);
    toolbar.set_content(Some(content));
    toolbar.set_extend_content_to_top_edge(false);
    adw::NavigationPage::with_tag(&toolbar, title, tag)
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

/// A header bar whose title fades in once the page's own title scrolls away.
fn fading_header(title: &str, adjustment: &gtk::Adjustment, threshold: f64) -> adw::HeaderBar {
    let window_title = adw::WindowTitle::new(title, "");
    window_title.add_css_class("fade-title");
    window_title.set_opacity(0.0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&window_title));
    adjustment.connect_value_changed(glib::clone!(
        #[weak]
        window_title,
        move |adj| {
            let shown = adj.value() > threshold;
            window_title.set_opacity(if shown { 1.0 } else { 0.0 });
        }
    ));
    header
}

fn spinner() -> gtk::Widget {
    let spinner = adw::Spinner::new();
    spinner.set_size_request(36, 36);
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_vexpand(true);
    spinner.set_margin_top(48);
    spinner.upcast()
}

fn play_button(tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name("onify-media-playback-start-symbolic");
    button.add_css_class("hero-play");
    button.add_css_class("circular");
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(gtk::Align::Center);
    button
}

fn glass_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("glass-button");
    button.add_css_class("circular");
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(gtk::Align::Center);
    button
}

/// The big cover, title and actions at the top of a collection page.
struct Hero {
    widget: gtk::Box,
    art: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    actions: gtk::Box,
}

fn hero(eyebrow: &str, title: &str, art: &impl IsA<gtk::Widget>) -> Hero {
    let widget = gtk::Box::builder()
        .spacing(32)
        .margin_top(12)
        .margin_bottom(28)
        .margin_start(GUTTER)
        .margin_end(GUTTER)
        .build();
    widget.add_css_class("hero");
    let art_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    art_box.add_css_class("hero-art");
    art_box.set_valign(gtk::Align::Center);
    art_box.append(art);
    widget.append(&art_box);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::End)
        .spacing(6)
        .hexpand(true)
        .build();
    info.append(&label(eyebrow, &["eyebrow"]));
    let title_label = label(title, &["title-hero"]);
    title_label.set_wrap(true);
    title_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title_label.set_lines(2);
    info.append(&title_label);
    let subtitle = label("", &["meta"]);
    subtitle.set_wrap(true);
    subtitle.set_lines(2);
    info.append(&subtitle);
    let actions = gtk::Box::builder().spacing(14).margin_top(18).build();
    info.append(&actions);
    widget.append(&info);
    Hero {
        widget,
        art: art_box,
        title: title_label,
        subtitle,
        actions,
    }
}

pub fn card(card: &Card) -> gtk::Widget {
    let button = gtk::Button::new();
    button.add_css_class("card-tile");
    button.add_css_class("flat");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .width_request(CARD)
        .build();
    let cover = if card.kind == Kind::Artist {
        Cover::round(CARD)
    } else {
        Cover::new(CARD, 14.0)
    };
    cover.set_url(card.images.pick(300));
    cover.set_margin_bottom(10);
    content.append(&cover);
    let name = label(&card.name, &["card-title"]);
    name.set_max_width_chars(1);
    name.set_hexpand(true);
    let subtitle = label(&card.subtitle, &["card-subtitle"]);
    subtitle.set_max_width_chars(1);
    subtitle.set_wrap(true);
    subtitle.set_lines(2);
    if card.kind == Kind::Artist {
        name.set_xalign(0.5);
        subtitle.set_xalign(0.5);
    }
    content.append(&name);
    content.append(&subtitle);
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(&card.name));
    let card = card.clone();
    button.connect_clicked(move |_| open_card(&card));
    button.upcast()
}

const CARD: i32 = 164;

/// A titled row of cards that scrolls sideways.
fn carousel(title: &str, cards: &[Card]) -> Option<gtk::Box> {
    if cards.is_empty() {
        return None;
    }
    let section = vbox(12);
    let heading = label(title, &["title-section"]);
    heading.set_margin_start(GUTTER);
    heading.set_margin_end(GUTTER);
    section.append(&heading);
    let row = gtk::Box::builder()
        .spacing(4)
        .margin_start(GUTTER - 12)
        .margin_end(GUTTER - 12)
        .build();
    for c in cards {
        row.append(&card(c));
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&row)
        .build();
    scroller.add_css_class("carousel");
    section.append(&scroller);
    Some(section)
}

/// Home's quick picks: compact tiles in a grid.
fn shortcuts(cards: &[Card]) -> gtk::FlowBox {
    let grid = gtk::FlowBox::builder()
        .homogeneous(true)
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(12)
        .row_spacing(12)
        .margin_start(GUTTER)
        .margin_end(GUTTER)
        .build();
    for c in cards {
        let button = gtk::Button::new();
        button.add_css_class("shortcut");
        let content = gtk::Box::builder().spacing(14).build();
        let cover = if c.kind == Kind::Artist { Cover::round(48) } else { Cover::new(48, 8.0) };
        cover.set_url(c.images.pick(100));
        content.append(&cover);
        let name = label(&c.name, &["shortcut-title"]);
        name.set_max_width_chars(1);
        name.set_hexpand(true);
        content.append(&name);
        button.set_child(Some(&content));
        let c = c.clone();
        button.connect_clicked(move |_| open_card(&c));
        grid.append(&button);
    }
    let mut child = grid.first_child();
    while let Some(c) = child {
        c.set_focusable(false);
        child = c.next_sibling();
    }
    grid
}

fn greeting() -> &'static str {
    let hour = glib::DateTime::now_local().map(|d| d.hour()).unwrap_or(12);
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

pub struct HomePage {
    pub page: adw::NavigationPage,
    body: gtk::Box,
}

impl HomePage {
    pub fn new() -> Self {
        let body = vbox(40);
        body.set_margin_top(8);
        body.set_margin_bottom(BAR_CLEARANCE);
        let title = label(greeting(), &["title-hero"]);
        title.set_margin_start(GUTTER);
        body.append(&title);
        body.append(&spinner());
        let scroller = scrolled(&body);
        let header = fading_header("Home", &scroller.vadjustment(), 60.0);
        let page = page("Home", "home", &scroller, &header);
        Self { page, body }
    }

    pub fn fill(&self, home: &Home) {
        while let Some(child) = self.body.last_child() {
            if child == self.body.first_child().unwrap() {
                break;
            }
            self.body.remove(&child);
        }
        let mut sections = home.sections.iter();
        // The first row is usually recent picks: show it as quick tiles.
        if let Some((_, cards)) = home.sections.first() {
            let picks: Vec<Card> = cards.iter().take(8).cloned().collect();
            self.body.append(&shortcuts(&picks));
            if cards.len() <= 8 {
                sections.next();
            }
        }
        for (title, cards) in sections {
            if let Some(section) = carousel(title, cards) {
                self.body.append(&section);
            }
        }
    }
}

pub struct SearchPage {
    pub page: adw::NavigationPage,
    pub entry: gtk::SearchEntry,
}

impl SearchPage {
    pub fn new() -> Self {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("What do you want to play?")
            .hexpand(true)
            .search_delay(160)
            .build();
        entry.add_css_class("search-pill");
        let clamp = adw::Clamp::builder().maximum_size(620).child(&entry).build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&clamp));

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(140)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("onify-system-search-symbolic")
            .title("Find your next favourite")
            .description("Songs, artists, albums and playlists")
            .build();
        let results = vbox(40);
        results.set_margin_top(12);
        results.set_margin_bottom(BAR_CLEARANCE);
        let nothing = adw::StatusPage::builder()
            .icon_name("onify-system-search-symbolic")
            .title("No results")
            .build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scrolled(&results), Some("results"));
        stack.add_named(&nothing, Some("nothing"));
        stack.add_named(&spinner(), Some("loading"));
        let page = page("Search", "search", &stack, &header);

        let generation = Rc::new(Cell::new(0u64));
        let cache: Rc<RefCell<HashMap<String, Rc<SearchResults>>>> = Rc::default();
        entry.connect_search_changed(move |entry| {
            let query = entry.text().trim().to_owned();
            let current = generation.get() + 1;
            generation.set(current);
            if query.is_empty() {
                stack.set_visible_child_name("empty");
                return;
            }
            if let Some(found) = cache.borrow().get(&query) {
                show_results(&stack, &results, found);
                return;
            }
            if stack.visible_child_name().as_deref() != Some("results") {
                stack.set_visible_child_name("loading");
            }
            let Some(api) = ctx().api() else { return };
            let (generation, cache, stack, results) =
                (generation.clone(), cache.clone(), stack.clone(), results.clone());
            glib::spawn_future_local(async move {
                let q = query.clone();
                let found = rt::spawn(async move { api.search(&q).await }).await;
                if generation.get() != current {
                    return;
                }
                match found {
                    Ok(found) => {
                        let found = Rc::new(found);
                        cache.borrow_mut().insert(query, found.clone());
                        show_results(&stack, &results, &found);
                    }
                    Err(e) => {
                        stack.set_visible_child_name("empty");
                        super::toast(&format!("Search failed: {e}"));
                    }
                }
            });
        });

        Self { page, entry }
    }
}

/// The best match, as a big glass tile.
fn top_result(card: &Card) -> gtk::Widget {
    let button = gtk::Button::new();
    button.add_css_class("top-result");
    let content = vbox(14);
    let cover = if card.kind == Kind::Artist { Cover::round(104) } else { Cover::new(104, 12.0) };
    cover.set_url(card.images.pick(300));
    content.append(&cover);
    let name = label(&card.name, &["title-page"]);
    name.set_max_width_chars(1);
    content.append(&name);
    let kind = match card.kind {
        Kind::Artist => "Artist",
        Kind::Album => "Album",
        _ => "Playlist",
    };
    let pill = label(kind, &["pill-label"]);
    pill.set_halign(gtk::Align::Start);
    content.append(&pill);
    button.set_child(Some(&content));
    let card = card.clone();
    button.connect_clicked(move |_| open_card(&card));
    button.upcast()
}

fn show_results(stack: &gtk::Stack, results: &gtk::Box, found: &SearchResults) {
    while let Some(child) = results.first_child() {
        results.remove(&child);
    }
    if found.tracks.is_empty() && found.artists.is_empty() && found.albums.is_empty() && found.playlists.is_empty() {
        stack.set_visible_child_name("nothing");
        return;
    }

    let top = gtk::Box::builder()
        .spacing(28)
        .margin_start(GUTTER)
        .margin_end(GUTTER)
        .build();
    let best = found.artists.first().or(found.albums.first()).or(found.playlists.first());
    if let Some(best) = best {
        let column = vbox(12);
        column.append(&label("Top result", &["title-section"]));
        let tile = top_result(best);
        tile.set_vexpand(true);
        column.append(&tile);
        column.set_size_request(340, -1);
        top.append(&column);
    }
    if !found.tracks.is_empty() {
        let column = vbox(12);
        column.set_hexpand(true);
        column.append(&label("Songs", &["title-section"]));
        let uris: Vec<String> = found.tracks.iter().map(|t| t.uri.clone()).collect();
        let tracks: Vec<Track> = found.tracks.iter().take(5).cloned().collect();
        let list = short_list(&tracks, RowMode::Compact, move |i| {
            ctx().with_engine(|e| e.play_tracks(uris.clone(), i));
        });
        list.set_margin_start(-12);
        column.append(&list);
        top.append(&column);
    }
    results.append(&top);

    for (title, cards) in [
        ("Artists", &found.artists),
        ("Albums", &found.albums),
        ("Playlists", &found.playlists),
    ] {
        if let Some(section) = carousel(title, cards) {
            results.append(&section);
        }
    }
    stack.set_visible_child_name("results");
}

/// A loaded (or loading) track list, kept for instant revisits.
pub struct Loaded {
    pub store: gio::ListStore,
    pub header: RefCell<Option<Header>>,
    pub listeners: RefCell<Vec<Box<dyn Fn(&Header)>>>,
}

fn load_tracks(kind: Kind, uri: &str) -> Rc<Loaded> {
    let key = if kind == Kind::Liked { "liked".to_owned() } else { uri.to_owned() };
    if let Some(loaded) = ctx().stores.borrow().get(&key) {
        return loaded.clone();
    }
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    store.append(&glib::BoxedAnyObject::new(HeaderSlot));
    let loaded = Rc::new(Loaded {
        store,
        header: RefCell::default(),
        listeners: RefCell::default(),
    });
    ctx().stores.borrow_mut().insert(key.clone(), loaded.clone());

    if kind == Kind::Local {
        scan_local(&loaded);
        return loaded;
    }
    let Some(api) = ctx().api() else { return loaded };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    rt::handle().spawn(api.stream_tracks(kind, id_of(uri).to_owned(), tx));
    let weak = Rc::downgrade(&loaded);
    glib::spawn_future_local(async move {
        while let Some(chunk) = rx.recv().await {
            let Some(loaded) = weak.upgrade() else { return };
            match chunk {
                Chunk::Header(header) => {
                    // The header arrives once; later visits read it directly.
                    for listener in loaded.listeners.take() {
                        listener(&header);
                    }
                    loaded.header.replace(Some(header));
                }
                Chunk::Tracks(tracks) => loaded.store.extend_from_slice(&objects(tracks)),
                Chunk::Failed(e) => {
                    ctx().stores.borrow_mut().remove(&key);
                    super::toast(&format!("Couldn't load: {e}"));
                }
            }
        }
        crate::memory::trim_soon();
    });
    loaded
}

/// Fills Local Files from the music folders, scanned off the UI thread.
fn scan_local(loaded: &Rc<Loaded>) {
    let folders = ctx().settings.borrow().local_folders.clone();
    let weak = Rc::downgrade(loaded);
    glib::spawn_future_local(async move {
        let scanned = folders.clone();
        let tracks = rt::spawn(async move {
            tokio::task::spawn_blocking(move || crate::local::scan(&scanned))
                .await
                .unwrap_or_default()
        })
        .await;
        let Some(loaded) = weak.upgrade() else { return };
        let places: Vec<String> = folders
            .iter()
            .map(|f| f.file_name().unwrap_or(f.as_os_str()).to_string_lossy().into_owned())
            .collect();
        let subtitle = match (tracks.len(), places.is_empty()) {
            (_, true) => "Choose a music folder to see your songs here.".to_owned(),
            (0, false) => format!("No songs found in {}. MP3, FLAC and MP4 files play here.", places.join(", ")),
            (1, false) => format!("1 song from {}", places.join(", ")),
            (n, false) => format!("{n} songs from {}", places.join(", ")),
        };
        let header = Header {
            title: String::new(),
            subtitle,
            images: Images::default(),
        };
        for listener in loaded.listeners.take() {
            listener(&header);
        }
        loaded.header.replace(Some(header));
        loaded.store.extend_from_slice(&objects(tracks));
    });
}

/// Every track URI in a loaded list, in order.
fn track_uris(store: &gio::ListStore) -> Vec<String> {
    store
        .iter::<glib::BoxedAnyObject>()
        .flatten()
        .filter_map(|o| o.try_borrow::<Track>().ok().map(|t| t.uri.clone()))
        .collect()
}

/// Plays a whole track list from the top. Local files have no Spotify
/// context, so they play as a plain list.
fn play_all(kind: Kind, context: &str, store: &gio::ListStore, shuffle: bool) {
    if kind == Kind::Local {
        let uris = track_uris(store);
        if !uris.is_empty() {
            ctx().with_engine(|e| e.play_list(uris, None, Some(shuffle)));
        }
    } else {
        ctx().with_engine(|e| e.play_context(context, None, Some(shuffle)));
    }
}

/// The gradient tile that stands in for a cover on Liked Songs and Local Files.
fn icon_tile(icon: &str, class: &str) -> gtk::Widget {
    let tile = gtk::Box::builder().width_request(212).height_request(212).build();
    tile.add_css_class("icon-tile");
    tile.add_css_class(class);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(72);
    image.set_hexpand(true);
    image.set_halign(gtk::Align::Center);
    tile.append(&image);
    tile.upcast()
}

pub fn tracks_page(kind: Kind, uri: &str, title: &str, images: &Images) -> adw::NavigationPage {
    let loaded = load_tracks(kind, uri);
    let context = match kind {
        Kind::Liked => ctx().collection_uri(),
        _ => uri.to_owned(),
    };

    let art: gtk::Widget = match kind {
        Kind::Liked => icon_tile("onify-heart-filled-symbolic", "liked-tile"),
        Kind::Local => icon_tile("onify-folder-music-symbolic", "local-tile"),
        _ => {
            let cover = Cover::new(212, 14.0);
            cover.set_url(images.pick(480));
            cover.upcast()
        }
    };
    let eyebrow = match kind {
        Kind::Album => "Album",
        Kind::Local => "On this computer",
        _ => "Playlist",
    };
    let hero = hero(eyebrow, title, &art);
    let play = play_button("Play");
    let shuffle = glass_button("onify-media-playlist-shuffle-symbolic", "Shuffle play");
    hero.actions.append(&play);
    hero.actions.append(&shuffle);
    if kind == Kind::Local {
        let folders = glass_button("onify-list-add-symbolic", "Choose music folders");
        folders.set_action_name(Some("app.preferences"));
        hero.actions.append(&folders);
    }

    let header_widget = vbox(0);
    header_widget.append(&hero.widget);
    let mode = if kind == Kind::Album { RowMode::Album } else { RowMode::Playlist };
    header_widget.append(&column_header(mode));

    let apply_header = {
        let title = hero.title.downgrade();
        let subtitle = hero.subtitle.downgrade();
        let art = hero.art.downgrade();
        move |h: &Header| {
            let (Some(t), Some(s)) = (title.upgrade(), subtitle.upgrade()) else { return };
            if !h.title.is_empty() {
                t.set_label(&h.title);
            }
            s.set_label(&h.subtitle);
            if let Some(cover) = art.upgrade().and_then(|a| a.first_child()).and_downcast::<Cover>() {
                if let Some(url) = h.images.pick(480) {
                    cover.set_url(Some(url));
                }
            }
        }
    };

    let selection = gtk::SingleSelection::new(Some(loaded.store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let list = gtk::ListView::builder()
        .model(&selection)
        .factory(&factory(mode, header_widget.upcast()))
        .single_click_activate(false)
        .build();
    list.add_css_class("tracks");
    let store = loaded.store.clone();
    let ctx_uri = context.clone();
    list.connect_activate(move |_, position| {
        let Some(object) = store.item(position).and_downcast::<glib::BoxedAnyObject>() else { return };
        let Ok(track) = object.try_borrow::<Track>() else { return };
        if !track.playable {
            return;
        }
        if kind == Kind::Local {
            // The header sits at position 0, so songs start at 1.
            let uris = track_uris(&store);
            ctx().with_engine(|e| e.play_list(uris, Some(position as usize - 1), None));
        } else {
            ctx().with_engine(|e| e.play_context(&ctx_uri, Some(&track.uri), None));
        }
    });

    let (ctx_uri, store) = (context.clone(), loaded.store.clone());
    play.connect_clicked(move |_| play_all(kind, &ctx_uri, &store, false));
    let store = loaded.store.clone();
    shuffle.connect_clicked(move |_| play_all(kind, &context, &store, true));

    let scroller = scrolled(&list);
    let header = fading_header(title, &scroller.vadjustment(), 200.0);
    let window_title = header.title_widget().and_downcast::<adw::WindowTitle>().map(|t| t.downgrade());
    let update = move |h: &Header| {
        apply_header(h);
        if let Some(t) = window_title.as_ref().and_then(|w| w.upgrade()) {
            if !h.title.is_empty() {
                t.set_title(&h.title);
            }
        }
    };
    let ready = loaded.header.borrow().clone();
    match ready {
        Some(h) => update(&h),
        None => loaded.listeners.borrow_mut().push(Box::new(update)),
    }
    let tag = if kind == Kind::Liked { "liked" } else { uri };
    page(title, tag, &scroller, &header)
}

pub fn artist_page(card: &Card) -> adw::NavigationPage {
    let body = vbox(40);
    body.set_margin_bottom(BAR_CLEARANCE);

    let avatar = Cover::round(212);
    avatar.set_url(card.images.pick(480));
    let hero = hero("Artist", &card.name, &avatar);
    let play = play_button("Play");
    let uri = card.uri.clone();
    play.connect_clicked(move |_| ctx().with_engine(|e| e.play_context(&uri, None, None)));
    hero.actions.append(&play);
    body.append(&hero.widget);
    let loading = spinner();
    body.append(&loading);

    let id = card.id().to_owned();
    let artist_uri = card.uri.clone();
    let (body_weak, avatar_weak) = (body.downgrade(), avatar.downgrade());
    let (title, subtitle) = (hero.title.clone(), hero.subtitle.clone());
    if let Some(api) = ctx().api() {
        glib::spawn_future_local(async move {
            let artist = rt::spawn(async move { api.artist(&id).await }).await;
            let (Some(body), Some(avatar)) = (body_weak.upgrade(), avatar_weak.upgrade()) else { return };
            body.remove(&loading);
            let artist = match artist {
                Ok(a) => a,
                Err(e) => {
                    super::toast(&format!("Couldn't load artist: {e}"));
                    return;
                }
            };
            avatar.set_url(artist.images.pick(480));
            title.set_label(&artist.name);
            if artist.followers > 0 {
                subtitle.set_label(&format!("{} followers", group_digits(artist.followers)));
            }
            if !artist.top.is_empty() {
                let popular = vbox(12);
                let heading = label("Popular", &["title-section"]);
                heading.set_margin_start(GUTTER);
                popular.append(&heading);
                let top: Vec<Track> = artist.top.iter().take(10).cloned().collect();
                let uris = top.clone();
                let list = short_list(&top, RowMode::Compact, move |i| {
                    let uri = uris[i].uri.clone();
                    ctx().with_engine(|e| e.play_context(&artist_uri, Some(&uri), None));
                });
                list.set_margin_start(GUTTER - 12);
                list.set_margin_end(GUTTER - 12);
                popular.append(&list);
                body.append(&popular);
            }
            for (title, cards) in [("Albums", &artist.albums), ("Singles and EPs", &artist.singles)] {
                if let Some(section) = carousel(title, cards) {
                    body.append(&section);
                }
            }
        });
    }

    let scroller = scrolled(&body);
    let header = fading_header(&card.name, &scroller.vadjustment(), 200.0);
    page(&card.name, &card.uri, &scroller, &header)
}

fn group_digits(n: u64) -> String {
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
