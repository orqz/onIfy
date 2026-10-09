# Flathub

Getting onify onto Flathub, so every Linux distro can install it from its
software store (GNOME Software, KDE Discover) and update it there.

## Submitting (once)

1. Release the version to submit as usual, then put its tag and the commit
   it points at into `io.github.orqz.onIfy.yml` here:
   `git rev-list -n 1 va.b.c.d`.
2. Regenerate the Rust sources list for that release and copy it here:
   `python3 packaging/flatpak/cargo-sources.py && cp packaging/flatpak/cargo-sources.json packaging/flathub/`
3. Check it with Flathub's own linter:
   `flatpak run --command=flatpak-builder-lint org.flatpak.Builder manifest io.github.orqz.onIfy.yml`
4. Fork https://github.com/flathub/flathub, make a branch off `new-pr`, add
   `io.github.orqz.onIfy.yml` and `cargo-sources.json` from here, and open a
   pull request against `new-pr` titled `Add io.github.orqz.onIfy`.
5. A Flathub reviewer builds it and asks for changes if needed. Once it's
   merged you get a repo `flathub/io.github.orqz.onIfy`; the app ID is under
   your GitHub account (io.github.orqz), so it counts as verified.

## After that

Each release: update the tag, commit and `cargo-sources.json` in the
`flathub/io.github.orqz.onIfy` repo (a pull request there builds a test
version first). onify's own updater leaves Flatpak installs to Flathub.
