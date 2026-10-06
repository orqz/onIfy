#!/bin/sh
# Builds dist/onIfy.app (and dist/onIfy-macos-arm64.dmg) from
# target/release/onify and the Homebrew GTK it links against. Run from the
# repo root on a Mac with `brew install gtk4 libadwaita librsvg
# adwaita-icon-theme dylibbundler`, after `cargo build --release`.
#
# Signed ad hoc, not with an Apple ID: the first open needs right-click →
# Open (or `xattr -dr com.apple.quarantine /Applications/onIfy.app`).
set -eu

brew=$(brew --prefix)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
minos="$(sw_vers -productVersion | cut -d. -f1).0"
app=dist/onIfy.app
contents=$app/Contents
rm -rf "$app" dist/dmg
mkdir -p "$contents/MacOS" "$contents/Resources" "$contents/Frameworks"

cp target/release/onify "$contents/MacOS/onify"
cp packaging/macos/onify.icns "$contents/Resources/"
sed -e "s/@VERSION@/$version/g" -e "s/@MINOS@/$minos/g" packaging/macos/Info.plist > "$contents/Info.plist"

# Image loaders (SVG icons go through librsvg's), with a loader list whose
# paths onIfy fills in at startup (use_bundled_gtk in main.rs).
pixbuf="$contents/Resources/lib/gdk-pixbuf-2.0/2.10.0"
mkdir -p "$pixbuf/loaders"
find "$brew/lib/gdk-pixbuf-2.0/2.10.0/loaders" \( -name '*.so' -o -name '*.dylib' \) -exec cp -L {} "$pixbuf/loaders/" \;
# Loaders keep their Homebrew path as their own name (librsvg's SVG one
# does); give each its place in the app instead.
for loader in "$pixbuf"/loaders/*; do
    chmod +w "$loader"
    install_name_tool -id "@executable_path/../Resources/lib/gdk-pixbuf-2.0/2.10.0/loaders/$(basename "$loader")" "$loader"
done
gdk-pixbuf-query-loaders "$brew"/lib/gdk-pixbuf-2.0/2.10.0/loaders/* 2>/dev/null \
    | sed -E 's|^"[^"]*/loaders/([^"/]+)"|"@LOADERS@/\1"|' > "$pixbuf/loaders.cache.in" || true

# Homebrew dylibs, copied in and relinked to load from inside the app.
bundle() {
    dylibbundler -b -x "$1" -d "$contents/Frameworks" -p @executable_path/../Frameworks/ -s "$brew/lib" "$2"
}
bundle "$contents/MacOS/onify" -od
for loader in "$pixbuf"/loaders/*; do
    if [ -e "$loader" ]; then
        bundle "$loader" -of
    fi
done

mkdir -p "$contents/Resources/share/glib-2.0/schemas" "$contents/Resources/share/icons/hicolor"
cp "$brew/share/glib-2.0/schemas/gschemas.compiled" "$contents/Resources/share/glib-2.0/schemas/"
cp -RL "$brew/share/icons/Adwaita" "$contents/Resources/share/icons/"
rm -rf "$contents/Resources/share/icons/Adwaita/cursors"
cp "$brew/share/icons/hicolor/index.theme" "$contents/Resources/share/icons/hicolor/" 2>/dev/null || true

# Nothing may still point into Homebrew, or the app only works on this Mac.
leaks=$(find "$contents" -type f \( -name '*.dylib' -o -name '*.so' -o -path '*/MacOS/*' \) \
    -exec otool -L {} \; | grep -E "^[^[:space:]]|^[[:space:]]+($brew|/usr/local/(opt|Cellar))" \
    | grep -B1 -E "^[[:space:]]" | grep -v '^--$' || true)
if [ -n "$leaks" ]; then
    echo "Still linked to Homebrew:" >&2
    echo "$leaks" >&2
    exit 1
fi

codesign --force --deep --sign - "$app"

mkdir -p dist/dmg
cp -R "$app" dist/dmg/
ln -s /Applications dist/dmg/Applications
# hdiutil is sometimes "busy" on CI machines; give it a few tries.
for attempt in 1 2 3 4 5; do
    hdiutil create -volname onIfy -srcfolder dist/dmg -ov -format UDZO dist/onIfy-macos-arm64.dmg && break
    sleep 5
done
rm -rf dist/dmg
ls -lh dist/onIfy-macos-arm64.dmg
