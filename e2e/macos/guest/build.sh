#!/usr/bin/env bash
set -euo pipefail
source_dir=$1
cd "$source_dir"
[[ $(uname -m) == arm64 ]] || { echo 'Native arm64 VM required' >&2; exit 1; }
available=$(/bin/df -k "$source_dir" | /usr/bin/awk 'NR==2 {print $4}')
(( available >= 25 * 1024 * 1024 )) || { echo 'Less than 25 GiB free; use CI DMG' >&2; exit 75; }
build_root=$(dirname "$source_dir")
export RUSTUP_HOME="$build_root/rustup" CARGO_HOME="$build_root/cargo"
export CARGO_TARGET_DIR="$build_root/target-mx"
export CARGO_BUILD_JOBS=6 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
export CARGO_PROFILE_RELEASE_DEBUG=0 MACOSX_DEPLOYMENT_TARGET=14.0
export PATH="$CARGO_HOME/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    /usr/bin/curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$source_dir/rustup-init.sh"
    /bin/sh "$source_dir/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain stable
fi
if ! rustup toolchain list | /usr/bin/grep -q '^stable-aarch64-apple-darwin'; then
    rustup toolchain install stable --profile minimal
fi
# The router helper is needed for M03/M06 and belongs beside the GUI binary.
cargo +stable build --release --locked --target aarch64-apple-darwin \
    -p superba-gui -p superba-cli -p superba-router &
build_pid=$!
cleanup_build() {
    if kill -0 "$build_pid" 2>/dev/null; then
        kill -INT "$build_pid" 2>/dev/null || true
        wait "$build_pid" || true
    fi
    rm -rf "$CARGO_TARGET_DIR"
}
trap cleanup_build EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
while kill -0 "$build_pid" 2>/dev/null; do
    available=$(/bin/df -k "$source_dir" | /usr/bin/awk 'NR==2 {print $4}')
    used=$(/usr/bin/du -sk "$CARGO_TARGET_DIR" 2>/dev/null | /usr/bin/awk '{print $1}')
    # Leave headroom below the 20 GiB cap for one compiler output between polls.
    if (( available < 5 * 1024 * 1024 || ${used:-0} > 18 * 1024 * 1024 )); then
        echo 'Build disk guard tripped; stopping cargo' >&2
        kill "$build_pid" 2>/dev/null || true
        wait "$build_pid" || true
        exit 75
    fi
    sleep 5
done
wait "$build_pid"
version=$(/usr/bin/sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | /usr/bin/head -1)
bins="$CARGO_TARGET_DIR/aarch64-apple-darwin/release"
[[ ! -e dist ]] || { echo 'dist already exists; use a fresh source ref directory' >&2; exit 1; }
mkdir dist
bash scripts/release/macos-bundle.sh --binary "$bins/superba" --helper "$bins/superba-router" \
    --version "$version" --out dist/stage
install -m 755 "$bins/spb" dist/stage/spb
codesign --force --sign - dist/stage/Superba.app/Contents/MacOS/superba-router
codesign --force --sign - dist/stage/Superba.app
codesign --force --sign - dist/stage/spb
codesign --verify --strict --deep dist/stage/Superba.app
ln -s /Applications dist/stage/Applications
hdiutil create -volname 'Superba development' -srcfolder dist/stage -format UDZO dist/superba-dev-arm64.dmg
