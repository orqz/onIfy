//! The window, navigation and the glue between the UI and the engine.

mod backdrop;
mod cover;
#[cfg(windows)]
mod frame;
mod glass;
mod integrations;
mod lyrics;
mod pages;
mod player_bar;
mod preferences;
mod track_row;
#[cfg(windows)]
mod tray;
mod updater;

#[cfg(windows)]
pub use frame::hand_over_to_running;

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use librespot_core::authentication::Credentials;
use librespot_core::error::ErrorKind;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{Api, Card, Images, Kind};
use crate::audio::Output;
use crate::settings::Settings;
use crate::spotify::{self, Engine, Event, NowPlaying};
use crate::{images, rt};
use backdrop::Backdrop;
use glass::{Glass, GlassHost};
use cover::Cover;
use pages::{HomePage, Loaded, SearchPage};
use player_bar::PlayerBar;

pub const APP_ID: &str = "io.github.orqz.onIfy";
const DISCORD_SERVER: &str = "https://discord.gg/WPKMx4gmwp";
const REPO: &str = "https://github.com/orqz/onIfy";

#[derive(Clone)]
enum Route {
    Home,
    Search,
    Card(Card),
}

impl Route {
    fn tag(&self) -> String {
        match self {
            Route::Home => "home".into(),
            Route::Search => "search".into(),
            Route::Card(card) if card.kind == Kind::Liked => "liked".into(),
            Route::Card(card) => card.uri.clone(),
        }
    }
}

/// Home, Search, Liked Songs and Local Files; library rows follow these.
const FIXED_ROWS: usize = 4;
/// Room kept under the pages for the floating player: its 62px capsule, its
/// 12px bottom gap and 8px more, so nothing ever sits behind it.
const PLAYER_SPACE: i32 = 82;

struct Login {
    widget: adw::ToolbarView,
    button: gtk::Button,
    status: gtk::Label,
}

pub struct Ctx {
    app: adw::Application,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    backdrop: Backdrop,
    host: GlassHost,
    root: gtk::Stack,
    login: Login,
    split: adw::NavigationSplitView,
    nav: adw::NavigationView,
    sidebar: gtk::ListBox,
    sidebar_routes: RefCell<Vec<Option<Route>>>,
    pub bar: Rc<PlayerBar>,
    home: HomePage,
    search: SearchPage,
    engine: RefCell<Option<Arc<Engine>>>,
    api: RefCell<Option<Api>>,
    output: Arc<Output>,
    events: UnboundedSender<Event>,
    generation: Cell<u64>,
    reconnect_attempts: Cell<u32>,
    pub stores: RefCell<HashMap<String, Rc<Loaded>>>,
    settings: RefCell<Settings>,
    save_pending: Cell<bool>,
    /// The local folders changed while music played; restart the engine at
    /// the next pause so it picks them up.
    engine_stale: Cell<bool>,
    last_unavailable: Cell<Option<std::time::Instant>>,
    /// Songs in a row that wouldn't load (see Event::Unavailable).
    unavailable_streak: Cell<u32>,
    #[cfg(target_os = "linux")]
    mpris: RefCell<Option<Rc<mpris_server::Player>>>,
    now: RefCell<Option<NowPlaying>>,
    /// A link to open once the library has loaded.
    pending_link: RefCell<Option<String>>,
    /// What the sidebar and Home show, to skip redrawing identical data.
    shown_library: RefCell<Option<Vec<Card>>>,
    shown_home: RefCell<Option<crate::api::Home>>,
}

thread_local! {
    static CTX: OnceCell<Rc<Ctx>> = const { OnceCell::new() };
}

pub fn ctx() -> Rc<Ctx> {
    CTX.with(|c| c.get().expect("UI not built yet").clone())
}

impl Ctx {
    pub fn with_engine(&self, f: impl FnOnce(&Engine)) {
        if let Some(engine) = self.engine.borrow().as_ref() {
            f(engine);
        }
    }

    pub fn api(&self) -> Option<Api> {
        self.api.borrow().clone()
    }

    /// The context URI that plays the user's Liked Songs.
    pub fn collection_uri(&self) -> String {
        let user = self
            .engine
            .borrow()
            .as_ref()
            .map(|e| e.session.username())
            .unwrap_or_default();
        format!("spotify:user:{user}:collection")
    }
}

pub fn toast(message: &str) {
    let toast = adw::Toast::new(message);
    toast.set_use_markup(false);
    toast.set_timeout(3);
    ctx().toasts.add_toast(toast);
}

fn spawn_local(fut: impl Future<Output = ()> + 'static) {
    glib::spawn_future_local(fut);
}

#[cfg(target_os = "linux")]
fn with_mpris<F, Fut>(f: F)
where
    F: FnOnce(Rc<mpris_server::Player>) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    let player = ctx().mpris.borrow().clone();
    if let Some(player) = player {
        spawn_local(f(player));
    }
}

pub fn startup(_: &adw::Application) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    // Windows and macOS take the taskbar/dock icon from here.
    gtk::Window::set_default_icon_name(APP_ID);
    // The sidebar already shows onIfy's logo; drop the second, smaller one
    // some desktops add beside the window buttons.
    if let Some(settings) = gtk::Settings::default() {
        strip_window_icon(&settings);
        settings.connect_gtk_decoration_layout_notify(strip_window_icon);
        #[cfg(windows)]
        windows_fonts(&settings);
    }
}

