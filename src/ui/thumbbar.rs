//! Windows: Like, Previous, Play/Pause and Next under onify's preview when
//! the pointer rests on its taskbar button (a thumbnail toolbar), as in
//! Spotify's app. Windows says when the taskbar button exists
//! (TaskbarButtonCreated, again whenever Explorer restarts); clicks come back
//! to the window as WM_COMMAND (see frame.rs).

use std::cell::{Cell, RefCell};

use gtk::gdk_pixbuf::prelude::*;
use gtk::prelude::*;
use gtk::{gdk_pixbuf, gio};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
};
use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THUMBBUTTON, TaskbarList};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO, RegisterWindowMessageW, SM_CXSMICON};
use windows_sys::core::{GUID, HRESULT};

const IID_TASKBAR_LIST3: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

/// ITaskbarList3, as far as onify calls it: the vtable in declaration order
/// (IUnknown, ITaskbarList, ITaskbarList2, then ITaskbarList3's own).
#[repr(C)]
struct TaskbarList3 {
    vtable: *const Vtable,
}

// Slots onify doesn't call are only there to keep the layout.
#[allow(dead_code)]
#[repr(C)]
struct Vtable {
    query_interface: usize,
    add_ref: usize,
    release: usize,
    hr_init: unsafe extern "system" fn(*mut TaskbarList3) -> HRESULT,
    add_tab: usize,
    delete_tab: usize,
    activate_tab: usize,
    set_active_alt: usize,
    mark_fullscreen_window: usize,
    set_progress_value: usize,
    set_progress_state: usize,
    register_tab: usize,
    unregister_tab: usize,
    set_tab_order: usize,
    set_tab_active: usize,
    thumb_bar_add_buttons: unsafe extern "system" fn(*mut TaskbarList3, HWND, u32, *const THUMBBUTTON) -> HRESULT,
    thumb_bar_update_buttons: unsafe extern "system" fn(*mut TaskbarList3, HWND, u32, *const THUMBBUTTON) -> HRESULT,
}

const LIKE: u32 = 1;
const PREVIOUS: u32 = 2;
const PLAY: u32 = 3;
const NEXT: u32 = 4;

struct Icons {
    heart: HICON,
    heart_filled: HICON,
    previous: HICON,
    play: HICON,
    pause: HICON,
    next: HICON,
}

struct Toolbar {
    taskbar: *mut TaskbarList3,
    hwnd: HWND,
    icons: Icons,
}

thread_local! {
    static TOOLBAR: RefCell<Option<Toolbar>> = const { RefCell::new(None) };
    static PLAYING: Cell<bool> = const { Cell::new(false) };
    static LIKED: Cell<bool> = const { Cell::new(false) };
    /// Only Spotify songs can be liked (not local files).
    static LIKEABLE: Cell<bool> = const { Cell::new(false) };
}

/// The message Windows sends once onify's taskbar button exists.
pub fn created_message() -> u32 {
    thread_local! {
        static MESSAGE: u32 = unsafe { RegisterWindowMessageW(wide("TaskbarButtonCreated").as_ptr()) };
    }
    MESSAGE.with(|m| *m)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The taskbar button (re)appeared: put the buttons under its preview.
pub fn taskbar_button_created(hwnd: HWND) {
    let made = TOOLBAR.with_borrow(|t| t.as_ref().map(|t| t.hwnd == hwnd));
    if made != Some(true) {
        let Some(toolbar) = (unsafe { make(hwnd) }) else { return };
        TOOLBAR.with_borrow_mut(|t| *t = Some(toolbar));
    }
    TOOLBAR.with_borrow(|toolbar| {
        let Some(toolbar) = toolbar else { return };
        let buttons = [LIKE, PREVIOUS, PLAY, NEXT].map(|id| button(toolbar, id));
        unsafe {
            let add = (*(*toolbar.taskbar).vtable).thumb_bar_add_buttons;
            add(toolbar.taskbar, toolbar.hwnd, buttons.len() as u32, buttons.as_ptr());
        }
    });
}

unsafe fn make(hwnd: HWND) -> Option<Toolbar> {
    unsafe {
        // GTK has COM set up on this thread already; this is a no-op then.
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let mut taskbar: *mut core::ffi::c_void = std::ptr::null_mut();
        let made = CoCreateInstance(&TaskbarList, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &IID_TASKBAR_LIST3, &mut taskbar);
        if made < 0 || taskbar.is_null() {
            log::warn!("no taskbar buttons: CoCreateInstance said {made:#x}");
            return None;
        }
        let taskbar = taskbar.cast::<TaskbarList3>();
        ((*(*taskbar).vtable).hr_init)(taskbar);
        // Small-icon size for the screen the window is on (16px at 100%).
        let px = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd).max(96)).max(16);
        let icons = Icons {
            heart: icon("onify-heart-symbolic", px),
            heart_filled: icon("onify-heart-filled-symbolic", px),
            previous: icon("onify-media-skip-backward-symbolic", px),
            play: icon("onify-media-playback-start-symbolic", px),
            pause: icon("onify-media-playback-pause-symbolic", px),
            next: icon("onify-media-skip-forward-symbolic", px),
        };
        Some(Toolbar { taskbar, hwnd, icons })
    }
}

