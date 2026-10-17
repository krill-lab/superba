#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'Usage: %s --target-dir DIR --version X.Y.Z --arch x86_64|aarch64 [--headless] [--notices FILE] --out DIR\n' "$0"
}

die() {
    printf 'package-linux: %s\n' "$*" >&2
    exit 1
}

target_dir=''
version=''
arch=''
out=''
notices=''
headless=false
while (($#)); do
    case "$1" in
        --target-dir|--version|--arch|--out|--notices)
            (($# >= 2)) || die "Missing value for $1"
            case "$1" in
                --target-dir) target_dir=$2 ;;
                --version) version=$2 ;;
                --arch) arch=$2 ;;
                --out) out=$2 ;;
                --notices) notices=$2 ;;
            esac
            shift 2
            ;;
        --headless) headless=true; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "Unknown argument: $1" ;;
    esac
done

[[ -n "$target_dir" && -n "$version" && -n "$arch" && -n "$out" ]] || { usage >&2; exit 1; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$ ]] || die 'Version must be X.Y.Z or X.Y.Z-alpha.N (without v)'
case "$arch" in x86_64|aarch64) ;; *) die 'Architecture must be x86_64 or aarch64' ;; esac
[[ "$headless" == false || "$arch" == x86_64 ]] || die 'The headless (musl) package supports x86_64 only'

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -- "$script_dir/../.." && pwd -P)
documents=(README.md LICENSE-MIT LICENSE-APACHE)
# The GUI embeds the Geist fonts (OFL-1.1 requires the license with every copy).
font_license=crates/superba-gui/assets/fonts/LICENSE-Geist-OFL.txt
# superba-router runs the opt-in local router; spb and superba start it from
# the directory they live in.
binaries=(superba spb superba-router)
package="superba-$version-linux-$arch"
if [[ "$headless" == true ]]; then
    binaries=(spb superba-router)
    package="spb-$version-linux-$arch-musl"
else
    documents+=("$font_license")
fi
[[ -z "$notices" || ( -f "$notices" && -r "$notices" ) ]] || die "Notices file is not readable: $notices"
for binary in "${binaries[@]}"; do
    [[ -f "$target_dir/$binary" && -r "$target_dir/$binary" ]] || die "Missing binary: $target_dir/$binary"
done
for document in "${documents[@]}"; do
    [[ -f "$repo_root/$document" && -r "$repo_root/$document" ]] || die "Missing release document: $document"
done

umask 022
mkdir -p -- "$out"
out=$(cd -- "$out" && pwd -P)
staging=$(mktemp -d "${TMPDIR:-/tmp}/superba-package.XXXXXX")
trap 'rm -rf -- "$staging"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -- "$staging/$package"
for binary in "${binaries[@]}"; do
    install -m 755 -- "$target_dir/$binary" "$staging/$package/$binary"
done
for document in "${documents[@]}"; do
    install -m 644 -- "$repo_root/$document" "$staging/$package/$(basename -- "$document")"
done
if [[ -n "$notices" ]]; then
    install -m 644 -- "$notices" "$staging/$package/THIRD-PARTY-NOTICES.txt"
fi
archive="$out/$package.tar.gz"
tar -czf "$staging/$package.tar.gz" -C "$staging" "$package"
mv -f -- "$staging/$package.tar.gz" "$archive"
printf '%s\n' "$archive"
