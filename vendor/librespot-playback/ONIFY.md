Vendored from librespot-playback 0.8.0 (https://github.com/librespot-org/librespot, MIT license).

onIfy changes: src/decoder/resampler.rs, and src/decoder/symphonia_decoder.rs now
resamples local files that are not 44.1 kHz and plays mono files on both channels.
src/player.rs: seeking waits for read_ahead_before_playback (1 s) of audio instead of
read_ahead_during_playback (5 s).
src/player.rs: the next song is preloaded as soon as the current one is fully downloaded,
not only in its last 30 seconds, so skips start instantly.
src/local_file.rs: files without artist/title tags also answer to URIs named after the file
("Artist - Title" when the name reads like that), and untagged files are no longer skipped.
