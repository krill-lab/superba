#!/usr/bin/env bash
# Builds Superba's drag-to-install disk image (user bug 84): a Finder window (640 x 400 pt
# under the title bar) with the background of packaging/macos/dmg-background{,@2x}.png, Superba.app on the left
# and an Applications link on the right (128 pt icons), no toolbar, sidebar or status bar,
# the app icon as the volume icon and the volume name "Superba <version>". macOS only.
#
#   macos-dmg.sh build  --staging DIR --version X.Y.Z --out FILE.dmg
#       layout, then finish (local builds; CI's dmg-package job).
#   macos-dmg.sh layout --version X.Y.Z --out LAYOUT.dmg
#       dmgbuild (packaging/macos/dmg-requirements.txt, pinned by hash, in a private venv)
#       writes a read-write image from packaging/macos/dmg-settings.py: the window, the
#       icon positions, the background, the Applications link and the volume icon, but
#       none of Superba's files. The layout is then read back and checked.
#   macos-dmg.sh finish --layout LAYOUT.dmg --staging DIR --out FILE.dmg
#       hdiutil, ditto and chflags only: copies everything in DIR (Superba.app, spb, the
#       release documents) into a copy of the layout, hides all but Superba.app in Finder,
#       compresses the image (UDZO) and compares it with DIR. The release runs this in the
#       signing step, so it runs no third-party code.
#   macos-dmg.sh inspect FILE.dmg
#       prints and checks the layout of an image: window, view options, icon positions,
#       background, volume icon, files and their Finder flags.
#
# PYTHON: Python 3.10+ for dmgbuild (default: the newest python3.1x, then python3).
# TMPDIR: parent of the private work directory (falls back to RUNNER_TEMP, then /tmp).
set -euo pipefail

usage() {
    printf 'Usage: %s build --staging DIR --version X.Y.Z --out FILE.dmg\n' "$0"
    printf '       %s layout --version X.Y.Z --out LAYOUT.dmg\n' "$0"
    printf '       %s finish --layout LAYOUT.dmg --staging DIR --out FILE.dmg\n' "$0"
    printf '       %s inspect FILE.dmg\n' "$0"
}

die() {
    printf 'macos-dmg: %s\n' "$*" >&2
    exit 1
}

[[ $(uname -s) == Darwin ]] || die 'This script requires macOS (hdiutil, ditto, tiffutil).'
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -- "$script_dir/../.." && pwd -P)
settings="$repo_root/packaging/macos/dmg-settings.py"
requirements="$repo_root/packaging/macos/dmg-requirements.txt"
background="$repo_root/packaging/macos/dmg-background.png"
volume_icon="$script_dir/Superba.icns"
layout_reader="$script_dir/macos-dmg-layout.py"
for file in "$settings" "$requirements" "$background" "${background%.png}@2x.png" \
    "$volume_icon" "$layout_reader"; do
    [[ -f "$file" ]] || die "missing $file"
done

command=${1:-}
(($#)) && shift
version='' out='' staging='' layout='' image=''
while (($#)); do
    case "$1" in
        --version|--out|--staging|--layout)
            (($# >= 2)) || die "Missing value for $1"
            case "$1" in
                --version) version=$2 ;;
                --out) out=$2 ;;
                --staging) staging=$2 ;;
                --layout) layout=$2 ;;
            esac
            shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        -*) die "Unknown option: $1" ;;
        *) [[ -z "$image" ]] || die "Unexpected argument: $1"; image=$1; shift ;;
    esac
done
check_version() {
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$ ]] ||
        die 'Version must be X.Y.Z or X.Y.Z-alpha.N (without v)'
}
absolute() {
    mkdir -p -- "$(dirname -- "$1")"
    printf '%s/%s' "$(cd -- "$(dirname -- "$1")" && pwd -P)" "$(basename -- "$1")"
}

