"""Scenario registry, step recording and the JSON / Markdown report.

A scenario is a function `fn(t)` registered with @scenario(...). It records steps:

    with t.step("MSI installs per-user") as s:
        r = t.run.spb("status")
        s.check(r["code"] == 0, "spb status exits 0")
        s.save("status.txt", r["stdout"])

A failed check or an exception fails the step; a failed required step ends the
scenario (the rest is reported as not run). `s.check(..., soft=True)` only warns.
`s.manual("...")` lists something a person has to judge on the saved evidence
(layout, clipping, glyphs); it does not fail the run but shows up in the report.
"""

import json
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path

REGISTRY = {}

PASS, FAIL, WARN, SKIP, NOTRUN = "PASS", "FAIL", "WARN", "SKIP", "NOT RUN"


@dataclass
class Scenario:
    id: str
    title: str
    fn: object
    area: str = ""
    snapshot: str = None      # restore this snapshot first; None = chain on the current VM state
    on: str = None            # chain only on a session restored from this snapshot (else restore it)
    net: str = "any"          # any | mock | real | none
    needs: tuple = ()         # prerequisite flows (lib/flows.py), run when the state lacks them
    conflicts: tuple = ()     # state that must not be there yet (else the base snapshot is restored first)
    tags: tuple = ()
    bugs: tuple = ()          # user-bugs.md numbers this scenario guards
    manual_only: bool = False # documented in MATRIX.md, steps need a person (or not automatable yet)
    order: int = 0


def scenario(id, title, **kw):
    def wrap(fn):
        REGISTRY[id] = Scenario(id=id, title=title, fn=fn, order=len(REGISTRY), **kw)
        return fn
    return wrap


class CheckFailed(Exception):
    pass


class StepRec:
    def __init__(self, t, name, required):
        self.t, self.name, self.required = t, name, required
        self.status = PASS
        self.checks = []
        self.evidence = []
        self.notes = []
        self.manuals = []
        self.error = None
        self.t0 = time.time()
        self.secs = 0

    # --- recording
    def check(self, cond, msg, soft=False):
        ok = bool(cond)
        self.checks.append({"ok": ok, "msg": msg, "soft": soft})
        self.t.run.log(f"    [{'ok' if ok else ('warn' if soft else 'FAIL')}] {msg}")
        if not ok:
            if soft:
                if self.status == PASS:
                    self.status = WARN
            else:
                self.status = FAIL
                raise CheckFailed(msg)
        return ok

    def info(self, msg):
        self.notes.append(msg)
        self.t.run.log(f"    {msg}")

    def manual(self, msg):
        self.manuals.append(msg)
        self.t.run.log(f"    [manual] {msg}")

    def add_evidence(self, path):
        p = Path(path)
        try:
            rel = str(p.relative_to(self.t.run.root))
        except ValueError:
            rel = str(p)
        self.evidence.append(rel)
        return p

    def save(self, name, content):
        p = self.t.evdir / f"{self.t.seq():02d}-{name}"
        if isinstance(content, (dict, list)):
            p.write_text(json.dumps(content, indent=1, ensure_ascii=False))
        else:
            p.write_text(content if isinstance(content, str) else str(content))
        return self.add_evidence(p)

    def shot(self, name, **kw):
        """Screenshot of the Superba window (default) or of `window=` / `full=True`."""
        p = self.t.run.screenshot(self.t.evdir / f"{self.t.seq():02d}-{name}.png", **kw)
        if p:
            self.add_evidence(p)
        return p

    def ui_dump(self, name, **sel):
        p = self.t.run.ui_dump(self.t.evdir / f"{self.t.seq():02d}-{name}.uia.txt", **sel)
        if p:
            self.add_evidence(p)
        return p

    def failure_evidence(self):
        """The screen as it is now (through the desktop agent, else the VM's framebuffer:
        a [setup] step often fails because the agent is gone) and the agent's log."""
        run = self.t.run
        seq = self.t.seq()
        shot = self.t.evdir / f"{seq:02d}-failure.png"
        p = None
        try:
            p = run.screenshot(shot, full=True)
        except Exception as e:
            run.log(f"    failure screenshot through the agent failed ({e}); taking the VM framebuffer")
            try:
                p = run.vm.screen(str(shot))
            except Exception as e2:
                run.log(f"    VM framebuffer failed too: {e2}")
        if p and Path(p).is_file():
            self.add_evidence(p)
        if run.agent:
            log = self.t.evdir / f"{seq:02d}-agent.log"
            try:
                run.agent.fetch_log(log)
            except Exception as e:
                run.log(f"    agent log not fetched: {e}")
            if log.is_file():
                self.add_evidence(log)

    # --- context manager
    def __enter__(self):
        self.t.run.log(f"  step: {self.name}")
        check_disk(self.t.run)
        return self

    def __exit__(self, et, ev, tb):
        self.secs = round(time.time() - self.t0, 1)
        if et is not None:
            self.status = FAIL
            if et is not CheckFailed:
                self.error = "".join(traceback.format_exception(et, ev, tb))[-3000:]
                self.t.run.log(f"    ERROR {et.__name__}: {ev}")
            else:
                self.error = str(ev)
            self.failure_evidence()
        self.t.steps.append(self)
        if et is KeyboardInterrupt:
            return False  # interrupted run (SIGINT/SIGTERM): stop everything, the runner tears down
        if self.status == FAIL and self.required:
            raise StopScenario(self.name)
        return True  # never propagate: the scenario decides via StopScenario


