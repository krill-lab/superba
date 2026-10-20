#!/usr/bin/env python3
"""Take Superba's documentation screenshots from the real binaries.

Nothing is faked inside Superba: `superba` and `spb` run unmodified against an
isolated HOME, a throwaway CA and a local mock relay (mockrelay.py: three https
lines with billing, usage and costs endpoints, a Codex-native model list with
speed tiers, an OpenAI-style `/v1/models` list and a desktop-app APT index that
SUPERBA_APP_DOWNLOADS points at). fixtures.py writes a router request log and a
Codex `state_5.sqlite` with session names into the isolated home, so the Usage
page has rows to show; with --rollouts rollouts.py adds Codex rollout files
(requests the router also recorded, plus a direct-only session), --no-router-log
leaves only those, and --router-on points Codex at a local router. Optionally the
real desktop app is launched through `spb launch` so the hot-switch countdown can
be captured.

Every page is shot at 1280x800 and 1440x900 (`--sizes`), dark and light, English
and Chinese; extra states: the command palette and the setup wizard (opened again
from the palette with an existing provider).

Requirements: Linux, Xvfb, xdotool, libX11 + libXtst (XTEST), scrot, openssl,
Python 3.11 (stdlib only).

Pointer moves and clicks go through XTEST (class Input): xdotool's mousemove
leaves the pointer where it is on these Xvfb displays, so the mouse was never
parked and clicks landed at the screen centre. Every move is checked with
XQueryPointer and a pointer that did not move stops the run. The screen is a
few pixels larger than the largest window so the parked pointer is outside the
window (no hover state in the shots). Keys and text still go through xdotool,
after `windowfocus` on the Superba window. Ad-hoc driver scripts load this file
(importlib) and use `Input(display)` or a Session's `s.input`:

    inp = s.input                 # or shoot.Input(":178")
    inp.click(120, 300)           # move (verified) + left click
    inp.move(600, 200)            # hover
    inp.scroll(700, 500, 5)       # wheel down 5 notches (negative: up)
    inp.park()                    # bottom-right corner of the screen, outside the window
    inp.key("ctrl+k"); inp.type("cdn"); inp.key("Return")

    python3 -I crates/superba-gui/tools/screenshots/shoot.py \\
        --bin-dir target/debug [--display :176] [--out docs/screenshots] \\
        [--app-dir /path/to/extracted/desktop-app] [--only overview,router] \\
        [--sizes 1280x800,1440x900] [--keep]

Every child gets an environment built from scratch (no inherited CODEX_*, XDG_*,
proxy or DBus variables), `SUPERBA_SECRET_BACKEND=file` and its own HOME under
/tmp (or $SUPERBA_SHOTS_WORKDIR), so the real ~/.codex, keyring and running apps are never touched. Processes
are stopped by PID. The key is a fixed fake value known only to the mock relay.

The desktop app given with --app-dir is copied into the work directory first,
so the pictures show a neutral temporary path rather than where it is kept; the
script refuses to shoot when `spb status` names a path under /data/ or the
user's real home.

Superba lists the user's running desktop apps and Codex processes, which an
isolated HOME cannot hide. On Linux the script therefore re-runs itself in an
unprivileged user + PID namespace (`unshare --user --map-current-user --pid --fork
--mount-proc`; the uid stays the same, so Superba still opens the app with a
debugging port), where only its own processes exist. As a guard it refuses to
start, and stops between shots, if a ChatGPT/Codex desktop app of another HOME is
still visible: its paths and environment would end up in the pictures.
"""

import argparse
import ctypes
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
SIZE = (1280, 800)
# Window sizes every page is shot at (UX §8); the first keeps the plain names.
SIZES = [(1280, 800), (1440, 900)]
# Fake key: accepted only by the mock relay started below.
KEY = "nb_shot_0f6c2d8e4a1b9c3d5e7f0a2b4c6d8e1f7a3c"
LINES = [
    ("cn-fast", "line.krill.cn-fast", "China Express", 38),
    ("overseas", "line.krill.overseas", "Overseas", 96),
    ("cdn", "line.krill.cdn", "Multi-carrier CDN", 154),
]
PAGES = ["overview", "tools", "providers", "models", "router", "usage", "settings", "sessions",
         "hosts", "claude-code", "claude-settings", "superba", "updates", "diagnostics",
         "data", "general", "appearance", "network", "notify"]