/// Windows hands GTK its 9pt menu font, while onIfy is laid out for 11pt
/// (what Linux uses), and Segoe UI rendered badly through GTK; onIfy brings
/// Adwaita Sans, the font libadwaita is drawn for (data/fonts).
#[cfg(windows)]
fn windows_fonts(settings: &gtk::Settings) {
    use gtk::pango::prelude::*;
    let font_map = gtk::Label::new(None).pango_context().font_map();
    let loaded = bundled_font().zip(font_map).is_some_and(|(file, map)| match map.add_font_file(&file) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("couldn't load {}: {e}", file.display());
            false
        }
    });
    settings.set_gtk_font_name(Some(if loaded { "Adwaita Sans 11" } else { "Segoe UI 11" }));
    settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
    settings.set_gtk_hint_font_metrics(true);
    settings.set_gtk_xft_antialias(1);
    settings.set_gtk_xft_hinting(1);
    settings.set_gtk_xft_hintstyle(Some("hintslight"));
}

/// The installer puts the font in share\onify\fonts next to bin\; a copy
/// run from the source tree uses the repo's.
#[cfg(windows)]
fn bundled_font() -> Option<std::path::PathBuf> {
    const FILE: &str = "AdwaitaSans-Regular.ttf";
    let exe = std::env::current_exe().ok()?;
    let installed = exe.parent()?.parent()?.join("share").join("onify").join("fonts").join(FILE);
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data").join("fonts").join(FILE);
    [installed, source].into_iter().find(|path| path.is_file())
}

fn strip_window_icon(settings: &gtk::Settings) {
    let Some(layout) = settings.gtk_decoration_layout() else { return };
    if !layout.contains("icon") {
        return;
    }
    let sides: Vec<String> = layout
        .split(':')
        .map(|side| side.split(',').filter(|b| *b != "icon").collect::<Vec<_>>().join(","))
        .collect();
    settings.set_gtk_decoration_layout(Some(&sides.join(":")));
}

pub fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }

    let settings = Settings::load();
    let output = Output::new(settings.volume);
    let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("onIfy")
        .default_width(1280)
        .default_height(820)
        .width_request(300)
        .height_request(480)
        .build();
    // Windows draws the frame, shadow and rounded corners instead of GTK
    // (frame.rs), so GTK's invisible resize border around the window goes.
    #[cfg(windows)]
    {
        window.set_decorated(false);
        window.add_css_class("platform-windows");
        window.connect_realize(|window| frame::install(window.upcast_ref()));
    }

    let login = build_login();
    let home = HomePage::new();
    let search = SearchPage::new();
    let nav = adw::NavigationView::new();
    nav.add(&home.page);
    nav.add(&search.page);
    nav.replace_with_tags(&["home"]);
    nav.set_margin_bottom(PLAYER_SPACE);
    let content = adw::NavigationPage::builder().title("onIfy").child(&nav).build();
    let (sidebar_page, sidebar, sidebar_glass) = build_sidebar();
    let split = adw::NavigationSplitView::builder()
        .sidebar(&sidebar_page)
        .content(&content)
        .min_sidebar_width(220.0)
        .max_sidebar_width(300.0)
        .sidebar_width_fraction(0.22)
        .build();
    let bar = PlayerBar::new();

    let root = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(200)
        .build();
    root.add_named(&login.widget, Some("login"));
    root.add_named(&split, Some("main"));
    // The blurred cover sits underneath everything, the pages scroll over it,
    // and the player floats on glass above both.
    let backdrop = Backdrop::new();
    backdrop.set_mood(backdrop::Mood::from_name(&settings.background));
    backdrop.set_covers(settings.cover_background);
    set_animations(settings.animations);
    images::set_low_memory(settings.low_memory);
    let host = GlassHost::new(&backdrop, &root, &bar.widget);
    host.set_lane(&content);
    host.add_panel(&sidebar_glass);
    // The player only shows over the app itself, never the login page (which
    // the stack starts on without notifying).
    bar.widget.set_visible(false);
    root.connect_visible_child_name_notify(glib::clone!(
        #[weak(rename_to = bar)]
        bar.widget,
        move |root| bar.set_visible(root.visible_child_name().as_deref() == Some("main"))
    ));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&host));
    let style = settings.style.clone();
    window.set_content(Some(&toasts));

    // Window size tiers: pages scale their headers, gutters and rows (see
    // pages::Tier). Only the last matching breakpoint applies, so each one
    // carries everything its size needs.
    let medium = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1100sp or max-height: 720sp").unwrap());
    medium.connect_apply(|bp| set_tier(bp, pages::Tier::Medium));
    medium.connect_unapply(|bp| set_tier(bp, pages::Tier::Large));
    window.add_breakpoint(medium);
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").unwrap());
    narrow.connect_apply(|bp| set_tier(bp, pages::Tier::Medium));
    narrow.connect_unapply(|bp| set_tier(bp, pages::Tier::Large));
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    // Collapsed, the sidebar fills the window, so it makes room for the player.
    narrow.add_setter(&sidebar_glass, "margin-bottom", Some(&(PLAYER_SPACE - 12).to_value()));
    for widget in &bar.volume_controls {
        narrow.add_setter(widget, "visible", Some(&false.to_value()));
    }
    narrow.add_setter(&bar.widget, "homogeneous", Some(&false.to_value()));
    window.add_breakpoint(narrow);
    // Phone-width windows: stack page headers, keep only the core controls.
    let compact = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 560sp").unwrap());
    compact.add_setter(&split, "collapsed", Some(&true.to_value()));
    compact.add_setter(&sidebar_glass, "margin-bottom", Some(&(PLAYER_SPACE - 12).to_value()));
    for widget in &bar.volume_controls {
        compact.add_setter(widget, "visible", Some(&false.to_value()));
    }
    compact.add_setter(&bar.widget, "homogeneous", Some(&false.to_value()));
    for widget in &bar.extras {
        compact.add_setter(widget, "visible", Some(&false.to_value()));
    }
    compact.connect_apply(|bp| set_tier(bp, pages::Tier::Compact));
    compact.connect_unapply(|bp| set_tier(bp, pages::Tier::Large));
    window.add_breakpoint(compact);

    let ctx = Rc::new(Ctx {
        app: app.clone(),
        window: window.clone(),
        toasts,
        backdrop,
        host,
        root,
        login,
        split,
        nav,
        sidebar,
        sidebar_routes: RefCell::new(vec![
            Some(Route::Home),
            Some(Route::Search),
            Some(Route::Card(liked_card())),
            Some(Route::Card(local_card())),
        ]),
        bar,
        home,
        search,
        engine: RefCell::default(),
        api: RefCell::default(),
        output,
        events,
        generation: Cell::new(0),
        reconnect_attempts: Cell::new(0),
        stores: RefCell::default(),
        settings: RefCell::new(settings),
        save_pending: Cell::new(false),
        engine_stale: Cell::new(false),
        last_unavailable: Cell::default(),
        unavailable_streak: Cell::new(0),
        #[cfg(target_os = "linux")]
        mpris: RefCell::default(),
        now: RefCell::default(),
        pending_link: RefCell::default(),
        shown_library: RefCell::default(),
        shown_home: RefCell::default(),
    });
    CTX.with(|c| {
        let _ = c.set(ctx.clone());
    });
    tier_classes(&ctx.window, pages::tier());
    apply_style(&style);

    ctx.bar.set_volume(ctx.settings.borrow().volume);
    // The lyrics button lights up while lyrics are showing.
    ctx.nav.connect_visible_page_notify(|nav| {
        let open = nav.visible_page().and_then(|p| p.tag()).as_deref() == Some(lyrics::TAG);
        self::ctx().bar.set_lyrics_open(open);
    });
    ctx.nav.connect_popped(|_, _| drop_hidden_pages());
    install_actions(app);
    install_keys(&window);
    if let Some(row) = ctx.sidebar.row_at_index(0) {
        ctx.sidebar.select_row(Some(&row));
    }

    spawn_local(async move {
        while let Some(event) = event_rx.recv().await {
            handle_event(event);
        }
    });

    window.connect_close_request(|_| {
        // Windows: closing keeps the music going from the tray icon.
        #[cfg(windows)]
        if self::ctx().settings.borrow().close_to_tray && tray::shown() {
            self::ctx().window.set_visible(false);
            return glib::Propagation::Stop;
        }
        quit();
        glib::Propagation::Stop
    });

    match spotify::cached_credentials() {
        Some(credentials) => {
            ctx.root.set_visible_child_name("main");
            show_cached_library();
            connect(credentials);
        }
        None => ctx.root.set_visible_child_name("login"),
    }
    window.present();

    integrations::start(window.upcast_ref());
    #[cfg(windows)]
    tray::start();
    images::prune_disk_cache();
    updater::start();
}