umask 022
work=$(mktemp -d "${TMPDIR:-${RUNNER_TEMP:-/tmp}}/superba-dmg.XXXXXX")
mounted=()
cleanup() {
    local status=$? mount_point
    trap - EXIT INT TERM
    set +e
    for mount_point in ${mounted[@]+"${mounted[@]}"}; do
        hdiutil detach -force "$mount_point" >/dev/null 2>&1
    done
    rm -rf -- "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# hdiutil on GitHub's macOS runners now and then fails with "Resource busy".
retry() {
    local attempt=1
    until "$@"; do
        ((attempt < 3)) || return 1
        printf 'macos-dmg: retrying %s (attempt %d failed)\n' "$1" "$attempt" >&2
        sleep $((attempt * 5))
        attempt=$((attempt + 1))
    done
}

is_mounted() {
    mount | grep -qF " on $1 ("
}

attach() { # IMAGE MOUNT_POINT -readonly|-readwrite
    mkdir -p -- "$2"
    retry hdiutil attach "$1" "$3" -nobrowse -noautoopen -noverify -owners off \
        -mountpoint "$2" >/dev/null
    mounted+=("$2")
}

detach() { # MOUNT_POINT
    local attempt kept=() mount_point
    sync
    for attempt in 1 2 3 4 5; do
        hdiutil detach "$1" >/dev/null 2>&1 && break
        sleep "$attempt"
    done
    if is_mounted "$1"; then
        hdiutil detach -force "$1" >/dev/null
    fi
    for mount_point in ${mounted[@]+"${mounted[@]}"}; do
        [[ "$mount_point" == "$1" ]] || kept+=("$mount_point")
    done
    mounted=(${kept[@]+"${kept[@]}"})
}

python=''
tools() {
    local candidate base=''
    if [[ -n "${PYTHON:-}" ]]; then
        base=$PYTHON
    else
        for candidate in python3.14 python3.13 python3.12 python3.11 python3.10 python3; do
            if command -v "$candidate" >/dev/null 2>&1; then
                base=$(command -v "$candidate")
                break
            fi
        done
    fi
    if [[ -z "$base" ]] || ! "$base" -I -c 'import sys; sys.exit(sys.version_info < (3, 10))'; then
        die 'dmgbuild needs Python 3.10 or newer; set PYTHON (for example a Homebrew python3)'
    fi
    "$base" -I -m venv "$work/venv"
    python="$work/venv/bin/python"
    "$python" -I -m pip install --quiet --disable-pip-version-check --no-cache-dir \
        --no-deps --only-binary :all: --require-hashes -r "$requirements"
}

check_layout() { # MOUNT_POINT VOLUME_NAME
    local mnt=$1 info flags
    "$python" -I "$layout_reader" "$mnt" "$settings" "$2"
    [[ -L "$mnt/Applications" && $(readlink "$mnt/Applications") == /Applications ]] ||
        die 'the image has no Applications link'
    cmp -s -- "$mnt/.VolumeIcon.icns" "$volume_icon" || die 'the volume icon is not Superba.icns'
    # kHasCustomIcon (0x0400): Finder flags are bytes 8-9 of the root's com.apple.FinderInfo.
    flags=$(xattr -px com.apple.FinderInfo "$mnt" 2>/dev/null | tr -d ' \n' | cut -c17-20)
    ((16#${flags:-0} & 0x0400)) || die 'the volume does not use its custom icon'
    printf 'volume icon: .VolumeIcon.icns (Finder flags 0x%s)\n' "$flags"
    info=$(tiffutil -info "$mnt/.background.tiff" 2>&1)
    if ! grep -q 'Image Width: 640 Image Length: 400' <<<"$info" ||
        ! grep -q 'Image Width: 1280 Image Length: 800' <<<"$info"; then
        printf '%s\n' "$info" >&2
        die '.background.tiff does not hold the 1x and 2x images'
    fi
    printf 'background: .background.tiff, %s\n' "$(awk '
        /Image Width:/ { if (size) printf "%s; ", size; size = $3 "x" $6 " px" }
        /Resolution:/ { sub(/,$/, "", $2); size = size " at " $2 " dpi" }
        END { print size }' <<<"$info")"
}

volume_name_of() { # MOUNT_POINT
    diskutil info "$1" | sed -n 's/^ *Volume Name: *//p' | head -n 1
}

do_layout() { # OUT
    local volume="Superba $version" built="$work/layout.dmg" attempt
    # dmgbuild records the background by volume name; a second volume of that name
    # would mount as "<name> 1".
    [[ ! -e "/Volumes/$volume" ]] || die "/Volumes/$volume exists: eject that image first"
    tools
    for attempt in 1 2 3; do
        rm -f -- "$built"
        if "$python" -I -m dmgbuild -s "$settings" -D "background=$background" \
            -D "volume_icon=$volume_icon" --detach-retries 12 "$volume" "$built"; then
            break
        fi
        if [[ -e "/Volumes/$volume" ]]; then hdiutil detach -force "/Volumes/$volume" || true; fi
        ((attempt < 3)) || die 'dmgbuild failed'
        sleep $((attempt * 5))
    done
    attach "$built" "$work/layout-check" -readonly
    check_layout "$work/layout-check" "$volume"
    detach "$work/layout-check"
    mv -f -- "$built" "$1"
}

do_finish() { # LAYOUT STAGING OUT
    local layout=$1 staging=$2 rw="$work/image.dmg" final="$work/final.dmg"
    local mnt="$work/image" check="$work/final-check" entry name need_kb flags
    local entries=()
    [[ -f "$layout" ]] || die "Not a disk image: $layout"
    [[ -d "$staging/Superba.app" ]] || die "No Superba.app in $staging"
    shopt -s nullglob dotglob
    entries=("$staging"/*)
    shopt -u nullglob dotglob

    cp -- "$layout" "$rw"
    need_kb=$(du -sk -- "$staging" | cut -f1)
    retry hdiutil resize -size "$((need_kb * 5 / 4 + 65536))k" "$rw"
    attach "$rw" "$mnt" -readwrite
    cp -- "$mnt/.DS_Store" "$work/layout.DS_Store"
    for entry in "${entries[@]}"; do
        name=$(basename -- "$entry")
        [[ ! -e "$mnt/$name" && ! -L "$mnt/$name" ]] || die "$name is part of the layout"
        ditto "$entry" "$mnt/$name"
        # Only Superba.app shows in the window; spb and the documents stay in the image.
        [[ "$name" == Superba.app ]] || chflags -h hidden "$mnt/$name"
    done
    rm -rf -- "$mnt/.fseventsd" "$mnt/.Trashes"
    detach "$mnt"
    retry hdiutil convert "$rw" -format UDZO -imagekey zlib-level=9 -ov -o "$final" >/dev/null
    rm -f -- "$rw"
    hdiutil verify -quiet "$final" || die 'hdiutil verify failed'

    attach "$final" "$check" -readonly
    cmp -s -- "$work/layout.DS_Store" "$check/.DS_Store" || die 'the .DS_Store changed'
    [[ -L "$check/Applications" && $(readlink "$check/Applications") == /Applications ]] ||
        die 'the image has no Applications link'
    for entry in "${entries[@]}"; do
        name=$(basename -- "$entry")
        diff -rq -- "$entry" "$check/$name" >/dev/null || die "$name differs from $entry"
        flags=$(stat -f %f "$check/$name")
        if [[ "$name" == Superba.app ]]; then
            ((!(flags & 0x8000))) || die 'Superba.app is hidden'
        else
            ((flags & 0x8000)) || die "$name is not hidden"
        fi
    done
    if codesign --verify --strict --deep "$staging/Superba.app" >/dev/null 2>&1; then
        codesign --verify --strict --deep "$check/Superba.app" ||
            die 'Superba.app in the image no longer verifies'
    fi
    detach "$check"
    mv -f -- "$final" "$3"
}

case "$command" in
    build)
        [[ -n "$staging" && -n "$version" && -n "$out" && -z "$layout$image" ]] || { usage >&2; exit 1; }
        check_version
        out=$(absolute "$out")
        do_layout "$work/layout-built.dmg"
        do_finish "$work/layout-built.dmg" "$staging" "$out"
        ;;
    layout)
        [[ -n "$version" && -n "$out" && -z "$staging$layout$image" ]] || { usage >&2; exit 1; }
        check_version
        out=$(absolute "$out")
        do_layout "$out"
        ;;
    finish)
        [[ -n "$layout" && -n "$staging" && -n "$out" && -z "$version$image" ]] || { usage >&2; exit 1; }
        out=$(absolute "$out")
        do_finish "$layout" "$staging" "$out"
        ;;
    inspect)
        [[ -n "$image" && -z "$version$out$staging$layout" ]] || { usage >&2; exit 1; }
        [[ -f "$image" ]] || die "Not a disk image: $image"
        tools
        attach "$image" "$work/inspect" -readonly
        check_layout "$work/inspect" "$(volume_name_of "$work/inspect")"
        ls -laO "$work/inspect"
        detach "$work/inspect"
        exit 0
        ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 1 ;;
esac
printf '%s\n' "$out"
