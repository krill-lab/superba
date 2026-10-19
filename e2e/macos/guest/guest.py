#!/usr/bin/env python3
"""One JSON request over SSH stdin. All test state is owned by a run marker.

Runs with the CLT Python (3.9+); host code may use Python 3.11. Never log requests.
"""

import base64
import hashlib
import json
import os
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import time
import uuid
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent
RUNS = ROOT / "runs"
APP = Path("/Applications/Superba.app")
LABEL = "com.krill.superba.router"
GUI_LABEL = "com.krill-code.superba.autostart"
ENV_NAMES = ("CODEX_AUTHAPI_BASE_URL", "CODEX_CLI_PATH")


class Blocked(Exception):
    pass


def command(args, **kwargs):
    return subprocess.run([str(arg) for arg in args], capture_output=True, text=True,
                          timeout=kwargs.pop("timeout", 120), **kwargs)


def checked(args, **kwargs):
    result = command(args, **kwargs)
    if result.returncode:
        raise RuntimeError("Guest command failed: " + Path(str(args[0])).name)
    return result.stdout


def processes():
    rows = []
    for line in checked(["/bin/ps", "-axo", "pid=,ppid=,command="]).splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) == 3:
            rows.append({"pid": int(fields[0]), "ppid": int(fields[1]), "command": fields[2]})
    return rows


def stamp(pid):
    return command(["/bin/ps", "-p", str(pid), "-o", "lstart="]).stdout.strip()


def state(root):
    return json.loads((root / "state.json").read_text())


def save(root, value):
    target = root / "state.json"
    tmp = root / "state.new"
    tmp.write_text(json.dumps(value))
    tmp.replace(target)


def session(args):
    ident = args["session"]
    if not re.fullmatch(r"[a-f0-9]{32}", ident):
        raise ValueError("Invalid session identifier")
    root = RUNS / ident
    if root.is_symlink() or not root.is_dir() or (root / ".owner").read_text() != ident:
        raise ValueError("Session ownership marker missing")
    return root


def inside(root, relative):
    path = root / relative
    if not path.resolve().is_relative_to(root.resolve()):
        raise ValueError("Path escapes the test session")
    return path


def environment(root, extra=None):
    home = str(root / "home")
    result = {
        "HOME": home, "CFFIXED_USER_HOME": home, "ZDOTDIR": home,
        "CODEX_HOME": home + "/.codex", "CLAUDE_CONFIG_DIR": home + "/.claude",
        "SUPERBA_DATA_DIR": home + "/Library/Application Support/superba",
        "SUPERBA_SECRET_BACKEND": "file", "SUPERBA_LOCALE": "en",
        "XDG_CONFIG_HOME": home + "/.config", "XDG_DATA_HOME": home + "/.local/share",
        "XDG_CACHE_HOME": home + "/.cache", "TMPDIR": home + "/tmp",
        "PATH": str(root / "bin") + ":/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "LANG": "en_US.UTF-8", "SHELL": "/bin/zsh",
        "DISABLE_AUTOUPDATER": "1", "npm_config_prefix": home + "/npm",
        "npm_config_cache": home + "/.npm", "SUPERBA_APP_ARGS": "--user-data-dir=" + home + "/desktop",
    }
    for overlay in (state(root).get("env", {}), extra or {}):
        for name, value in overlay.items():
            if value is None:
                result.pop(name, None)
            else:
                result[name] = value
    return result


def preflight():
    if APP.exists() or APP.is_symlink():
        raise Blocked("/Applications/Superba.app already exists; its owner must remove it before testing")
    real_home = Path.home()
    existing = list((real_home / "Library/LaunchAgents").glob("*superba*"))
    if existing:
        raise Blocked("Existing Superba login items must be removed by their owner")
    for label in (LABEL, GUI_LABEL):
        target = "gui/{}/{}".format(os.getuid(), label)
        if command(["/bin/launchctl", "print", target]).returncode == 0:
            raise Blocked("A Superba service is already loaded")
    for row in processes():
        executable = row["command"].split(" ", 1)[0]
        if Path(executable).name.lower() in ("superba", "superba-router", "chatgpt", "codex"):
            raise Blocked("An existing Superba/Codex/ChatGPT process must be stopped by its owner")
    return {"arch": checked(["/usr/bin/uname", "-m"]).strip(),
            "free_bytes": shutil.disk_usage(ROOT).free,
            "console": checked(["/usr/bin/stat", "-f", "%Su", "/dev/console"]).strip(),
            "boot": checked(["/usr/sbin/sysctl", "-n", "kern.boottime"]).strip()}