fn liked_card() -> Card {
    Card {
        kind: Kind::Liked,
        uri: "liked".into(),
        name: "Liked Songs".into(),
        subtitle: String::new(),
        images: Images::default(),
    }
}

fn local_card() -> Card {
    Card {
        kind: Kind::Local,
        uri: "local".into(),
        name: "Local Files".into(),
        subtitle: String::new(),
        images: Images::default(),
    }
}

fn build_login() -> Login {
    let button = gtk::Button::builder()
        .label("Log in with Spotify")
        .halign(gtk::Align::Center)
        .build();
    button.add_css_class("pill");
    button.add_css_class("suggested-action");
    let status = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
    status.add_css_class("dim-label");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    content.append(&button);
    content.append(&status);
    let page = adw::StatusPage::builder()
        .icon_name(APP_ID)
        .title("onify")
        .description("Spotify, smooth as butter.\nRequires Spotify Premium.")
        .child(&content)
        .vexpand(true)
        .build();
    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    let widget = adw::ToolbarView::new();
    widget.add_top_bar(&header);
    widget.set_content(Some(&page));

    button.connect_clicked(|button| {
        button.set_sensitive(false);
        let ctx = ctx();
        ctx.login.status.set_label("Finish logging in in your browser…");
        spawn_local(async move {
            let credentials =
                rt::spawn(async { tokio::task::spawn_blocking(spotify::login_in_browser).await }).await;
            match credentials {
                Ok(Ok(credentials)) => {
                    self::ctx().login.status.set_label("Connecting…");
                    connect(credentials);
                }
                Ok(Err(e)) => show_login(&format!("Login failed: {e}")),
                Err(e) => show_login(&format!("Login failed: {e}")),
            }
        });
    });
    Login { widget, button, status }
}

fn show_login(message: &str) {
    let ctx = ctx();
    ctx.login.status.set_label(message);
    ctx.login.button.set_sensitive(true);
    ctx.root.set_visible_child_name("login");
}

fn nav_row(icon: &str, name: &str) -> gtk::ListBoxRow {
    let content = gtk::Box::builder().spacing(14).build();
    content.append(&gtk::Image::from_icon_name(icon));
    let label = gtk::Label::builder().label(name).xalign(0.0).build();
    content.append(&label);
    let row = gtk::ListBoxRow::builder().child(&content).build();
    row.add_css_class("nav");
    row
}

fn library_row(card: &Card) -> gtk::ListBoxRow {
    let content = gtk::Box::builder().spacing(12).build();
    let cover = if card.kind == Kind::Artist { Cover::round(42) } else { Cover::new(42, 8.0) };
    cover.set_url(card.images.pick(100));
    content.append(&cover);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .spacing(1)
        .build();
    let name = gtk::Label::builder()
        .label(&card.name)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    name.add_css_class("library-title");
    let kind = match card.kind {
        Kind::Album => "Album",
        Kind::Artist => "Artist",
        _ => "Playlist",
    };
    let detail = gtk::Label::builder().label(kind).xalign(0.0).build();
    detail.add_css_class("library-detail");
    text.append(&name);
    text.append(&detail);
    content.append(&text);
    let row = gtk::ListBoxRow::builder().child(&content).build();
    row.add_css_class("library");
    row.set_tooltip_text(Some(&card.name));
    row
}

