Vendored from librespot-connect 0.8.0 (https://github.com/librespot-org/librespot, MIT license).

onify changes: src/spirc.rs, handle_prev: with no earlier song to go back to (the first song
played after starting a playlist, always the case when it's shuffled), Previous starts the
song over instead of stopping playback.
