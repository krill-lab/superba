#!/usr/bin/env bash
# Regenerates every raster icon from the two masters:
#   assets/icon/superba.svg (Windows/Linux, the tile fills 928 of 1024 px) ->
#     assets/icon/png/superba-{16,32,48,64,128,256,512,1024}.png
#     assets/icon/superba.ico        (16/24/32/48/64/256, embedded in the Windows exe and MSI)
#     packaging/windows/superba-dialog.bmp (493x312) and superba-banner.bmp (493x58): the
#                                    MSI's WixUIDialogBmp / WixUIBannerBmp (24-bit, light)
#   assets/icon/superba-macos.svg (Apple's grid: 824 px tile at 100,100) ->
#     scripts/release/Superba.icns   (16..1024 incl. @2x, copied into the macOS bundle)
#     assets/icon/png/superba-macos-256.png (runtime Dock icon of an unbundled macOS binary)
# Needs only cargo (a pure-Rust renderer: resvg); output is deterministic.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(cd -- "$here/../.." && pwd -P)
: "${CARGO_TARGET_DIR:=$root/target-icons}"
export CARGO_TARGET_DIR
cargo run --quiet --release --manifest-path "$here/render/Cargo.toml" -- \
    "$root/assets/icon/superba.svg" "$root/assets/icon/superba-macos.svg" "$root/assets/icon/png" \
    "$root/assets/icon/superba.ico" "$root/scripts/release/Superba.icns" \
    "$root/packaging/windows/superba-dialog.bmp" "$root/packaging/windows/superba-banner.bmp"
