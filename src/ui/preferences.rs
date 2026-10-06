//! Preferences: local file folders, Discord and Last.fm.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

use super::{ctx, integrations};
use crate::{lastfm, rt};

pub fn present(window: &adw::ApplicationWindow) {
    let page = adw::PreferencesPage::new();
    page.add(&look_group());
    page.add(&performance_group());
    page.add(&local_group());
    page.add(&discord_group());
    page.add(&lastfm_group());
    let dialog = adw::PreferencesDialog::builder().title("Preferences").build();
    dialog.add(&page);
    dialog.present(Some(window));
}

fn open_url(url: &str) {
    gtk::UriLauncher::new(url).launch(None::<&gtk::Window>, gio::Cancellable::NONE, |_| {});
}

/// A row that opens a web page, for "where do I get this" hints.
fn link_row(title: &str, subtitle: &str, url: &'static str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    row.connect_activated(move |_| open_url(url));
    row
}

fn save() {
    ctx().settings.borrow().save();
}

fn look_group() -> adw::PreferencesGroup {
    use super::backdrop::Mood;
    let group = adw::PreferencesGroup::builder().title("Appearance").build();
    let labels = gtk::StringList::new(&["Bright", "Normal", "Dark"]);
    let current = Mood::from_name(&ctx().settings.borrow().background);
    let background = adw::ComboRow::builder()
        .title("Background")
        .subtitle("How much the blurred album cover is dimmed")
        .model(&labels)
        .selected(Mood::ALL.iter().position(|m| *m == current).unwrap_or(1) as u32)
        .build();
    background.connect_selected_notify(|row| {
        let mood = Mood::ALL.get(row.selected() as usize).copied().unwrap_or_default();
        ctx().settings.borrow_mut().background = mood.name().to_owned();
        save();
        ctx().backdrop.set_mood(mood);
    });
    group.add(&background);

    let styles = gtk::StringList::new(&["Vinyl (experimental)", "Liquid glass"]);
    let style = adw::ComboRow::builder()
        .title("Style")
        .subtitle("Vinyl drops the glass panels and spins the record while music plays")
        .model(&styles)
        .selected(if ctx().settings.borrow().style == "glass" { 1 } else { 0 })
        .build();
    style.connect_selected_notify(|row| {
        let name = if row.selected() == 1 { "glass" } else { "vinyl" };
        ctx().settings.borrow_mut().style = name.to_owned();
        save();
        super::apply_style(name);
    });
    group.add(&style);
    group
}

fn switch(title: &str, subtitle: &str, active: bool, changed: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build();
    row.connect_active_notify(move |row| {
        changed(row.is_active());
        save();
    });
    row
}

fn performance_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Performance")
        .description("onIfy is light already; these make it lighter still.")
        .build();
    let settings = ctx().settings.borrow().clone_switches();
    group.add(&switch("Animations", "Fades, slides and button effects", settings.0, |on| {
        ctx().settings.borrow_mut().animations = on;
        super::set_animations(on);
    }));
    group.add(&switch("Album Cover Background", "The blurred cover behind everything", settings.1, |on| {
        ctx().settings.borrow_mut().cover_background = on;
        ctx().backdrop.set_covers(on);
    }));
    group.add(&switch("Preload Songs on Hover", "Songs start instantly; uses a little bandwidth", settings.2, |on| {
        ctx().settings.borrow_mut().hover_preload = on;
    }));
    group.add(&switch("Low Memory Mode", "Keeps fewer covers and pages in memory", settings.3, |on| {
        ctx().settings.borrow_mut().low_memory = on;
        crate::images::set_low_memory(on);
        super::drop_hidden_pages();
    }));
    group
}

fn local_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Local Files")
        .description("Songs in these folders show up in Local Files and play wherever they appear in your playlists.")
        .build();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    fill_folders(&list);
    group.add(&list);

    let add = gtk::Button::builder()
        .icon_name("onify-list-add-symbolic")
        .tooltip_text("Add Folder")
        .valign(gtk::Align::Center)
        .build();
    add.add_css_class("flat");
    add.connect_clicked(glib::clone!(
        #[weak]
        list,
        move |button| {
            let parent = button.root().and_downcast::<gtk::Window>();
            glib::spawn_future_local(async move {
                let dialog = gtk::FileDialog::builder().title("Choose a Music Folder").modal(true).build();
                let Ok(folder) = dialog.select_folder_future(parent.as_ref()).await else { return };
                let Some(path) = folder.path() else { return };
                change_folders(|folders| {
                    if !folders.contains(&path) {
                        folders.push(path);
                    }
                });
                fill_folders(&list);
            });
        }
    ));
    group.set_header_suffix(Some(&add));
    group
}

fn fill_folders(list: &gtk::ListBox) {
    list.remove_all();
    let folders = ctx().settings.borrow().local_folders.clone();
    if folders.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No folders yet")
            .subtitle("Add one with the + button")
            .build();
        list.append(&row);
    }
    for folder in folders {
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder.display().to_string());
        let row = adw::ActionRow::builder()
            .title(name)
            .subtitle(folder.display().to_string())
            .build();
        let remove = gtk::Button::builder()
            .icon_name("onify-list-remove-symbolic")
            .tooltip_text("Remove Folder")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        remove.connect_clicked(glib::clone!(
            #[weak]
            list,
            move |_| {
                change_folders(|folders| folders.retain(|f| *f != folder));
                fill_folders(&list);
            }
        ));
        row.add_suffix(&remove);
        list.append(&row);
    }
}

