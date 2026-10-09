Vendored from librespot-connect 0.8.0 (https://github.com/librespot-org/librespot, MIT license).

onify changes: src/spirc.rs, handle_prev: with no earlier song to go back to (the first song
played after starting a playlist, always the case when it's shuffled), Previous starts the
song over instead of stopping playback.
src/spirc.rs, handle_prev: a second Previous within 5 s of one that started the song over
always goes back a song (the first may still be buffering, or the song may be past 3 s
again), and with no earlier song it starts the current one over instead of jumping to the
top of the playlist.
src/state/handle.rs, handle_shuffle: turning shuffle on keeps the songs played before, so
Previous still goes back to them.
src/cluster.rs and Spirc::clusters: the account's devices and the active one's playback, from
the cluster updates, for onify's device picker and "Playing on <device>".
