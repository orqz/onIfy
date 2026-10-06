#!/bin/sh
# Builds and installs the onIfy package from this clone instead of GitHub,
# e.g. to try local changes as a real package. Update with:
# git pull && packaging/arch/local.sh
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sed "s|git+https://github.com/orqz/onIfy.git|git+file://$repo|" "$repo/packaging/arch/PKGBUILD" > "$work/PKGBUILD"
cd "$work" && makepkg -si
