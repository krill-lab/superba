"""Reads the Finder layout of a mounted Superba disk image back (user bug 84).

Prints the window, the view options, the background and every icon position from
the image's .DS_Store and compares them with packaging/macos/dmg-settings.py.
scripts/release/macos-dmg.sh runs it with the pinned ds_store and mac_alias
(packaging/macos/dmg-requirements.txt) after dmgbuild and for `inspect`.

Usage: macos-dmg-layout.py MOUNT_POINT SETTINGS_FILE VOLUME_NAME
Exits 1 when something differs from the settings.
"""

import sys

from ds_store import DSStore
from mac_alias import Alias


def load_settings(path):
    # dmgbuild runs the settings file the same way, with its -D values in `defines`.
    values = {"defines": {"background": "", "volume_icon": ""}}
    with open(path, encoding="utf-8") as settings:
        exec(compile(settings.read(), path, "exec"), values)
    return values


def text(value):
    return value.decode("utf-8", "replace") if isinstance(value, bytes) else str(value)


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    mount, settings_path, volume = sys.argv[1:]
    want = load_settings(settings_path)
    records = {}
    with DSStore.open(f"{mount}/.DS_Store", "r") as store:
        for entry in store:
            records.setdefault(entry.filename, {})[text(entry.code)] = entry.value
    root = records.get(".", {})
    bwsp = root.get("bwsp") or {}
    icvp = root.get("icvp") or {}
    problems = []

    def expect(what, got, wanted):
        if got != wanted:
            problems.append(f"{what}: {got!r}, expected {wanted!r}")

    (x, y), (w, h) = want["window_rect"]
    expect("window bounds", bwsp.get("WindowBounds"), f"{{{{{x}, {y}}}, {{{w}, {h}}}}}")
    bars = {
        "ShowToolbar": "show_toolbar",
        "ShowPathbar": "show_pathbar",
        "ShowSidebar": "show_sidebar",
        "ShowTabView": "show_tab_view",
        "ShowStatusBar": "show_status_bar",
    }
    for key, setting in bars.items():
        expect(key, bwsp.get(key), want[setting])
    print(
        f"window: {bwsp.get('WindowBounds')} pt, "
        + ", ".join(f"{key}={bwsp.get(key)}" for key in bars)
    )

    expect("default view", text(root.get("icvl", b"")), "icnv")
    expect("icon size", icvp.get("iconSize"), float(want["icon_size"]))
    expect("label size", icvp.get("textSize"), float(want["text_size"]))
    expect("labels below icons", icvp.get("labelOnBottom"), want["label_pos"] == "bottom")
    expect("arrange by", icvp.get("arrangeBy"), "none")
    expect("background type (2 = picture)", icvp.get("backgroundType"), 2)
    print(
        f"icon view: icons {icvp.get('iconSize')} pt, labels {icvp.get('textSize')} pt"
        f" {'below' if icvp.get('labelOnBottom') else 'beside'}, arranged by"
        f" {icvp.get('arrangeBy')}, background type {icvp.get('backgroundType')}"
    )
    if "backgroundImageAlias" in icvp:
        alias = Alias.from_bytes(icvp["backgroundImageAlias"])
        target = text(alias.target.posix_path or alias.target.filename)
        print(f"background picture: volume {text(alias.volume.name)!r}, {target}")
        expect("background volume", text(alias.volume.name), volume)
        expect("background file", text(alias.target.filename), ".background.tiff")
    else:
        problems.append("no background picture")

    print("icon positions (centre, window points):")
    for name, record in sorted(records.items()):
        if "Iloc" in record:
            print(f"  {name}: {tuple(record['Iloc'])}")
    for name, position in want["icon_locations"].items():
        got = records.get(name, {}).get("Iloc")
        expect(f"position of {name}", tuple(got) if got else None, tuple(position))

    for problem in problems:
        print(f"macos-dmg-layout: {problem}", file=sys.stderr)
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
