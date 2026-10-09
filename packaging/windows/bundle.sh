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

echo "Copying image loaders…"
# Image loaders (SVG icons go through librsvg's). gdk-pixbuf is relocatable
# on Windows, so the loader list is rewritten with paths relative to the app.
cp -r "$MINGW_PREFIX/lib/gdk-pixbuf-2.0" "$out/lib/"
loaders="$out/lib/gdk-pixbuf-2.0/2.10.0"
GDK_PIXBUF_MODULEDIR="$loaders/loaders" gdk-pixbuf-query-loaders \
    | sed -E 's|^"[^"]*[/\\]+(lib[/\\]+gdk-pixbuf-2\.0[/\\]+2\.10\.0[/\\]+loaders[/\\]+[^"/\\]+)"|"\1"|' \
    > "$loaders/loaders.cache"

# Every MSYS2 DLL that onify.exe or a loader links against, however deep.
# Read from each file's import table rather than with ldd, which starts the
# program under a debugger to see what it loads: on the ARM runner (where
# MSYS2's own tools run emulated) that could hang for good.
echo "Copying DLLs…"
objdump=$(command -v llvm-objdump || command -v objdump)
imports() {
    "$objdump" -p "$1" 2>/dev/null | sed -n 's/^[[:space:]]*DLL Name:[[:space:]]*//p' | tr -d '\r'
}
queue="$out/bin/onify.exe"
for loader in "$loaders"/loaders/*.dll; do
    [ -e "$loader" ] && queue="$queue
$loader"
done
seen=" "
while [ -n "$queue" ]; do
    file=$(printf '%s\n' "$queue" | head -n 1)
    queue=$(printf '%s\n' "$queue" | tail -n +2)
    for name in $(imports "$file"); do
        key=$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')
        case "$seen" in *" $key "*) continue ;; esac
        seen="$seen$key "
        dll=$(find "$MINGW_PREFIX/bin" -maxdepth 1 -iname "$name" | head -n 1)
        # Not in MSYS2: one of Windows' own.
        [ -n "$dll" ] || continue
        cp -u "$dll" "$out/bin/"
        queue="$queue
$dll"
    done
done
echo "$(ls "$out/bin" | wc -l) files in bin"

cp "$MINGW_PREFIX/share/glib-2.0/schemas/gschemas.compiled" "$out/share/glib-2.0/schemas/"
# Fontconfig's setup, for Pango's FreeType text rendering (see windows_fonts
# in ui/mod.rs); fontconfig looks for it in ..\etc\fonts next to its DLL.
if [ -d "$MINGW_PREFIX/etc/fonts" ]; then
    mkdir -p "$out/etc"
    cp -r "$MINGW_PREFIX/etc/fonts" "$out/etc/"
fi
# GTK's own widgets (search field, spinners, window buttons) use these icons.
cp -r "$MINGW_PREFIX/share/icons/Adwaita" "$out/share/icons/"
rm -rf "$out/share/icons/Adwaita/cursors"
mkdir -p "$out/share/icons/hicolor"
cp "$MINGW_PREFIX/share/icons/hicolor/index.theme" "$out/share/icons/hicolor/" 2>/dev/null || true

echo "Loader list:"
cat "$loaders/loaders.cache" | grep '^"' || true
du -sh "$out"
