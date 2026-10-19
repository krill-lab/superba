#!/usr/bin/env python3
"""The tray icon / "close to tray" (item 70) on Linux, from the real `superba`
binary, with screenshots and a transcript.

A private session bus (`dbus-daemon`) and a minimal StatusNotifierItem host
(`examples/sni_host.rs`: owns org.kde.StatusNotifierWatcher, reads an item's
dbusmenu and clicks it like a desktop's tray) stand in for KDE Plasma / the
GNOME AppIndicator extension. Steps, each checked:

1. Superba → General with a tray (light/dark, en/zh): "When the window is
   closed" not chosen yet, "Start hidden in the tray".
2. The window's close button (WM_DELETE_WINDOW): the first-close question
   (en/zh shots); "Keep running in the tray" hides the window (unmapped) and
   the item stays registered; its menu is dumped (tray-menu.txt).
3. Activate (a click on the icon) shows the window again; closing again hides
   it without asking; a second `superba` start shows it (single instance) and
   ends.
4. "Quit Superba" from the tray menu ends the process.
5. `superba --start-in-tray`: no window, the item registers.
6. Without a host (the watcher stopped): Superba → General shows both rows disabled
   with the reason, and the close button quits.

Like the other shot scripts it re-runs itself in an unprivileged user + PID
namespace with an isolated HOME. Build first:
`cargo build -p superba-gui --bin superba --example sni_host` and `-p superba-cli`.

    python3 -I crates/superba-gui/tools/screenshots/tray_shots.py \\
        --bin-dir target/debug --host-bin target/debug/examples/sni_host \\
        --display :171 --out .private/v3/tray-shots --work-dir /var/tmp
"""

import argparse
import ctypes
import importlib.util
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_TRAY_SHOTS_NS"
SIZE = (1280, 800)

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)

LOG = []


def note(msg):
    shoot.log(msg)
    LOG.append(msg)


def check(cond, what):
    note(("PASS " if cond else "FAIL ") + what)
    if not cond:
        raise SystemExit(f"check failed: {what}")


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


class XEvent(ctypes.Union):
    class Client(ctypes.Structure):
        _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong),
                    ("send_event", ctypes.c_int), ("display", ctypes.c_void_p),
                    ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                    ("format", ctypes.c_int), ("data", ctypes.c_long * 5)]
    _fields_ = [("xclient", Client), ("pad", ctypes.c_long * 24)]


def close_button(display, wid):
    """What a window manager sends for the title bar's close button."""
    x = ctypes.CDLL("libX11.so.6")
    x.XOpenDisplay.restype = ctypes.c_void_p
    x.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x.XInternAtom.restype = ctypes.c_ulong
    x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    x.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long,
                             ctypes.c_void_p]
    x.XFlush.argtypes = [ctypes.c_void_p]
    x.XCloseDisplay.argtypes = [ctypes.c_void_p]
    dpy = x.XOpenDisplay(display.encode())
    ev = XEvent()
    ev.xclient.type = 33  # ClientMessage
    ev.xclient.window = wid
    ev.xclient.message_type = x.XInternAtom(dpy, b"WM_PROTOCOLS", 0)
    ev.xclient.format = 32
    ev.xclient.data[0] = x.XInternAtom(dpy, b"WM_DELETE_WINDOW", 0)
    x.XSendEvent(dpy, wid, 0, 0, ctypes.byref(ev))
    x.XFlush(dpy)
    x.XCloseDisplay(dpy)


