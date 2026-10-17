#!/usr/bin/env bash
# Usage: macos-sign.sh [--env-file PATH] [--no-notarize|--require-notarize] PATH...
# PATH is an app bundle, Mach-O binary, ZIP, or DMG. ZIP contents are signed and
# repacked; sign a DMG's contents before creating the image.
#
# Configuration (read only at run time; never enable shell tracing):
# SUPERBA_SIGNING_ENV: optional shell env file. --env-file takes precedence;
# otherwise use .secrets/macos-signing.env relative to the repository, if present.
# MACOS_SIGNING_IDENTITY: required, e.g. Developer ID Application: NAME (TEAMID).
# MACOS_TEAM_ID: optional team ID, also a fallback for APPLE_TEAM_ID.
# MACOS_SIGNING_P12: optional certificate file (relative paths, or a missing
# absolute path, are looked up next to the env file); takes precedence over
# MACOS_SIGNING_P12_BASE64: optional base64-encoded certificate.
# MACOS_SIGNING_P12_PASSWORD: certificate password (an empty password is allowed).
# With no certificate, use an identity already in the user's keychains.
# APPLE_API_KEY_ID / APPLE_API_ISSUER_ID: App Store Connect API credentials.
# APPLE_API_KEY_P8: API private key file; takes precedence over
# APPLE_API_KEY_P8_BASE64: base64-encoded API private key.
# NOTARY_KEYCHAIN_PROFILE: alternative notarytool store-credentials profile.
# APPLE_ID / APPLE_TEAM_ID / APPLE_APP_PASSWORD: alternative Apple ID credentials;
# APPLE_TEAM_ID falls back to MACOS_TEAM_ID. Prefer API credentials or a profile.
# Missing notary credentials warn and skip unless --require-notarize is set.
# TMPDIR: parent of private temporary files (use RUNNER_TEMP in CI).
# Optional entitlements.plist beside this script is applied only when present.
set +x
set -euo pipefail

usage() {
    printf 'Usage: %s [--env-file PATH] [--no-notarize|--require-notarize] PATH...\n' "$0"
}

die() {
    printf 'macos-sign: %s\n' "$*" >&2
    exit 1
}

warn() {
    printf 'macos-sign: warning: %s\n' "$*" >&2
}

env_file=''
explicit_env_file=false
no_notarize=false
require_notarize=false
paths=()
while (($#)); do
    case "$1" in
        --env-file)
            (($# >= 2)) || die 'Missing path after --env-file'
            [[ -n "$2" ]] || die '--env-file needs a nonempty path'
            env_file=$2
            explicit_env_file=true
            shift 2
            ;;
        --no-notarize) no_notarize=true; shift ;;
        --require-notarize) require_notarize=true; shift ;;
        -h|--help) usage; exit 0 ;;
        --) shift; paths+=("$@"); break ;;
        -*) die "Unknown option: $1" ;;
        *) paths+=("$1"); shift ;;
    esac
done

