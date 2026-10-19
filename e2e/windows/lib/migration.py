"""W22's localized migration observations and real data checks (no VM fixture writes)."""
import re
import time

from ui import UiError
from vm import psq


def migration_count(strings, text, total=600, restore=False):
    key = "switch.sessions.restored" if restore else "switch.sessions.migrated"
    pattern = re.escape(strings.table[key]).replace(r"\{done\}", r"(?P<done>\d+)")
    pattern = pattern.replace(r"\{total\}", r"(?P<total>\d+)")
    patterns = [pattern]
    if strings.lang == "en":
        # Former harness copy used a slash; keep it precise, never match a
        # scanned total, table pager or migration-history count as progress.
        verb = "Restored" if restore else "Migrated"
        patterns.append(verb + r" (?P<done>\d+)\s*/\s*(?P<total>\d+)(?: sessions)?")
    for pattern in patterns:
        match = re.fullmatch(pattern, text.strip())
        if match:
            done, of = int(match["done"]), int(match["total"])
            if of == total and 0 <= done <= of:
                return done, of
    return None


def migration_busy(u, names):
    titles = ("movers.title.migrate", "movers.title.restore", "movers.title.stopping", "confirm.title")
    return any(u.L(key) in names for key in titles) or any(
        re.fullmatch(u.R(key), name) for key in
        ("banner.running.title", "banner.armed.title", "banner.reapply.title", "banner.planning")
        for name in names)


def wait_migration(run, s, u, total=600, restore=False, stop=False, timeout=300):
    """Wait for the actual modal to close, even while Migration history remains.

    Completion alone cannot satisfy the intermediate-progress assertion. The
    post-flow scan/hash checks establish the data outcome independently.
    """
    start = time.monotonic()
    observations, errors = [], []
    seen_active = stopped = confirmed = False
    pid = (run.agent.procs(r"^superba\.exe$") or [{}])[0].get("pid")
    try:
        while time.monotonic() - start < timeout:
            names = u.texts()
            active = migration_busy(u, names)
            seen_active |= active
            errors.extend(n for n in names if re.search(r"(?i)holds|too many open files|os error|EMFILE", n))
            if u.L("confirm.title") in names:
                if confirmed:
                    raise UiError("session migration repeated its full-restart confirmation")
                s.shot("migration-restart-confirm")
                s.ui_dump("migration-restart-confirm")
                u.click(key="confirm.full_restart", type="Button")
                confirmed = True
                continue
            count = next((c for n in names if (c := migration_count(u.s, n, total, restore))), None)
            current = next((n for n in names if re.fullmatch(u.R("movers.now"), n)), None)
            handles = run.vm.ps(f"(Get-Process -Id {pid}).HandleCount", check=False).stdout.strip() if pid else ""
            observations.append({"t": round(time.monotonic() - start, 1), "count": count,
                                 "active": active, "current": current, "handles": handles})
            if count and active and len([o for o in observations if o["count"]]) <= 6:
                s.shot(f"{'restoring' if restore else 'migrating'}-{len(observations)}")
            if stop and not stopped and active and count and 0 < count[0] < count[1]:
                s.check(u.exists(key="movers.stop", type="Button", enabled=True),
                        "Stop and undo is enabled during actual migration progress")
                s.ui_dump("before-stop-and-undo")
                u.click(key="movers.stop", type="Button")
                stopped = True
            if seen_active and not active:
                break
            time.sleep(1)
        else:
            raise UiError(f"session migration did not finish within {timeout} s")
    finally:
        s.save("progress-and-handles.json", observations)
        s.save("migration-errors.json", errors)
    s.check(not errors, f"no lock / EMFILE error (bug 31): {errors!r}")
    if not restore:
        s.check(any(o["active"] and o["count"] and 0 < o["count"][0] < total for o in observations),
                f"intermediate migrated progress for {total} sessions was visible")
        s.check(any(o["current"] for o in observations), "the current session/file was shown")
    if stop:
        s.check(stopped, "Stop and undo was actually clicked during migration")


def rollout_hashes(run, s, label):
    path = run.guest["CODEX_HOME"] + r"\sessions"
    out = run.vm.ps(f"(Get-ChildItem {psq(path)} -Recurse -File | Get-FileHash | Sort-Object Path |"
                    " ForEach-Object { $_.Hash + ' ' + $_.Path }) -join \"`n\"").stdout
    s.save(f"hashes-{label}.txt", out)
    return {path.casefold(): digest for line in out.splitlines() if line.strip()
            for digest, path in [line.strip().split(" ", 1)]}


def scan_sessions(run, s, label):
    result = run.spb_json("sessions", "scan", timeout=300, label="sessions-" + label)
    s.save("sessions-" + label + ".json", result)
    s.check(result["code"] == 0, "session scan completed")
    data = result["json"]
    s.check(data["report"]["inconsistent"] == 0 and not data["report"]["warnings"],
            "database rows and rollout providers agree, with no warnings")
    return data


def provider_counts(scan):
    return {provider: {key: row[key] for key in ("threads", "rollouts", "distinct")}
            for provider, row in scan["report"]["by_provider"].items() if row["distinct"]}


def check_migrated(s, before, after, hashes_before, hashes_after, fixture_paths):
    expected = provider_counts(before)
    moved = expected.pop("krill")
    s.check(moved == dict(threads=600, rollouts=600, distinct=600), "600 source rows and rollout files")
    target = expected.setdefault("openai", dict(threads=0, rollouts=0, distinct=0))
    for key, n in moved.items():
        target[key] += n
    s.check(provider_counts(after) == expected, "all 600 rows and rollout providers migrated to openai")
    old_ids = {x["id"] for x in before["history"]}
    receipts = [x for x in after["history"] if x["id"] not in old_ids and not x["rolled_back"]]
    s.check(len(receipts) == 1 and receipts[0]["by_source_provider"] == {"krill": 600}
            and receipts[0]["threads"] == 600 and receipts[0]["rollouts"] == 600,
            "new migration ledger records all 600 rows and files")
    changed = {path for path, digest in hashes_before.items() if hashes_after.get(path) != digest}
    s.check(hashes_after.keys() == hashes_before.keys() and changed == set(fixture_paths),
            "exactly the 600 fixture rollouts changed; no files added or lost")
    return receipts[0]["id"]


def check_restored(s, before, after, hashes_before, hashes_after):
    s.check(provider_counts(after) == provider_counts(before), "original database row/provider counts restored")
    s.check(hashes_after == hashes_before, "every original rollout restored byte-for-byte (SHA256), no added files")
