//! MPRIS, so media keys, playerctl and desktop widgets control onIfy.

use std::rc::Rc;

use mpris_server::{LoopStatus, Player};

use crate::ui::{self, ctx};

pub async fn start() -> Option<Rc<Player>> {
    let player = Player::builder("onify")
        .identity("onIfy")
        .desktop_entry(ui::APP_ID)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .can_control(true)
        .can_raise(true)
        .can_quit(true)
        .build()
        .await
        .map_err(|e| log::warn!("MPRIS unavailable: {e}"))
        .ok()?;

    player.connect_play_pause(|_| ui::play_pause());
    player.connect_play(|_| {
        if !ctx().bar.is_playing() {
            ui::play_pause();
        }
    });
    player.connect_pause(|_| {
        if ctx().bar.is_playing() {
            ui::play_pause();
        }
    });
    player.connect_stop(|_| {
        if ctx().bar.is_playing() {
            ui::play_pause();
        }
    });
    player.connect_next(|_| ctx().with_engine(|e| e.next()));
    player.connect_previous(|_| ctx().with_engine(|e| e.prev()));
    player.connect_seek(|_, offset| {
        ui::seek_to(ctx().bar.position_ms() as i64 + offset.as_millis());
    });
    player.connect_set_position(|_, _, position| ui::seek_to(position.as_millis()));
    player.connect_set_shuffle(|_, shuffle| ctx().with_engine(|e| e.set_shuffle(shuffle)));
    player.connect_set_loop_status(|_, status| {
        let (context, track) = match status {
            LoopStatus::None => (false, false),
            LoopStatus::Playlist => (true, false),
            LoopStatus::Track => (false, true),
        };
        ctx().with_engine(|e| e.set_repeat(context, track));
    });
    player.connect_set_volume(|_, volume| ctx().bar.set_volume_by_user(volume));
    player.connect_raise(|_| ui::raise());
    player.connect_quit(|_| ui::quit());

    let player = Rc::new(player);
    gtk::glib::spawn_future_local(player.run());
    Some(player)
}
