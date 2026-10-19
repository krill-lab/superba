#!/usr/bin/env bash
set -euo pipefail
# The Python driver owns the lease, archive validation, rsync and CI fallback.
exec python3 "$(dirname "$0")/build.py" "$@"
