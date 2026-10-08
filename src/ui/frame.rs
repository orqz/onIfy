//! Windows: onify draws its own title bar (the page headers), so Windows sees
//! a borderless window, and borderless windows don't snap when dragged to the
//! screen's top or sides, don't animate opening, closing and minimising, and
//! get no Snap Layouts on the maximise button. This gives the window a normal
//! Windows frame, hides it (WM_NCCALCSIZE), and tells Windows which parts of
//! onify are its title bar and its maximise button (WM_NCHITTEST), so moving,
//! snapping and resizing are Windows' own.
//!
//! GTK sizes the window as if the frame showed (and, should it take the frame
//! off again, shrinks the window by it); `subclass` undoes both.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::{gdk, glib};
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, GlobalFree, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea,
    DwmSetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, ScreenToClient};
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AdjustWindowRectEx, AllowSetForegroundWindow, EnumWindows, GWL_EXSTYLE, GWL_STYLE, GetPropW, GetWindowLongPtrW,
    GetWindowRect, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTMAXBUTTON, HTRIGHT, HTTOP,
    HTTOPLEFT, HTTOPRIGHT, IsIconic, IsZoomed, NCCALCSIZE_PARAMS, PostMessageW, RegisterWindowMessageW, STYLESTRUCT,
    SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetPropW,
    SetWindowLongPtrW, SetWindowPos, WINDOWPOS, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_NCACTIVATE, WM_NCCALCSIZE,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    WM_STYLECHANGING, WM_WINDOWPOSCHANGING, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU,
    WS_THICKFRAME,
};

/// The native frame's parts that GTK takes off and this puts back.
const FRAME: u32 = WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
/// Resizing starts this far in from the window's edge, in logical pixels...
const EDGE: i32 = 5;
/// ...and resizes diagonally this far along the edges from a corner.
const CORNER: i32 = 16;

/// What to do with GTK's SetWindowPos right after it took the frame off.
#[derive(Clone, Copy, PartialEq)]
enum Undo {
    Nothing,
    /// Keep the window's size and place (GTK shrinks it by the frame).
    Move,
    /// Same, and nothing else about the style changed, so no redraw either.
    MoveAndRedraw,
}

thread_local! {
    static WINDOW: RefCell<Option<glib::WeakRef<gtk::Window>>> = const { RefCell::new(None) };
    static UNDO: Cell<Undo> = const { Cell::new(Undo::Nothing) };
    /// Windows is moving or resizing the window for the user.
    static SIZING: Cell<bool> = const { Cell::new(false) };
    /// The maximise button while Windows treats it as its own (for Snap
    /// Layouts): GTK doesn't see the pointer there, so hover and press are
    /// shown by hand.
    static MAX_BUTTON: RefCell<Option<gtk::Widget>> = const { RefCell::new(None) };
    static MAX_PRESSED: Cell<bool> = const { Cell::new(false) };
    /// The title bar part the pointer was last over (0: onify's own area).
    static NC_PART: Cell<u32> = const { Cell::new(0) };
}

pub fn install(window: &gtk::Window) {
    let Some(hwnd) = hwnd(window) else { return };
    WINDOW.with_borrow_mut(|w| *w = Some(window.downgrade()));
    let id = window.application().and_then(|app| app.application_id()).unwrap_or_default();
    unsafe {
        SetWindowSubclass(hwnd, Some(subclass), 1, 0);
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style | FRAME) as isize);
    }
    // GTK times frames with GLib timers, which Windows rounds up to its
    // 15.6 ms tick unless asked for finer ones; GTK only asks during its own
    // animations, so scrolling ran at about 32 fps. While the window shows:
    window.connect_map(|_| unsafe {
        timeBeginPeriod(1);
    });
    window.connect_unmap(|_| unsafe {
        timeEndPeriod(1);
    });
    // Counted as decorated, GTK wants the frame as well and stops taking it
    // off on every layout (which cost two window calls per animation frame).
    if let Some(toplevel) = window.surface().and_downcast::<gdk::Toplevel>() {
        toplevel.set_decorated(true);
    }
    unsafe {
        // A pixel of Windows' frame inside the window brings its shadow back.
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
        DwmExtendFrameIntoClientArea(hwnd, &margins);
        let dark: i32 = 1;
        DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, (&raw const dark).cast(), 4);
        let corners = DWMWCP_ROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE as u32, (&raw const corners).cast(), 4);
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        // Marks the window for a second onify to find (hand_over_to_running).
        SetPropW(hwnd, wide(&id).as_ptr(), std::ptr::without_provenance_mut(1));
    }
}