fn build_sidebar() -> (adw::NavigationPage, gtk::ListBox, Glass) {
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.append(&nav_row("onify-go-home-symbolic", "Home"));
    list.append(&nav_row("onify-system-search-symbolic", "Search"));
    list.append(&nav_row("onify-heart-filled-symbolic", "Liked Songs"));
    list.append(&nav_row("onify-folder-music-symbolic", "Local Files"));
    list.connect_row_activated(|_, row| {
        let route = ctx().sidebar_routes.borrow().get(row.index() as usize).cloned().flatten();
        if let Some(route) = route {
            navigate(route, true);
        }
    });

    let menu = gio::Menu::new();
    let main_section = gio::Menu::new();
    main_section.append(Some("Preferences"), Some("app.preferences"));
    main_section.append(Some("Join the Discord Server"), Some("app.discord"));
    main_section.append(Some("About onIfy"), Some("app.about"));
    menu.append_section(None, &main_section);
    let session_section = gio::Menu::new();
    session_section.append(Some("Log Out"), Some("app.logout"));
    session_section.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &session_section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("onify-settings-symbolic")
        .menu_model(&menu)
        .tooltip_text("Settings")
        .build();
    // Logo centred over the menu icons below it, the name level with their
    // labels.
    let brand = gtk::Box::builder().spacing(8).margin_start(12).build();
    // The logo in the playing cover's colour (style.css, .brand-logo).
    let logo = gtk::Image::from_icon_name("onify-logo-symbolic");
    logo.add_css_class("brand-logo");
    logo.set_pixel_size(28);
    let name = gtk::Label::builder().label("onify").xalign(0.0).build();
    name.add_css_class("brand");
    let version = gtk::Label::builder().label(concat!("v", env!("ONIFY_VERSION"))).xalign(0.0).build();
    version.add_css_class("brand-version");
    let names = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).build();
    names.append(&name);
    names.append(&version);
    brand.append(&logo);
    brand.append(&names);
    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    header.pack_start(&brand);
    header.pack_end(&menu_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    pages::glide_wheel(&scroller);
    toolbar.set_content(Some(&scroller));
    toolbar.set_vexpand(true);
    // A floating glass panel; rows scroll inside its rounded corners.
    let glass = Glass::new(gtk::Orientation::Vertical, 0, 24.0);
    glass.add_css_class("sidebar-glass");
    glass.set_overflow(gtk::Overflow::Hidden);
    glass.append(&toolbar);
    // No `.sidebar` class: libadwaita draws a divider line for it.
    let page = adw::NavigationPage::builder().title("onIfy").child(&glass).build();
    (page, list, glass)
}

fn fill_sidebar(playlists: &[Card]) {
    let ctx = ctx();
    let mut routes = ctx.sidebar_routes.borrow_mut();
    while routes.len() > FIXED_ROWS {
        routes.pop();
        if let Some(row) = ctx.sidebar.row_at_index(FIXED_ROWS as i32) {
            ctx.sidebar.remove(&row);
        }
    }
    if playlists.is_empty() {
        return;
    }
    let heading = gtk::Label::builder().label("Your Library").xalign(0.0).build();
    heading.add_css_class("sidebar-heading");
    let heading_row = gtk::ListBoxRow::builder()
        .child(&heading)
        .activatable(false)
        .selectable(false)
        .build();
    ctx.sidebar.append(&heading_row);
    routes.push(None);
    for playlist in playlists {
        ctx.sidebar.append(&library_row(playlist));
        routes.push(Some(Route::Card(playlist.clone())));
    }
}

/// Pages follow the window's size tier; style.css keys spacing off the
/// window's `tier-medium` / `tier-compact` classes.
fn set_tier(_: &adw::Breakpoint, tier: pages::Tier) {
    pages::set_tier(tier);
    if let Some(ctx) = CTX.with(|c| c.get().cloned()) {
        tier_classes(&ctx.window, tier);
    }
}

fn tier_classes(window: &adw::ApplicationWindow, tier: pages::Tier) {
    window.remove_css_class("tier-medium");
    window.remove_css_class("tier-compact");
    match tier {
        pages::Tier::Medium => window.add_css_class("tier-medium"),
        pages::Tier::Compact => window.add_css_class("tier-compact"),
        pages::Tier::Large => {}
    }
}

/// "vinyl": no glass, panels straight on the cover with a soft shade, the
/// player's cover a turning record, accents taken from the cover (style.css,
/// `window.style-vinyl`). "glass": the liquid glass look.
pub fn apply_style(name: &str) {
    let ctx = ctx();
    let vinyl = name != "glass";
    if vinyl {
        ctx.window.add_css_class("style-vinyl");
    } else {
        ctx.window.remove_css_class("style-vinyl");
    }
    ctx.host.set_shade_only(vinyl);
    ctx.bar.set_record(vinyl);
    pages::set_vinyl(vinyl);
}

/// Off means onIfy's own animations and transitions are skipped; on follows
/// the system setting.
pub fn set_animations(on: bool) {
    if let Some(settings) = gtk::Settings::default() {
        if on {
            settings.reset_property("gtk-enable-animations");
        } else {
            settings.set_gtk_enable_animations(false);
        }
    }
}

/// Low memory mode: forget loaded pages that aren't on screen or in the back
/// history. They reopen from the disk cache, so it costs no waiting.
pub fn drop_hidden_pages() {
    let ctx = ctx();
    if !ctx.settings.borrow().low_memory {
        return;
    }
    let nav = ctx.nav.clone();
    ctx.stores.borrow_mut().retain(|key, _| stack_has(&nav, key));
    crate::memory::trim_soon();
}

