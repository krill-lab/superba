#!/usr/bin/env python3
"""Frames of the session migration dialog during a real migration, from the
real `superba` binary (user bug 33).

It reuses shoot.py's Session (isolated HOME, throwaway CA, mock relay, Xvfb,
XTEST pointer) and writes, into that HOME only, a few hundred Codex sessions
recorded under the legacy provider id `krill` (rollout files of a chosen size
and their `state_5.sqlite` rows). For each theme and language it opens the
Sessions page, clicks "Migrate to openai…", confirms the full restart and
captures the window every `--every` seconds while the sessions move; with
`--reduce-motion` the Superba → Appearance choice is set first. While capturing it also
samples the CPU time of Superba's UI thread (the main thread) and of the whole
process.

Like shoot.py it re-runs itself in an unprivileged user + PID namespace, so no
desktop app or Codex process of the real user is visible, and the scratch HOME
lives under --work-dir.

    python3 -I crates/superba-gui/tools/screenshots/movers_shots.py \\
        --bin-dir target/release --display :173 --out .private/brand/anim/app \\
        --work-dir .private/movers/work [--sessions 600] [--kb 400] \\
        [--variants light-en,dark-en,light-zh,dark-zh] [--reduce-motion] \\
        [--migrate-at X,Y] [--confirm-at X,Y] [--explore]

--explore only shoots the Sessions page and the confirmation (to find the
two click positions for the window size: the migrate button is right-aligned
in its row, the confirmation's first button is "Full restart").
"""

import argparse
import importlib.util
import json
import os
import random
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_MOVERS_SHOTS_NS"
SIZE = (1280, 800)

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)

TITLES = [
    "Fix the login redirect loop", "重构会话迁移的文件锁", "Add retries to the upload client",
    "排查 macOS 上的打开文件数上限", "Write the release notes for 0.2", "把路由页的统计改成按小时",
    "Plan the database migration", "Speed up the settings catalog", "给 CLI 加一个进度行",
    "Review the MSI installer", "Why does the window icon blur?", "整理 Windows 的路径测试",
]


