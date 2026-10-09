//! Offers new versions (see crate::update for how each install updates).

use std::cell::{Cell, RefCell};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use super::{ctx, toast};
use crate::rt;
use crate::update::{self, Install, Release};

thread_local! {
    /// One update question at a time.
    static ASKING: Cell<bool> = const { Cell::new(false) };
}

/// Checks a little after startup and every 6 hours, while the setting is on.
/// These checks are quiet: offline or no releases, nothing shows.
pub fn start() {
    let install = update::install();
    if install == Install::WindowsInstaller {
        glib::timeout_add_local_once(Duration::from_secs(30), update::clean_up_installers);
    }
    if install == Install::Flatpak || (install == Install::Manual && std::env::var_os("ONIFY_DEV").is_none()) {
        return;
    }
    glib::timeout_add_local_once(Duration::from_secs(15), check);
    glib::timeout_add_local(Duration::from_secs(6 * 60 * 60), || {
        check();
        glib::ControlFlow::Continue
    });
}

/// A quiet check: only a new version shows anything.
fn check() {
    if !ctx().settings.borrow().check_updates || ASKING.get() {
        return;
    }
    let install = update::install();
    glib::spawn_future_local(async move {
        let asked = install.clone();
        if let Ok(Some(release)) = rt::spawn(async move { update::newer(&asked).await }).await {
            offer(release, install);
        }
    });
}

/// Check Now in Preferences: the answer shows inside Preferences (the main
/// window's toasts sit behind it), and the row says it's checking meanwhile.
pub fn check_in(dialog: &adw::PreferencesDialog, row: &adw::ButtonRow) {
    if ASKING.get() {
        return;
    }
    row.set_sensitive(false);
    row.set_title("Checking…");
    let install = update::install();
    let (dialog, row) = (dialog.downgrade(), row.downgrade());
    glib::spawn_future_local(async move {
        let asked = install.clone();
        let found = rt::spawn(async move { update::newer(&asked).await }).await;
        if let Some(row) = row.upgrade() {
            row.set_sensitive(true);
            row.set_title("Check Now");
        }
        let say = |message: String| match dialog.upgrade() {
            Some(dialog) => {
                let toast = adw::Toast::new(&message);
                toast.set_use_markup(false);
                toast.set_timeout(3);
                dialog.add_toast(toast);
            }
            None => toast(&message),
        };
        match found {
            Ok(Some(release)) => offer(release, install),
            Ok(None) => say(format!("You're up to date: onify {} is the latest version", update::current())),
            Err(e) => say(format!("Couldn't check for updates: {e}")),
        }
    });
}

fn offer(release: Release, install: Install) {
    let automatic = release.installable(&install);
    let mut body = format!("onify {} is out. You have {}.", release.version, update::current());
    match install {
        _ if automatic => body.push_str(" onify restarts once it's installed."),
        Install::Package => body.push_str("\n\nUpdate it with your package manager: yay -Syu"),
        _ => body.push_str("\n\nGet it from the download page."),
    }
    if !release.notes.is_empty() {
        let notes: String = release.notes.chars().take(500).collect();
        body.push_str("\n\n");
        body.push_str(&notes);
    }
    let dialog = adw::AlertDialog::builder().heading("Update Available").body(&body).build();
    if automatic {
        dialog.add_responses(&[("later", "Later"), ("update", "Update and Restart")]);
        dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("update"));
    } else if install == Install::Package {
        dialog.add_responses(&[("later", "OK")]);
    } else {
        dialog.add_responses(&[("later", "Later"), ("page", "Open Download Page")]);
        dialog.set_response_appearance("page", adw::ResponseAppearance::Suggested);
    }
    dialog.set_close_response("later");

    let release = RefCell::new(Some(release));
    dialog.connect_response(None, move |_, response| {
        ASKING.set(false);
        let Some(release) = release.take() else { return };
        match response {
            "update" => apply(release, install.clone()),
            "page" => {
                gtk::UriLauncher::new(&release.page).launch(None::<&gtk::Window>, gtk::gio::Cancellable::NONE, |_| {});
            }
            _ => {}
        }
    });
    ASKING.set(true);
    dialog.present(Some(&ctx().window));
}

/// Developer aid (ONIFY_DEV): installs the newest release without asking.
pub fn update_now() {
    let install = update::install();
    glib::spawn_future_local(async move {
        let asked = install.clone();
        match rt::spawn(async move { update::newer(&asked).await }).await {
            Ok(Some(release)) => apply(release, install),
            Ok(None) => toast("No newer version"),
            Err(e) => toast(&format!("Couldn't check for updates: {e}")),
        }
    });
}

fn apply(release: Release, install: Install) {
    toast(&format!("Downloading onify {}…", release.version));
    glib::spawn_future_local(async move {
        match rt::spawn(async move { update::install_release(&release, &install).await }).await {
            Ok(()) => super::quit(),
            Err(e) => toast(&format!("Update failed: {e}")),
        }
    });
}
