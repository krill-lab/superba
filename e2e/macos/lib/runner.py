"""Independent scenarios, scrubbed evidence, mandatory cleanup and honest status."""

import json
import time
from dataclasses import dataclass
from pathlib import Path

from .agent import Agent
from .security import Redactor, load_env
from .ui import Strings, Ui
from .vm import Blocked

REGISTRY = {}
DATA = "home/Library/Application Support/superba"


@dataclass
class Scenario:
    id: str
    title: str
    fn: object
    counterpart: str = ""


def scenario(id, title, counterpart=""):
    def register(fn):
        REGISTRY[id] = Scenario(id, title, fn, counterpart)
        return fn
    return register


class Case:
    def __init__(self, runner, spec):
        self.runner, self.spec = runner, spec
        self.session = None
        self.root = None
        self.ui = None
        self.fixture = None
        self.evdir = runner.out / spec.id
        self.evdir.mkdir()
        self.report = {"id": spec.id, "title": spec.title, "status": "NOT RUN",
                       "checks": [], "evidence": [], "cleanup": None}

    @property
    def agent(self):
        return self.runner.agent

    @property
    def strings(self):
        return self.runner.strings

    def rpc(self, operation, **args):
        return self.runner.vm.rpc(operation, session=self.session, **args)

    def save(self, name, value):
        path = self.evdir / name
        content = self.runner.redactor.json(value) if isinstance(value, (list, dict)) else self.runner.redactor.text(value)
        path.write_text(content)
        self.report["evidence"].append(str(path.relative_to(self.runner.out)))
        return path

    def check(self, condition, message):
        self.report["checks"].append({"ok": bool(condition), "message": message})
        self.runner.log(f"{self.spec.id} {'PASS' if condition else 'FAIL'} {message}")
        if not condition:
            raise AssertionError(message)

    def spb(self, *argv, stdin=None, env=None, ok=True, gui=False):
        result = self.rpc("spb", argv=list(argv), stdin=stdin, env=env, gui=gui)
        number = len(list(self.evdir.glob("spb-*.json"))) + 1
        self.save(f"spb-{number:02d}.json", {"argv": argv, **result})
        if ok:
            self.check(result["code"] == 0, "spb " + " ".join(argv[:2]) + " exits 0")
        return result

    def read(self, path):
        return self.rpc("read", path=path)

    def write(self, path, text):
        return self.rpc("write", path=path, text=text)

    def install(self, dmg=None, upgrade=False):
        source = Path(dmg or self.runner.opts.dmg).resolve()
        destination = self.root + ("/upgrade.dmg" if upgrade else "/install.dmg")
        self.runner.vm.put(source, destination)
        result = self.rpc("install", dmg=destination, upgrade=upgrade, rpc_timeout=300)
        self.save("upgrade.json" if upgrade else "install.json", result)
        self.check("arm64" in result["architecture"].split(), "Installed app contains native arm64")
        return result

    def launch(self):
        result = self.rpc("start", argv=["--lang", "en", "--size", "1120x800"])
        self.ui = Ui(self, pid=result["pid"])
        self.ui.wait_window()
        return self.ui

    def shot(self, name):
        # Do not capture a credentials page or save AX values while a key is being entered.
        if not self.ui:
            return
        if self.ui.exists(key="wizard.credentials.custom.title") or self.ui.exists(key_re="wizard.credentials.key.title"):
            raise RuntimeError("Refusing to capture the credential entry screen")
        remote = self.root + "/" + name + ".png"
        self.agent.shot(remote, **self.ui.win)
        path = self.evdir / (name + ".png")
        self.runner.vm.get(remote, path)
        self.report["evidence"].append(str(path.relative_to(self.runner.out)))
        self.save(name + ".ax.json", self.ui.tree())

    def start_fixture(self):
        if not self.fixture:
            self.fixture = self.rpc("fixture")["base"]
        return self.fixture

    def endpoint(self):
        if self.runner.opts.net == "real":
            upstream = self.runner.secrets.get("SUPERBA_E2E_UPSTREAM")
            key = self.runner.secrets.get("SUPERBA_E2E_KEY")
            if not upstream or not key:
                raise Blocked("Real mode requires SUPERBA_E2E_UPSTREAM and SUPERBA_E2E_KEY")
            return upstream.rstrip("/") + self.runner.opts.responses_path, key
        key = "sk-e2e-macos-fixture-only"
        self.runner.redactor.add(key)
        return self.start_fixture() + "/v1", key

    def provider(self, name="mac-test", endpoint=None):
        base, key = endpoint or self.endpoint()
        self.spb("site", "add", name, "--name", name, "--base-url", base)
        self.spb("profile", "add", name, "--name", name, "--site", name, "--auth", "apikey",
                 "--key-stdin", stdin=key + "\n")
        return name, base, key

    def baseline(self):
        self.write("home/.codex/config.toml", '# e2e original\nmodel = "e2e-model"\n')

    def configured_ui(self):
        from e2e.shared.desktop_flows import wizard_skip
        ui = self.launch()
        if ui.exists(key="wizard.detect.title", timeout=4):
            wizard_skip(ui)
        return ui

    def run(self):
        started = time.monotonic()
        try:
            created = self.runner.vm.rpc("create")
            self.session, self.root = created["session"], created["root"]
            self.save("session.json", created)
            self.spec.fn(self)
            self.report["status"] = "PASS"
        except Blocked as error:
            self.report.update(status="BLOCKED", reason=self.runner.redactor.text(error))
        except Exception as error:
            self.report.update(status="FAIL", reason=self.runner.redactor.text(str(error)))
            try:
                self.shot("failure")
            except Exception as capture_error:
                self.report["capture_error"] = self.runner.redactor.text(capture_error)
        finally:
            if self.session:
                try:
                    log = self.read("logs/process.log")
                    if log:
                        self.save("process.log", log)
                except Exception as error:
                    self.report["log_error"] = self.runner.redactor.text(error)
                try:
                    cleanup = self.rpc("cleanup", rpc_timeout=240)
                    self.report["cleanup"] = cleanup
                    if not cleanup["ok"]:
                        self.report["status"] = "FAIL"
                except Exception as error:
                    self.report.update(status="FAIL", cleanup={"ok": False,
                                       "error": self.runner.redactor.text(error), "session": self.session})
            self.report["seconds"] = round(time.monotonic() - started, 2)
            self.save("result.json", self.report)
        return self.report