def write_legacy_sessions(codex_home, n, kb, seed=7):
    """`n` sessions under the legacy provider id `krill` (rollouts of about `kb`
    KiB each, plus their rows in state_5.sqlite). Replaces earlier ones."""
    rng = random.Random(seed)
    root = codex_home / "sessions" / "2026" / "09"
    shutil.rmtree(root, ignore_errors=True)
    db = codex_home / "state_5.sqlite"
    conn = sqlite3.connect(db)
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS threads (
          id TEXT PRIMARY KEY, rollout_path TEXT, created_at INTEGER, updated_at INTEGER,
          source TEXT, model_provider TEXT, cwd TEXT, title TEXT, first_user_message TEXT,
          archived INTEGER NOT NULL DEFAULT 0);
    """)
    conn.execute("DELETE FROM threads WHERE cwd = '/home/user/project'")
    base = 1_758_000_000  # 2025-09-16
    filler_line = None
    for i in range(n):
        created = base + i * 1800
        tm = time.gmtime(created)
        day = tm.tm_mday
        uid = (f"{0x01992a00 + i:08x}-{rng.getrandbits(16):04x}-7{rng.getrandbits(12):03x}"
               f"-8{rng.getrandbits(12):03x}-{rng.getrandbits(48):012x}")
        stamp = time.strftime("%Y-%m-%dT%H-%M-%S", tm)
        iso = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", tm)
        d = root / f"{day:02d}"
        d.mkdir(parents=True, exist_ok=True)
        path = d / f"rollout-{stamp}-{uid}.jsonl"
        title = f"{TITLES[i % len(TITLES)]} ({i + 1})"
        meta = {"timestamp": iso, "type": "session_meta", "payload": {
            "session_id": uid, "id": uid, "timestamp": iso, "cwd": "/home/user/project",
            "originator": "Codex Desktop", "cli_version": "0.162.0", "source": "vscode",
            "thread_source": "user", "model_provider": "krill", "history_mode": "paginated"}}
        lines = [json.dumps(meta, separators=(",", ":"))]
        if filler_line is None:
            # Synthetic, text-free response items (no prompts or answers).
            filler = {"timestamp": iso, "type": "response_item", "payload": {
                "type": "reasoning", "summary": [], "encrypted_content": "x" * 3800}}
            filler_line = json.dumps(filler, separators=(",", ":"))
        lines += [filler_line] * max(1, (kb * 1024) // (len(filler_line) + 1))
        path.write_text("\n".join(lines) + "\n")
        conn.execute(
            "INSERT OR REPLACE INTO threads (id, rollout_path, created_at, updated_at, source,"
            " model_provider, cwd, title, first_user_message) VALUES (?,?,?,?,?,?,?,?,?)",
            (uid, str(path), created, created + 600, "vscode", "krill", "/home/user/project",
             title, title))
    conn.commit()
    conn.close()


def set_reduce_motion(s, on):
    profiles = s.home / ".local/share/superba/profiles.json"
    data = json.loads(profiles.read_text())
    data.setdefault("settings", {})["reduce_motion"] = bool(on)
    profiles.write_text(json.dumps(data, indent=2))


def cpu_ticks(pid):
    """(main thread utime+stime, whole process utime+stime) in clock ticks."""
    def ticks(stat):
        f = Path(stat).read_text().rsplit(")", 1)[1].split()
        return int(f[11]) + int(f[12])
    return ticks(f"/proc/{pid}/task/{pid}/stat"), ticks(f"/proc/{pid}/stat")


def capture(s, args, name, theme, lang, explore):
    shoot.guard(s.home)
    w, h = SIZE
    gui = subprocess.Popen(
        [str(args.bin_dir / "superba"), "--size", f"{w}x{h}", "--theme", theme, "--lang", lang,
         "--page", "sessions"],
        env=s.env(**({"SUPERBA_SCENE_TIME": str(args.scene_time)}
                     if args.scene_time is not None else {})),
        cwd=s.home, stdout=subprocess.DEVNULL,
        stderr=open(s.work / "gui.err", "a"), start_new_session=True)
    out = args.out
    stats = {}
    try:
        time.sleep(8)
        s.input.park()
        subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(out / f"{name}-0-sessions.png")],
                       env=s.env(), check=True)
        s.input.click(*args.migrate_at)
        s.input.park()
        time.sleep(3)
        subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(out / f"{name}-1-confirm.png")],
                       env=s.env(), check=True)
        if explore:
            return stats
        hz = os.sysconf("SC_CLK_TCK")
        s.input.click(*args.confirm_at)
        s.input.park()
        start = time.monotonic()
        t0 = cpu_ticks(gui.pid)
        frame = 0
        stopped = args.stop_after is None
        while time.monotonic() - start < args.duration:
            if not stopped and time.monotonic() - start >= args.stop_after:
                # "Stop and undo": the migration puts every session back.
                s.input.click(*args.stop_at)
                s.input.park()
                stopped = True
            frame += 1
            subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}",
                            str(out / f"{name}-f{frame:02d}.png")], env=s.env(), check=True)
            time.sleep(args.every)
        t1 = cpu_ticks(gui.pid)
        span = time.monotonic() - start
        stats["migrating"] = {"seconds": round(span, 1),
                              "ui_thread_cpu_pct": round(100 * (t1[0] - t0[0]) / hz / span, 1),
                              "process_cpu_pct": round(100 * (t1[1] - t0[1]) / hz / span, 1)}
        # After the dialog: the window idles.
        time.sleep(4)
        t2 = cpu_ticks(gui.pid)
        time.sleep(5)
        t3 = cpu_ticks(gui.pid)
        stats["after"] = {"ui_thread_cpu_pct": round(100 * (t3[0] - t2[0]) / hz / 5, 1),
                          "process_cpu_pct": round(100 * (t3[1] - t2[1]) / hz / 5, 1)}
        subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(out / f"{name}-9-after.png")],
                       env=s.env(), check=True)
        return stats
    finally:
        shoot.kill(gui.pid)
        try:
            gui.wait(timeout=5)
        except subprocess.TimeoutExpired:
            shoot.kill(gui.pid, signal.SIGKILL)


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def point(v):
    x, y = v.split(",")
    return int(x), int(y)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/release")
    ap.add_argument("--display", default=":173")
    ap.add_argument("--out", type=Path, default=REPO / ".private/brand/anim/app")
    ap.add_argument("--work-dir", type=Path, default=Path("/var/tmp"))
    ap.add_argument("--sessions", type=int, default=600)
    ap.add_argument("--kb", type=int, default=400, help="size of each rollout in KiB")
    ap.add_argument("--variants", default="light-en,dark-en,light-zh,dark-zh")
    ap.add_argument("--reduce-motion", action="store_true")
    ap.add_argument("--migrate-at", type=point, default=(1170, 330))
    ap.add_argument("--confirm-at", type=point, default=(420, 520))
    ap.add_argument("--stop-after", type=float,
                    help="click --stop-at this many seconds into the migration")
    ap.add_argument("--stop-at", type=point, default=(335, 673))
    ap.add_argument("--scene-time", type=float,
                    help="hold the scene at this moment of its cycle (SUPERBA_SCENE_TIME)")
    ap.add_argument("--every", type=float, default=0.7)
    ap.add_argument("--duration", type=float, default=12.0)
    ap.add_argument("--explore", action="store_true")
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    work = a.work_dir.resolve()
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
    for name in ("superba", "spb"):
        shutil.copy2(a.bin_dir / name, s.bin / name)
    args.bin_dir = s.bin
    a.bin_dir = s.bin
    results = {}
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        s.check_paths()
        set_reduce_motion(s, a.reduce_motion)
        for variant in a.variants.split(","):
            theme, lang = variant.split("-")
            lang = "zh-CN" if lang == "zh" else "en"
            name = f"{'still' if a.reduce_motion else 'anim'}-{variant}"
            if a.stop_after is not None:
                name += "-stop"
            if a.scene_time is not None:
                name += f"-t{a.scene_time:g}"
            started = time.monotonic()
            write_legacy_sessions(s.home / ".codex", a.sessions, a.kb)
            shoot.log(f"{a.sessions} sessions of {a.kb} KiB written in "
                      f"{time.monotonic() - started:.1f} s")
            results[name] = capture(s, a, name, theme, lang, a.explore)
            shoot.log(f"{name}: {results[name]}")
        (a.out / f"cpu-{'still' if a.reduce_motion else 'anim'}.json").write_text(
            json.dumps(results, indent=2))
    finally:
        s.close()


if __name__ == "__main__":
    main()
