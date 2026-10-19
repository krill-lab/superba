"""One harness run: VM session lifecycle, evidence paths, guest helpers.

A run owns e2e/out/windows-<timestamp>/ (report.json, report.md, run.log,
evidence/<scenario>/..., net/ with the relay / forwarder logs, transcripts/).
"""

import json
import os
import platform
import re
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path

from agent import Agent, AgentError, GUEST_ROOT, list2cmdline
from net import Net
from strings import Strings
from vm import REPO, Vm, VmError, psq

GUEST_SHOTS = GUEST_ROOT + r"\shots"
GUEST_DL = r"C:\Users\Public\Downloads"
BASE_SNAPSHOT = "wintest"
FINAL_SNAPSHOT = "clean"
UNINSTALL_ALL_PS = r"""
$installer = New-Object -ComObject WindowsInstaller.Installer
$products = $installer.GetType().InvokeMember('ProductsEx', 'GetProperty', $null, $installer, @('', '', 7))
foreach ($prod in $products) {
    $code = $prod.GetType().InvokeMember('ProductCode', 'GetProperty', $null, $prod, $null)
    $name = $prod.GetType().InvokeMember('InstallProperty', 'GetProperty', $null, $prod, @('ProductName'))
    if ($name -notlike 'Superba*') { continue }
    $p = Start-Process msiexec.exe -ArgumentList "/x $code /qn /norestart" -Wait -PassThru
    "uninstalled $name $code -> exit $($p.ExitCode)"
}
"""


# Variables the harness itself puts in the user environment; leftover checks ignore them.
HARNESS_ENV = {"SUPERBA_TEST_LINES", "SUPERBA_TEST_SITES"}
# --base: the machine every scenario starts from. `wintest` = UAC on (a retail Windows);
# `uacoff` = the `clean` image (UAC off: every process of the admin is elevated) with its old
# Superba MSI removed after each restore; `wintest-wsl1` = wintest with WSL 1 and the
# distribution `Ubuntu-WSL1` (what the WSL scenarios run on).
BASES = {"wintest": (BASE_SNAPSHOT, False), "uacoff": ("clean", True), "wintest-wsl1": ("wintest-wsl1", False)}


WORK = REPO / "e2e" / ".work" / "windows"


def session_file(instance):
    return WORK / f"session-vm{instance or 1}.json"


