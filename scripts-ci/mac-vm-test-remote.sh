#!/bin/bash
# Invoked by mac_vm_test.py, under an isolated environment and HOME.
set -euo pipefail
sha=$1
ci_root=$2
run_dir="$ci_root/logs-$sha"
src_dir="$ci_root/src-$sha"
export CARGO_HOME="$ci_root/cargo-home"
export RUSTUP_HOME="$ci_root/rustup"
export CARGO_TARGET_DIR="$ci_root/target"
export PATH="$CARGO_HOME/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
mkdir -p "$HOME" "$TMPDIR" "$CARGO_HOME" "$RUSTUP_HOME" "$CARGO_TARGET_DIR"
exec > >(tee "$run_dir/full.log") 2>&1
# shellcheck disable=SC2329 # Invoked by the EXIT trap.
finish() {
    rc=$?
    trap - EXIT
    cd "$ci_root"
    if ! rm -rf "$src_dir" "$HOME" || ! rmdir "$ci_root/unit-test.lock"; then
        echo 'Failed to clean up source/HOME or release unit-test.lock' >&2
        rc=1
    fi
    printf '%s\n' "$rc" > "$run_dir/exit-code"
    exit "$rc"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
# The controller generates shell-quoted values from the archived CI workflow.
# shellcheck disable=SC1091
source "$run_dir/config.sh"
cd "$src_dir"
printf 'Commit: %s\nStarted: %s\n' "$GIT_COMMIT" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
uname -sm
sw_vers
if [[ ! -x "$CARGO_HOME/bin/rustup" ]]; then
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
        https://sh.rustup.rs -o "$run_dir/rustup-init.sh"
    /bin/bash "$run_dir/rustup-init.sh" -y --no-modify-path --profile minimal --default-toolchain none
fi
# rustup reuses an installed toolchain; its files live outside the disposable HOME.
if ! rustup run "$RUSTUP_TOOLCHAIN" rustc --version >/dev/null 2>&1; then
    rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal
fi
rustc --version
cargo --version
available_kb=$(df -Pk "$ci_root" | awk 'END {print $4}')
if (( available_kb < 20 * 1024 * 1024 )); then
    cargo clean
fi
# Match jobs.test in the archived workflow; only CPU scheduling/jobs differ.
export CARGO_BUILD_JOBS=5
printf 'Command: %s\n' "$TEST_COMMAND"
printf 'HOME=%s\nCARGO_TARGET_DIR=%s\n' "$HOME" "$CARGO_TARGET_DIR"
set +e
nice -n 10 /bin/bash --noprofile --norc -e -o pipefail -c "$TEST_COMMAND" 2>&1 | tee "$run_dir/tests.log"
statuses=("${PIPESTATUS[@]}")
rc=${statuses[0]}
if (( rc == 0 && statuses[1] != 0 )); then
    rc=${statuses[1]}
fi
set -e
printf 'Finished: %s\nCargo exit code: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$rc"
exit "$rc"