VARIANTS = [("dark", "en"), ("light", "en"), ("dark", "zh-CN"), ("light", "zh-CN")]
# Extra screen beyond the largest window: the parked pointer sits there.
PARK_MARGIN = 8


def log(msg):
    print(f"[shoot] {msg}", flush=True)


class Input:
    """Pointer and keyboard input on one X display.

    The pointer moves through the XTEST extension (libXtst via ctypes) because
    xdotool 3.20160805's mousemove has no effect on these Xvfb displays; each
    move is verified with XQueryPointer and raises when the pointer is not where
    it was sent. Keys and text use xdotool (it maps any character, CJK too) after
    focusing the Superba window, without which they do not reach it."""

    def __init__(self, display, window="^Superba$"):
        self.display = display
        self.window = window
        self.env = {"DISPLAY": display, "PATH": "/usr/local/bin:/usr/bin:/bin"}
        try:
            self._x = ctypes.CDLL("libX11.so.6")
            self._t = ctypes.CDLL("libXtst.so.6")
        except OSError as e:
            raise SystemExit(f"libX11/libXtst not found ({e}); install libxtst6") from e
        x, t, p = self._x, self._t, ctypes.c_void_p
        x.XOpenDisplay.restype = p
        x.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x.XCloseDisplay.argtypes = [p]
        x.XSync.argtypes = [p, ctypes.c_int]
        x.XDefaultRootWindow.restype = ctypes.c_ulong
        x.XDefaultRootWindow.argtypes = [p]
        u, i = ctypes.c_ulong, ctypes.c_int
        x.XQueryPointer.argtypes = [p, u, ctypes.POINTER(u), ctypes.POINTER(u),
                                    ctypes.POINTER(i), ctypes.POINTER(i), ctypes.POINTER(i),
                                    ctypes.POINTER(i), ctypes.POINTER(ctypes.c_uint)]
        x.XDisplayWidth.argtypes = x.XDisplayHeight.argtypes = [p, i]
        t.XTestQueryExtension.argtypes = [p] + [ctypes.POINTER(i)] * 4
        t.XTestFakeMotionEvent.argtypes = [p, i, i, i, u]
        t.XTestFakeButtonEvent.argtypes = [p, ctypes.c_uint, i, u]
        self._d = x.XOpenDisplay(display.encode())
        if not self._d:
            raise SystemExit(f"cannot open display {display}")
        ev, err, major, minor = (ctypes.c_int() for _ in range(4))
        if not t.XTestQueryExtension(self._d, ev, err, major, minor):
            raise SystemExit(f"display {display} has no XTEST extension")
        self._root = x.XDefaultRootWindow(self._d)
        self.screen = (x.XDisplayWidth(self._d, 0), x.XDisplayHeight(self._d, 0))

    def close(self):
        if self._d:
            self._x.XCloseDisplay(self._d)
            self._d = None

    # ---- pointer -------------------------------------------------------------
    def where(self):
        """The pointer position on the screen."""
        root, child = ctypes.c_ulong(), ctypes.c_ulong()
        rx, ry, wx, wy = (ctypes.c_int() for _ in range(4))
        mask = ctypes.c_uint()
        self._x.XQueryPointer(self._d, self._root, root, child, rx, ry, wx, wy, mask)
        return rx.value, ry.value

    def move(self, x, y, settle=0.15):
        """Move the pointer to (x, y) and check that it got there."""
        x, y = int(x), int(y)
        self._t.XTestFakeMotionEvent(self._d, -1, x, y, 0)
        self._x.XSync(self._d, 0)
        for _ in range(25):
            if self.where() == (x, y):
                time.sleep(settle)
                return
            time.sleep(0.02)
        raise SystemExit(f"the pointer did not move to {x},{y} on {self.display}: "
                         f"it is at {self.where()}")

    def park(self):
        """Move the pointer to the bottom-right corner of the screen, which lies
        outside the window when the screen has the PARK_MARGIN (no hover)."""
        self.move(self.screen[0] - 1, self.screen[1] - 1)

    def button(self, button=1, count=1):
        """Press and release a mouse button where the pointer is."""
        for _ in range(count):
            self._t.XTestFakeButtonEvent(self._d, button, 1, 0)
            self._x.XSync(self._d, 0)
            time.sleep(0.03)
            self._t.XTestFakeButtonEvent(self._d, button, 0, 0)
            self._x.XSync(self._d, 0)
            time.sleep(0.08)

    def click(self, x, y, button=1, count=1):
        """Move (verified) and click; count=2 double-clicks."""
        self.move(x, y)
        self.button(button, count)

    def scroll(self, x, y, notches):
        """Mouse wheel at (x, y): positive scrolls down, negative up."""
        self.move(x, y)
        for _ in range(abs(int(notches))):
            self.button(5 if notches > 0 else 4)

    # ---- keyboard ------------------------------------------------------------
    def xdo(self, *args, timeout=20):
        p = subprocess.run(["xdotool", *map(str, args)], env=self.env,
                           capture_output=True, text=True, timeout=timeout)
        if p.returncode != 0:
            raise SystemExit(f"xdotool {' '.join(map(str, args))} failed: {p.stderr.strip()}")
        return p.stdout

    def focus(self):
        """Give the window keyboard focus (there is no window manager)."""
        self.xdo("search", "--sync", "--name", self.window, "windowfocus", "--sync")

    def key(self, *combos):
        """Key presses such as "ctrl+k", "Return", "Escape", "Tab"."""
        self.focus()
        self.xdo("key", "--delay", "60", *combos)

    def type(self, text, delay=60):
        """Type text (any characters)."""
        self.focus()
        self.xdo("type", "--delay", str(delay), text)