class Run:
    def __init__(self, s, a):
        self.s, self.a = s, a
        self.bus = None
        self.address = None
        self.host = None
        self.items = []
        self.gui = None

    # ---- the bus and the host ----------------------------------------------
    def start_bus(self):
        sock = self.s.work / "bus"
        self.bus = subprocess.Popen(
            ["dbus-daemon", "--session", "--nofork", "--nopidfile", "--print-address=1",
             f"--address=unix:path={sock}"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, start_new_session=True)
        self.s.pids.append(self.bus.pid)
        self.address = self.bus.stdout.readline().strip()
        note(f"session bus up ({'unix:path=<work>/bus' if self.address else 'none'})")

    def start_host(self):
        out = open(self.s.work / "host.out", "w")
        self.host = subprocess.Popen([str(self.a.host_bin), "serve", self.address],
                                     stdout=out, stderr=subprocess.STDOUT,
                                     start_new_session=True)
        self.s.pids.append(self.host.pid)
        time.sleep(0.5)

    def stop_host(self):
        shoot.kill(self.host.pid)
        self.host.wait(timeout=5)
        self.host = None

    def registered(self):
        p = self.s.work / "host.out"
        return [l.strip() for l in p.read_text().splitlines() if l.strip()] if p.exists() else []

    def wait_item(self, n, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            items = self.registered()
            if len(items) >= n:
                return items[n - 1]
            time.sleep(0.2)
        return None

    def host_cmd(self, *args):
        p = subprocess.run([str(self.a.host_bin), args[0], self.address, *args[1:]],
                           capture_output=True, text=True, timeout=20)
        return p.returncode, p.stdout, p.stderr

    # ---- the GUI ---------------------------------------------------------------
    def env(self):
        return self.s.env(DBUS_SESSION_BUS_ADDRESS=self.address)

    def launch(self, *flags, theme="light", lang="en", page="overview"):
        self.gui = subprocess.Popen(
            [str(self.s.bin / "superba"), "--size", f"{SIZE[0]}x{SIZE[1]}", "--theme", theme,
             "--lang", lang, "--page", page, *flags],
            env=self.env(), cwd=self.s.home, stdout=subprocess.DEVNULL,
            stderr=open(self.s.work / "gui.err", "a"), start_new_session=True)
        return self.gui

    def windows(self, visible=True):
        args = ["search"] + (["--onlyvisible"] if visible else []) + ["--name", "^Superba$"]
        p = subprocess.run(["xdotool", *args], env={"DISPLAY": self.s.display,
                           "PATH": "/usr/bin:/bin"}, capture_output=True, text=True)
        return [int(w) for w in p.stdout.split()]

    def wait(self, what, cond, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            if cond():
                return True
            time.sleep(0.2)
        return False

    def close_window(self):
        for wid in self.windows(visible=False):
            close_button(self.s.display, wid)

    def shot(self, name):
        out = self.a.out / f"{name}.png"
        subprocess.run(["scrot", "-o", "-a", f"0,0,{SIZE[0]},{SIZE[1]}", str(out)],
                       env=self.s.env(), check=True)
        note(f"shot {out.name}")

    def stop_gui(self):
        if self.gui and self.gui.poll() is None:
            shoot.kill(self.gui.pid)
            try:
                self.gui.wait(timeout=5)
            except subprocess.TimeoutExpired:
                shoot.kill(self.gui.pid, signal.SIGKILL)
        self.gui = None

    def scroll_to_window_section(self):
        # Superba → General: switching controls, then window behavior.
        self.s.input.scroll(760, 450, 8)
        time.sleep(0.8)
        self.s.input.park()


def run(r):
    s, a = r.s, r.a
    r.start_bus()
    r.start_host()

    # 1. Superba → General with a tray.
    for theme, lang in (("light", "en"), ("dark", "zh-CN"), ("dark", "en"), ("light", "zh-CN")):
        r.launch(theme=theme, lang=lang, page="general")
        time.sleep(6)
        r.scroll_to_window_section()
        r.shot(f"prefs-window-{theme}-{'zh' if lang == 'zh-CN' else 'en'}")
        r.stop_gui()
    item_count = len(r.registered())
    check(item_count >= 4, f"every start registered a tray item ({item_count})")

    # 2. The first close asks.
    for theme, lang in (("light", "zh-CN"), ("dark", "en")):
        r.launch(theme=theme, lang=lang)
        time.sleep(6)
        r.close_window()
        time.sleep(1.5)
        s.input.park()
        r.shot(f"first-close-{theme}-{'zh' if lang == 'zh-CN' else 'en'}")
        check(r.gui.poll() is None and bool(r.windows()), "the question keeps the window")
        if lang == "en":
            break
        r.stop_gui()
    # "Keep running in the tray": the primary button, first in the dialog.
    s.input.key("Tab")
    time.sleep(0.4)
    s.input.key("Return")
    hidden = r.wait("hidden", lambda: not r.windows() and r.gui.poll() is None)
    check(hidden, "'Keep running in the tray' hides the window; Superba keeps running")
    item = r.registered()[-1]
    rc, menu, err = r.host_cmd("menu", item)
    (a.out / "tray-menu.txt").write_text(menu)
    note("tray menu:\n" + menu)
    check(rc == 0 and "Quit Superba" in menu and "Open Superba" in menu,
          "the tray menu has Open Superba ... Quit Superba")
    rc, tip, _ = r.host_cmd("tooltip", item)
    note(f"tooltip: {tip.strip()}")
    prefs = (s.home / ".local/share/superba/profiles.json").read_text()
    check('"close_action": "tray"' in prefs, "the answer is saved (close_action = tray)")

    # 3. A click on the icon, closing again, a second start.
    r.host_cmd("activate", item)
    check(r.wait("shown", lambda: bool(r.windows())), "a click on the icon shows the window")
    time.sleep(1)
    s.input.park()
    r.shot("shown-from-tray-dark-en")
    r.close_window()
    check(r.wait("hidden again", lambda: not r.windows()), "closing again hides without asking")
    second = subprocess.run([str(s.bin / "superba")], env=r.env(), cwd=s.home,
                            capture_output=True, timeout=30)
    check(second.returncode == 0, "a second start ends at once (rc 0)")
    check(r.wait("shown by the second start", lambda: bool(r.windows())),
          "a second start shows the running window")
    check(len(r.registered()) == item_count + 2, "the second start registered no item of its own")

    # 4. Quit from the tray.
    r.close_window()
    r.wait("hidden", lambda: not r.windows())
    rc, _, err = r.host_cmd("click", item, "Quit Superba")
    check(rc == 0, f"'Quit Superba' clicked {err.strip()}")
    check(r.wait("exited", lambda: r.gui.poll() is not None), "Quit Superba ends the process")
    r.gui = None

    # 5. Start in the tray.
    before = len(r.registered())
    r.launch("--start-in-tray", theme="dark", lang="en")
    check(r.wait_item(before + 1) is not None, "--start-in-tray registers the item")
    time.sleep(4)
    check(not r.windows() and r.gui.poll() is None, "--start-in-tray shows no window")
    r.host_cmd("activate", r.registered()[-1])
    check(r.wait("shown", lambda: bool(r.windows())), "the icon opens the window")
    r.stop_gui()

    # 6. No tray host.
    r.stop_host()
    for theme, lang in (("light", "en"), ("dark", "zh-CN")):
        r.launch(theme=theme, lang=lang, page="general")
        time.sleep(6)
        r.scroll_to_window_section()
        r.shot(f"prefs-window-no-tray-{theme}-{'zh' if lang == 'zh-CN' else 'en'}")
        r.close_window()
        check(r.wait("exited", lambda: r.gui.poll() is not None),
              "without a tray host the close button quits")
        r.gui = None


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--host-bin", type=Path, default=REPO / "target/debug/examples/sni_host")
    ap.add_argument("--display", default=":171")
    ap.add_argument("--out", type=Path, default=REPO / ".private/v3/tray-shots")
    ap.add_argument("--work-dir", type=Path, default=Path("/var/tmp"))
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    a.bin_dir, a.host_bin, a.out = a.bin_dir.resolve(), a.host_bin.resolve(), a.out.resolve()
    work = a.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    for tool in ("Xvfb", "xdotool", "scrot", "openssl", "dbus-daemon"):
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
    for name in ("superba", "spb", "superba-router"):
        if (a.bin_dir / name).exists():
            shutil.copy2(a.bin_dir / name, s.bin / name)
    args.bin_dir = s.bin
    r = Run(s, a)
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        s.check_paths()
        run(r)
        note("ALL CHECKS PASSED")
    finally:
        r.stop_gui()
        (a.out / "transcript.txt").write_text("\n".join(LOG) + "\n")
        s.close()


if __name__ == "__main__":
    main()