class StopScenario(Exception):
    pass


# Abort below this much free space on the output's file system (alpha.10 H5:
# /data reached 0 GB twice while WSL snapshots were restored during a run).
MIN_FREE_BYTES = 10 * 1024**3


class LowDisk(KeyboardInterrupt):
    """Too little disk left: stop the run like an interrupt (the runner tears
    the VM down and writes the report)."""


def check_disk(run, minimum=MIN_FREE_BYTES):
    import os
    import shutil
    root = getattr(run, "root", None)
    # Real runs only (run.py turns it on); unit tests run in a small /tmp.
    if root is None or not os.environ.get("SUPERBA_E2E_DISK_CHECK"):
        return
    try:
        free = shutil.disk_usage(root).free
    except OSError:
        return
    if free < minimum:
        msg = f"only {free / 1024**3:.1f} GB free under {root}: stopping the run (needs {minimum / 1024**3:.0f} GB)"
        run.log("ABORT: " + msg)
        raise LowDisk(msg)


class T:
    """Per-scenario context handed to scenario functions."""

    def __init__(self, run, sc):
        self.run, self.sc = run, sc
        self.steps = []
        self.evdir = run.root / "evidence" / sc.id
        self.evdir.mkdir(parents=True, exist_ok=True)
        self._seq = 0

    def seq(self):
        self._seq += 1
        return self._seq

    def step(self, name, required=True):
        return StepRec(self, name, required)

    def skip(self, why):
        raise SkipScenario(why)


class SkipScenario(Exception):
    pass


def status_of(steps):
    if not steps:
        return SKIP
    st = [s.status for s in steps]
    if FAIL in st:
        return FAIL
    if WARN in st:
        return WARN
    return PASS


def step_json(s):
    return {"name": s.name, "status": s.status, "secs": s.secs, "checks": s.checks,
            "notes": s.notes, "manual": s.manuals, "evidence": s.evidence, "error": s.error}


def write_report(run, results):
    data = {
        "run": run.meta(),
        "summary": {k: sum(1 for r in results if r["status"] == k) for k in (PASS, WARN, FAIL, SKIP, NOTRUN)},
        "scenarios": results,
    }
    (run.root / "report.json").write_text(json.dumps(data, indent=1, ensure_ascii=False))
    md = [f"# Superba Windows e2e run {run.root.name}", ""]
    for k, v in run.meta().items():
        md.append(f"- **{k}**: {v}")
    md += ["", "| Scenario | Title | Status | Time | Failed / warned steps |", "|---|---|---|---|---|"]
    for r in results:
        bad = [s["name"] for s in r["steps"] if s["status"] in (FAIL, WARN)]
        md.append(f"| {r['id']} | {r['title']} | {r['status']} | {r['secs']} s | {'; '.join(bad) or (r.get('reason') or '')} |")
    md.append("")
    for r in results:
        md += [f"## {r['id']} {r['title']} — {r['status']}", ""]
        if r.get("reason"):
            md += [f"_{r['reason']}_", ""]
        for s in r["steps"]:
            md.append(f"- **{s['status']}** {s['name']} ({s['secs']} s)")
            for c in s["checks"]:
                mark = "ok" if c["ok"] else ("warn" if c["soft"] else "FAIL")
                md.append(f"  - [{mark}] {c['msg']}")
            for n in s["notes"]:
                md.append(f"  - {n}")
            for m in s["manual"]:
                md.append(f"  - [manual] {m}")
            for e in s["evidence"]:
                md.append(f"  - evidence: `{e}`")
            if s["error"]:
                md.append("  - error:")
                md.append("    ```")
                md += ["    " + line for line in s["error"].splitlines()[-12:]]
                md.append("    ```")
        md.append("")
    (run.root / "report.md").write_text("\n".join(md))
    return data