/// Puts `text` on the clipboard right away. GTK's Windows clipboard only
/// hands text over once something pastes, and song links went missing.
pub fn copy_text(text: &str) -> bool {
    const CF_UNICODETEXT: u32 = 13;
    let window = WINDOW.with_borrow(|w| w.as_ref().and_then(|w| w.upgrade()));
    let Some(hwnd) = window.as_ref().and_then(hwnd) else { return false };
    let text = wide(text);
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return false;
        }
        EmptyClipboard();
        let mut copied = false;
        let memory = GlobalAlloc(GMEM_MOVEABLE, text.len() * 2);
        if !memory.is_null() {
            let target = GlobalLock(memory).cast::<u16>();
            if !target.is_null() {
                std::ptr::copy_nonoverlapping(text.as_ptr(), target, text.len());
                GlobalUnlock(memory);
                copied = !SetClipboardData(CF_UNICODETEXT, memory).is_null();
            }
            if !copied {
                GlobalFree(memory);
            }
        }
        CloseClipboard();
        copied
    }
}

fn hwnd(window: &gtk::Window) -> Option<HWND> {
    let surface = window.surface()?.downcast::<gdk4_win32::Win32Surface>().ok()?;
    Some(surface.handle().0 as HWND)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Sent by a second onify to the running one: show yourself.
fn show_message() -> u32 {
    thread_local! {
        static MESSAGE: u32 = unsafe { RegisterWindowMessageW(wide("io.github.orqz.onIfy.show").as_ptr()) };
    }
    MESSAGE.with(|m| *m)
}

/// Windows has no session bus for GTK to keep onify to one copy. When one is
/// already running, this brings it forward and returns true; the caller then
/// exits.
pub fn hand_over_to_running(app_id: &str) -> bool {
    unsafe {
        // Held until onify exits.
        let name = wide(&format!("Local\\{app_id}"));
        CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if GetLastError() != ERROR_ALREADY_EXISTS {
            return false;
        }
        // The running copy may still be opening its window.
        let prop = wide(app_id);
        for _ in 0..30 {
            let mut found: HWND = std::ptr::null_mut();
            let search = (prop.as_ptr(), &raw mut found);
            EnumWindows(Some(find_marked), (&raw const search) as LPARAM);
            if !found.is_null() {
                AllowSetForegroundWindow(ASFW_ANY);
                PostMessageW(found, show_message(), 0, 0);
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        true
    }
}

unsafe extern "system" fn find_marked(hwnd: HWND, lparam: LPARAM) -> i32 {
    unsafe {
        let (prop, found) = *(lparam as *const (*const u16, *mut HWND));
        if GetPropW(hwnd, prop).is_null() {
            return 1;
        }
        *found = hwnd;
        0
    }
}

unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    unsafe {
        match msg {
            // All of the window is onify's; maximised, Windows makes it a bit
            // bigger than the screen (for the hidden frame), so keep to it.
            WM_NCCALCSIZE => {
                if wparam != 0 && IsZoomed(hwnd) != 0 {
                    let params = &mut *(lparam as *mut NCCALCSIZE_PARAMS);
                    fit_to_work_area(&mut params.rgrc[0]);
                }
                return 0;
            }
            WM_NCHITTEST => return hit_test(hwnd, lparam) as LRESULT,
            // -1: don't paint a title bar for the (in)active change.
            WM_NCACTIVATE => return DefSubclassProc(hwnd, msg, wparam, -1),
            WM_STYLECHANGING if wparam as i32 == GWL_STYLE => {
                let style = &mut *(lparam as *mut STYLESTRUCT);
                // A popup window is GTK going fullscreen; let it.
                if style.styleNew & WS_POPUP == 0 && style.styleNew & FRAME != FRAME {
                    style.styleNew |= FRAME;
                    let only_frame = style.styleNew == style.styleOld;
                    UNDO.set(if only_frame { Undo::MoveAndRedraw } else { Undo::Move });
                }
            }
            WM_STYLECHANGING if wparam as i32 == GWL_EXSTYLE && UNDO.get() == Undo::MoveAndRedraw => {
                UNDO.set(Undo::Move);
            }
            WM_WINDOWPOSCHANGING => {
                let pos = &mut *(lparam as *mut WINDOWPOS);
                match UNDO.replace(Undo::Nothing) {
                    Undo::MoveAndRedraw => pos.flags = (pos.flags | SWP_NOMOVE | SWP_NOSIZE) & !SWP_FRAMECHANGED,
                    Undo::Move => pos.flags |= SWP_NOMOVE | SWP_NOSIZE,
                    Undo::Nothing if is_gtk_resize(hwnd, pos.flags) => {
                        // GTK asked for its size plus the frame.
                        let (width, height) = frame_size(hwnd);
                        pos.cx -= width;
                        pos.cy -= height;
                    }
                    Undo::Nothing => {}
                }
            }
            WM_ENTERSIZEMOVE => SIZING.set(true),
            WM_EXITSIZEMOVE => SIZING.set(false),
            WM_MOUSEMOVE => NC_PART.set(0),
            WM_NCMOUSEMOVE => {
                let part = wparam as u32;
                if NC_PART.replace(part) != part {
                    // GTK only follows the pointer over its own area: show it
                    // where the pointer went, or the button it just left
                    // stays highlighted.
                    let mut point = POINT { x: (lparam & 0xffff) as i16 as i32, y: ((lparam >> 16) & 0xffff) as i16 as i32 };
                    ScreenToClient(hwnd, &mut point);
                    let client = ((point.y as u16 as isize) << 16) | point.x as u16 as isize;
                    DefSubclassProc(hwnd, WM_MOUSEMOVE, 0, client);
                }
                hover_max_button(hwnd, part == HTMAXBUTTON);
            }
            WM_NCMOUSELEAVE => {
                NC_PART.set(0);
                MAX_PRESSED.set(false);
                hover_max_button(hwnd, false);
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam as u32 == HTMAXBUTTON => {
                MAX_PRESSED.set(true);
                set_max_button_state(gtk::StateFlags::ACTIVE, true);
                return 0;
            }
            WM_NCLBUTTONUP if wparam as u32 == HTMAXBUTTON => {
                set_max_button_state(gtk::StateFlags::ACTIVE, false);
                if MAX_PRESSED.replace(false) {
                    let button = MAX_BUTTON.with_borrow(Clone::clone);
                    if let Some(button) = button.and_then(|b| b.downcast::<gtk::Button>().ok()) {
                        // Not from inside the window procedure: maximising
                        // sends it more messages.
                        glib::idle_add_local_once(move || button.emit_clicked());
                    }
                }
                return 0;
            }
            m if m == show_message() => {
                glib::idle_add_local_once(super::raise);
                return 0;
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}

/// GTK resizing the window (gdk_win32_surface_resize), rather than Windows
/// or the user.
unsafe fn is_gtk_resize(hwnd: HWND, flags: u32) -> bool {
    const KINDS: u32 = SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_HIDEWINDOW;
    unsafe {
        !SIZING.get() && flags & KINDS == SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE && IsZoomed(hwnd) == 0 && IsIconic(hwnd) == 0
    }
}

/// The frame GTK adds to the size it wants, worked out the way GTK does.
unsafe fn frame_size(hwnd: HWND) -> (i32, i32) {
    unsafe {
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        AdjustWindowRectEx(&mut rect, style, 0, ex_style);
        (rect.right - rect.left, rect.bottom - rect.top)
    }
}

unsafe fn fit_to_work_area(rect: &mut RECT) {
    unsafe {
        let monitor = MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let work = info.rcWork;
            rect.left = rect.left.max(work.left);
            rect.top = rect.top.max(work.top);
            rect.right = rect.right.min(work.right);
            rect.bottom = rect.bottom.min(work.bottom);
        }
    }
}

unsafe fn hit_test(hwnd: HWND, lparam: LPARAM) -> u32 {
    let point = POINT { x: (lparam & 0xffff) as i16 as i32, y: ((lparam >> 16) & 0xffff) as i16 as i32 };
    unsafe {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        if IsZoomed(hwnd) == 0 {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let (edge, corner) = (EDGE * dpi / 96, CORNER * dpi / 96);
            let left = point.x < rect.left + edge;
            let right = point.x >= rect.right - edge;
            let top = point.y < rect.top + edge;
            let bottom = point.y >= rect.bottom - edge;
            let near_left = point.x < rect.left + corner;
            let near_right = point.x >= rect.right - corner;
            let near_top = point.y < rect.top + corner;
            let near_bottom = point.y >= rect.bottom - corner;
            let part = if (top && near_left) || (left && near_top) {
                HTTOPLEFT
            } else if (top && near_right) || (right && near_top) {
                HTTOPRIGHT
            } else if (bottom && near_left) || (left && near_bottom) {
                HTBOTTOMLEFT
            } else if (bottom && near_right) || (right && near_bottom) {
                HTBOTTOMRIGHT
            } else if top {
                HTTOP
            } else if bottom {
                HTBOTTOM
            } else if left {
                HTLEFT
            } else if right {
                HTRIGHT
            } else {
                0
            };
            if part != 0 {
                return part;
            }
        }
        let mut client = point;
        ScreenToClient(hwnd, &mut client);
        header_part(client.x, client.y)
    }
}

/// Header bars (their GtkWindowHandle) are the title bar, except for what's
/// in them that takes clicks; the maximise button is Windows' own.
fn header_part(x: i32, y: i32) -> u32 {
    let Some(window) = WINDOW.with_borrow(|w| w.as_ref().and_then(|w| w.upgrade())) else {
        return HTCLIENT;
    };
    let Some(surface) = window.surface() else { return HTCLIENT };
    let scale = surface.scale();
    let (dx, dy) = window.surface_transform();
    let mut current = window.pick(x as f64 / scale - dx, y as f64 / scale - dy, gtk::PickFlags::DEFAULT);
    while let Some(widget) = current {
        if widget.is::<gtk::WindowHandle>() {
            return HTCAPTION;
        }
        if widget.is::<gtk::Button>() {
            if widget.has_css_class("maximize") && widget.parent().is_some_and(|p| p.is::<gtk::WindowControls>()) {
                MAX_BUTTON.with_borrow_mut(|b| *b = Some(widget));
                return HTMAXBUTTON;
            }
            return HTCLIENT;
        }
        if widget.is_focusable() || widget.is::<gtk::Editable>() || widget.is::<gtk::Range>() || widget.is::<gtk::Switch>() {
            return HTCLIENT;
        }
        current = widget.parent();
    }
    HTCLIENT
}

fn hover_max_button(hwnd: HWND, hovered: bool) {
    set_max_button_state(gtk::StateFlags::PRELIGHT, hovered);
    if hovered {
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE | TME_NONCLIENT,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        unsafe { TrackMouseEvent(&mut track) };
    } else {
        set_max_button_state(gtk::StateFlags::ACTIVE, false);
    }
}

fn set_max_button_state(state: gtk::StateFlags, on: bool) {
    MAX_BUTTON.with_borrow(|button| {
        let Some(button) = button else { return };
        if on {
            button.set_state_flags(state, false);
        } else {
            button.unset_state_flags(state);
        }
    });
}
