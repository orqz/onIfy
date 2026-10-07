//! Windows: onIfy's icon next to the clock. Closing the window leaves onIfy
//! playing there (Preferences can turn that off); clicking the icon brings
//! the window back, right-clicking it has play/pause, next, previous and quit.

use std::cell::RefCell;

use gtk::{gdk_pixbuf, glib};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

use super::ctx;
use crate::spotify::NowPlaying;

struct Tray {
    icon: TrayIcon,
    play: MenuItem,
    next: MenuItem,
    previous: MenuItem,
    show: MenuItem,
    quit: MenuItem,
}

enum Pick {
    PlayPause,
    Next,
    Previous,
    Show,
    Quit,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

pub fn start() {
    let play = MenuItem::new("Play", true, None);
    let next = MenuItem::new("Next", true, None);
    let previous = MenuItem::new("Previous", true, None);
    let show = MenuItem::new("Show onIfy", true, None);
    let quit = MenuItem::new("Quit onIfy", true, None);
    let menu = Menu::new();
    let separator = PredefinedMenuItem::separator();
    if let Err(e) = menu.append_items(&[&play, &next, &previous, &separator, &show, &quit]) {
        log::warn!("tray menu unavailable: {e}");
        return;
    }
    let Some(image) = icon() else {
        log::warn!("tray icon unavailable: no image");
        return;
    };
    let built = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(image)
        .with_tooltip("onIfy")
        .build();
    let icon = match built {
        Ok(icon) => icon,
        Err(e) => {
            log::warn!("tray icon unavailable: {e}");
            return;
        }
    };
    // Both arrive inside the icon's window procedure; act once it returns.
    MenuEvent::set_event_handler(Some(|event: MenuEvent| {
        glib::idle_add_once(move || picked(&event.id));
    }));
    TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            glib::idle_add_once(super::raise);
        }
    }));
    TRAY.with_borrow_mut(|t| *t = Some(Tray { icon, play, next, previous, show, quit }));
}

/// Whether the icon is there to bring a closed window back from.
pub fn shown() -> bool {
    TRAY.with_borrow(Option::is_some)
}

/// Takes the icon away now; left to exit, Windows keeps showing it until
/// the pointer passes over it.
pub fn stop() {
    let tray = TRAY.with_borrow_mut(Option::take);
    drop(tray);
}

fn picked(id: &MenuId) {
    let pick = TRAY.with_borrow(|tray| {
        let tray = tray.as_ref()?;
        [
            (&tray.play, Pick::PlayPause),
            (&tray.next, Pick::Next),
            (&tray.previous, Pick::Previous),
            (&tray.show, Pick::Show),
            (&tray.quit, Pick::Quit),
        ]
        .into_iter()
        .find(|(item, _)| item.id() == id)
        .map(|(_, pick)| pick)
    });
    match pick {
        Some(Pick::PlayPause) => super::play_pause(),
        Some(Pick::Next) => ctx().with_engine(|e| e.next()),
        Some(Pick::Previous) => ctx().with_engine(|e| e.prev()),
        Some(Pick::Show) => super::raise(),
        Some(Pick::Quit) => super::quit(),
        None => {}
    }
}

pub fn set_track(now: &NowPlaying) {
    // Windows cuts tooltips off at 127 characters.
    let text: String = format!("{} · {}", now.name, crate::api::join_names(&now.artists)).chars().take(120).collect();
    TRAY.with_borrow(|tray| {
        if let Some(tray) = tray {
            let _ = tray.icon.set_tooltip(Some(text));
        }
    });
}

pub fn set_playing(playing: bool) {
    TRAY.with_borrow(|tray| {
        if let Some(tray) = tray {
            tray.play.set_text(if playing { "Pause" } else { "Play" });
        }
    });
}

/// The installer puts onIfy's .ico next to bin\, sharp at every tray size; a
/// copy run from the source tree draws its logo instead.
fn icon() -> Option<Icon> {
    let installed = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("onify.ico")));
    if let Some(path) = installed.filter(|path| path.is_file()) {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
        if let Ok(icon) = Icon::from_path(&path, Some((size, size))) {
            return Some(icon);
        }
    }
    let logo = format!("/io/github/orqz/onIfy/icons/scalable/apps/{}.svg", super::APP_ID);
    let pixbuf = gdk_pixbuf::Pixbuf::from_resource_at_scale(&logo, 32, 32, true).ok()?;
    if !pixbuf.has_alpha() || pixbuf.n_channels() != 4 {
        return None;
    }
    let (width, height, stride) = (pixbuf.width() as usize, pixbuf.height() as usize, pixbuf.rowstride() as usize);
    let pixels = pixbuf.read_pixel_bytes();
    let rgba = (0..height).flat_map(|row| &pixels[row * stride..row * stride + width * 4]).copied().collect();
    Icon::from_rgba(rgba, width as u32, height as u32).ok()
}
