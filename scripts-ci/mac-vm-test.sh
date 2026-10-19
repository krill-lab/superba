#!/usr/bin/env bash
# SSH-only macOS workspace tests; see e2e/macos/README.md.
set -euo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/mac_vm_test.py" "$@"