class Runner:
    def __init__(self, vm, opts):
        self.vm, self.opts = vm, opts
        self.agent = Agent(vm)
        self.out = Path(opts.out).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.secrets = load_env(opts.env)
        self.redactor = Redactor(self.secrets.values())
        self.strings = {"en": Strings("en"), "zh-CN": Strings("zh-CN")}
        self.results = []

    def log(self, message):
        message = self.redactor.text(message)
        print(message, flush=True)
        with (self.out / "run.log").open("a") as stream:
            stream.write(message + "\n")

    def report(self):
        value = {"results": self.results, "counts": {status: sum(r["status"] == status for r in self.results)
                 for status in ("PASS", "FAIL", "BLOCKED", "NOT RUN")}}
        (self.out / "report.json").write_text(self.redactor.json(value))
        lines = ["# macOS e2e results", "", "| ID | Scenario | Result | Checks |", "|---|---|---|---|"]
        reasons = []
        for result in self.results:
            checks = result.get("checks", [])
            lines.append(f"| {result['id']} | {result['title']} | {result['status']} | "
                         f"{sum(c['ok'] for c in checks)}/{len(checks)} |")
            if result.get("reason"):
                reasons.append(result["id"] + ": " + result["reason"])
        lines.extend(["", *reasons])
        (self.out / "report.md").write_text(self.redactor.text("\n".join(lines)) + "\n")
        return value

    def run(self, ids):
        try:
            build = self.vm.prepare()
            (self.out / "agent-build.json").write_text(self.redactor.json(build))
            disk = self.vm.rpc("disk")
            disk["build_minimum_bytes"] = 25 * 1024**3
            disk["development_build_allowed"] = disk["free_bytes"] >= disk["build_minimum_bytes"]
            (self.out / "disk-check.json").write_text(self.redactor.json(disk))
            self.log(f"VM free space: {disk['free_bytes'] / 1024**3:.1f} GiB; "
                     + ("development build allowed" if disk["development_build_allowed"] else "use CI DMG (below 25 GiB)"))
            permissions = self.agent.preflight()
            (self.out / "permissions.json").write_text(self.redactor.json(permissions))
        except Blocked as error:
            self.log(str(error))
            self.results = [{"id": id, "title": REGISTRY[id].title, "status": "BLOCKED",
                             "reason": str(error), "checks": [], "cleanup": "no scenario state created"} for id in ids]
            self.report()
            return 2
        for index, id in enumerate(ids):
            self.log("Starting " + id)
            result = Case(self, REGISTRY[id]).run()
            self.results.append(result)
            self.report()
            self.log(id + " " + result["status"])
            if isinstance(result.get("cleanup"), dict) and not result["cleanup"].get("ok"):
                for remaining in ids[index + 1:]:
                    self.results.append({"id": remaining, "title": REGISTRY[remaining].title,
                                         "status": "NOT RUN", "reason": "Earlier cleanup failed", "checks": []})
                self.report()
                break
        counts = self.report()["counts"]
        return 1 if counts["FAIL"] else 2 if counts["BLOCKED"] or counts["NOT RUN"] else 0
