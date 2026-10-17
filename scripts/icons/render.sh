#!/usr/bin/env bash
# Regenerates every raster icon from assets/icon/superba.svg:
#   assets/icon/png/superba-{16,32,48,64,128,256,512,1024}.png
#   assets/icon/superba.ico          (16/24/32/48/64/256, embedded in the Windows exe and MSI)
#   scripts/release/Superba.icns     (16..512 plus @2x, copied into the macOS bundle)
# Needs only cargo (a pure-Rust renderer: resvg); output is deterministic.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(cd -- "$here/../.." && pwd -P)
: "${CARGO_TARGET_DIR:=$root/target-icons}"
export CARGO_TARGET_DIR
cargo run --quiet --release --manifest-path "$here/render/Cargo.toml" -- \
    "$root/assets/icon/superba.svg" "$root/assets/icon/png" \
    "$root/assets/icon/superba.ico" "$root/scripts/release/Superba.icns"
