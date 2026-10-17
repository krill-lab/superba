"""A pre-existing "legacy Krill" Codex home, made by the real Codex.

The config follows the old Krill tutorials (custom provider id "krill"), plus the
things a takeover must leave alone: comments, an MCP server, a [projects] table,
an unrelated top-level key and a legacy feature name (`[features] collab`, which
the settings scenario migrates). Threads are created through `codex app-server`
(source "vscode", listed by the desktop app) and one through `codex exec`, so
state_5.sqlite and the rollout files are Codex's own.
"""

import json
import sqlite3
import sys
from pathlib import Path

from infra import LIB

LEGACY_CONFIG = """\
# Krill tutorial (legacy): custom provider id
# keep: user comment above the provider
model_provider = "krill"
model = "gpt-6-luna"
model_reasoning_effort = "low"
approval_policy = "on-request"   # keep: inline comment
file_opener = "none"

[model_providers.krill]
name = "krill"
base_url = "{base}/backend-api/codex"
wire_api = "responses"
requires_openai_auth = true

# keep: MCP server (disabled so the app does not start it)
[mcp_servers.e2e-docs]
command = "npx"
args = ["-y", "@example/docs-mcp"]
enabled = false

[projects."{proj}"]
trust_level = "trusted"

# keep: legacy feature name (an alias of features.multi_agent since Codex 0.15x)
[features]
collab = false
"""

USER_RC = "# e2e: user's own rc content (must survive)\nexport E2E_USER_VAR=keep\n"

PROMPTS = [
    "LEGACY-ALPHA: reply with exactly the word ok",
    "LEGACY-BETA: reply with exactly the word ok",
]
EXEC_PROMPT = "LEGACY-EXEC: reply with exactly the word ok"


def build(run):
    home = Path(run.home)
    codex_home = run.codex_home
    proj = home / "proj"
    proj.mkdir(parents=True, exist_ok=True)
    codex_home.mkdir(parents=True, exist_ok=True)
    (codex_home / "config.toml").write_text(
        LEGACY_CONFIG.format(base=run.line_url("a"), proj=str(proj))
    )
    (codex_home / "auth.json").write_text(
        json.dumps({"OPENAI_API_KEY": run.key}, indent=2) + "\n"
    )
    for rc in (".bashrc", ".profile"):
        (home / rc).write_text(USER_RC)
    codex = str(Path(run.d["app_dir"]) / "resources" / "codex")
    env = run.base_env()
    r = run.run(
        [sys.executable, str(LIB / "codex_as.py"), codex, str(proj), *PROMPTS],
        env=env,
        cwd=str(proj),
        timeout=600,
        label="legacy threads (app-server)",
    )
    threads = [json.loads(x) for x in r["out"].splitlines() if x.startswith("{")]
    r2 = run.run(
        [codex, "exec", "--skip-git-repo-check", EXEC_PROMPT],
        env=env,
        cwd=str(proj),
        timeout=300,
        label="legacy thread (exec)",
        stdin="",
    )
    return {"threads": threads, "exec_rc": r2["rc"], "as_rc": r["rc"]}


def sqlite_providers(codex_home):
    db = Path(codex_home) / "state_5.sqlite"
    if not db.exists():
        return {}
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        return {
            row[0]: {"provider": row[1], "source": row[2], "title": row[3]}
            for row in c.execute("select id, model_provider, source, title from threads")
        }
    finally:
        c.close()


def rollout_providers(codex_home):
    out = {}
    for sub in ("sessions", "archived_sessions"):
        for f in sorted((Path(codex_home) / sub).rglob("rollout-*.jsonl")):
            try:
                with open(f, encoding="utf-8") as fh:
                    first = json.loads(fh.readline())
            except (OSError, json.JSONDecodeError):
                continue
            p = first.get("payload", {})
            out[p.get("id") or f.name] = {
                "provider": p.get("model_provider"),
                "file": str(f.relative_to(codex_home)),
            }
    return out


def snapshot(run, label):
    """Byte copies of the owned files plus provider tables, for later diffs."""
    dst = run.out / "snap" / label
    dst.mkdir(parents=True, exist_ok=True)
    home = Path(run.home)
    files = {}
    for rel in (".codex/config.toml", ".codex/auth.json", ".bashrc", ".profile", ".zshenv",
                ".bash_profile", ".config/environment.d/60-superba.conf",
                ".config/fish/conf.d/superba.fish"):
        src = home / rel
        if src.exists():
            data = src.read_bytes()
            target = dst / rel.replace("/", "__")
            # auth.json carries the test key: keep only its shape in out/.
            if rel.endswith("auth.json"):
                try:
                    j = json.loads(data)
                    target.write_text(json.dumps({k: ("<key>" if isinstance(v, str) and len(v) > 20 else v) for k, v in j.items()}, indent=1))
                except json.JSONDecodeError:
                    target.write_text("<unparsable>")
            else:
                target.write_bytes(data)
            files[rel] = data
        else:
            files[rel] = None
    sq = sqlite_providers(run.codex_home)
    ro = rollout_providers(run.codex_home)
    (dst / "providers.json").write_text(json.dumps({"sqlite": sq, "rollout": ro}, indent=1))
    return {"files": files, "sqlite": sq, "rollout": ro}