class Session:
    def __init__(self, args):
        self.args = args
        self.work = Path(tempfile.mkdtemp(prefix="sbshots-",
                                         dir=os.environ.get("SUPERBA_SHOTS_WORKDIR", "/tmp")))
        self.home = self.work / "home"
        self.bin = self.work / "bin"
        self.tls = self.work / "tls"
        self.pids = []
        for d in (self.home, self.bin, self.work / "xdg-runtime"):
            d.mkdir(parents=True)
        os.chmod(self.work / "xdg-runtime", 0o700)
        self.display = args.display
        self.xvfb = None
        self.input = None
        self.ports = {}

    # ---- environment ---------------------------------------------------------
    def env(self, **extra):
        env = {
            "PATH": f"{self.bin}:/usr/local/bin:/usr/bin:/bin",
            "HOME": str(self.home),
            "USER": os.environ.get("USER", "shots"),
            "LOGNAME": os.environ.get("USER", "shots"),
            "SHELL": "/bin/bash",
            "LANG": "C.UTF-8",
            "TERM": "dumb",
            "DISPLAY": self.display,
            "XDG_RUNTIME_DIR": str(self.work / "xdg-runtime"),
            "SSL_CERT_FILE": str(self.tls / "bundle.pem"),
            "CODEX_CA_CERTIFICATE": str(self.tls / "bundle.pem"),
            "SUPERBA_SECRET_BACKEND": "file",
            "SUPERBA_TEST_SITES": str(self.work / "sites.json"),
            "NO_COLOR": "1",
        }
        if self.ports:
            # The desktop-app update check reads the mock relay's APT index.
            env["SUPERBA_APP_DOWNLOADS"] = f"https://127.0.0.1:{self.ports['overseas']}"
        if self.args.app_dir:
            env["SUPERBA_APP_PATH"] = str(self.app_dir())
            env["SUPERBA_APP_ARGS"] = "--no-sandbox --disable-gpu --password-store=basic"
        env.update(extra)
        return env

    def run(self, argv, stdin=None, check=True, timeout=180):
        p = subprocess.run(argv, env=self.env(), cwd=self.home, input=stdin,
                           capture_output=True, text=True, timeout=timeout)
        if check and p.returncode != 0:
            raise SystemExit(f"{argv[0]} {' '.join(argv[1:3])} failed ({p.returncode}):\n"
                             f"{p.stdout}\n{p.stderr}")
        return p

    def app_dir(self):
        """The copy of the desktop app the shots use (see the module docs)."""
        return self.work / "ChatGPT"

    def copy_app(self):
        if not self.args.app_dir:
            return
        log("copying the desktop app into the work directory")
        shutil.copytree(self.args.app_dir, self.app_dir(), symlinks=True)

    def check_paths(self):
        """Refuse to shoot when Superba would show a private path."""
        spb = str(self.args.bin_dir / "spb")
        p = self.run([spb, "--json", "status"], check=False)
        real_home = os.path.expanduser("~")
        # An explicit SUPERBA_SHOTS_WORKDIR may itself sit under /data/ or ~.
        shown = p.stdout.replace(str(self.work), "")
        for bad in ("/data/", real_home + "/"):
            if bad in shown:
                raise SystemExit(f"spb status names a path under {bad}; it would be in the pictures")

    # ---- infrastructure ------------------------------------------------------
    def start_xvfb(self):
        num = self.display.lstrip(":")
        if Path(f"/tmp/.X11-unix/X{num}").exists():
            raise SystemExit(f"display {self.display} is in use; pick another with --display")
        # A margin beyond the largest window holds the parked pointer.
        w = max(w for w, _ in self.args.sizes) + PARK_MARGIN
        h = max(h for _, h in self.args.sizes) + PARK_MARGIN
        self.xvfb = subprocess.Popen(
            ["Xvfb", self.display, "-screen", "0", f"{w}x{h}x24", "-nolisten", "tcp"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        self.pids.append(self.xvfb.pid)
        for _ in range(50):
            if Path(f"/tmp/.X11-unix/X{num}").exists():
                break
            time.sleep(0.1)
        else:
            raise SystemExit("Xvfb did not start")
        for _ in range(50):
            try:
                self.input = Input(self.display)
                break
            except SystemExit:
                time.sleep(0.1)
        else:
            self.input = Input(self.display)
        # Fails loudly here when the pointer cannot be moved at all.
        self.input.park()

    def start_relay(self):
        subprocess.run(["bash", str(REPO / "e2e/lib/mkcert.sh"), str(self.tls)], check=True)
        ports_file = self.work / "ports.json"
        argv = [sys.executable, "-I", str(HERE / "mockrelay.py"),
                "--lines", ",".join(l[0] for l in LINES),
                "--ports-file", str(ports_file), "--api-key", KEY,
                "--cert", str(self.tls / "server.pem"), "--key", str(self.tls / "server.key"),
                "--latency", ",".join(f"{l[0]}={l[3]}" for l in LINES),
                "--edge-block", "cn-fast", "--log", str(self.work / "relay.jsonl")]
        relay = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=open(self.work / "relay.err", "w"),
                                 start_new_session=True)
        self.pids.append(relay.pid)
        for _ in range(100):
            if ports_file.exists():
                self.ports = json.loads(ports_file.read_text())
                break
            time.sleep(0.1)
        else:
            raise SystemExit("mock relay did not start")

    def write_sites(self):
        lines = [{"id": lid, "name": {"key": key, "fallback": name},
                  "origin": f"https://127.0.0.1:{self.ports[lid]}", "source": "test"}
                 for lid, key, name, _ in LINES]
        site = {
            "id": "krill-local",
            "kind": "krill",
            "name": {"key": "site.krill", "fallback": "Krill"},
            "homepage": "https://www.krill-code.com/",
            "lines": lines,
            "capabilities": {
                "auth_modes": ["pat", "apikey"],
                "codex_paths": ["backend_api_codex", "codex_v1"],
                "latency_probe": True, "key_check": True, "account_summary": True,
                "usage_api": True, "model_catalog": True,
            },
            "endpoints": {
                "models_unauth": "/v1/models",
                "whoami": "/v1/user-auth-credential/whoami",
                "accounts_check": "/backend-api/wham/accounts/check",
                "backend_codex_models": "/backend-api/codex/models",
                "codex_v1_models": "/codex/v1/models",
            },
            "default_line": "overseas",
        }
        (self.work / "sites.json").write_text(json.dumps([site], indent=1))

    def provision(self):
        """Profile with the fake key on the Overseas line, applied (no app restart)."""
        spb = str(self.args.bin_dir / "spb")
        if self.args.app_dir:
            codex = self.app_dir() / "resources" / "codex"
            if codex.exists():
                (self.bin / "codex").symlink_to(codex)
        self.run([spb, "profile", "add", "krill", "--name", "Krill", "--site", "krill-local",
                  "--line", "overseas", "--auth", "pat", "--key-stdin"], stdin=KEY + "\n")
        self.run([spb, "profile", "use", "krill", "--yes", "--countdown", "0"], check=False)
        for line in self.args.disable_line or []:
            # A disabled line (user bug 108) for the Providers page's shots.
            self.run([spb, "profile", "edit", "krill", "--disable-line", line])
        p = self.run([spb, "status"], check=False)
        log("status after setup:\n" + p.stdout.strip())
        self.run([spb, "site", "add", "example-api", "--name", "Example API",
                  "--base-url", f"https://127.0.0.1:{self.ports['overseas']}/v1"], check=False)
        # A longer countdown keeps the hot-switch overlay on screen for its shot.
        profiles = self.home / ".local/share/superba/profiles.json"
        if profiles.exists():
            data = json.loads(profiles.read_text())
            data.setdefault("settings", {})["countdown_secs"] = 10
            profiles.write_text(json.dumps(data, indent=2))
        self.fixtures()
        if self.args.app_dir:
            p = self.run([spb, "launch"], check=False, timeout=240)
            log(f"spb launch rc={p.returncode}")
            time.sleep(6)

    def fixtures(self):
        """A router request log and session names (fixtures.py), in this HOME only.
        With a desktop app its Codex creates the state database first: the app
        cannot open a hand-made one (fixtures.py)."""
        argv = [sys.executable, "-I", str(HERE / "fixtures.py"),
                "--data-dir", str(self.home / ".local/share/superba"),
                "--codex-home", str(self.home / ".codex"),
                "--upstream", f"127.0.0.1:{self.ports['overseas']}",
                "--line-host", f"cn-fast=127.0.0.1:{self.ports['cn-fast']}"]
        codex = self.app_dir() / "resources" / "codex"
        if self.args.app_dir and codex.exists():
            argv += ["--codex", str(codex)]
        p = self.run(argv)
        log(f"fixtures: {p.stdout.strip()}")
        data = self.home / ".local/share/superba"
        if self.args.no_router_log:
            # Rows of the Usage page then come from the rollouts only.
            (data / "router" / "usage.sqlite3").unlink(missing_ok=True)
        if self.args.rollouts:
            p = self.run([sys.executable, "-I", str(HERE / "rollouts.py"),
                          "--codex-home", str(self.home / ".codex"),
                          "--data-dir", str(data), "--backdate-state"])
            log(f"rollouts: {p.stdout.strip()}")
        if self.args.router_on:
            # Router mode on with the provider as its member, written by Superba
            # itself (`--write-only`: no router runs, the pages show the
            # configuration and the history). A hand edit of config.toml would
            # only show Superba's "changed outside Superba" dialog.
            spb = str(self.args.bin_dir / "spb")
            p = self.run([spb, "router", "enable", "--member", "krill", "--yes",
                          "--write-only"], check=False)
            log(f"spb router enable rc={p.returncode}")
            if p.returncode == 0:
                return
            # Older builds: point Codex at a local router by hand.
            cfg = self.home / ".codex" / "config.toml"
            text = cfg.read_text() if cfg.exists() else ""
            lines = [l for l in text.splitlines() if not l.startswith("openai_base_url")]
            first_table = next((i for i, l in enumerate(lines) if l.startswith("[")), len(lines))
            lines.insert(first_table,
                         'openai_base_url = "http://127.0.0.1:18431/backend-api/codex"')
            cfg.write_text("\n".join(lines) + "\n")

    # ---- capture -------------------------------------------------------------
    def xdo(self, *args):
        """xdotool on this display (keys and window focus; the pointer goes
        through self.input)."""
        return self.input.xdo(*args)

    def shot(self, name, page, theme, lang, wait=6.0, actions=None, size=SIZE):
        guard(self.home)
        out = self.args.out / f"{name}.png"
        gui = subprocess.Popen(
            [str(self.args.bin_dir / "superba"), "--size", f"{size[0]}x{size[1]}",
             "--theme", theme, "--lang", lang, "--page", page],
            env=self.env(), cwd=self.home, stdout=subprocess.DEVNULL,
            stderr=open(self.work / "gui.err", "a"), start_new_session=True)
        try:
            time.sleep(wait)
            # Outside the window: nothing in the shot is hovered.
            self.input.park()
            if actions:
                actions(self)
            time.sleep(0.6)
            guard(self.home)
            w, h = size
            subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(out)], env=self.env(), check=True)
            log(f"{out.relative_to(REPO) if out.is_relative_to(REPO) else out}")
        finally:
            kill(gui.pid)
            try:
                gui.wait(timeout=5)
            except subprocess.TimeoutExpired:
                kill(gui.pid, signal.SIGKILL)

    def app_pids(self):
        """Processes started for this session's HOME (the desktop app launched by
        `spb launch` and its children), other than the ones started here. The app's
        main process has no readable HOME, so whole sessions are matched."""
        sessions, members = set(), {}
        for d in Path("/proc").iterdir():
            if not d.name.isdigit() or int(d.name) in self.pids:
                continue
            try:
                stat = (d / "stat").read_text()
                sid = int(stat.rsplit(")", 1)[1].split()[3])
                env = (d / "environ").read_bytes().split(b"\0")
            except (OSError, ValueError, IndexError):
                continue
            members.setdefault(sid, []).append(int(d.name))
            if f"HOME={self.home}".encode() in env:
                sessions.add(sid)
        own = {os.getsid(p) for p in self.pids if _alive(p)}
        return [p for sid in sessions - own for p in members.get(sid, [])]

    def close(self):
        if self.input is not None:
            self.input.close()
        for pid in self.app_pids():
            kill(pid)
        time.sleep(1)
        for pid in self.app_pids():
            kill(pid, signal.SIGKILL)
        for pid in reversed(self.pids):
            kill(pid)
        num = self.display.lstrip(":")
        time.sleep(0.5)
        for f in (f"/tmp/.X11-unix/X{num}", f"/tmp/.X{num}-lock"):
            if self.xvfb is not None and os.path.exists(f):
                try:
                    os.remove(f)
                except OSError:
                    pass
        if self.args.keep:
            log(f"kept {self.work}")
        else:
            shutil.rmtree(self.work, ignore_errors=True)


