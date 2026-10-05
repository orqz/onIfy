//! Keeps glibc's allocator from hoarding memory onIfy has already freed.
//!
//! glibc gives each thread that allocates its own arena and rarely hands freed
//! pages back, so a burst of work (a page of tracks, a screenful of covers)
//! leaves the process tens of MB bigger than it needs to be. Other platforms'
//! allocators return memory on their own.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
use std::cell::Cell;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
use std::time::Duration;

/// Call first thing in `main`, before any threads start.
pub fn init() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        // Two arenas are plenty for onIfy's few busy threads.
        libc::mallopt(libc::M_ARENA_MAX, 2);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 1 << 20);
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
thread_local! {
    static PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Hands freed memory back to the system shortly after a burst of work ends.
pub fn trim_soon() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if !PENDING.replace(true) {
        gtk::glib::timeout_add_local_once(Duration::from_secs(2), || {
            PENDING.set(false);
            unsafe {
                libc::malloc_trim(0);
            }
        });
    }
}
