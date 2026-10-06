#!/bin/sh
# Packs target/release/onify into dist/onIfy-x86_64.AppImage with the GTK it
# was built against. Run from the repo root after `cargo build --release`.
#
# GTK 4.20 and libadwaita 1.8 are newer than most distros ship, so they go in
# the AppImage. glibc doesn't, so it runs on systems with a glibc at least as
# new as the build machine's (CI builds on Arch: other rolling distros).
set -eu

tools=${TOOLS:-$PWD/target/appimage-tools}
appdir=$PWD/target/AppDir
mkdir -p "$tools" dist
rm -rf "$appdir"

fetch() {
    [ -x "$tools/$1" ] || { curl -fsSL -o "$tools/$1" "$2" && chmod +x "$tools/$1"; }
}
fetch linuxdeploy https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
fetch linuxdeploy-plugin-gtk.sh https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
# GTK 4.20 builds its media and print backends in, so there's no gtk-4.0
# modules folder to copy; the plugin fails without this guard.
sed -i 's|^\(\s*\)copy_lib_tree "$gtk4_libdir" "$APPDIR/"$|\1[ -d "$gtk4_libdir" ] \&\& copy_lib_tree "$gtk4_libdir" "$APPDIR/"|' \
    "$tools/linuxdeploy-plugin-gtk.sh"
export PATH="$tools:$PATH"
# No FUSE in containers; the tools unpack themselves instead.
export APPIMAGE_EXTRACT_AND_RUN=1
export DEPLOY_GTK_VERSION=4

# GTK's own widgets (search field, spinners, window buttons) use these icons.
mkdir -p "$appdir/usr/share/icons"
cp -r /usr/share/icons/Adwaita "$appdir/usr/share/icons/"
rm -rf "$appdir/usr/share/icons/Adwaita/cursors"
mkdir -p "$appdir/usr/share/icons/hicolor"
cp /usr/share/icons/hicolor/index.theme "$appdir/usr/share/icons/hicolor/" 2>/dev/null || true

linuxdeploy --appdir "$appdir" \
    --executable target/release/onify \
    --desktop-file data/dev.orqz.onIfy.desktop \
    --icon-file data/icons/dev.orqz.onIfy.svg \
    --plugin gtk

# The GTK plugin forces X11 (blurry under scaling on Wayland) and a plain
# Adwaita theme over libadwaita's; onIfy wants neither.
sed -i -e '/^export GDK_BACKEND=/d' -e '/^export GTK_THEME=/d' "$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"

LDAI_OUTPUT=dist/onIfy-x86_64.AppImage OUTPUT=dist/onIfy-x86_64.AppImage \
    linuxdeploy --appdir "$appdir" --output appimage
ls -lh dist/onIfy-x86_64.AppImage