def foreign_apps(home):
    """PIDs of desktop app main processes of this user that do not belong to `home`.

    Chromium rewrites the main process's environ, so an app counts as ours when
    some process in its session (the app-server child) has HOME=`home`."""
    uid = os.getuid()
    procs = {}
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        try:
            if d.stat().st_uid != uid:
                continue
            stat = (d / "stat").read_text()
            sid = int(stat.rsplit(")", 1)[1].split()[3])
            comm = (d / "comm").read_text().strip()
            # Chromium may rewrite argv into one space-separated string.
            cmd = (d / "cmdline").read_bytes().replace(b"\0", b" ")
        except (OSError, ValueError, IndexError):
            continue
        try:
            env = (d / "environ").read_bytes().split(b"\0")
        except OSError:
            env = []
        procs[int(d.name)] = (sid, comm, cmd, env)
    ours = {sid for sid, _, _, env in procs.values() if f"HOME={home}".encode() in env}
    foreign = []
    for pid, (sid, comm, cmd, _) in procs.items():
        # Electron main processes only: Superba already ignores Codex processes
        # whose environment names another home.
        main = comm in ("ChatGPT", "Codex", "chatgpt") and b"--type=" not in cmd
        if main and sid not in ours:
            foreign.append(pid)
    return sorted(foreign)


