#!/usr/bin/env bash
# Called only on a fresh hosted macOS runner, at the dispatch's exact SHA.
set -euo pipefail
[[ $(uname -m) == arm64 ]] || { echo 'An arm64 runner is required' >&2; exit 1; }
: "${RUNNER_TEMP:?}" "${GITHUB_SHA:?}" "${CARGO_TARGET_DIR:?}" "${E2E_PYTHON:?}"
[[ "$CARGO_TARGET_DIR" == "$RUNNER_TEMP/mac-e2e-target" ]]
version=$("$E2E_PYTHON" -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
[[ $(git rev-parse HEAD) == "$GITHUB_SHA" ]]
git diff --exit-code HEAD --
out="$RUNNER_TEMP/mac-e2e-artifacts"
stage="$RUNNER_TEMP/mac-e2e-stage"
mkdir -p "$out/package"
[[ ! -e "$stage" ]] || { echo 'Stage must be fresh' >&2; exit 1; }
export CARGO_PROFILE_RELEASE_DEBUG=0 MACOSX_DEPLOYMENT_TARGET=14.0

cargo install --locked cargo-about --version 0.9.2 --features cli --root "$RUNNER_TEMP/mac-e2e-tools"
# cargo-about's build is no longer needed. Keep each build batch below 20 GiB.
rm -rf -- "$CARGO_TARGET_DIR"
"$RUNNER_TEMP/mac-e2e-tools/bin/cargo-about" generate --workspace -c about.toml \
    scripts/release/third-party-notices.hbs -o "$out/package/THIRD-PARTY-NOTICES.txt"
cargo build --release --locked -p superba-gui -p superba-cli -p superba-router
scripts/release/macos-bundle.sh --binary "$CARGO_TARGET_DIR/release/superba" \
    --helper "$CARGO_TARGET_DIR/release/superba-router" --version "$version" --out "$stage" \
    --notices "$out/package/THIRD-PARTY-NOTICES.txt"
install -m 755 "$CARGO_TARGET_DIR/release/spb" "$stage/spb"
cp README.md LICENSE-MIT LICENSE-APACHE "$out/package/THIRD-PARTY-NOTICES.txt" \
    crates/superba-gui/assets/fonts/LICENSE-Geist-OFL.txt "$stage/"
"$E2E_PYTHON" e2e/macos/package.py stamp --stage "$stage" --source-sha "$GITHUB_SHA"
codesign --force --sign - "$stage/Superba.app/Contents/MacOS/superba-router"
codesign --force --sign - "$stage/Superba.app"
codesign --force --sign - "$stage/spb"
codesign --verify --strict --deep "$stage/Superba.app"
rm -rf -- "$CARGO_TARGET_DIR"

dmg="$out/package/superba-$version-${GITHUB_SHA:0:12}-macos-arm64.dmg"
PYTHON="$E2E_PYTHON" scripts/release/macos-dmg.sh build --staging "$stage" --version "$version" --out "$dmg"
"$E2E_PYTHON" e2e/macos/package.py record --stage "$stage" --dmg "$dmg" --output "$out/package/package.json"
PYTHON="$E2E_PYTHON" scripts/release/macos-dmg.sh inspect "$dmg" > "$out/package/layout.txt"
printf 'dmg=%s\nmanifest=%s\n' "$dmg" "$out/package/package.json" >> "$GITHUB_OUTPUT"
