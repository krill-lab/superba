"""Infrastructure for the Superba e2e harness: isolated home, throwaway CA, line
forwarders, Xvfb, command runner, process bookkeeping and evidence files.

Nothing here touches the real user's home, Codex home, shell rc files or
processes: every child process gets an environment built from scratch (no
inherited CODEX_*, XDG_*, DBUS or proxy variables) and only processes this
harness started (recorded pids and their descendants) are ever signalled.
"""

import json
import os
import signal
import socket
import ssl
import subprocess
import sys
import time
from pathlib import Path

E2E_DIR = Path(__file__).resolve().parent.parent
REPO = E2E_DIR.parent
LIB = E2E_DIR / "lib"
STATE_FILE = E2E_DIR / ".work" / "current.json"
# One Xvfb display for the run (override with SUPERBA_E2E_DISPLAY; never another
# session's display). :176 is the integration e2e display of the build plan.
DISPLAYS = [os.environ.get("SUPERBA_E2E_DISPLAY") or ":176"]
MOCKRELAY = REPO / "crates" / "superba-gui" / "tools" / "screenshots" / "mockrelay.py"
# Fake key known only to the local mock relay (account data scenario).
MOCK_KEY = "nb_e2e_mock_5b0c1d7e9f2a4c6e8a0b3d5f7e9c1a2b4d6f"
BINARIES = ("spb", "superba", "superba-router")
# Codex packages for the pin / remote-install scenarios (s21, s22): a cache
# outside the repository (override with SUPERBA_E2E_PKGCACHE), filled from
# GitHub once and checksum-verified (pkgcache.ensure_cached).
PKGCACHE = Path(os.environ.get("SUPERBA_E2E_PKGCACHE") or E2E_DIR / ".work" / "pkgcache")
CODEX_PKG_VERSION = "0.162.0"
CODEX_PKG_TARGET = "x86_64-unknown-linux-musl"
# Feed signing key of the repository (gitignored .secrets/, or
# SUPERBA_E2E_SIGNING_KEY): signs the newer feed the local mirror serves (s17).
# Never printed.
SIGNING_KEY = Path(os.environ.get("SUPERBA_E2E_SIGNING_KEY") or REPO / ".secrets" / "catalog-signing.key")
# The desktop-app build the local download fixture announces (s23).
FIXTURE_APP_VERSION = "26.1020.10000"
FIXTURE_APP_CODEX = "0.163.0"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:
        with open(f"/proc/{pid}/stat") as f:
            return f.read().split(") ", 1)[1].split()[0] != "Z"
    except OSError:
        return False