fn navigate(route: Route, root: bool) {
    let ctx = ctx();
    let tag = route.tag();
    let on_top = ctx.nav.visible_page().and_then(|p| p.tag()).as_deref() == Some(tag.as_str());
    if !on_top {
        if root {
            match &route {
                Route::Home | Route::Search => ctx.nav.replace_with_tags(&[tag.as_str()]),
                Route::Card(card) => ctx.nav.replace(&[page_for(card)]),
            }
        } else if stack_has(&ctx.nav, &tag) {
            ctx.nav.pop_to_tag(&tag);
        } else {
            match &route {
                Route::Home | Route::Search => ctx.nav.push_by_tag(&tag),
                Route::Card(card) => ctx.nav.push(&page_for(card)),
            }
        }
    }
    if ctx.split.is_collapsed() {
        ctx.split.set_show_content(true);
    }
    if root {
        let index = ctx
            .sidebar_routes
            .borrow()
            .iter()
            .position(|r| r.as_ref().is_some_and(|r| r.tag() == tag));
        match index.and_then(|i| ctx.sidebar.row_at_index(i as i32)) {
            Some(row) => ctx.sidebar.select_row(Some(&row)),
            None => ctx.sidebar.unselect_all(),
        }
    }
    if matches!(route, Route::Search) {
        ctx.search.entry.grab_focus();
    }
    drop_hidden_pages();
}

fn stack_has(nav: &adw::NavigationView, tag: &str) -> bool {
    nav.navigation_stack()
        .iter::<adw::NavigationPage>()
        .flatten()
        .any(|p| p.tag().as_deref() == Some(tag))
}

fn page_for(card: &Card) -> adw::NavigationPage {
    match card.kind {
        Kind::Artist => pages::artist_page(card),
        kind => pages::tracks_page(kind, &card.uri, &card.name, &card.images),
    }
}
/// Shows the lyrics, or goes back from them if they're already showing.
pub fn toggle_lyrics() {
    let ctx = ctx();
    let on_top = ctx.nav.visible_page().and_then(|p| p.tag()).as_deref() == Some(lyrics::TAG);
    if on_top {
        ctx.nav.pop();
        return;
    }
    if stack_has(&ctx.nav, lyrics::TAG) {
        ctx.nav.pop_to_tag(lyrics::TAG);
    } else {
        ctx.nav.push(&lyrics::page());
    }
    if ctx.split.is_collapsed() {
        ctx.split.set_show_content(true);
    }
}

pub fn open_card(card: &Card) {
    navigate(Route::Card(card.clone()), false);
}

/// Opens a `spotify:album|artist|playlist:…` URI.
pub fn open_uri(uri: &str) {
    let kind = match uri.split(':').nth(1) {
        Some("album") => Kind::Album,
        Some("artist") => Kind::Artist,
        Some("playlist") => Kind::Playlist,
        _ => return,
    };
    let name = match kind {
        Kind::Album => "Album",
        Kind::Artist => "Artist",
        _ => "Playlist",
    };
    open_card(&Card {
        kind,
        uri: uri.to_owned(),
        name: name.into(),
        subtitle: String::new(),
        images: Images::default(),
    });
}

pub fn open_album_of(track_uri: &str) {
    if !track_uri.starts_with("spotify:track:") {
        return;
    }
    let Some(api) = ctx().api() else { return };
    let uri = track_uri.to_owned();
    spawn_local(async move {
        match rt::spawn(async move { api.album_of(&uri).await }).await {
            Ok(card) => open_card(&card),
            Err(e) => toast(&format!("Couldn't open album: {e}")),
        }
    });
}

pub fn check_liked(uri: &str) {
    let Some(api) = ctx().api() else { return };
    let uri = uri.to_owned();
    spawn_local(async move {
        let track = uri.clone();
        if let Ok(liked) = rt::spawn(async move { api.is_liked(&track).await }).await {
            let ctx = ctx();
            if ctx.bar.track_uri() == uri {
                ctx.bar.set_liked(liked);
            }
        }
    });
}

pub fn set_liked(uri: &str, liked: bool) {
    let Some(api) = ctx().api() else { return };
    let uri = uri.to_owned();
    spawn_local(async move {
        let track = uri.clone();
        let result = rt::spawn(async move { api.set_liked(&track, liked).await }).await;
        let ctx = ctx();
        match result {
            Ok(()) => {
                // Liked Songs reloads fresh next time it's opened.
                ctx.stores.borrow_mut().remove("liked");
                toast(if liked { "Added to Liked Songs" } else { "Removed from Liked Songs" });
            }
            Err(e) => {
                if ctx.bar.track_uri() == uri {
                    ctx.bar.set_liked(!liked);
                }
                toast(&format!("Couldn't update Liked Songs: {e}"));
            }
        }
    });
}

fn connect(credentials: Credentials) {
    let ctx = ctx();
    let generation = ctx.generation.get() + 1;
    ctx.generation.set(generation);
    let (device_id, folders, quality) = {
        let settings = ctx.settings.borrow();
        (settings.device_id.clone(), settings.local_folders.clone(), spotify::Quality::from_name(&settings.quality))
    };
    let (output, events) = (ctx.output.clone(), ctx.events.clone());
    spawn_local(async move {
        let started =
            rt::spawn(Engine::start(credentials, device_id, quality, folders, output, events, generation)).await;
        let ctx = self::ctx();
        if ctx.generation.get() != generation {
            if let Ok(engine) = started {
                engine.shutdown();
            }
            return;
        }
        match started {
            Ok(engine) => {
                ctx.reconnect_attempts.set(0);
                let first = ctx.api.borrow().is_none();
                ctx.api.replace(Some(Api::new(engine.session.clone())));
                ctx.engine.replace(Some(engine));
                ctx.root.set_visible_child_name("main");
                if first {
                    load_library();
                }
            }
            Err(e) => {
                log::warn!("connecting failed: {e}");
                let auth = matches!(e.kind, ErrorKind::Unauthenticated | ErrorKind::PermissionDenied);
                if auth {
                    spotify::forget_credentials();
                    show_login(&format!("Spotify said no: {e}"));
                } else {
                    reconnect();
                }
            }
        }
    });
}

