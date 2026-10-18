#!/usr/bin/env bash
# Opens a Superba disk image's window in Finder and takes screenshots of it in light
# and dark appearance (user bug 84), and logs what Finder and the window server report
# for the window and its icons. Runs in CI's dmg-package jobs on GitHub's macOS runners
# (a logged-in desktop); never on a machine someone is using, since it switches the
# system appearance and closes Finder windows.
# Usage: test-dmg.sh FILE.dmg OUT_DIR
set -euo pipefail

[[ $# -eq 2 && -f "$1" ]] || { printf 'Usage: %s FILE.dmg OUT_DIR\n' "$0" >&2; exit 1; }
[[ $(uname -s) == Darwin ]] || { printf 'test-dmg: macOS only\n' >&2; exit 1; }
dmg=$1
mkdir -p -- "$2"
out=$(cd -- "$2" && pwd -P)
work=$(mktemp -d "${TMPDIR:-${RUNNER_TEMP:-/tmp}}/superba-dmg-test.XXXXXX")
mount_point=''
cleanup() {
    set +e
    osascript -e 'tell application "Finder" to close every window' >/dev/null 2>&1
    appearance light >/dev/null 2>&1
    [[ -z "$mount_point" ]] || hdiutil detach -force "$mount_point" >/dev/null 2>&1
    rm -rf -- "$work"
}
trap cleanup EXIT

log() { printf '\n== %s\n' "$*"; }

log 'system'
sw_vers
system_profiler SPDisplaysDataType 2>/dev/null | grep -E 'Resolution|Retina|UI Looks like' || true

# Finder windows, from the window server: id, title (needs screen recording access),
# frame in points (with the title bar).
cat > "$work/windows.swift" <<'SWIFT'
import CoreGraphics
import Foundation
let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] ?? []
for window in info where window[kCGWindowOwnerName as String] as? String == "Finder"
    && window[kCGWindowLayer as String] as? Int == 0 {
    let frame = window[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let values = ["X", "Y", "Width", "Height"].map { "\(frame[$0] ?? "?")" }
    print("\(window[kCGWindowNumber as String] ?? "?")\t\(window[kCGWindowName as String] ?? "")\t\(values.joined(separator: ","))")
}
SWIFT
swiftc -O -o "$work/windows" "$work/windows.swift" 2>&1 | tail -n 5 || true

appearance() { # light|dark
    local dark=false
    [[ "$1" == dark ]] && dark=true
    if osascript -e "tell application \"System Events\" to tell appearance preferences to set dark mode to $dark"; then
        return 0
    fi
    printf 'System Events refused; setting AppleInterfaceStyle and restarting Finder\n'
    if [[ "$dark" == true ]]; then
        defaults write -g AppleInterfaceStyle -string Dark
    else
        defaults delete -g AppleInterfaceStyle 2>/dev/null || true
    fi
    killall Finder 2>/dev/null || true
    sleep 5
}

log 'attach'
attach_output=$(hdiutil attach "$dmg" -noverify -noautoopen)
printf '%s\n' "$attach_output"
mount_point=$(printf '%s\n' "$attach_output" | awk -F'\t' '/\/Volumes\// {print $NF}' | tail -n 1 |
    sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
[[ -d "$mount_point" ]] || { printf 'test-dmg: no mount point\n' >&2; exit 1; }
volume=$(basename -- "$mount_point")
ls -laO "$mount_point"

for mode in light dark; do
    log "$mode appearance"
    appearance "$mode"
    osascript -e 'tell application "Finder" to close every window' >/dev/null 2>&1 || true
    open "$mount_point"
    sleep 6
    osascript - "$volume" <<'APPLESCRIPT' || printf 'Finder scripting failed\n'
on run argv
    set volumeName to item 1 of argv
    set report to ""
    tell application "Finder"
        set w to container window of disk volumeName
        try
            set b to bounds of w
            set report to report & "bounds (left, top, right, bottom; frame from the top left): " & (item 1 of b) & ", " & (item 2 of b) & ", " & (item 3 of b) & ", " & (item 4 of b) & linefeed
        end try
        try
            set report to report & "view " & (current view of w as text) & ", toolbar " & (toolbar visible of w) & ", status bar " & (statusbar visible of w) & ", path bar " & (pathbar visible of w) & linefeed
        end try
        try
            set o to icon view options of w
            set report to report & "icon size " & (icon size of o) & ", text size " & (text size of o) & ", arrangement " & (arrangement of o as text) & ", labels " & (label position of o as text) & linefeed
        end try
        try
            set itemNames to name of every item of disk volumeName
            repeat with itemName in itemNames
                set p to position of item (itemName as text) of disk volumeName
                set report to report & "visible item " & itemName & " at " & (item 1 of p) & ", " & (item 2 of p) & linefeed
            end repeat
        on error message
            set report to report & "items: " & message & linefeed
        end try
    end tell
    return report
end run
APPLESCRIPT
    screencapture -x "$out/$mode-screen.png"
    window_line=''
    if [[ -x "$work/windows" ]]; then
        "$work/windows" | tee "$out/$mode-windows.txt"
        window_line=$(awk -F'\t' -v name="$volume" '$2 == name' "$out/$mode-windows.txt" | head -n 1)
        [[ -n "$window_line" ]] || window_line=$(head -n 1 "$out/$mode-windows.txt")
    fi
    if [[ -n "$window_line" ]]; then
        screencapture -x -o -l "$(cut -f1 <<<"$window_line")" "$out/$mode-window.png"
        printf 'window frame (x,y,w,h, with the title bar): %s\n' "$(cut -f3 <<<"$window_line")"
    fi
done

log 'files'
ls -l "$out"
