#!/usr/bin/env python3
"""Screenshots of "Edit Codex files" (GitHub issue 9) from the real `superba`
and `spb` binaries.

shoot.py's Session provisions an isolated HOME (throwaway CA, mock relay, the
Krill profile applied with a PAT, so config.toml holds keys Superba manages and
auth.json its key). A fake `codex` on PATH answers `--version` with 0.162.0, so
the editor checks config.toml against that version of the settings catalog.
The script then writes a hand-edited config.toml (comments, a misspelt key, a
value of the wrong type) and takes one backup through `spb config edit` (an
editor script that appends a comment), so the Backups dialog lists the
editor's backup next to the switch's pre-image and the original.

Shots, at 1280x800 in light and dark, English and Chinese:

- `codex-files-*`: Settings -> Edit Codex files on config.toml (highlighting,
  line numbers, the problems of the check, the managed keys);
- `codex-files-review-*`: a line typed into the editor, then Ctrl+S: the review
  with the diff and what happens on save;
- `codex-files-auth-*`: auth.json with the key hidden;
- `codex-files-backups-*`: the Backups dialog (palette "Restore config.toml
  from a backup");
- `codex-files-restore-*`: one backup chosen there, with its diff to the editor.

Like shoot.py it re-runs itself in an unprivileged user + PID namespace and
refuses to shoot while a desktop app of another HOME is visible.

    python3 -I crates/superba-gui/tools/screenshots/codex_files_shots.py \\
        --bin-dir <target>/debug --display :157 --out <dir> [--work-dir <scratch dir>]
"""

import argparse
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_FILES_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]
HEAD = """# Codex settings of this computer
# (edited by hand; Superba keeps these comments)
modle = "gpt-5.5"
approval_policy = "sometimes"
"""
TAIL = """
[mcp_servers.docs]
command = "npx" # started by Codex
args = ["-y", "docs-mcp"]
"""

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def fake_codex(s):
    path = s.bin / "codex"
    path.write_text('#!/bin/sh\ncase "$1" in --version) echo "codex-cli 0.162.0";; '
                    '*) exit 1;; esac\n')
    path.chmod(0o755)


def hand_edit(s, bin_dir):
    """A hand-written config.toml, then one save through `spb config edit`."""
    cfg = s.home / ".codex" / "config.toml"
    cfg.write_text(HEAD + cfg.read_text() + TAIL)
    editor = s.work / "append-comment.sh"
    editor.write_text('#!/bin/sh\nprintf "\\n# reviewed with spb config edit\\n" >> "$1"\n')
    editor.chmod(0o755)
    p = subprocess.run([str(bin_dir / "spb"), "config", "edit", "--yes", "--force"],
                       env=s.env(EDITOR=str(editor)), cwd=s.home, capture_output=True,
                       text=True, timeout=120)
    shoot.log(f"spb config edit rc={p.returncode}\n{p.stdout.strip()}\n{p.stderr.strip()}")


def open_editor(query):
    def act(s):
        shoot.palette_action(query)(s)
        s.input.key("Return")
        time.sleep(2.5)
        s.input.park()
    return act


def review(s):
    """Change a key Superba manages by hand (service_tier), then Ctrl+S."""
    open_editor("Edit config.toml")(s)
    lines = (s.home / ".codex" / "config.toml").read_text().splitlines()
    row = next(i for i, l in enumerate(lines) if l.startswith("service_tier"))
    # Into the text: below the toolbar and the notes, on a line of the file.
    s.input.click(756, 352)
    s.input.key("ctrl+Home")
    for _ in range(row):
        s.input.key("Down")
    s.input.key("Home", "shift+End")
    s.input.type('service_tier = "flex"', delay=30)
    time.sleep(0.8)
    s.input.key("ctrl+s")
    time.sleep(1.2)
    s.input.park()


def restore(s):
    """The Backups dialog, then its one backup (the editor's) with its diff."""
    open_editor("Restore config.toml")(s)
    # The only row of the centered dialog at 1280x800.
    s.input.click(640, 382)
    time.sleep(1.5)
    s.input.park()


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--display", default=":157")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--work-dir", type=Path, help="scratch space (default: the system temp directory)")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--only", help="comma-separated shot name prefixes")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    work = (a.work_dir or Path(tempfile.gettempdir())).resolve()
    work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    for tool in ("Xvfb", "xdotool", "scrot", "openssl"):
        if shutil.which(tool) is None:
            raise SystemExit(f"{tool} not found")
    enter_namespace()

    real_mkdtemp = tempfile.mkdtemp
    shoot.tempfile = SimpleNamespace(
        mkdtemp=lambda prefix="", dir=None: real_mkdtemp(prefix=prefix, dir=str(work)))
    args = SimpleNamespace(display=a.display, sizes=[SIZE], app_dir=None, no_router_log=False,
                           rollouts=False, router_on=False, bin_dir=a.bin_dir, out=a.out,
                           keep=a.keep)
    s = shoot.Session(args)
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        fake_codex(s)
        s.provision()
        s.check_paths()
        hand_edit(s, a.bin_dir)
        data = s.home / ".local/share/superba"
        codex = s.home / ".codex"
        snapshots = {d: s.work / f"snapshot-{d.name}" for d in (data, codex)}
        for src, dst in snapshots.items():
            shutil.copytree(src, dst)

        def restored():
            for src, dst in snapshots.items():
                shutil.rmtree(src)
                shutil.copytree(dst, src)

        only = [p for p in (a.only or "").split(",") if p]
        plan = [
            ("codex-files", open_editor("Edit config.toml")),
            ("codex-files-review", review),
            ("codex-files-auth", open_editor("Edit auth.json")),
            ("codex-files-backups", open_editor("Restore config.toml")),
            ("codex-files-restore", restore),
        ]
        for stem, actions in plan:
            for theme, lang in VARIANTS:
                name = f"{stem}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                if only and not any(name == p or name.startswith(p + "-") for p in only):
                    continue
                restored()
                s.shot(name, "settings", theme, lang, 8.0, actions, SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