fn reconnect() {
    let ctx = ctx();
    let attempt = ctx.reconnect_attempts.get();
    ctx.reconnect_attempts.set(attempt + 1);
    if attempt == 0 {
        toast("Can't reach Spotify. Retrying…");
    }
    let delay = [1, 2, 5, 10, 30][attempt.min(4) as usize];
    glib::timeout_add_local_once(Duration::from_secs(delay), || {
        if self::ctx().engine.borrow().is_some() {
            return;
        }
        match spotify::cached_credentials() {
            Some(credentials) => connect(credentials),
            None => show_login("Please log in again."),
        }
    });
}

/// The player's settings (streaming quality, local folders) are fixed when it
/// starts, so a change needs a fresh engine. That waits for a pause if music
/// is playing.
pub fn player_settings_changed() {
    let ctx = ctx();
    if ctx.engine.borrow().is_none() {
        return;
    }
    ctx.engine_stale.set(true);
    if !ctx.bar.is_playing() {
        restart_if_stale();
    }
}

/// librespot indexes the local folders when its player starts, so new folders
/// need a fresh engine. That waits for a pause if music is playing.
pub fn local_folders_changed() {
    let ctx = ctx();
    ctx.stores.borrow_mut().remove("local");
    if ctx.engine.borrow().is_none() {
        return;
    }
    ctx.engine_stale.set(true);
    if !ctx.bar.is_playing() {
        restart_if_stale();
    }
}

fn restart_if_stale() {
    let ctx = ctx();
    if !ctx.engine_stale.replace(false) {
        return;
    }
    let Some(credentials) = spotify::cached_credentials() else { return };
    ctx.generation.set(ctx.generation.get() + 1);
    if let Some(engine) = ctx.engine.take() {
        engine.shutdown();
    }
    // Give the old device a moment to sign off before this one takes its id.
    glib::timeout_add_local_once(Duration::from_millis(300), move || connect(credentials));
}

/// Last time's library and Home from disk, the moment the window opens:
/// connecting to Spotify takes a few seconds.
fn show_cached_library() {
    spawn_local(async {
        let cached = Api::cache_only();
        let (library, home) = rt::spawn(async move { tokio::join!(cached.library(), cached.home()) }).await;
        let ctx = ctx();
        if let Ok(library) = library {
            if ctx.shown_library.borrow().is_none() {
                fill_sidebar(&library);
                ctx.shown_library.replace(Some(library));
            }
        }
        if let Ok(home) = home {
            if ctx.shown_home.borrow().is_none() {
                ctx.home.fill(&home);
                ctx.shown_home.replace(Some(home));
            }
        }
    });
}

/// Spotify's current library and Home, redrawn only where they changed.
fn load_library() {
    let Some(api) = ctx().api() else { return };
    if !api.is_premium() {
        toast("Playback needs Spotify Premium");
    }
    spawn_local(async move {
        let (library, home) = rt::spawn(async move { tokio::join!(api.library(), api.home()) }).await;
        let ctx = ctx();
        match library {
            Ok(library) => {
                let same = ctx.shown_library.borrow().as_deref().is_some_and(|shown| same_cards(shown, &library));
                if !same {
                    fill_sidebar(&library);
                }
                ctx.shown_library.replace(Some(library));
            }
            Err(e) if ctx.shown_library.borrow().is_none() => toast(&format!("Couldn't load your library: {e}")),
            Err(_) => {}
        }
        if let Some(link) = ctx.pending_link.take() {
            open_link(&link);
        }
        match home {
            Ok(home) => {
                let same = ctx.shown_home.borrow().as_ref().is_some_and(|shown| {
                    shown.sections.len() == home.sections.len()
                        && shown.sections.iter().zip(&home.sections).all(|(a, b)| a.0 == b.0 && same_cards(&a.1, &b.1))
                });
                if !same {
                    ctx.home.fill(&home);
                }
                ctx.shown_home.replace(Some(home));
            }
            Err(e) if ctx.shown_home.borrow().is_none() => {
                ctx.home.fill(&Default::default());
                toast(&format!("Couldn't load Home: {e}"));
            }
            Err(_) => {}
        }
    });
}

/// Whether two card lists would look the same.
fn same_cards(a: &[Card], b: &[Card]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| a.uri == b.uri && a.name == b.name && a.images.pick(100) == b.images.pick(100))
}

fn logout() {
    let ctx = ctx();
    ctx.generation.set(ctx.generation.get() + 1);
    if let Some(engine) = ctx.engine.take() {
        engine.shutdown();
    }
    ctx.api.take();
    ctx.stores.borrow_mut().clear();
    ctx.shown_library.take();
    ctx.shown_home.take();
    fill_sidebar(&[]);
    spotify::forget_credentials();
    navigate(Route::Home, true);
    show_login("");
}

