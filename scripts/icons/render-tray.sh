#!/usr/bin/env bash
# Regenerates the tray icon rasters (item 70) from their two masters:
#   assets/icon/superba-tray.svg          -> assets/icon/tray/superba-tray-*.png, superba-tray.ico
#   assets/icon/superba-tray-template.svg -> assets/icon/tray/superba-tray-template-{18,36}.png
# Needs only cargo (resvg); output is deterministic.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(cd -- "$here/../.." && pwd -P)
: "${CARGO_TARGET_DIR:=$root/target-icons}"
export CARGO_TARGET_DIR
cargo run --quiet --release --manifest-path "$here/tray/Cargo.toml" -- \
    "$root/assets/icon/superba-tray.svg" "$root/assets/icon/superba-tray-template.svg" \
    "$root/assets/icon/tray"