def create():
    info = preflight()
    if info["console"] in ("root", "loginwindow"):
        raise Blocked("An interactive user must log in first")
    ident = uuid.uuid4().hex
    root = RUNS / ident
    root.mkdir(parents=True, mode=0o700)
    (root / ".owner").write_text(ident)
    for relative in ("home/.codex", "home/.claude", "home/tmp", "home/Library/LaunchAgents", "bin", "logs"):
        (root / relative).mkdir(parents=True, exist_ok=True)
    initial_env = {name: command(["/bin/launchctl", "getenv", name]).stdout.rstrip("\n") for name in ENV_NAMES}
    save(root, {"pids": [], "installed": False, "env": {}, "launch_env": initial_env,
                "login_plists": [], "mounts": [], "router": False})
    return {"session": ident, "root": str(root), "preflight": info}


def install(root, dmg, upgrade=False):
    dmg = Path(dmg)
    if not dmg.is_file() or not dmg.resolve().is_relative_to(root):
        raise ValueError("DMG must be uploaded inside the session")
    st = state(root)
    if APP.exists() and not (upgrade and st["installed"]):
        raise Blocked("Refusing to replace an app not installed by this session")
    mount = root / "mount"
    mount.mkdir(exist_ok=True)
    st["mounts"].append(str(mount))
    save(root, st)
    checked(["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mount, dmg])
    try:
        source = mount / "Superba.app"
        if not (source / "Contents/MacOS/superba").is_file() or not (mount / "spb").is_file():
            raise RuntimeError("DMG lacks Superba.app or spb")
        # Claim the destination before copying so partial installs are cleaned, too.
        st["installed"] = True
        save(root, st)
        if upgrade and APP.exists():
            shutil.rmtree(APP)
        checked(["/usr/bin/ditto", source, APP])
        shutil.copy2(mount / "spb", root / "bin/spb")
        helper = APP / "Contents/MacOS/superba-router"
        if helper.exists():
            shutil.copy2(helper, root / "bin/superba-router")
        arch = checked(["/usr/bin/lipo", "-archs", APP / "Contents/MacOS/superba"]).strip()
        if "arm64" not in arch.split():
            raise Blocked("DMG does not contain arm64 (CI filenames are not architecture evidence)")
        info = plistlib.loads((APP / "Contents/Info.plist").read_bytes())
        return {"architecture": arch, "version": info.get("CFBundleShortVersionString"),
                "dmg_sha256": hashlib.sha256(dmg.read_bytes()).hexdigest(),
                "spb": checked([root / "bin/spb", "--version"], env=environment(root)).strip()}
    finally:
        checked(["/usr/bin/hdiutil", "detach", mount])
        st = state(root)
        st["mounts"].remove(str(mount))
        save(root, st)


def start(root, executable, args, extra=None):
    # Every process gets an isolated, explicit environment, including GUI processes.
    with (root / "logs/process.log").open("ab") as log:
        process = subprocess.Popen([str(executable), *args], env=environment(root, extra),
                                   cwd=root / "home", stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=log, start_new_session=True)
    st = state(root)
    st["pids"].append({"pid": process.pid, "stamp": stamp(process.pid)})
    save(root, st)
    return {"pid": process.pid}


def spb(root, args, stdin=None, extra=None, gui=False):
    if args[:2] in (["router", "install"], ["router", "enable"]):
        st = state(root)
        st["router"] = True
        save(root, st)
    binary = APP / "Contents/MacOS/superba" if gui else root / "bin/spb"
    result = command([binary, *args], input=stdin, env=environment(root, extra),
                     cwd=root / "home", timeout=180)
    return {"code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}


def stop_owned(root):
    st = state(root)
    rows = processes()
    owned = {item["pid"] for item in st["pids"] if stamp(item["pid"]) == item["stamp"] and item["stamp"]}
    # Desktop helpers may be children of launchd. Only processes whose argv contains
    # this session's directory are additionally eligible (never a global pkill).
    owned.update(row["pid"] for row in rows if str(root) + "/" in row["command"])
    for _ in range(10):
        children = {row["pid"] for row in rows if row["ppid"] in owned}
        if children <= owned:
            break
        owned.update(children)
    owned.discard(os.getpid())
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for pid in owned:
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass
        time.sleep(0.5)
        alive = {row["pid"] for row in processes()}
        owned &= alive
    st["pids"] = []
    save(root, st)
    return {"remaining": sorted(owned)}


def login_item(root, source):
    st = state(root)
    source = inside(root, source)
    data = plistlib.loads(source.read_bytes())
    # A reboot only loads plists in the real login user's Library/LaunchAgents.
    # Preserve the product's plist but give the child an explicitly temporary HOME.
    data.setdefault("EnvironmentVariables", {}).update(environment(root))
    destination = Path.home() / "Library/LaunchAgents" / source.name
    if destination.exists() or destination.is_symlink():
        raise Blocked("Login item destination already exists")
    if not str(data.get("Label", "")).startswith("com.krill"):
        raise ValueError("Unexpected login item label")
    if str(root) not in json.dumps(data):
        raise ValueError("Login item is not isolated")
    st["login_plists"].append(str(destination))
    save(root, st)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(plistlib.dumps(data))
    destination.chmod(0o600)
    return {"label": data["Label"], "path": str(destination), "environment_isolated": True}


def cleanup(root):
    errors = []
    st = state(root)
    target = "gui/{}/{}".format(os.getuid(), LABEL)
    if st["router"]:
        loaded = command(["/bin/launchctl", "print", target])
        if loaded.returncode == 0 and str(root) not in loaded.stdout:
            raise RuntimeError("Router ownership changed; refusing to unload someone else's service")
        if (root / "bin/spb").is_file():
            result = spb(root, ["router", "uninstall"])
            if result["code"]:
                errors.append("product router uninstall failed")
        command(["/bin/launchctl", "bootout", target])
        if command(["/bin/launchctl", "print", target]).returncode == 0:
            errors.append("router still loaded")
    for destination in st["login_plists"]:
        path = Path(destination)
        if path.exists():
            data = plistlib.loads(path.read_bytes())
            if str(root) not in json.dumps(data):
                errors.append("login plist ownership changed")
            else:
                label = data["Label"]
                target = "gui/{}/{}".format(os.getuid(), label)
                loaded = command(["/bin/launchctl", "print", target])
                if loaded.returncode == 0 and str(root) not in loaded.stdout:
                    errors.append("login service ownership changed")
                    continue
                command(["/bin/launchctl", "bootout", target])
                if command(["/bin/launchctl", "print", target]).returncode == 0:
                    errors.append("login service still loaded")
                path.unlink()
    remaining = stop_owned(root)["remaining"]
    if remaining:
        errors.append("test processes survived termination")
    for name, value in st["launch_env"].items():
        args = ["/bin/launchctl", "setenv", name, value] if value else ["/bin/launchctl", "unsetenv", name]
        if command(args).returncode:
            errors.append("could not restore login environment")
    for mount in st["mounts"]:
        if command(["/usr/bin/hdiutil", "detach", mount]).returncode:
            errors.append("could not detach test DMG")
    if st["installed"] and APP.exists():
        shutil.rmtree(APP)
    if errors:
        # Keep the ownership marker for recovery; don't hide a cleanup failure.
        return {"ok": False, "errors": errors, "root": str(root)}
    shutil.rmtree(root)
    return {"ok": not root.exists() and not APP.exists(), "errors": []}


def dispatch(op, args):
    if op == "preflight":
        return preflight()
    if op == "create":
        return create()
    if op == "disk":
        return {"free_bytes": shutil.disk_usage(ROOT).free}
    root = session(args)
    if op == "install":
        return install(root, args["dmg"], args.get("upgrade", False))
    if op == "start":
        return start(root, APP / "Contents/MacOS/superba", args.get("argv", ["--lang", "en"]))
    if op == "spb":
        return spb(root, args["argv"], args.get("stdin"), args.get("env"), args.get("gui", False))
    if op == "stop":
        return stop_owned(root)
    if op == "read":
        path = inside(root, args["path"])
        return path.read_text() if path.is_file() else None
    if op == "write":
        path = inside(root, args["path"])
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(args["text"])
        path.chmod(0o600)
        return True
    if op == "list":
        base = inside(root, args.get("path", "home"))
        return [str(path.relative_to(root)) for path in base.rglob("*") if path.is_file()]
    if op == "env":
        st = state(root)
        st["env"].update(args["values"])
        save(root, st)
        return True
    if op == "desktop":
        installed = [path for path in (Path("/Applications/Codex.app"), Path("/Applications/ChatGPT.app"))
                     if path.is_dir()]
        if not installed:
            raise Blocked("ChatGPT/Codex desktop app is not installed; no app will be downloaded")
        source = installed[0]
        info = plistlib.loads((source / "Contents/Info.plist").read_bytes())
        executable = source / "Contents/MacOS" / info["CFBundleExecutable"]
        st = state(root)
        st["env"]["SUPERBA_APP_PATH"] = str(source)
        save(root, st)
        result = start(root, executable, ["--user-data-dir=" + str(root / "home/desktop")])
        result["app"] = str(source)
        return result
    if op == "desktop_installed":
        return any(Path(path).is_dir() for path in ("/Applications/Codex.app", "/Applications/ChatGPT.app"))
    if op == "fixture":
        start(root, "/usr/bin/python3", [str(ROOT / "fixture.py"), str(root)])
        for _ in range(50):
            if (root / "fixture-port").is_file():
                return {"base": "http://127.0.0.1:" + (root / "fixture-port").read_text()}
            time.sleep(0.1)
        raise RuntimeError("Fixture did not start")
    if op == "router_request":
        data = Path(environment(root)["SUPERBA_DATA_DIR"])
        config = json.loads((data / "router/config.json").read_text())
        token = json.loads((data / "router/secrets.json").read_text())["router_token"]
        url = "http://127.0.0.1:{}/v1/responses".format(config["listen_port"])
        payload = json.dumps({"model": "e2e-model", "input": "Reply MACOS_OK", "stream": False}).encode()
        request = urllib.request.Request(url, payload, {"Authorization": "Bearer " + token,
                                                      "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=30) as response:
            result = json.load(response)
        return {"id": result.get("id"), "status": result.get("status"), "port": config["listen_port"]}
    if op == "port_closed":
        import socket
        try:
            with socket.create_connection(("127.0.0.1", int(args["port"])), timeout=1):
                return False
        except OSError:
            return True
    if op == "procs":
        # Only test-marked argv is returned; no other user's environment is read.
        return [row for row in processes() if str(root) + "/" in row["command"]]
    if op == "login_item":
        return login_item(root, args["path"])
    if op == "service":
        label = args.get("label", LABEL)
        if label not in (LABEL, GUI_LABEL):
            raise ValueError("Unsupported service label")
        result = command(["/bin/launchctl", "print", "gui/{}/{}".format(os.getuid(), label)])
        # A launchd-started GUI has no run directory in argv; adopt only a job
        # whose printed environment/arguments contain this session's directory.
        pid = re.search(r"^\s*pid = (\d+)\s*$", result.stdout, re.M)
        if result.returncode == 0 and str(root) in result.stdout and pid:
            st = state(root)
            st["pids"].append({"pid": int(pid[1]), "stamp": stamp(pid[1])})
            save(root, st)
        return {"code": result.returncode, "stdout": result.stdout, "boot":
                checked(["/usr/sbin/sysctl", "-n", "kern.boottime"]).strip()}
    if op == "cleanup":
        return cleanup(root)
    if op == "hashes":
        base = inside(root, args["path"])
        return {str(path.relative_to(base)): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in base.rglob("*") if path.is_file() and not path.is_symlink()}
    if op == "read_bytes":
        return base64.b64encode(inside(root, args["path"]).read_bytes()).decode()
    raise ValueError("Unknown guest operation")


if __name__ == "__main__":
    os.umask(0o077)
    try:
        request = json.load(sys.stdin)
        result = dispatch(request["op"], request.get("args", {}))
        print(json.dumps({"ok": True, "result": result}))
    except Blocked as error:
        print(json.dumps({"ok": False, "code": "BLOCKED", "error": str(error)}))
        sys.exit(2)
    except Exception as error:
        # Never echo request data, subprocess input, key material or file contents.
        print(json.dumps({"ok": False, "code": "GUEST_ERROR", "error": type(error).__name__}))
        sys.exit(1)
