#!/usr/bin/env bash
# Builds and checks the Linux spb that the Windows MSI carries for WSL (user bug
# 54(d)): Superba runs it inside a WSL distribution to migrate the sessions there
# (superba_switch::wsl looks for spb-linux-x86_64 next to superba.exe). A static
# musl build, so it runs on any distribution whatever its libc. Run on an x86_64
# Linux machine with musl-gcc installed (Debian/Ubuntu: musl-tools):
#
#   scripts/release/build-wsl-helper.sh --out DIR [--no-build]
#
# --no-build checks and copies an spb built before (the release's musl job builds
# spb and superba-router together).
set -euo pipefail
target=x86_64-unknown-linux-musl out= build=1
while [ $# -gt 0 ]; do
  case $1 in
    --out) out=$2; shift 2 ;;
    --no-build) build=0; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] || { echo "--out is required" >&2; exit 2; }
if [ "$build" = 1 ]; then
  export CC_x86_64_unknown_linux_musl=musl-gcc CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
  rustup target add "$target"
  cargo build --release --locked -p superba-cli --bin spb --target "$target"
fi
bin=target/$target/release/spb
[ -f "$bin" ] || { echo "$bin was not built" >&2; exit 1; }
# Static: no program interpreter (a dynamic one would need the distribution's libc).
if readelf -lW "$bin" | grep -q 'Requesting program interpreter'; then
  echo "::error::$bin needs a program interpreter (not a static build)" >&2
  exit 1
fi
readelf -hW "$bin" | grep -q 'Machine:.*X86-64' || { echo "::error::$bin is not x86-64" >&2; exit 1; }
"$bin" --version
mkdir -p "$out"
install -m 755 "$bin" "$out/spb-linux-x86_64"
ls -l "$out/spb-linux-x86_64"