fn handle_event(event: Event) {
    let ctx = ctx();
    match event {
        Event::Track(now) => {
            track_row::set_now_playing(&now.uri);
            ctx.bar.set_track(&now);
            ctx.backdrop.set_cover(now.cover(300));
            let artists = crate::api::join_names(&now.artists);
            if ctx.bar.is_playing() {
                ctx.window.set_title(Some(&format!("{} • {artists}", now.name)));
            }
            ctx.now.replace(Some(now.clone()));
            integrations::track_changed(&now);
            lyrics::track_changed(&now.uri);
        }
        Event::Playing { position_ms } => {
            ctx.bar.set_playing(true, position_ms);
            // Spotify applies its own idea of the volume when playback moves
            // here, and ignored any change made before; the user's choice wins.
            let wanted = (ctx.bar.volume() * u16::MAX as f64).round() as u16;
            if ctx.output.volume() != wanted {
                ctx.output.set_volume(wanted);
                ctx.with_engine(|e| e.set_volume(wanted));
            }
            if let Some(now) = ctx.now.borrow().as_ref() {
                let artists = crate::api::join_names(&now.artists);
                ctx.window.set_title(Some(&format!("{} • {artists}", now.name)));
            }
            integrations::playing(position_ms);
            lyrics::resync();
        }
        Event::Paused { position_ms } => {
            ctx.bar.set_playing(false, position_ms);
            ctx.window.set_title(Some("onIfy"));
            integrations::paused(position_ms);
            lyrics::resync();
            restart_if_stale();
        }
        Event::Position { position_ms } => {
            ctx.bar.set_position(position_ms);
            integrations::seeked(position_ms);
            lyrics::resync();
        }
        Event::Loading => {}
        Event::Stopped => {
            ctx.bar.stopped();
            ctx.window.set_title(Some("onIfy"));
            integrations::paused(0);
            restart_if_stale();
        }
        Event::Shuffle(shuffle) => {
            ctx.bar.set_shuffle(shuffle);
            ctx.with_engine(|e| e.note_shuffle(shuffle));
            integrations::shuffle(shuffle);
        }
        Event::Repeat { context, track } => {
            ctx.bar.set_repeat(context, track);
            ctx.with_engine(|e| e.note_repeat(context));
            integrations::repeat(context, track);
        }
        Event::Volume(volume) => {
            if let Some(wanted) = ctx.bar.reported_volume(volume) {
                ctx.output.set_volume(wanted);
                ctx.with_engine(|e| e.set_volume(wanted));
                return;
            }
            // The bar may have ignored a stale echo; keep what it shows.
            let volume = (ctx.bar.volume() * u16::MAX as f64).round() as u16;
            ctx.settings.borrow_mut().volume = volume;
            if !ctx.save_pending.replace(true) {
                glib::timeout_add_local_once(Duration::from_secs(1), || {
                    let ctx = self::ctx();
                    ctx.save_pending.set(false);
                    ctx.settings.borrow().save();
                });
            }
            integrations::volume(volume);
        }
        Event::Unavailable => {
            // librespot skips on to the next song when one won't load. Several
            // in a row means this session stopped getting song keys from
            // Spotify (it happens after a while); a fresh session gets them at
            // once, so stop skipping through the queue and reconnect.
            let now = std::time::Instant::now();
            let recent = ctx.last_unavailable.get().is_some_and(|t| now.duration_since(t) < Duration::from_secs(10));
            let streak = if recent { ctx.unavailable_streak.get() + 1 } else { 1 };
            ctx.unavailable_streak.set(streak);
            ctx.last_unavailable.set(Some(now));
            match streak {
                1 => toast("This song isn't available"),
                3 => {
                    ctx.with_engine(|e| e.pause());
                    ctx.engine_stale.set(true);
                    restart_if_stale();
                    toast("Spotify stopped sending songs, so onIfy reconnected. Press play to continue");
                }
                _ => {}
            }
        }
        Event::Disconnected(generation) => {
            if generation == ctx.generation.get() {
                ctx.engine.take();
                reconnect();
            }
        }
        Event::NotPremium(generation) => {
            if generation == ctx.generation.get() {
                logout();
                show_login("onIfy needs Spotify Premium: Spotify only streams to other apps for Premium accounts.");
            }
        }
    }
}

pub fn play_pause() {
    ctx().bar.toggle_play();
}

fn nudge_volume(delta: f64) {
    let bar = ctx().bar.clone();
    bar.set_volume_by_user(bar.volume() + delta);
}

pub fn seek_to(ms: i64) {
    ctx().with_engine(|e| e.seek(ms.max(0) as u32));
}

