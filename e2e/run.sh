#!/usr/bin/env bash
# Superba end-to-end harness: real desktop app, real Codex, isolated HOME.
#
#   SUPERBA_E2E_ENV=/path/to/e2e.env e2e/run.sh            # build, then run all scenarios
#   SUPERBA_E2E_ENV=... e2e/run.sh --no-build               # reuse SPB_BIN / SUPERBA_BIN
#   SUPERBA_E2E_ENV=... e2e/run.sh step setup|s1..s23|teardown|spb ...|cdp ...|exec ...|report
#
# The env file (never committed) provides:
#   SUPERBA_E2E_UPSTREAM  https origin of a test relay (Krill-compatible)
#   SUPERBA_E2E_KEY       a key for that relay
#   SUPERBA_E2E_APP_DIR   an extracted Linux desktop app (contains ChatGPT and resources/codex)
#
# Outputs go to e2e/out/<run>/ (report.md, report.json, forwarder logs, transcripts,
# screenshots); the isolated home lives in e2e/.work/<run>/home behind a short
# /tmp/sbe2e-<n> symlink. Both are gitignored. See e2e/README.md.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/.." && pwd)

env_file=${SUPERBA_E2E_ENV:-}
if [[ -z $env_file || ! -r $env_file ]]; then
  echo "SUPERBA_E2E_ENV must point at a readable env file (see e2e/README.md)" >&2
  exit 2
fi
# Load only the three expected keys; never echo their values.
while IFS= read -r line || [[ -n $line ]]; do
  [[ $line =~ ^[[:space:]]*# || -z ${line//[[:space:]]/} ]] && continue
  key=${line%%=*}
  case $key in
    SUPERBA_E2E_UPSTREAM | SUPERBA_E2E_KEY | SUPERBA_E2E_APP_DIR) export "$key=${line#*=}" ;;
  esac
done < "$env_file"
for k in SUPERBA_E2E_UPSTREAM SUPERBA_E2E_KEY SUPERBA_E2E_APP_DIR; do
  [[ -n ${!k:-} ]] || { echo "$k missing in $env_file" >&2; exit 2; }
done
[[ -x $SUPERBA_E2E_APP_DIR/ChatGPT && -x $SUPERBA_E2E_APP_DIR/resources/codex ]] || {
  echo "SUPERBA_E2E_APP_DIR does not contain ChatGPT and resources/codex" >&2
  exit 2
}

for tool in python3 Xvfb xdotool scrot openssl cargo unshare nsenter; do
  command -v "$tool" > /dev/null || { echo "missing tool: $tool" >&2; exit 2; }
done
python3 -c 'import aiohttp, websockets' 2> /dev/null || {
  echo "python3 needs aiohttp and websockets (pip install aiohttp websockets zstandard)" >&2
  exit 2
}

# Never let the caller's Codex/Superba environment leak into the run (the driver
# also builds every child environment from scratch).
unset CODEX_HOME CODEX_SQLITE_HOME CODEX_AUTHAPI_BASE_URL SUPERBA_DATA_DIR SUPERBA_TEST_SITES SUPERBA_TEST_LINES

# One shared build directory (disk policy); the driver copies the binaries out of
# it before the run, so a concurrent build cannot replace them mid-run.
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$repo/target-v2-shared}
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only
build=1
if [[ ${1:-} == --no-build ]]; then
  build=0
  shift
fi
if [[ ${1:-} == step ]]; then
  build=0
fi
if ((build)); then
  (cd "$repo" && cargo build --release --locked -p superba-cli -p superba-gui -p superba-router -p superba-catalog-gen)
fi
# spb, superba and superba-router are taken from the directory of SPB_BIN.
export SPB_BIN=${SPB_BIN:-$CARGO_TARGET_DIR/release/spb}

if [[ ${1:-} == step ]]; then
  shift
  exec python3 -B "$here/lib/driver.py" "$@"
fi
exec python3 -B "$here/lib/driver.py" all
