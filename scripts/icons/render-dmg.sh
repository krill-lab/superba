#!/usr/bin/env bash
# Regenerates the macOS disk image's window background (user bug 84) from its master:
#   packaging/macos/dmg-background.svg -> packaging/macos/dmg-background.png (640x400)
#                                         packaging/macos/dmg-background@2x.png (1280x800)
# scripts/release/macos-dmg.sh hands the pair to dmgbuild, which joins them into one
# multi-resolution TIFF. Needs cargo (resvg) and two typefaces: Geist from
# crates/superba-gui/assets/fonts and Noto Sans CJK SC Regular for the Chinese hint
# (Debian/Ubuntu: fonts-noto-cjk; macOS: brew install --cask font-noto-sans-cjk-sc),
# or SUPERBA_CJK_FONT=<file>. Output is deterministic for the same font files.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(cd -- "$here/../.." && pwd -P)
: "${CARGO_TARGET_DIR:=$root/target-icons}"
export CARGO_TARGET_DIR

cjk=${SUPERBA_CJK_FONT:-}
if [[ -z "$cjk" ]] && command -v fc-match >/dev/null; then
    # fc-match always answers; take its file only when the family really matches.
    if [[ $(fc-match --format='%{family}' 'Noto Sans CJK SC:style=Regular') == *'Noto Sans CJK SC'* ]]; then
        cjk=$(fc-match --format='%{file}' 'Noto Sans CJK SC:style=Regular')
    fi
fi
if [[ -z "$cjk" ]]; then
    for candidate in "$HOME/Library/Fonts/NotoSansCJKsc-Regular.otf" \
        /Library/Fonts/NotoSansCJKsc-Regular.otf "$HOME/Library/Fonts/NotoSansCJK-Regular.ttc"; do
        if [[ -f "$candidate" ]]; then cjk=$candidate; break; fi
    done
fi
[[ -n "$cjk" && -f "$cjk" ]] || {
    printf 'render-dmg: Noto Sans CJK SC Regular not found; install it or set SUPERBA_CJK_FONT\n' >&2
    exit 1
}

fonts="$root/crates/superba-gui/assets/fonts"
cargo run --quiet --release --manifest-path "$here/dmg/Cargo.toml" -- \
    "$root/packaging/macos/dmg-background.svg" "$root/packaging/macos" \
    "$fonts/Geist-Regular.ttf" "$fonts/Geist-Medium.ttf" "$cjk"
