#!/bin/sh
set -eu

# An explicit fixture root lets local regressions avoid the real user's files.
hash_root=${1:-$HOME}
for f in "$hash_root/.codex/config.toml" "$hash_root/.codex/auth.json" \
         "$hash_root/.profile" "$hash_root/.bashrc" "$hash_root/.zshenv" "$hash_root/.bash_profile"; do
    if [ -f "$f" ]; then
        sha256sum -- "$f"
    elif [ ! -e "$f" ] && [ ! -L "$f" ]; then
        printf 'missing %s\n' "$f"
    else
        printf 'not a regular file: %s\n' "$f" >&2
        exit 1
    fi
done
