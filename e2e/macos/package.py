#!/usr/bin/env python3
"""Bind the locally built DMG to its exact checkout, version and license texts."""

import argparse
import hashlib
import json
import plistlib
import re
import subprocess
import tomllib
from pathlib import Path


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def metadata(source_sha, repo=Path(".")):
    if not re.fullmatch(r"[a-f0-9]{40}", source_sha):
        raise ValueError("Expected a full source commit SHA")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    if head != source_sha:
        raise ValueError("Checkout does not match the dispatched commit")
    subprocess.run(["git", "diff", "--exit-code", "HEAD", "--"], cwd=repo, check=True,
                   stdout=subprocess.DEVNULL)
    manifest = tomllib.loads((repo / "Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    return {"source_sha": head, "version": version, "bundle_version": version.split("-", 1)[0],
            "signing": "ad-hoc; not a signed/notarized release", "notices_generator": "cargo-about 0.9.2"}


def stamp(stage, build):
    app = stage / "Superba.app"
    notices = app / "Contents/Resources/THIRD-PARTY-NOTICES.txt"
    content = notices.read_text()
    if (len(content) < 1024 or "Superba third-party notices" not in content
            or "Used by:" not in content or "placeholder" in content.lower()):
        raise ValueError("Actual cargo-about notices are required")
    if sha256(stage / "THIRD-PARTY-NOTICES.txt") != sha256(notices):
        raise ValueError("Bundle and DMG root notices differ")
    build = {**build, "notices_sha256": sha256(notices)}
    plist_path = app / "Contents/Info.plist"
    info = plistlib.loads(plist_path.read_bytes())
    if any(info.get(key) != build["bundle_version"] for key in ("CFBundleVersion", "CFBundleShortVersionString")):
        raise ValueError("Bundle version does not match the workspace version")
    # Apple's numeric version fields intentionally omit the prerelease suffix.
    # Keep the exact Rust/CLI version and source SHA in the bundle as well.
    info.update(SuperbaVersion=build["version"], SuperbaSourceSHA=build["source_sha"])
    plist_path.write_bytes(plistlib.dumps(info))
    (app / "Contents/Resources/e2e-build.json").write_text(json.dumps(build, indent=2) + "\n")
    return build


def record(stage, dmg, output):
    app = stage / "Superba.app"
    build = json.loads((app / "Contents/Resources/e2e-build.json").read_text())
    result = {"build": build, "dmg_sha256": sha256(dmg), "dmg_name": dmg.name,
              "executables": {name: sha256(stage / "spb" if name == "spb" else app / "Contents/MacOS" / name)
                              for name in ("superba", "spb", "superba-router")}}
    output.write_text(json.dumps(result, indent=2) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("stamp", "record"))
    parser.add_argument("--stage", type=Path, required=True)
    parser.add_argument("--source-sha")
    parser.add_argument("--dmg", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.command == "stamp":
        stamp(args.stage, metadata(args.source_sha))
    else:
        if not args.dmg or not args.output:
            parser.error("record requires --dmg and --output")
        record(args.stage, args.dmg, args.output)


if __name__ == "__main__":
    main()