def guard(home):
    others = foreign_apps(home)
    if others:
        raise SystemExit(
            f"another desktop app is running for this user (pids {others[:5]}); "
            "Superba would show it in the screenshots. Close it or run later.")


def _alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def kill(pid, sig=signal.SIGTERM):
    try:
        os.kill(pid, sig)
    except ProcessLookupError:
        pass


def focus(s):
    """Give the Superba window keyboard focus (there is no window manager)."""
    s.input.focus()
    # The empty top of the sidebar (user bug 85: the window's drag area; a
    # single click there neither moves nor maximizes the window), below the
    # resize edge Linux has along the top.
    s.input.click(110, 24)
    s.input.park()
    time.sleep(0.3)


def palette_action(query):
    def act(s):
        focus(s)
        s.input.key("ctrl+k")
        time.sleep(0.6)
        s.input.type(query)
        time.sleep(0.4)
    return act


def overview_actions(size):
    """Open the Overview Actions menu without selecting an operation."""
    def act(s):
        focus(s)
        s.input.click(size[0] - 54, 70)
        s.input.park()
    return act


def click_action(x, y):
    """Click one point of the window (a rail item), then park the pointer."""
    def act(s):
        focus(s)
        s.input.click(x, y)
        time.sleep(0.4)
        s.input.park()
    return act