fn change_folders(change: impl FnOnce(&mut Vec<PathBuf>)) {
    let ctx = ctx();
    change(&mut ctx.settings.borrow_mut().local_folders);
    save();
    super::local_folders_changed();
}

fn discord_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Discord")
        .description("Shows the song you're listening to on your profile, in the Discord app or in arRPC clients like onCord.")
        .build();
    let shared = ctx();
    let settings = shared.settings.borrow();
    let enabled = adw::SwitchRow::builder()
        .title("Show Listening Activity")
        .active(settings.discord_enabled)
        .build();
    let id = adw::EntryRow::builder()
        .title("Application ID")
        .text(settings.discord_client_id.as_str())
        .show_apply_button(true)
        .input_purpose(gtk::InputPurpose::Digits)
        .build();
    drop(settings);
    enabled.bind_property("active", &id, "sensitive").sync_create().build();

    enabled.connect_active_notify(|row| {
        ctx().settings.borrow_mut().discord_enabled = row.is_active();
        save();
        integrations::discord_settings_changed();
    });
    id.connect_apply(|row| {
        ctx().settings.borrow_mut().discord_client_id = row.text().trim().to_owned();
        save();
        integrations::discord_settings_changed();
    });

    group.add(&enabled);
    group.add(&id);
    group.add(&link_row(
        "Get an Application ID",
        "Create an application named onIfy and copy its ID",
        "https://discord.com/developers/applications",
    ));
    group
}

fn lastfm_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Last.fm for Local Files")
        .description("Spotify's own Last.fm connection already scrobbles what you play in onIfy. Only set this up if your local files don't show up on Last.fm.")
        .build();
    let account = ctx().settings.borrow().lastfm.clone();
    let key = adw::EntryRow::builder()
        .title("API Key")
        .text(account.api_key.as_str())
        .show_apply_button(true)
        .build();
    let secret = adw::PasswordEntryRow::builder()
        .title("Shared Secret")
        .text(account.secret.as_str())
        .show_apply_button(true)
        .build();
    let status = adw::ActionRow::builder().title("Account").build();
    let connect = gtk::Button::builder().valign(gtk::Align::Center).build();
    status.add_suffix(&connect);
    show_account(&status, &connect);
    // A new key or secret needs a new login.
    let credentials_changed = glib::clone!(
        #[weak]
        status,
        #[weak]
        connect,
        #[weak]
        key,
        #[weak]
        secret,
        move |_: &adw::EntryRow| {
            {
                let ctx = ctx();
                let mut settings = ctx.settings.borrow_mut();
                settings.lastfm.api_key = key.text().trim().to_owned();
                settings.lastfm.secret = secret.text().trim().to_owned();
                settings.lastfm.session.clear();
                settings.lastfm.username.clear();
            }
            save();
            show_account(&status, &connect);
        }
    );
    key.connect_apply(credentials_changed.clone());
    secret.connect_apply(move |row| credentials_changed(row.upcast_ref()));

    connect.connect_clicked(glib::clone!(
        #[weak]
        status,
        move |button| {
            let account = ctx().settings.borrow().lastfm.clone();
            if account.is_connected() {
                {
                    let ctx = ctx();
                    let mut settings = ctx.settings.borrow_mut();
                    settings.lastfm.session.clear();
                    settings.lastfm.username.clear();
                }
                save();
                show_account(&status, button);
                return;
            }
            button.set_sensitive(false);
            status.set_subtitle("Approve onIfy in your browser…");
            let (status, button) = (status.clone(), button.clone());
            glib::spawn_future_local(async move {
                let login = account.clone();
                let token = rt::spawn(async move { lastfm::request_token(&login).await }).await;
                let result = match token {
                    Ok((token, url)) => {
                        open_url(&url);
                        let login = account.clone();
                        rt::spawn(async move { lastfm::finish_login(&login, &token).await }).await
                    }
                    Err(e) => Err(e),
                };
                match result {
                    Ok((session, username)) => {
                        {
                            let ctx = ctx();
                            let mut settings = ctx.settings.borrow_mut();
                            settings.lastfm.session = session;
                            settings.lastfm.username = username;
                        }
                        save();
                        show_account(&status, &button);
                    }
                    Err(e) => {
                        show_account(&status, &button);
                        status.set_subtitle(&format!("Couldn't connect: {e}"));
                    }
                }
            });
        }
    ));

    group.add(&key);
    group.add(&secret);
    group.add(&link_row(
        "Get an API Key",
        "Free from Last.fm; any application name works",
        "https://www.last.fm/api/account/create",
    ));
    group.add(&status);
    group
}

fn show_account(row: &adw::ActionRow, button: &gtk::Button) {
    let account = ctx().settings.borrow().lastfm.clone();
    if account.is_connected() {
        row.set_subtitle(&format!("Scrobbling as {}", account.username));
        button.set_label("Disconnect");
        button.remove_css_class("suggested-action");
    } else {
        row.set_subtitle(if account.can_authenticate() {
            "Not connected"
        } else {
            "Enter your API key and shared secret first"
        });
        button.set_label("Connect");
        button.add_css_class("suggested-action");
    }
    button.set_sensitive(account.is_connected() || account.can_authenticate());
}