def children_map():
    out = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                ppid = int(f.read().split(") ", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
        out.setdefault(ppid, []).append(int(d))
    return out


def descendants(pid):
    m = children_map()
    out, stack = [], [pid]
    while stack:
        p = stack.pop()
        for c in m.get(p, []):
            out.append(c)
            stack.append(c)
    return out


def cmdline(pid):
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return f.read().replace(b"\0", b" ").decode(errors="replace").strip()
    except OSError:
        return ""


def environ(pid):
    try:
        with open(f"/proc/{pid}/environ", "rb") as f:
            raw = f.read()
    except OSError:
        return {}
    env = {}
    for part in raw.split(b"\0"):
        if b"=" in part:
            k, v = part.split(b"=", 1)
            env[k.decode(errors="replace")] = v.decode(errors="replace")
    return env


def start_time_ticks(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            return int(f.read().split(") ", 1)[1].split()[19])
    except (OSError, IndexError, ValueError):
        return None


def pids_with_home(home):
    """Processes of this user whose environment has HOME=home (ours by construction)."""
    me = os.getuid()
    out = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            if os.stat(f"/proc/{d}").st_uid != me:
                continue
        except OSError:
            continue
        if environ(int(d)).get("HOME") == home:
            out.append(int(d))
    return out


def kill_pids(pids, grace=5.0):
    pids = [p for p in dict.fromkeys(pids) if p and p != os.getpid() and pid_alive(p)]
    for p in pids:
        try:
            os.kill(p, signal.SIGTERM)
        except ProcessLookupError:
            pass
    end = time.time() + grace
    while time.time() < end and any(pid_alive(p) for p in pids):
        time.sleep(0.2)
    for p in pids:
        if pid_alive(p):
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
    return pids


class Run:
    """State of one e2e run, persisted to .work/current.json so steps can be
    re-entered while developing (`driver.py step ...`)."""

    def __init__(self, data):
        self.d = data

    # ---- persistence -------------------------------------------------------
    @classmethod
    def create(cls):
        run_id = time.strftime("%Y%m%d-%H%M%S")
        work = E2E_DIR / ".work" / run_id
        out = E2E_DIR / "out" / run_id
        work.mkdir(parents=True)
        out.mkdir(parents=True)
        n = 1
        while os.path.lexists(f"/tmp/sbe2e-{n}"):
            n += 1
        home_link = f"/tmp/sbe2e-{n}"
        (work / "home").mkdir()
        os.symlink(work / "home", home_link)
        r = cls(
            {
                "run_id": run_id,
                "work": str(work),
                "out": str(out),
                "home": home_link,
                "pids": {},
                "apps": [],
                "report": {},
            }
        )
        r.save()
        return r

    @classmethod
    def load(cls):
        return cls(json.loads(STATE_FILE.read_text()))

    def save(self):
        STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
        tmp = STATE_FILE.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.d, indent=1))
        tmp.replace(STATE_FILE)
        (Path(self.d["out"]) / "report.json").write_text(
            json.dumps(self.d["report"], indent=1, ensure_ascii=False)
        )

    # ---- private settings (environment only, never persisted) -------------
    @property
    def key(self):
        return os.environ["SUPERBA_E2E_KEY"]

    @property
    def upstream(self):
        return os.environ["SUPERBA_E2E_UPSTREAM"]

    # ---- paths -------------------------------------------------------------
    @property
    def work(self):
        return Path(self.d["work"])

    @property
    def out(self):
        return Path(self.d["out"])

    @property
    def home(self):
        return self.d["home"]

    @property
    def codex_home(self):
        return Path(self.home) / ".codex"

    @property
    def data_dir(self):
        return Path(self.home) / ".local" / "share" / "superba"

    @property
    def tls(self):
        return self.work / "tls"

    @property
    def bin_dir(self):
        """Run-local PATH entries: `codex` (the app's bundled CLI) and a fake
        `systemctl` that fails, so the router service never reaches a real
        systemd user manager (it falls back to XDG autostart + its supervisor)."""
        return self.work / "bin"

    @property
    def router_dir(self):
        return self.data_dir / "router"

    def feed_mirrors(self):
        """Base URLs of the local feed mirrors m1..m4 (feedmirror.py)."""
        return [f"https://127.0.0.1:{self.d['feed_ports'][m]}/feeds/" for m in ("m1", "m2", "m3", "m4")]

    def line_url(self, line):
        return f"https://127.0.0.1:{self.d['ports'][line]}"

    def fwd_log(self, line):
        return self.out / f"line-{line}.jsonl"

    # ---- environment -------------------------------------------------------
    def base_env(self, **extra):
        """Environment for every child: built from scratch, nothing inherited
        except PATH-like basics."""
        env = {
            "PATH": f"{self.bin_dir}:/usr/local/bin:/usr/bin:/bin",
            "HOME": self.home,
            "USER": os.environ.get("USER", "e2e"),
            "LOGNAME": os.environ.get("LOGNAME", os.environ.get("USER", "e2e")),
            "SHELL": "/bin/bash",
            "LANG": "C.UTF-8",
            "TERM": "dumb",
            "DISPLAY": self.d.get("display", ""),
            "XDG_RUNTIME_DIR": str(self.work / "xdg-runtime"),
            "SSL_CERT_FILE": str(self.tls / "bundle.pem"),
            "CODEX_CA_CERTIFICATE": str(self.tls / "bundle.pem"),
            "SUPERBA_SECRET_BACKEND": "file",
            "SUPERBA_APP_PATH": self.d["app_dir"],
            # --password-store=basic keeps Chromium away from the desktop keyring.
            "SUPERBA_APP_ARGS": "--no-sandbox --disable-gpu --password-store=basic",
            "SUPERBA_TEST_SITES": str(self.work / "sites.json"),
            "NO_COLOR": "1",
        }
        # Local stand-ins for GitHub releases and the desktop-app downloads, so
        # no scenario depends on (or downloads from) them; the feed mirrors only
        # once s17 turned them on (before that, Superba uses its built-in feed).
        if self.d.get("ui_locale"):
            # The UI language Superba writes the default stats line in (s20).
            env["SUPERBA_LOCALE"] = self.d["ui_locale"]
        if self.d.get("pkg_port"):
            env["SUPERBA_CODEX_PACKAGES"] = f"https://127.0.0.1:{self.d['pkg_port']}"
        if self.d.get("feed_ports"):
            env["SUPERBA_APP_DOWNLOADS"] = f"https://127.0.0.1:{self.d['feed_ports']['app']}"
            if self.d.get("feed_mirrors_on"):
                env["SUPERBA_FEED_MIRRORS"] = ",".join(self.feed_mirrors())
        env.update({k: str(v) for k, v in extra.items() if v is not None})
        return env

    # ---- commands ----------------------------------------------------------
    def run(self, argv, *, env=None, timeout=300, cwd=None, label=None, stdin=None):
        """Run a command, log it (argv, rc, duration, output) to cmds.log."""
        t0 = time.time()
        try:
            p = subprocess.run(
                argv,
                env=env or self.base_env(),
                cwd=cwd or self.home,
                input=stdin,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
            rc, so, se = p.returncode, p.stdout, p.stderr
        except subprocess.TimeoutExpired as e:
            rc = "timeout"
            so = e.stdout.decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
            se = e.stderr.decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
        dt = time.time() - t0
        with open(self.out / "cmds.log", "a", encoding="utf-8") as f:
            f.write(
                f"\n### [{time.strftime('%H:%M:%S')}] {label or ''} $ {' '.join(map(str, argv))}\n"
                f"### rc={rc} {dt:.2f}s\n{so}"
                + (f"--- stderr\n{se}" if se.strip() else "")
            )
        return {"rc": rc, "out": so, "err": se, "secs": round(dt, 2)}

    def spb(self, *args, timeout=300, label=None, extra_env=None, json_out=False):
        argv = [self.d["spb"], *(["--json"] if json_out else []), *args]
        r = self.run(argv, env=self.base_env(**(extra_env or {})), timeout=timeout, label=label)
        if json_out:
            r["json"] = parse_json_tail(r["out"])
        return r

    # ---- report ------------------------------------------------------------
    def record(self, sid, name, status, evidence, **details):
        self.d["report"][sid] = {
            "name": name,
            "status": status,
            "evidence": evidence,
            "details": details,
            "at": time.strftime("%H:%M:%S"),
        }
        self.save()

    def note(self, sid, msg):
        rep = self.d["report"].setdefault(sid, {"evidence": []})
        rep.setdefault("notes", []).append(msg)
        self.save()


def parse_json_tail(text):
    """Last JSON value printed (spb --json prints one document; progress goes to
    stderr)."""
    text = text.strip()
    if not text:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    # Fallback: last line that parses.
    for line in reversed(text.splitlines()):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return None


# ---- infrastructure pieces --------------------------------------------------

def make_ca(run):
    subprocess.run(["bash", str(LIB / "mkcert.sh"), str(run.tls)], check=True)


def fault_file(run, line):
    return run.work / f"fault-{line}"


def set_fault(run, line, status=None, path=None):
    """Make forwarder `line` answer `status` (to every request, or to paths
    containing `path`) instead of forwarding; None restores forwarding."""
    f = fault_file(run, line)
    if status is None:
        f.unlink(missing_ok=True)
    else:
        f.write_text(f"{status} {path or ''}".strip() + "\n")


def start_forwarder(run, line, port=None):
    port = port or run.d.get("ports", {}).get(line) or free_port()
    run.d.setdefault("ports", {})[line] = port
    log = open(run.out / f"line-{line}.stderr", "a")
    env = {"PATH": "/usr/bin:/bin", "HOME": run.home, "LANG": "C.UTF-8"}
    p = subprocess.Popen(
        [
            sys.executable,
            str(LIB / "linefwd.py"),
            line.upper(),
            str(port),
            run.upstream,
            str(run.fwd_log(line)),
            "--tls",
            str(run.tls / "server.pem"),
            str(run.tls / "server.key"),
            "--fault-file",
            str(fault_file(run, line)),
        ],
        env=env,
        stdout=log,
        stderr=log,
        start_new_session=True,
    )
    run.d["pids"][f"fwd-{line}"] = p.pid
    run.save()
    ctx = ssl.create_default_context(cafile=str(run.tls / "ca.pem"))
    end = time.time() + 15
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1) as s:
                with ctx.wrap_socket(s, server_hostname="127.0.0.1"):
                    return p.pid
        except OSError:
            time.sleep(0.2)
    raise RuntimeError(f"forwarder {line} did not start")


def stop_forwarder(run, line):
    pid = run.d["pids"].pop(f"fwd-{line}", None)
    if pid:
        kill_pids([pid])
    run.save()
    return pid


def install_bins(run, src_dir):
    """Copy spb / superba / superba-router out of the (shared) build directory, so
    a concurrent build cannot replace them mid-run; superba-router must sit next
    to spb (that is where Superba looks for it). Also creates the run's PATH
    entries (bin_dir)."""
    import shutil

    sbin = run.work / "sbin"
    sbin.mkdir(exist_ok=True)
    for name in BINARIES:
        src = Path(src_dir) / name
        if not src.exists():
            raise RuntimeError(f"{src} is missing (build -p superba-cli -p superba-gui -p superba-router)")
        shutil.copy2(src, sbin / name)
    # Maintainer tool, only for the `codex features list` cross-check (s11).
    gen = Path(src_dir) / "superba-catalog-gen"
    if gen.exists():
        shutil.copy2(gen, sbin / gen.name)
    run.bin_dir.mkdir(exist_ok=True)
    codex = run.bin_dir / "codex"
    if not codex.exists():
        codex.symlink_to(Path(run.d["app_dir"]) / "resources" / "codex")
    fake = run.bin_dir / "systemctl"
    fake.write_text("#!/bin/sh\n# e2e: no systemd user manager in the isolated run\nexit 1\n")
    fake.chmod(0o755)
    # Superba must never run privileged commands (app updates show the command
    # instead): these stand-ins only record that they were called (s23 asserts
    # the record stays empty).
    for name in ("sudo", "pkexec", "doas"):
        tool = run.bin_dir / name
        tool.write_text(f"#!/bin/sh\necho \"$0 $*\" >> {run.work / 'privileged-calls.log'}\nexit 1\n")
        tool.chmod(0o755)
    run.d["spb"], run.d["superba"] = str(sbin / "spb"), str(sbin / "superba")
    run.d["router_bin"] = str(sbin / "superba-router")
    run.save()


def start_mockrelay(run, lines=("m1", "m2")):
    """The screenshot mock relay (billing, organization usage and costs) on https
    loopback lines, with a per-request fault file (scenario 14)."""
    ports_file = run.work / "mock-ports.json"
    ports_file.unlink(missing_ok=True)
    fault = run.work / "mock-faults.json"
    fault.write_text("{}")
    p = subprocess.Popen(
        [sys.executable, "-I", str(MOCKRELAY), "--lines", ",".join(lines), "--ports-file", str(ports_file),
         "--api-key", MOCK_KEY, "--cert", str(run.tls / "server.pem"), "--key", str(run.tls / "server.key"),
         "--log", str(run.out / "mockrelay.jsonl"), "--fault-file", str(fault)],
        env={"PATH": "/usr/bin:/bin", "HOME": run.home, "LANG": "C.UTF-8"},
        stdout=subprocess.DEVNULL, stderr=open(run.out / "mockrelay.stderr", "a"), start_new_session=True)
    run.d["pids"]["mockrelay"] = p.pid
    end = time.time() + 15
    while time.time() < end:
        if ports_file.exists():
            run.d["mock_ports"] = json.loads(ports_file.read_text())
            run.save()
            return run.d["mock_ports"]
        time.sleep(0.1)
    raise RuntimeError("mock relay did not start")


def _start_server(run, name, argv, ports_file):
    """Start one of the run's local https servers (feedmirror / pkgcache) and
    wait for its ports file."""
    ports_file.unlink(missing_ok=True)
    p = subprocess.Popen([sys.executable, "-B", *argv, "--ports-file", str(ports_file),
                          "--cert", str(run.tls / "server.pem"), "--key", str(run.tls / "server.key")],
                         env={"PATH": "/usr/bin:/bin", "HOME": run.home, "LANG": "C.UTF-8"},
                         stdout=subprocess.DEVNULL, stderr=open(run.out / f"{name}.stderr", "a"),
                         start_new_session=True)
    run.d["pids"][name] = p.pid
    end = time.time() + 15
    while time.time() < end:
        if ports_file.exists():
            return json.loads(ports_file.read_text())
        if p.poll() is not None:
            break
        time.sleep(0.1)
    raise RuntimeError(f"{name} did not start (see {run.out / (name + '.stderr')})")


def start_feedmirror(run):
    """Four local feed mirrors with fault modes and the desktop-app download
    fixture (feedmirror.py). The feed trees are built by `build_feed_trees`."""
    root = run.work / "feedmirror"
    for d in ("valid", "stale", "app"):
        (root / d).mkdir(parents=True, exist_ok=True)
    control = root / "control.json"
    control.write_text("{}")
    ports = _start_server(run, "feedmirror", [str(LIB / "feedmirror.py"), "--valid", str(root / "valid"),
                                              "--stale", str(root / "stale"), "--app", str(root / "app"),
                                              "--log", str(run.out / "feedmirror.jsonl"), "--control", str(control)],
                          root / "ports.json")
    run.d["feed_ports"] = ports
    run.save()
    return ports


def set_feed_modes(run, **modes):
    """Fault modes of the feed mirrors (`m1="404"`, ...; see feedmirror.py)."""
    (run.work / "feedmirror" / "control.json").write_text(json.dumps(modes))


def build_feed_trees(run):
    """The trees the mirrors serve: `stale` = the repository's signed feeds as
    shipped (the built-in serial), `valid` = the same feeds with the fixture app
    build added, re-indexed (serial + 1) and signed with the repository's feed
    key; `app` = an APT index + fixture package for the desktop-app download
    check. Returns a dict with the serials and the fixture package digest."""
    import hashlib
    import shutil

    root = run.work / "feedmirror"
    src = REPO / "catalog" / "feeds"
    stale = root / "stale"
    shutil.rmtree(stale, ignore_errors=True)
    shutil.copytree(src, stale)
    # The fixture app package (not a real package: the e2e run only downloads and
    # verifies it, it never installs anything).
    deb_rel = f"linux/deb/pool/main/c/chatgpt/chatgpt_{FIXTURE_APP_VERSION}_amd64.deb"
    deb = root / "app" / deb_rel
    deb.parent.mkdir(parents=True, exist_ok=True)
    deb.write_bytes(b"!<arch>\n" + hashlib.sha256(b"superba-e2e-fixture").digest() * 2048)
    digest = hashlib.sha256(deb.read_bytes()).hexdigest()
    size = deb.stat().st_size
    base = f"https://127.0.0.1:{run.d['feed_ports']['app']}"
    idx = root / "app" / "linux/deb/dists/stable/main/binary-amd64/Packages"
    idx.parent.mkdir(parents=True, exist_ok=True)
    idx.write_text("\n".join([
        "Package: chatgpt", f"Version: {FIXTURE_APP_VERSION}", "Architecture: amd64",
        "Maintainer: e2e fixture <e2e@example.invalid>", f"Filename: pool/main/c/chatgpt/chatgpt_{FIXTURE_APP_VERSION}_amd64.deb",
        f"Size: {size}", f"SHA256: {digest}", "Description: fixture package of the Superba e2e run", "", ""]))
    # The newer feed: the fixture build (with the Codex it bundles) on top.
    tree = root / "gen" / "catalog"
    shutil.rmtree(tree.parent, ignore_errors=True)
    shutil.copytree(src, tree / "feeds")
    apps_file = tree / "feeds" / "app-versions.json"
    apps = json.loads(apps_file.read_text())
    apps["releases"].insert(0, {
        "version": FIXTURE_APP_VERSION, "date": int(time.time() * 1000), "bundled_codex": FIXTURE_APP_CODEX,
        "packages": [{"os": "linux", "kind": "deb", "arch": "amd64", "url": f"{base}/{deb_rel}", "sha256": digest, "size": size}],
    })
    apps_file.write_text(json.dumps(apps, indent=1) + "\n")
    gen = str(run.work / "sbin" / "superba-catalog-gen")
    if not SIGNING_KEY.exists():
        raise RuntimeError(f"no feed signing key at {SIGNING_KEY}")
    logs = []
    for args in (["feeds", "index"], ["feeds", "sign", "--key", str(SIGNING_KEY)], ["feeds", "verify", "--require-signature"]):
        r = subprocess.run([gen, "--catalog", str(tree), *args], capture_output=True, text=True, timeout=120)
        logs.append(f"$ superba-catalog-gen {args[0]} {args[1]} -> rc={r.returncode}\n{r.stdout}{r.stderr}")
        if r.returncode != 0:
            raise RuntimeError("feeds " + args[1] + " failed: " + (r.stderr or r.stdout)[-400:])
    (run.out / "feedmirror-build.log").write_text("\n".join(logs))
    valid = root / "valid"
    shutil.rmtree(valid, ignore_errors=True)
    shutil.copytree(tree / "feeds", valid)
    serial = lambda d: json.loads((d / "index.json").read_text())["serial"]  # noqa: E731
    info = {"stale_serial": serial(stale), "valid_serial": serial(valid), "app_sha256": digest, "app_size": size}
    run.d["feeds"] = info
    run.save()
    return info


def start_pkgcache(run):
    """Local stand-in for GitHub's Codex release downloads (pkgcache.py) with the
    package of CODEX_PKG_VERSION cached (downloaded once if missing)."""
    import pkgcache

    pkgcache.ensure_cached(PKGCACHE, CODEX_PKG_VERSION, CODEX_PKG_TARGET)
    control = run.work / "pkgcache-control.json"
    control.write_text(json.dumps({"mode": "normal"}))
    ports = _start_server(run, "pkgcache", [str(LIB / "pkgcache.py"), "--cache", str(PKGCACHE),
                                            "--log", str(run.out / "pkgcache.jsonl"), "--control", str(control)],
                          run.work / "pkgcache-ports.json")
    run.d["pkg_port"] = ports["port"]
    run.save()
    return ports["port"]


def set_pkg_mode(run, mode):
    (run.work / "pkgcache-control.json").write_text(json.dumps({"mode": mode}))


def set_mock_faults(run, faults):
    (run.work / "mock-faults.json").write_text(json.dumps(faults))


def start_xvfb(run):
    for disp in DISPLAYS:
        num = disp[1:]
        if os.path.exists(f"/tmp/.X11-unix/X{num}") or os.path.exists(f"/tmp/.X{num}-lock"):
            continue
        p = subprocess.Popen(
            ["Xvfb", disp, "-screen", "0", "1440x960x24", "-nolisten", "tcp"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        for _ in range(50):
            if os.path.exists(f"/tmp/.X11-unix/X{num}"):
                run.d["display"] = disp
                run.d["pids"]["xvfb"] = p.pid
                run.save()
                return disp
            time.sleep(0.1)
        kill_pids([p.pid])
    raise RuntimeError(f"display {DISPLAYS[0]} is in use (set SUPERBA_E2E_DISPLAY to a free one of :171-:179)")


def stop_xvfb(run):
    pid = run.d["pids"].pop("xvfb", None)
    if pid:
        kill_pids([pid])
        num = run.d["display"][1:]
        for f in (f"/tmp/.X11-unix/X{num}", f"/tmp/.X{num}-lock"):
            try:
                os.remove(f)
            except OSError:
                pass
    run.save()


def write_sites(run):
    """One Krill-kind test site whose lines are the local https forwarders.

    The test relay closes unauthenticated /v1/models, so the latency probe uses
    /backend-api/codex/models (401 without a key)."""
    def line(lid):
        return {
            "id": lid,
            "name": {"key": "", "fallback": f"E2E line {lid.upper()}"},
            "origin": run.line_url(lid),
            "source": "test",
        }

    site = {
        "id": "krill-e2e",
        "kind": "krill",
        "name": {"key": "", "fallback": "Krill e2e"},
        "homepage": "https://www.krill-code.com/",
        "lines": [line(x) for x in sorted(run.d["ports"]) if x in ("a", "b", "c")],
        "capabilities": {
            "auth_modes": ["pat", "apikey"],
            "codex_paths": ["backend_api_codex", "codex_v1"],
            "latency_probe": True,
            "key_check": True,
            "account_summary": False,
            "model_catalog": True,
        },
        "endpoints": {
            "models_unauth": "/backend-api/codex/models",
            "whoami": "/v1/user-auth-credential/whoami",
            "accounts_check": "/backend-api/wham/accounts/check",
            "backend_codex_models": "/backend-api/codex/models",
            "codex_v1_models": "/codex/v1/models",
        },
        "default_line": "a",
    }
    sites = [site]
    if "e" in run.d["ports"]:
        # A second Krill-kind test site whose relay maps tier-suffixed model names
        # (`<slug>-fast`, `<slug>-ultrafast`) to speed tiers: the desktop app gets
        # Fast / Ultrafast in API-key mode through the model name (s19).
        sfx = json.loads(json.dumps(site))
        sfx.update({"id": "krill-sfx", "name": {"key": "", "fallback": "Krill e2e (tier names)"},
                    "lines": [line("e")], "default_line": "e"})
        sfx["capabilities"]["tier_model_suffix"] = True
        sites.append(sfx)
    if run.d.get("mock_ports"):
        # Krill-kind site served by the local mock relay: billing + organization
        # usage/costs endpoints for the account data scenario.
        sites.append({
            "id": "krill-mock",
            "kind": "krill",
            "name": {"key": "", "fallback": "Krill mock"},
            "homepage": "https://www.krill-code.com/",
            "lines": [{"id": lid, "name": {"key": "", "fallback": f"Mock {lid.upper()}"},
                       "origin": f"https://127.0.0.1:{port}", "source": "test"}
                      for lid, port in sorted(run.d["mock_ports"].items())],
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
            "default_line": sorted(run.d["mock_ports"])[0],
        })
    (run.work / "sites.json").write_text(json.dumps(sites, indent=1))


def fwd_entries(run, line, since=0.0, kind="http"):
    path = run.fwd_log(line)
    if not path.exists():
        return []
    out = []
    for raw in path.read_text().splitlines():
        try:
            e = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if e.get("ts", 0) >= since and (kind is None or e.get("kind") == kind):
            out.append(e)
    return out


def responses_posts(run, line, since=0.0):
    return [
        e
        for e in fwd_entries(run, line, since)
        if e.get("method") == "POST" and e.get("path", "").split("?")[0].endswith("/responses")
    ]


def wait_for(what, fn, timeout=60, interval=0.5):
    end = time.time() + timeout
    last = None
    while time.time() < end:
        try:
            last = fn()
        except Exception as e:  # keep polling through transient errors
            last = e
        if last and not isinstance(last, Exception):
            return last
        time.sleep(interval)
    raise TimeoutError(f"timed out: {what} (last: {last!r})"[:500])


# ---- router -------------------------------------------------------------------

def router_runtime(run):
    """runtime.json of the running router (pid, port, admin token), or None."""
    try:
        return json.loads((run.router_dir / "runtime.json").read_text())
    except (OSError, json.JSONDecodeError):
        return None


def router_admin(run, path="status", timeout=5):
    """GET /_superba/v1/<path> of the running router (admin token from its 0600
    runtime.json; never printed)."""
    import urllib.request

    rt = router_runtime(run)
    if not rt:
        return None
    req = urllib.request.Request(f"http://127.0.0.1:{rt['port']}/_superba/v1/{path}",
                                 headers={"Authorization": f"Bearer {rt['admin_token']}"})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=timeout) as r:
            return json.loads(r.read())
    except Exception:
        return None


def router_request(run, method, path, token=None, body=None, headers=None, timeout=60):
    """A request to the router as Codex would send it; returns (status,
    x-superba-route, body prefix). The token is the router token from auth.json
    (never printed)."""
    import urllib.request

    rt = router_runtime(run)
    url = f"http://127.0.0.1:{rt['port']}{path}"
    hdr = dict(headers or {})
    if token:
        hdr["Authorization"] = f"Bearer {token}"
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        hdr["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=hdr)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(req, timeout=timeout) as r:
            return r.status, r.headers.get("x-superba-route"), r.read(400).decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("x-superba-route"), e.read(400).decode(errors="replace")
