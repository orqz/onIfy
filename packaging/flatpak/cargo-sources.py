#!/usr/bin/env python3
"""Writes cargo-sources.json: every crate in Cargo.lock as a flatpak-builder
source, so the Flatpak builds offline. Re-run after Cargo.lock changes."""

import json
import pathlib
import tomllib

root = pathlib.Path(__file__).resolve().parents[2]
lock = tomllib.loads((root / "Cargo.lock").read_text())

sources = []
for package in lock["package"]:
    if not package.get("source", "").startswith("registry+"):
        continue
    name, version, checksum = package["name"], package["version"], package["checksum"]
    dest = f"cargo/vendor/{name}-{version}"
    sources.append({
        "type": "archive",
        "archive-type": "tar-gzip",
        "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
        "sha256": checksum,
        "dest": dest,
    })
    sources.append({
        "type": "inline",
        "contents": json.dumps({"package": checksum, "files": {}}),
        "dest": dest,
        "dest-filename": ".cargo-checksum.json",
    })
sources.append({
    "type": "inline",
    "contents": '[source.vendored-sources]\ndirectory = "cargo/vendor"\n\n'
                '[source.crates-io]\nreplace-with = "vendored-sources"\n',
    "dest": "cargo",
    "dest-filename": "config.toml",
})

out = pathlib.Path(__file__).with_name("cargo-sources.json")
out.write_text(json.dumps(sources, indent=4) + "\n")
print(f"{len(sources) // 2} crates")