fn button(toolbar: &Toolbar, id: u32) -> THUMBBUTTON {
    let icons = &toolbar.icons;
    let (playing, liked, likeable) = (PLAYING.get(), LIKED.get(), LIKEABLE.get());
    let (icon, tip, enabled) = match id {
        LIKE if liked => (icons.heart_filled, "Remove from Liked Songs", likeable),
        LIKE => (icons.heart, "Save to Liked Songs", likeable),
        PREVIOUS => (icons.previous, "Previous", true),
        PLAY if playing => (icons.pause, "Pause", true),
        PLAY => (icons.play, "Play", true),
        _ => (icons.next, "Next", true),
    };
    let mut button: THUMBBUTTON = unsafe { std::mem::zeroed() };
    button.dwMask = THB_ICON | THB_TOOLTIP | THB_FLAGS;
    button.iId = id;
    button.hIcon = icon;
    for (slot, unit) in button.szTip.iter_mut().zip(tip.encode_utf16().take(259)) {
        *slot = unit;
    }
    button.dwFlags = if enabled { THBF_ENABLED } else { THBF_DISABLED };
    button
}

fn update(id: u32) {
    TOOLBAR.with_borrow(|toolbar| {
        let Some(toolbar) = toolbar else { return };
        let button = button(toolbar, id);
        unsafe {
            let update = (*(*toolbar.taskbar).vtable).thumb_bar_update_buttons;
            update(toolbar.taskbar, toolbar.hwnd, 1, &button);
        }
    });
}

/// A button under the preview was clicked.
pub fn clicked(id: u32) {
    match id {
        LIKE => super::ctx().bar.like.emit_clicked(),
        PREVIOUS => super::prev(),
        PLAY => super::play_pause(),
        NEXT => super::next(),
        _ => {}
    }
}

pub fn set_playing(playing: bool) {
    if PLAYING.replace(playing) != playing {
        update(PLAY);
    }
}

pub fn set_liked(liked: bool) {
    if LIKED.replace(liked) != liked {
        update(LIKE);
    }
}

pub fn set_likeable(likeable: bool) {
    if LIKEABLE.replace(likeable) != likeable {
        update(LIKE);
    }
}

/// One of onify's symbolic icons, white, as a `px`-sized Windows icon.
fn icon(name: &str, px: i32) -> HICON {
    let path = format!("/io/github/orqz/onIfy/icons/scalable/actions/{name}.svg");
    let Ok(data) = gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE) else {
        return std::ptr::null_mut();
    };
    let svg = String::from_utf8_lossy(&data).replace("#2e3436", "#ffffff");
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.set_size(px, px);
    if loader.write(svg.as_bytes()).is_err() || loader.close().is_err() {
        return std::ptr::null_mut();
    }
    match loader.pixbuf() {
        Some(pixbuf) => unsafe { to_hicon(&pixbuf) },
        None => std::ptr::null_mut(),
    }
}

/// An RGBA pixbuf as a 32-bit Windows icon (its alpha does the masking).
unsafe fn to_hicon(pixbuf: &gdk_pixbuf::Pixbuf) -> HICON {
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let (stride, channels) = (pixbuf.rowstride() as usize, pixbuf.n_channels() as usize);
    let pixels = pixbuf.read_pixel_bytes();
    unsafe {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        // Negative: rows run top to bottom, as in the pixbuf.
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(std::ptr::null_mut(), &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        let out = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (width * height * 4) as usize);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let i = y * stride + x * channels;
                let o = (y * width as usize + x) * 4;
                let alpha = if channels == 4 { pixels[i + 3] } else { 255 };
                out[o] = pixels[i + 2];
                out[o + 1] = pixels[i + 1];
                out[o + 2] = pixels[i];
                out[o + 3] = alpha;
            }
        }
        // An empty mask: with a 32-bit colour bitmap, Windows takes what
        // shows from its alpha.
        let mask_bits = vec![0u8; (width as usize).div_ceil(16) * 2 * height as usize];
        let mask = CreateBitmap(width, height, 1, 1, mask_bits.as_ptr().cast());
        let icon_info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&icon_info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}
