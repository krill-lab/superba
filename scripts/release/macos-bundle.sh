#!/usr/bin/env bash
set -euo pipefail

# File operations only: this script also runs on Linux. The caller builds the
# universal binary; optional Superba.icns lives beside this script.
usage() {
    printf 'Usage: %s --binary PATH [--helper PATH]... --version X.Y.Z --out DIR [--bundle-id ID] [--notices FILE]\n' "$0"
}

die() {
    printf 'macos-bundle: %s\n' "$*" >&2
    exit 1
}

binary=''
version=''
out=''
bundle_id='com.krill-code.superba'
notices=''
helpers=()
while (($#)); do
    case "$1" in
        --helper)
            (($# >= 2)) || die "Missing value for $1"
            helpers+=("$2")
            shift 2
            ;;
        --binary|--version|--out|--bundle-id|--notices)
            (($# >= 2)) || die "Missing value for $1"
            case "$1" in
                --binary) binary=$2 ;;
                --version) version=$2 ;;
                --out) out=$2 ;;
                --bundle-id) bundle_id=$2 ;;
                --notices) notices=$2 ;;
            esac
            shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        *) die "Unknown argument: $1" ;;
    esac
done

[[ -n "$binary" && -n "$version" && -n "$out" ]] || { usage >&2; exit 1; }
[[ -f "$binary" && -r "$binary" ]] || die "Binary is not a readable file: $binary"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$ ]] || die 'Version must be X.Y.Z or X.Y.Z-alpha.N (without v)'
[[ "$bundle_id" =~ ^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$ ]] || die 'Invalid bundle identifier'
[[ -z "$notices" || ( -f "$notices" && -r "$notices" ) ]] || die "Notices file is not readable: $notices"
for helper in ${helpers[@]+"${helpers[@]}"}; do
    [[ -f "$helper" && -r "$helper" ]] || die "Helper is not a readable file: $helper"
    case "$(basename -- "$helper")" in
        superba|''|.*) die "Invalid helper name: $helper" ;;
    esac
done

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
template="$script_dir/Info.plist.in"
[[ -f "$template" ]] || die 'Info.plist.in is missing'
umask 022
mkdir -p -- "$out"
out=$(cd -- "$out" && pwd -P)
destination="$out/Superba.app"
[[ ! -e "$destination" && ! -L "$destination" ]] || die "Output already exists: $destination"
staging=$(mktemp -d "$out/.superba-app.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

app="$staging/Superba.app"
mkdir -p -- "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 -- "$binary" "$app/Contents/MacOS/superba"
# Helper executables (superba-router) live next to the main executable, where
# superba finds them; codesign signs every Mach-O in Contents/MacOS.
for helper in ${helpers[@]+"${helpers[@]}"}; do
    install -m 755 -- "$helper" "$app/Contents/MacOS/$(basename -- "$helper")"
done
icon_entry=''
if [[ -f "$script_dir/Superba.icns" ]]; then
    install -m 644 -- "$script_dir/Superba.icns" "$app/Contents/Resources/Superba.icns"
    icon_entry='<key>CFBundleIconFile</key><string>Superba.icns</string>'
fi
sed -e "s|@VERSION@|${version%%-*}|g" \
    -e "s|@BUNDLE_ID@|$bundle_id|g" \
    -e 's|@EXECUTABLE@|superba|g' \
    -e 's|@NAME@|Superba|g' \
    -e "s|@ICON_ENTRY@|$icon_entry|g" \
    "$template" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
# License texts: the embedded Geist fonts (OFL-1.1) and the linked crates.
repo_root=$(cd -- "$script_dir/../.." && pwd -P)
install -m 644 -- "$repo_root/crates/superba-gui/assets/fonts/LICENSE-Geist-OFL.txt" \
    "$app/Contents/Resources/LICENSE-Geist-OFL.txt"
if [[ -n "$notices" ]]; then
    install -m 644 -- "$notices" "$app/Contents/Resources/THIRD-PARTY-NOTICES.txt"
fi
mv -- "$app" "$destination"
printf '%s\n' "$destination"