def wizard_action(query):
    """Open the setup wizard from the palette (a re-run with an existing provider)
    and go to its provider step."""
    def act(s):
        palette_action(query)(s)
        s.input.key("Return")
        time.sleep(1.0)
        s.input.key("Return")
        time.sleep(0.4)
    return act


def countdown_action(query):
    def act(s):
        palette_action(query)(s)
        s.input.key("Return")
        time.sleep(6.0)
    return act


NS_ENV = "SUPERBA_SHOTS_NS"


def enter_namespace():
    """Re-run this script in its own user + PID namespace (Linux), once."""
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found; it isolates the shots from other apps")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/release")
    ap.add_argument("--display", default=":176")
    ap.add_argument("--out", type=Path, default=REPO / "docs/screenshots")
    ap.add_argument("--app-dir", type=Path)
    ap.add_argument("--only", help="comma-separated shot name prefixes")
    ap.add_argument("--keep", action="store_true", help="keep the work directory")
    ap.add_argument("--rollouts", action="store_true",
                    help="write Codex rollout files (rollouts.py) for the Usage page")
    ap.add_argument("--no-router-log", action="store_true",
                    help="no router request log (Usage rows from the rollouts only)")
    ap.add_argument("--router-on", action="store_true",
                    help="point Codex at a local router (router mode on, nothing runs)")
    ap.add_argument("--sizes", default=",".join(f"{w}x{h}" for w, h in SIZES),
                    help="comma-separated window sizes WxH (the first keeps plain names)")
    ap.add_argument("--size", help="a single window size WxH (same as --sizes WxH)")
    ap.add_argument("--disable-line", action="append", metavar="LINE",
                    help="disable this line of the Krill provider (repeatable)")
    args = ap.parse_args()
    sizes = args.size or args.sizes
    args.sizes = [tuple(int(x) for x in v.lower().split("x", 1)) for v in sizes.split(",") if v]
    args.bin_dir = args.bin_dir.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    for tool in ("Xvfb", "xdotool", "scrot", "openssl"):
        if shutil.which(tool) is None:
            raise SystemExit(f"{tool} not found")
    enter_namespace()

    s = Session(args)
    try:
        guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.copy_app()
        s.provision()
        s.check_paths()
        shots = []
        for i, size in enumerate(args.sizes):
            tail = "" if i == 0 else f"-{size[0]}x{size[1]}"
            for theme, lang in VARIANTS:
                suffix = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}{tail}"
                for page in PAGES:
                    shots.append((f"{page}-{suffix}", page, theme, lang, 7.0, None, size))
                shots.append((f"overview-actions-{suffix}", "overview", theme, lang, 7.0,
                              overview_actions(size), size))
                # Codex settings, "New in latest" (third rail item).
                shots.append((f"settings-new-{suffix}", "settings", theme, lang, 7.0,
                              click_action(290, 240), size))
            shots.append((f"palette-dark-en{tail}", "overview", "dark", "en", 6.0,
                          palette_action("cdn"), size))
            shots.append((f"palette-light-zh{tail}", "overview", "light", "zh-CN", 6.0,
                          palette_action("over"), size))
            shots.append((f"wizard-dark-en{tail}", "overview", "dark", "en", 6.0,
                          wizard_action("onboarding"), size))
            shots.append((f"wizard-light-zh{tail}", "overview", "light", "zh-CN", 6.0,
                          wizard_action("onboarding"), size))
            if args.app_dir:
                shots.append((f"countdown-dark-en{tail}", "overview", "dark", "en", 7.0,
                              countdown_action("cdn"), size))
                shots.append((f"countdown-light-zh{tail}", "overview", "light", "zh-CN", 7.0,
                              countdown_action("cdn"), size))
        only = [p for p in (args.only or "").split(",") if p]
        for name, page, theme, lang, wait, act, size in shots:
            if only and not any(name.startswith(p) for p in only):
                continue
            s.shot(name, page, theme, lang, wait, act, size)
    finally:
        s.close()


if __name__ == "__main__":
    main()
