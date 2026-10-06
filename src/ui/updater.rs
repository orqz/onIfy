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
    if install == Install::Flatpak || (install == Install::Manual && std::env::var_os("ONIFY_DEV").is_none()) {
        return;
    }
    glib::timeout_add_local_once(Duration::from_secs(15), || check(false));
    glib::timeout_add_local(Duration::from_secs(6 * 60 * 60), || {
        check(false);
        glib::ControlFlow::Continue
    });
}

/// `manual`: from Preferences, so say when there's nothing new.
pub fn check(manual: bool) {
    if (!manual && !ctx().settings.borrow().check_updates) || ASKING.get() {
        return;
    }
    let install = update::install();
    glib::spawn_future_local(async move {
        let asked = install.clone();
        match rt::spawn(async move { update::newer(&asked).await }).await {
            Ok(Some(release)) => offer(release, install),
            Ok(None) if manual => toast(&format!("onIfy {} is the latest version", update::current())),
            Err(e) if manual => toast(&format!("Couldn't check for updates: {e}")),
            _ => {}
        }
    });
}

fn offer(release: Release, install: Install) {
    let automatic = release.installable(&install);
    let mut body = format!("onIfy {} is out. You have {}.", release.version, update::current());
    match install {
        _ if automatic => body.push_str(" onIfy restarts once it's installed."),
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
    toast(&format!("Downloading onIfy {}…", release.version));
    glib::spawn_future_local(async move {
        match rt::spawn(async move { update::install_release(&release, &install).await }).await {
            Ok(()) => super::quit(),
            Err(e) => toast(&format!("Update failed: {e}")),
        }
    });
}
