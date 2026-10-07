//! Preferences: appearance, performance and local file folders.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::glib;

use super::ctx;

pub fn present(window: &adw::ApplicationWindow) {
    let page = adw::PreferencesPage::new();
    page.add(&look_group());
    page.add(&playback_group());
    page.add(&performance_group());
    page.add(&local_group());
    page.add(&updates_group());
    let dialog = adw::PreferencesDialog::builder().title("Preferences").build();
    dialog.add(&page);
    dialog.present(Some(window));
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

    let styles = gtk::StringList::new(&["Vinyl", "Liquid glass"]);
    let style = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Vinyl spins a record while music plays; Liquid glass floats frosted panels")
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

fn playback_group() -> adw::PreferencesGroup {
    use crate::spotify::Quality;
    let group = adw::PreferencesGroup::builder().title("Playback").build();
    let labels: Vec<&str> = Quality::ALL.iter().map(|q| q.label()).collect();
    let current = Quality::from_name(&ctx().settings.borrow().quality);
    let quality = adw::ComboRow::builder()
        .title("Streaming Quality")
        .subtitle("Higher sounds better and uses more data. Takes effect at the next pause")
        .model(&gtk::StringList::new(&labels))
        .selected(Quality::ALL.iter().position(|q| *q == current).unwrap_or(2) as u32)
        .build();
    quality.connect_selected_notify(|row| {
        let quality = Quality::ALL.get(row.selected() as usize).copied().unwrap_or(Quality::VeryHigh);
        ctx().settings.borrow_mut().quality = quality.name().to_owned();
        save();
        super::player_settings_changed();
    });
    group.add(&quality);
    group
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


fn updates_group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Updates")
        .description(format!("This is onIfy {}.", crate::update::current()))
        .build();
    let active = ctx().settings.borrow().check_updates;
    group.add(&switch("Check for Updates", "Asks before installing anything", active, |on| {
        ctx().settings.borrow_mut().check_updates = on;
        save();
    }));
    let now = adw::ButtonRow::builder().title("Check Now").build();
    now.connect_activated(|_| super::updater::check(true));
    group.add(&now);
    group
}
