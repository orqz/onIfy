#!/bin/sh
# Installs onIfy for the current user: the binary to ~/.local/bin, plus the
# app menu entry and icon. Re-run it after building to update.
#
#   packaging/linux/install.sh                 # target/release/onify
#   packaging/linux/install.sh ~/Downloads/onify   # e.g. the CI build
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
bin=${1:-$root/target/release/onify}
prefix=${XDG_DATA_HOME:-$HOME/.local/share}

if [ ! -f "$bin" ]; then
    echo "No onIfy binary at $bin. Build one first: cargo build --release" >&2
    exit 1
fi

install -Dm755 "$bin" "$HOME/.local/bin/onify"
install -Dm644 "$root/data/dev.orqz.onIfy.desktop" "$prefix/applications/dev.orqz.onIfy.desktop"
install -Dm644 "$root/data/icons/dev.orqz.onIfy.svg" "$prefix/icons/hicolor/scalable/apps/dev.orqz.onIfy.svg"
# Absolute path, so the menu entry works even if ~/.local/bin isn't on PATH.
sed -i "s|^Exec=onify|Exec=$HOME/.local/bin/onify|" "$prefix/applications/dev.orqz.onIfy.desktop"
update-desktop-database -q "$prefix/applications" 2>/dev/null || true
gtk-update-icon-cache -q -t "$prefix/icons/hicolor" 2>/dev/null || true
echo "Installed onIfy to $HOME/.local/bin/onify"