fn install_actions(app: &adw::Application) {
    let string = Some(glib::VariantTy::STRING);
    let action = |name: &str, f: fn()| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        app.add_action(&a);
    };
    action("search", || navigate(Route::Search, true));
    action("play-pause", play_pause);
    action("next", || ctx().with_engine(|e| e.next()));
    action("prev", || ctx().with_engine(|e| e.prev()));
    action("volume-up", || nudge_volume(0.05));
    action("volume-down", || nudge_volume(-0.05));
    action("logout", logout);
    action("lyrics", toggle_lyrics);
    action("preferences", || preferences::present(&ctx().window));
    action("quit", quit);
    action("about", || {
        let about = adw::AboutDialog::builder()
            .application_name("onIfy")
            .application_icon(APP_ID)
            .version(env!("ONIFY_VERSION"))
            .comments("A native Spotify client that stays smooth and light.")
            .developer_name("orqz")
            .license_type(gtk::License::MitX11)
            .website(REPO)
            .issue_url(format!("{REPO}/issues"))
            .build();
        about.add_link("Discord Server", DISCORD_SERVER);
        about.present(Some(&ctx().window));
    });
    action("discord", || {
        gtk::UriLauncher::new(DISCORD_SERVER).launch(Some(&ctx().window), gio::Cancellable::NONE, |_| {});
    });

    let with_string = |name: &str, f: fn(&str)| {
        let a = gio::SimpleAction::new(name, string);
        a.connect_activate(move |_, v| {
            if let Some(s) = v.and_then(|v| v.str()) {
                f(s);
            }
        });
        app.add_action(&a);
    };
    with_string("open", open_uri);
    with_string("album-of", open_album_of);
    // Developer aid: with ONIFY_DEV set, `app.dev-render` saves the window as a
    // PNG at 2x, even while it's on another workspace.
    if std::env::var_os("ONIFY_DEV").is_some() {
        // Plays one song (Spotify or local) as if clicked, for testing.
        with_string("dev-play", |uri| ctx().with_engine(|e| e.play_tracks(vec![uri.to_owned()], 0)));
        // Opens home, search, liked or local, as the sidebar would.
        with_string("dev-route", |name| {
            let route = match name {
                "home" => Route::Home,
                "search" => Route::Search,
                "liked" => Route::Card(liked_card()),
                "local" => Route::Card(local_card()),
                _ => return,
            };
            navigate(route, true);
        });
        with_string("dev-update", |_| updater::update_now());
        with_string("dev-volume", |_| ctx().bar.show_volume_pop());
        with_string("dev-shuffle", |_| ctx().bar.toggle_shuffle());
        // Plays a playlist or album from the top, as its Play button does.
        with_string("dev-play-context", |uri| ctx().with_engine(|e| e.play_context(uri, None, None)));
        with_string("dev-search", |query| {
            navigate(Route::Search, true);
            ctx().search.entry.set_text(query);
        });
        with_string("dev-render", |path| {
            // A widget paintable only fills in on the next redraw, so ask for
            // one and save a moment later.
            let window = ctx().window.clone();
            let content = window.content().unwrap_or_else(|| window.clone().upcast());
            let paintable = gtk::WidgetPaintable::new(Some(&content));
            content.queue_draw();
            let path = path.to_owned();
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let (w, h) = (window.width() as f32, window.height() as f32);
                let snapshot = gtk::Snapshot::new();
                snapshot.scale(2.0, 2.0);
                paintable.current_image().snapshot(&snapshot, w as f64, h as f64);
                let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) else {
                    log::warn!("dev-render: nothing to render ({w}x{h})");
                    return;
                };
                let viewport = gtk::graphene::Rect::new(0.0, 0.0, w * 2.0, h * 2.0);
                if let Err(e) = renderer.render_texture(&node, Some(&viewport)).save_to_png(&path) {
                    log::warn!("dev-render failed: {e}");
                }
            });
        });
    }
    with_string("copy-text", |text| {
        #[cfg(windows)]
        let copied = frame::copy_text(text);
        #[cfg(not(windows))]
        let copied = false;
        if !copied {
            ctx().window.clipboard().set_text(text);
        }
        toast("Link copied");
    });
    with_string("queue", |uri| {
        let Some(api) = ctx().api() else { return };
        let uri = uri.to_owned();
        spawn_local(async move {
            match rt::spawn(async move { api.add_to_queue(&uri).await }).await {
                Ok(()) => toast("Added to queue"),
                Err(e) => toast(&format!("Couldn't add to queue: {e}")),
            }
        });
    });

    app.set_accels_for_action("app.search", &["<Ctrl>k", "<Ctrl>l", "<Ctrl>f"]);
    app.set_accels_for_action("app.next", &["<Ctrl>Right"]);
    app.set_accels_for_action("app.prev", &["<Ctrl>Left"]);
    app.set_accels_for_action("app.volume-up", &["<Ctrl>Up"]);
    app.set_accels_for_action("app.volume-down", &["<Ctrl>Down"]);
    app.set_accels_for_action("app.quit", &["<Ctrl>q"]);
    app.set_accels_for_action("app.preferences", &["<Ctrl>comma"]);
}

/// Space plays/pauses from anywhere except while typing.
fn install_keys(window: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|controller, key, _, modifiers| {
        if key != gdk::Key::space || !modifiers.is_empty() {
            return glib::Propagation::Proceed;
        }
        let typing = controller
            .widget()
            .and_then(|w| w.root())
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::TextView>());
        if typing || ctx().root.visible_child_name().as_deref() != Some("main") {
            return glib::Propagation::Proceed;
        }
        play_pause();
        glib::Propagation::Stop
    });
    window.add_controller(keys);
}

pub fn raise() {
    ctx().window.present();
}

/// Lets Spotify know this device is going away, then exits. Not through
/// closing the window: with a dialog open (Preferences), libadwaita closes
/// the dialog instead and onIfy would keep running (which stalled updates).
pub fn quit() {
    let ctx = ctx();
    ctx.generation.set(ctx.generation.get() + 1);
    ctx.window.set_visible(false);
    #[cfg(windows)]
    tray::stop();
    let goodbye = match ctx.engine.take() {
        Some(engine) => {
            engine.shutdown();
            300
        }
        None => 0,
    };
    let app = ctx.app.clone();
    glib::timeout_add_local_once(Duration::from_millis(goodbye), move || app.quit());
}

pub fn open_links(app: &adw::Application, files: &[gio::File], _hint: &str) {
    activate(app);
    for file in files {
        let link = file.uri().to_string();
        if self::ctx().api().is_some() {
            open_link(&link);
        } else {
            self::ctx().pending_link.replace(Some(link));
        }
    }
}

/// `spotify:kind:id` for `https://open.spotify.com/[intl-xx/]kind/id`;
/// anything else as it is.
pub fn uri_of_link(link: &str) -> Option<String> {
    let Some(path) = link.strip_prefix("https://open.spotify.com/") else {
        return Some(link.to_owned());
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.starts_with("intl-")).collect();
    match parts.as_slice() {
        [kind, id, ..] => Some(format!("spotify:{kind}:{id}")),
        _ => None,
    }
}

/// Opens `spotify:kind:id` or `https://open.spotify.com/[intl-xx/]kind/id`.
fn open_link(link: &str) {
    let Some(uri) = uri_of_link(link) else { return };
    if uri.starts_with("spotify:track:") {
        ctx().with_engine(|e| e.play_tracks(vec![uri.clone()], 0));
    } else {
        open_uri(&uri);
    }
}