class Run:
    def __init__(self, opts, root=None):
        self.opts = opts
        ts = datetime.now().strftime("%Y%m%d-%H%M%S")
        self.root = Path(root) if root else (REPO / "e2e" / "out" / f"windows-{ts}").resolve()
        (self.root / "evidence").mkdir(parents=True, exist_ok=True)
        (self.root / "transcripts").mkdir(parents=True, exist_ok=True)
        self._log = open(self.root / "run.log", "a", buffering=1)
        self.vm = Vm(opts.vm or "1", log=self.log)
        self.just_acquired = None  # snapshot the pool just restored and booted
        self.agent = None
        self.net = None
        self.strings = {"en": Strings("en"), "zh-CN": Strings("zh-CN")}
        self.harness_env = {}
        self.state = {}
        self.session_snapshot = None
        self.dirty = True
        self.msi = Path(opts.msi).resolve() if opts.msi else None
        self.old_msi = Path(opts.old_msi).resolve() if getattr(opts, "old_msi", None) else None
        self.net_mode = opts.net
        self.base_snapshot, self.base_uninstall = BASES[getattr(opts, "base", None) or "wintest"]
        self.started = datetime.now(timezone.utc).isoformat(timespec="seconds")
        self.guest = {}
        self._tseq = 0

    # ------------------------------------------------------------- logging
    def log(self, msg):
        line = f"{datetime.now().strftime('%H:%M:%S')} {msg}"
        self._log.write(line + "\n")
        if not getattr(self.opts, "quiet", False):
            print(line, flush=True)

    def meta(self):
        return {
            "started": self.started,
            "vm": f"{self.vm.id} ({self.vm.name})",
            "net": self.net_mode,
            "msi": str(self.msi) if self.msi else None,
            "msi_sha256": self.state.get("msi_sha256"),
            "superba_version": self.state.get("version"),
            "chatgpt_package": self.state.get("chatgpt_package"),
            "base_snapshot": f"{self.base_snapshot} (created {self.vm.snapshot_created(self.base_snapshot)})"
                             + (" + old Superba removed (UAC off)" if self.base_uninstall else ""),
            "host": platform.node(),
            "out": str(self.root),
        }

    # ------------------------------------------------------------- sessions
    def fresh(self, snapshot=None, reuse=False):
        """Restore `snapshot`, boot, start the desktop agent and the network
        (reuse=True: keep the VM as it is, for debugging sessions)."""
        snapshot = snapshot or self.base_snapshot
        self.stop_session()
        if reuse:
            self.log(f"== reusing VM {self.vm.id} as it is")
            self.vm.wait_ssh(120)
            self.vm.wait_desktop(120)
        elif self.just_acquired == snapshot:
            self.log(f"== VM {self.vm.id} was just restored to {snapshot} by pool.sh acquire")
        else:
            self.log(f"== restoring snapshot {snapshot} on VM {self.vm.id}")
            self.vm.restore(snapshot)
            self.vm.start()
        self.just_acquired = None
        self.session_snapshot = snapshot
        self.state = {k: v for k, v in self.state.items() if k in ("msi_sha256", "version")}
        self.harness_env = {}
        self.start_agent()
        if self.net_mode != "none":
            self.net = Net(self, self.net_mode)
            self.net.start()
            self.net.prepare_guest()
        self.agent.call("env.broadcast")
        self.load_guest_paths()
        if self.base_uninstall and snapshot == self.base_snapshot and not reuse:
            self.log("base uacoff: removing the image's old Superba MSI")
            self.log(self.uninstall_all_superba().strip())
        self.state["chatgpt_package"] = self.chatgpt_package()
        self.dirty = False

    def uninstall_all_superba(self):
        return self.guest_ps(UNINSTALL_ALL_PS, timeout=600)

    def start_agent(self, user=None, run_level="Limited"):
        if self.agent:
            self.agent.stop()
        self.agent = Agent(self.vm, log=self.log, user=user, run_level=run_level)
        self.agent.start()

    def reconnect(self):
        """After a guest reboot: new desktop agent, new reverse tunnel (host relay kept)."""
        self.start_agent()
        if self.net:
            self.net.retunnel()
        self.agent.call("env.broadcast")

    def stop_session(self):
        if self.agent:
            try:
                self.agent.fetch_log(self.root / f"agent-{int(time.time())}.log")
            except Exception:
                pass
            self.agent.stop(remove=False)
            self.agent = None
        if self.net:
            self.net.stop()
            self.net = None

    def teardown(self, final=True):
        """Stop the session; give the VM back restored to `clean` (dispatch rule)."""
        self.stop_session()
        if not final or getattr(self.opts, "keep_vm", False):
            return
        try:
            if self.vm.pool_held:
                self.log(f"== pool.sh release {self.vm.id} (restores {FINAL_SNAPSHOT}, leaves it stopped)")
                self.vm.release()
            elif getattr(self.opts, "held", False):
                self.log(f"== restoring {FINAL_SNAPSHOT} on VM {self.vm.id} (the lock stays with its holder)")
                self.vm.restore(FINAL_SNAPSHOT)
            else:
                # Never acquired (e.g. interrupted while queueing for the pool): the VM id is only
                # the default, and another run may hold that VM. Leave it alone.
                self.log(f"== no VM was acquired by this run: nothing to give back (VM {self.vm.id} not touched)")
        except VmError as e:
            self.log(f"giving the VM back failed: {e}")

    def acquire(self, owner=None, ttl="2h", snapshot=None):
        """Take a VM from the pool (unless --held), restored to the base snapshot."""
        if getattr(self.opts, "held", False):
            self.log(f"using VM {self.vm.id}, held by the caller (--held)")
            return
        snapshot = snapshot or self.base_snapshot
        vm_id = self.vm.acquire(snapshot, vm_id=self.opts.vm or None, owner=owner, ttl=ttl)
        self.just_acquired = snapshot
        self.log(f"pool: VM {vm_id} ({self.vm.name}) locked for this run")

    # ------------------------------------------------------------- attachable sessions (run.py session ...)
    def save_session(self):
        WORK.mkdir(parents=True, exist_ok=True)
        f = session_file(self.vm.id)
        data = {"root": str(self.root), "snapshot": self.session_snapshot, "net_mode": self.net_mode,
                "agent": self.agent.to_state() if self.agent else None,
                "net": self.net.to_state() if self.net else None,
                "guest": self.guest, "harness_env": self.harness_env, "state": self.state,
                "msi": str(self.msi) if self.msi else None, "pool_held": self.vm.pool_held}
        f.write_text(json.dumps(data, indent=1))
        os.chmod(f, 0o600)

    @classmethod
    def attach(cls, opts):
        f = session_file(opts.vm or "1")
        if not f.exists():
            raise SystemExit(f"no open session for VM {opts.vm or '1'} (run.py --vm ID session up)")
        d = json.loads(f.read_text())
        opts.net = d["net_mode"]
        if not getattr(opts, "msi", None):
            opts.msi = d.get("msi")
        run = cls(opts, root=d["root"])
        run.session_snapshot = d["snapshot"]
        run.guest, run.harness_env, run.state = d["guest"], d["harness_env"], d["state"]
        if d["agent"]:
            run.agent = Agent.from_state(run.vm, d["agent"], log=run.log)
        if d["net"]:
            run.net = Net.from_state(run, d["net"])
        run.vm.pool_held = d.get("pool_held", False)
        run.dirty = False
        return run

    def close_session_file(self):
        session_file(self.vm.id).unlink(missing_ok=True)

    def ensure_session(self, snapshot=None, on=None):
        if snapshot or self.dirty or self.agent is None:
            self.fresh(snapshot or on or self.base_snapshot)
        elif not self.agent.alive():
            self.log("agent not answering; restarting it")
            self.start_agent()

    # ------------------------------------------------------------- guest facts
    def load_guest_paths(self):
        r = self.agent.exec("cmd.exe", ["/d", "/c", "set"], timeout=30)
        env = dict(l.split("=", 1) for l in r["stdout"].splitlines() if "=" in l)
        self.guest = {k: env.get(k, "") for k in ("USERPROFILE", "LOCALAPPDATA", "APPDATA", "USERNAME", "COMPUTERNAME")}
        self.guest["INSTALL"] = self.guest["LOCALAPPDATA"] + r"\Programs\Superba"
        self.guest["DATA"] = self.guest["LOCALAPPDATA"] + r"\krill-code\Superba\data"
        self.guest["CODEX_HOME"] = self.guest["USERPROFILE"] + r"\.codex"

    def chatgpt_package(self):
        try:  # as the desktop user: the package is registered for that user only
            return self.guest_ps("(Get-AppxPackage -Name OpenAI.Codex | Select-Object -First 1).Version").strip() or None
        except Exception:
            return None

    @property
    def spb_exe(self):
        return self.guest["INSTALL"] + r"\spb.exe"

    @property
    def superba_exe(self):
        return self.guest["INSTALL"] + r"\superba.exe"

    # ------------------------------------------------------------- commands
    def spb(self, *args, timeout=300, stdin=None, env=None, label=None, locale="en"):
        """`spb` in the user's desktop session with the unelevated token and a
        fresh user environment (as a new terminal would get it)."""
        argv = list(args)
        if locale and "--locale" not in argv:
            argv += ["--locale", locale]
        r = self.agent.exec(self.spb_exe, argv, timeout=timeout, stdin=stdin, env=env)
        self._tseq += 1
        name = f"{self._tseq:03d}-spb-{label or (args[0] if args else 'x')}.txt"
        shown = " ".join(argv)
        (self.root / "transcripts" / name).write_text(
            f"$ spb {shown}\n# exit {r['code']} in {r['ms']} ms{' (timed out)' if r['timed_out'] else ''}\n"
            f"--- stdout\n{r['stdout']}\n--- stderr\n{r['stderr']}\n")
        r["transcript"] = f"transcripts/{name}"
        self.log(f"  spb {shown} -> {r['code']} ({r['ms']} ms)")
        return r

    def spb_json(self, *args, **kw):
        r = self.spb(*args, "--json", **kw)
        try:
            r["json"] = json.loads(r["stdout"]) if r["stdout"].strip() else None
        except ValueError:
            r["json"] = None
        return r

    def screenshot(self, host_path, window=None, full=False, **sel):
        host_path = Path(host_path)
        guest = GUEST_SHOTS + "\\" + host_path.name
        args = {}
        if not full:
            if window:
                args.update(window)
            elif sel:
                args.update(sel)
            else:
                args["process_re"] = "^superba$"
        try:
            self.agent.shot(guest, **args)
        except AgentError as e:
            if full:
                raise
            self.log(f"  window shot failed ({e}); taking the full screen")
            self.agent.shot(guest)
        self.vm.scp_from(guest.replace("\\", "/"), host_path)
        return host_path

    def ui_dump(self, host_path, **sel):
        sel = sel or {"process_re": "^superba$"}
        sel.setdefault("raw", True)
        try:
            tree = self.agent.call("uia.tree", timeout=120, **sel)
        except AgentError as e:
            Path(host_path).write_text(f"uia.tree failed: {e}\n")
            return host_path
        lines = []
        for e in tree:
            extra = " ".join(f"{k}={e[k]!r}" for k in ("toggle", "value", "selected", "expanded", "help") if k in e)
            lines.append(f"{'  ' * e['depth']}{e['type']} {e['name']!r} {e['rect']}"
                         f"{'' if e['enabled'] else ' DISABLED'} {extra}".rstrip())
        Path(host_path).write_text("\n".join(lines) + "\n")
        return host_path

    def guest_read(self, path, max_bytes=2_000_000):
        if self.agent:
            try:
                t = self.agent.call("file.read", path=path, max_bytes=max_bytes)
                return None if t is None else t.lstrip("\ufeff")
            except AgentError:
                pass  # an agent without file.read, or a file only the admin can read
        r = self.vm.ps(f"if (Test-Path -LiteralPath {psq(path)}) {{ Get-Content -Raw -Encoding UTF8 -LiteralPath {psq(path)} }}"
                       f" else {{ '<<missing>>' }}", check=False, timeout=120)
        t = r.stdout
        return None if t.strip() == "<<missing>>" else t[:max_bytes]

    def user_env(self):
        """HKCU\\Environment of the desktop user (unexpanded)."""
        return self.agent.call("env.user")

    def guest_ps(self, script, timeout=300):
        """PowerShell as the desktop user (unelevated, through the agent); returns stdout."""
        from vm import encode_ps
        r = self.agent.exec("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                                               "-EncodedCommand", encode_ps("$ProgressPreference='SilentlyContinue'\n" + script)],
                            timeout=timeout)
        if r["code"] != 0:
            raise RuntimeError(f"guest PowerShell (agent) failed ({r['code']}): {(r['stdout'] + r['stderr'])[-800:]}")
        return r["stdout"]

    def guest_json(self, path):
        t = self.guest_read(path)
        if t is None:
            return None
        try:
            return json.loads(t.lstrip("\ufeff"))
        except ValueError:
            return None