[[ $(uname -s) == Darwin ]] || die 'This script requires macOS (codesign, security, and Xcode command line tools).'
((${#paths[@]} > 0)) || { usage >&2; exit 1; }
[[ "$no_notarize" == false || "$require_notarize" == false ]] || die '--no-notarize and --require-notarize conflict'
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -- "$script_dir/../.." && pwd -P)

if [[ "$explicit_env_file" == false ]]; then
    if [[ -n "${SUPERBA_SIGNING_ENV:-}" ]]; then
        env_file=$SUPERBA_SIGNING_ENV
    elif [[ -f "$repo_root/.secrets/macos-signing.env" ]]; then
        env_file="$repo_root/.secrets/macos-signing.env"
    fi
fi
if [[ -n "$env_file" ]]; then
    [[ -r "$env_file" ]] || die 'The signing env file is not readable'
    set -a
    # Only source trusted shell files. Never display the file or its values.
    # shellcheck disable=SC1090
    source "$env_file"
    set +a
    set +x
    set -euo pipefail
fi
# Paths in the env file may be relative to it, or absolute paths from another
# machine (e.g. a .secrets/ directory copied to the Mac): fall back to a file
# with the same name next to the env file.
resolve_beside_env() {
    local value=$1 env_dir candidate
    if [[ -z "$value" || -f "$value" || -z "$env_file" ]]; then
        printf '%s' "$value"
        return
    fi
    env_dir=$(cd -- "$(dirname -- "$env_file")" && pwd -P)
    for candidate in "$env_dir/$value" "$env_dir/$(basename -- "$value")"; do
        if [[ -f "$candidate" ]]; then
            printf '%s' "$candidate"
            return
        fi
    done
    printf '%s' "$value"
}
MACOS_SIGNING_P12=$(resolve_beside_env "${MACOS_SIGNING_P12:-}")
APPLE_API_KEY_P8=$(resolve_beside_env "${APPLE_API_KEY_P8:-}")

[[ -n "${MACOS_SIGNING_IDENTITY:-}" ]] || die 'Set MACOS_SIGNING_IDENTITY to a Developer ID Application identity'

umask 077
work_dir=$(mktemp -d "${TMPDIR:-${RUNNER_TEMP:-/tmp}}/superba-sign.XXXXXX")
temporary_keychain=''
original_keychains=()
search_list_changed=false

cleanup() {
    local status=$?
    local restore_args=(list-keychains -d user -s)
    trap - EXIT INT TERM
    set +e
    if [[ "$search_list_changed" == true ]]; then
        if ((${#original_keychains[@]} > 0)); then
            restore_args+=("${original_keychains[@]}")
        fi
        if ! security "${restore_args[@]}" >/dev/null; then
            warn 'Could not restore the original keychain search list'
            status=1
        fi
    fi
    if [[ -n "$temporary_keychain" ]]; then
        if ! security delete-keychain "$temporary_keychain" >/dev/null; then
            warn 'Could not delete the temporary signing keychain'
            status=1
        fi
    fi
    rm -rf -- "$work_dir" || status=1
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

sign_args=(--force --timestamp --options runtime --sign "$MACOS_SIGNING_IDENTITY")
if [[ -f "$script_dir/entitlements.plist" ]]; then
    sign_args+=(--entitlements "$script_dir/entitlements.plist")
fi

p12_file=${MACOS_SIGNING_P12:-}
if [[ -z "$p12_file" && -n "${MACOS_SIGNING_P12_BASE64:-}" ]]; then
    p12_file="$work_dir/certificate.p12"
    printf '%s' "$MACOS_SIGNING_P12_BASE64" | /usr/bin/base64 -D > "$p12_file"
fi
if [[ -n "$p12_file" ]]; then
    [[ -f "$p12_file" && -r "$p12_file" ]] || die 'MACOS_SIGNING_P12 must name a readable certificate file'
    security list-keychains -d user > "$work_dir/keychains.before"
    while IFS= read -r keychain; do
        keychain=${keychain#*\"}
        keychain=${keychain%\"*}
        [[ -z "$keychain" ]] || original_keychains+=("$keychain")
    done < "$work_dir/keychains.before"
    keychain_password=$(LC_ALL=C od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
    temporary_keychain="$work_dir/signing.keychain-db"
    # Creating a keychain may itself alter the search list. Restore it even if
    # creation, import, or partition setup fails before the explicit list update.
    search_list_changed=true
    security create-keychain -p "$keychain_password" "$temporary_keychain" >/dev/null
    security set-keychain-settings -lut 21600 "$temporary_keychain" >/dev/null
    security unlock-keychain -p "$keychain_password" "$temporary_keychain" >/dev/null
    security import "$p12_file" -k "$temporary_keychain" \
        -P "${MACOS_SIGNING_P12_PASSWORD:-}" -T /usr/bin/codesign -T /usr/bin/security >/dev/null
    security set-key-partition-list -S apple-tool:,apple:,codesign: \
        -s -k "$keychain_password" "$temporary_keychain" >/dev/null
    if ((${#original_keychains[@]} > 0)); then
        security list-keychains -d user -s "$temporary_keychain" "${original_keychains[@]}" >/dev/null
    else
        security list-keychains -d user -s "$temporary_keychain" >/dev/null
    fi
    sign_args+=(--keychain "$temporary_keychain")
    unset keychain_password
fi

notarize_enabled=false
notary_args=()
if [[ "$no_notarize" == false ]]; then
    api_key_file=${APPLE_API_KEY_P8:-}
    if [[ -n "${APPLE_API_KEY_ID:-}" && -n "${APPLE_API_ISSUER_ID:-}" && \
          ( -n "$api_key_file" || -n "${APPLE_API_KEY_P8_BASE64:-}" ) ]]; then
        if [[ -z "$api_key_file" ]]; then
            api_key_file="$work_dir/AuthKey.p8"
            printf '%s' "$APPLE_API_KEY_P8_BASE64" | /usr/bin/base64 -D > "$api_key_file"
        fi
        [[ -f "$api_key_file" && -r "$api_key_file" ]] || die 'APPLE_API_KEY_P8 must name a readable private key file'
        notary_args=(--key "$api_key_file" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID")
        notarize_enabled=true
    elif [[ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
        notary_args=(--keychain-profile "$NOTARY_KEYCHAIN_PROFILE")
        notarize_enabled=true
    elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-${MACOS_TEAM_ID:-}}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
        notary_args=(--apple-id "$APPLE_ID" --team-id "${APPLE_TEAM_ID:-$MACOS_TEAM_ID}" --password "$APPLE_APP_PASSWORD")
        notarize_enabled=true
    elif [[ "$require_notarize" == true ]]; then
        die 'Notarization is required, but no complete notarization credentials are configured'
    else
        warn 'No complete notarization credentials; signing only'
    fi
else
    printf 'Notarization disabled by --no-notarize.\n'
fi

is_macho() {
    [[ $(file -b "$1") == *Mach-O* ]]
}

sign_one() {
    codesign "${sign_args[@]}" "$1"
    codesign --verify --strict --deep --verbose=2 "$1"
    codesign -dvv "$1"
}

sign_app() {
    local app=$1 nested
    [[ -d "$app/Contents/MacOS" ]] || die "App has no Contents/MacOS directory: $app"
    while IFS= read -r -d '' nested; do
        if is_macho "$nested"; then
            sign_one "$nested"
        fi
    done < <(find "$app/Contents/MacOS" -type f -print0)
    # Seal any helper app bundles after their code, from the inside out.
    while IFS= read -r -d '' nested; do
        sign_one "$nested"
    done < <(find "$app/Contents/MacOS" -depth -type d -name '*.app' -print0)
    sign_one "$app"
}

submit_notarization() {
    local upload=$1 result submission_id submission_status submit_failed=false
    result="$work_dir/notary-result.json"
    if ! xcrun notarytool submit "$upload" "${notary_args[@]}" \
        --wait --timeout 30m --output-format json > "$result"; then
        submit_failed=true
    fi
    submission_id=$(/usr/bin/plutil -extract id raw -o - "$result" 2>/dev/null || true)
    submission_status=$(/usr/bin/plutil -extract status raw -o - "$result" 2>/dev/null || true)
    if [[ "$submit_failed" == true || "$submission_status" != Accepted ]]; then
        if [[ -n "$submission_id" ]]; then
            warn 'Notarization failed; retrieving the Apple submission log'
            xcrun notarytool log "$submission_id" "${notary_args[@]}" || warn 'Unable to retrieve the notarization log'
        else
            warn 'Notarization failed before a submission ID was returned; no log is available'
        fi
        die 'Notarization did not finish with Accepted status'
    fi
    printf 'Notarization accepted: %s\n' "$submission_id"
}

assess() {
    case "$1" in
        *.app) spctl -a -vvv -t exec "$1" || warn 'Gatekeeper app assessment did not pass' ;;
        *.dmg) spctl -a -t open --context context:primary-signature -v "$1" || warn 'Gatekeeper disk image assessment did not pass' ;;
    esac
}

sign_zip() {
    local archive=$1 zip_work nested signed_count=0
    zip_work=$(mktemp -d "$work_dir/zip.XXXXXX")
    mkdir "$zip_work/contents"
    ditto -x -k "$archive" "$zip_work/contents"
    while IFS= read -r -d '' nested; do
        if is_macho "$nested"; then
            sign_one "$nested"
            signed_count=$((signed_count + 1))
        fi
    done < <(find "$zip_work/contents" -type f -print0)
    while IFS= read -r -d '' nested; do
        sign_one "$nested"
    done < <(find "$zip_work/contents" -depth -type d -name '*.app' -print0)
    ((signed_count > 0)) || die 'ZIP contains no Mach-O code to sign'
    # No --keepParent here: preserve the original ZIP's top-level layout.
    ditto -c -k "$zip_work/contents" "$zip_work/signed.zip"
    if [[ "$notarize_enabled" == true ]]; then
        submit_notarization "$zip_work/signed.zip"
        while IFS= read -r -d '' nested; do
            xcrun stapler staple "$nested"
            assess "$nested"
        done < <(find "$zip_work/contents" -depth -type d -name '*.app' -print0)
        rm -- "$zip_work/signed.zip"
        ditto -c -k "$zip_work/contents" "$zip_work/signed.zip"
        printf 'ZIPs and bare binaries cannot be stapled; app bundles inside the ZIP were stapled.\n'
    fi
    mv -f -- "$zip_work/signed.zip" "$archive"
}

for path in "${paths[@]}"; do
    # Make every path absolute so leading dashes cannot be treated as options.
    path=${path%/}
    [[ -e "$path" ]] || die "Input does not exist: $path"
    path="$(cd -- "$(dirname -- "$path")" && pwd -P)/$(basename -- "$path")"
    case "$path" in
        *.app)
            [[ -d "$path" ]] || die "Not an app bundle: $path"
            sign_app "$path"
            if [[ "$notarize_enabled" == true ]]; then
                ditto -c -k --keepParent "$path" "$work_dir/submission.zip"
                submit_notarization "$work_dir/submission.zip"
                rm -- "$work_dir/submission.zip"
                xcrun stapler staple "$path"
            fi
            assess "$path"
            ;;
        *.zip)
            [[ -f "$path" ]] || die "Not a ZIP file: $path"
            sign_zip "$path"
            ;;
        *.dmg)
            [[ -f "$path" ]] || die "Not a disk image: $path"
            sign_one "$path"
            if [[ "$notarize_enabled" == true ]]; then
                submit_notarization "$path"
                xcrun stapler staple "$path"
            fi
            assess "$path"
            ;;
        *)
            if [[ ! -f "$path" ]] || ! is_macho "$path"; then
                die "Not a Mach-O binary: $path"
            fi
            sign_one "$path"
            if [[ "$notarize_enabled" == true ]]; then
                ditto -c -k --keepParent "$path" "$work_dir/submission.zip"
                submit_notarization "$work_dir/submission.zip"
                rm -- "$work_dir/submission.zip"
                printf 'Bare binaries cannot be stapled; Gatekeeper retrieves their tickets online.\n'
            fi
            ;;
    esac
done
