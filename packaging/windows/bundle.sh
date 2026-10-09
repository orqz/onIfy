#!/bin/sh
# Lays out dist/onify: onify.exe plus the GTK DLLs and data it needs, in the
# bin/ lib/ share/ shape GTK looks for next to its own DLLs on Windows. Run
# from the repo root in an MSYS2 UCRT64 shell after `cargo build --release`;
# onify.iss turns the folder into an installer.
set -eu

out=dist/onify
rm -rf "$out"
mkdir -p "$out/bin" "$out/lib" "$out/share/glib-2.0/schemas" "$out/share/icons"
cp target/release/onify.exe "$out/bin/"
cp packaging/windows/onify.ico "$out/"

# Image loaders (SVG icons go through librsvg's). gdk-pixbuf is relocatable
# on Windows, so the loader list is rewritten with paths relative to the app.
cp -r "$MINGW_PREFIX/lib/gdk-pixbuf-2.0" "$out/lib/"
loaders="$out/lib/gdk-pixbuf-2.0/2.10.0"
GDK_PIXBUF_MODULEDIR="$loaders/loaders" gdk-pixbuf-query-loaders \
    | sed -E 's|^"[^"]*[/\\]+(lib[/\\]+gdk-pixbuf-2\.0[/\\]+2\.10\.0[/\\]+loaders[/\\]+[^"/\\]+)"|"\1"|' \
    > "$loaders/loaders.cache"

# Every MSYS2 DLL that onify.exe or a loader links against, however deep.
for file in "$out/bin/onify.exe" "$loaders"/loaders/*.dll; do
    [ -e "$file" ] || continue
    ldd "$file" | awk '{print $3}' | grep -i "^$MINGW_PREFIX/bin/" | while read -r dll; do
        cp -u "$dll" "$out/bin/"
    done
done

cp "$MINGW_PREFIX/share/glib-2.0/schemas/gschemas.compiled" "$out/share/glib-2.0/schemas/"
# GTK's own widgets (search field, spinners, window buttons) use these icons.
cp -r "$MINGW_PREFIX/share/icons/Adwaita" "$out/share/icons/"
rm -rf "$out/share/icons/Adwaita/cursors"
mkdir -p "$out/share/icons/hicolor"
cp "$MINGW_PREFIX/share/icons/hicolor/index.theme" "$out/share/icons/hicolor/" 2>/dev/null || true

echo "Loader list:"
cat "$loaders/loaders.cache" | grep '^"' || true
du -sh "$out"
