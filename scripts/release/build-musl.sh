#!/usr/bin/env bash
# Builds the static musl programs of a release: spb and superba-router for the
# headless Linux archive (spb-<v>-linux-x86_64-musl.tar.gz), and spb again as
# spb-linux-x86_64, the helper the Windows MSI carries for WSL session migration
# (user bug 54(d)). The release (release.yml) and CI (ci.yml) both run exactly
# this script on the same runner image (ubuntu-22.04), so they cannot diverge.
#
#   scripts/release/build-musl.sh [--helper-out DIR]
#
# Needs rustup and musl-gcc (Debian/Ubuntu: musl-tools).
#
# Linking: rustc links the musl target as a static PIE with its own musl start
# files and libc ("self-contained"). The C code (aws-lc, SQLite, zstd) is
# compiled with musl-gcc, but musl-gcc must not be the linker: Ubuntu 22.04's
# musl 1.2.2 specs do not handle -static-pie and silently produce a dynamic PIE
# that needs /lib/ld-musl-x86_64.so.1, which glibc distributions do not have
# (the v0.1.1-alpha.3 musl archive shipped like that; the v0.1.1-alpha.5 build
# stopped at the helper check below).
#
# Every program is checked: x86-64, no program interpreter (static); spb
# --version runs, also in a glibc-only Debian container when Docker is there.
set -euo pipefail
target=x86_64-unknown-linux-musl helper=
while [ $# -gt 0 ]; do
  case $1 in
    --helper-out) helper=$2; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
command -v musl-gcc >/dev/null || { echo "musl-gcc missing (apt-get install musl-tools)" >&2; exit 1; }
export CC_x86_64_unknown_linux_musl=musl-gcc
unset CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER
rustup target add "$target"
cargo build --release --locked -p superba-cli -p superba-router --target "$target"
dir=target/$target/release
for program in spb superba-router; do
  bin=$dir/$program
  [ -f "$bin" ] || { echo "::error::$bin was not built" >&2; exit 1; }
  if readelf -lW "$bin" | grep -q 'Requesting program interpreter'; then
    echo "::error::$bin needs a program interpreter (not a static build)" >&2
    readelf -lW "$bin" | grep -i interpreter >&2
    exit 1
  fi
  readelf -hW "$bin" | grep -q 'Machine:.*X86-64' || { echo "::error::$bin is not x86-64" >&2; exit 1; }
  if command -v file >/dev/null; then file "$bin"; fi
done
"$dir/spb" --version
if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  docker run --rm -v "$PWD/$dir:/w:ro" debian:bookworm-slim /w/spb --version \
    || { echo "::error::spb does not run on a glibc-only Debian" >&2; exit 1; }
fi
if [ -n "$helper" ]; then
  mkdir -p "$helper"
  install -m 755 "$dir/spb" "$helper/spb-linux-x86_64"
  ls -l "$helper/spb-linux-x86_64"
fi
