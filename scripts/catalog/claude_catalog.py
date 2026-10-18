#!/usr/bin/env python3
"""Refresh the Claude Code settings catalog under catalog/claude/ (user bug 79).

    claude_catalog.py schema [--from FILE]    official schema from schemastore.org
                                              -> catalog/claude/claude-code-settings.schema.json
    claude_catalog.py binary PATH [--version V]
                                              keys only the installed Claude Code knows
                                              (its zod settings schema: `key:()=>....describe("...")`,
                                              `@internal` ones left out) -> catalog/claude/binary-keys.json
    claude_catalog.py check                   every catalog key has a Chinese name and text in
                                              catalog/claude/zh.json (prints the missing ones)

The schema is upstream data only; the Chinese overlay (zh.json) is written by people.
Nothing here reads or writes a real ~/.claude directory.
"""

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "catalog" / "claude"
SCHEMA_URL = "https://www.schemastore.org/claude-code-settings.json"


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    print(f"wrote {path.relative_to(REPO)}")


def cmd_schema(args):
    if args.from_file:
        data = json.loads(Path(args.from_file).read_text(encoding="utf-8"))
    else:
        req = urllib.request.Request(SCHEMA_URL, headers={"User-Agent": "superba-catalog"})
        with urllib.request.urlopen(req, timeout=60) as r:
            data = json.loads(r.read().decode("utf-8"))
    props = data.get("properties", {})
    env = props.get("env", {}).get("properties", {})
    print(f"{len(props)} top-level keys, {len(env)} documented environment variables")
    write_json(OUT / "claude-code-settings.schema.json", data)


# --- a small scanner for the minified zod schema in the Claude Code binary


def skip_string(s, i):
    q = s[i]
    i += 1
    while i < len(s):
        if s[i] == "\\":
            i += 2
            continue
        if q == "`" and s.startswith("${", i):
            i = skip_expr(s, i + 2, "}")
            continue
        if s[i] == q:
            return i + 1
        if q != "`" and s[i] == "\n":
            return i
        i += 1
    return i


def skip_expr(s, i, end):
    depth = 0
    while i < len(s):
        c = s[i]
        if c in "\"'`":
            i = skip_string(s, i)
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0 and c == end:
                return i + 1
            depth -= 1
        i += 1
    return i


def entry_expr(s, i, limit=200000):
    j, depth, end = i, 0, min(len(s), i + limit)
    while j < end:
        c = s[j]
        if c in "\"'`":
            j = skip_string(s, j)
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            if depth == 0:
                break
            depth -= 1
        elif c == "," and depth == 0:
            break
        j += 1
    return s[i:j]


def parse_string(s, i):
    raw = s[i:skip_string(s, i)]
    try:
        if raw[0] == "`":
            return raw[1:-1]
        if raw[0] == '"':
            return json.loads(raw)
        return json.loads('"' + raw[1:-1].replace('"', '\\"') + '"')
    except (ValueError, IndexError):
        return raw[1:-1]


def top_describe(expr):
    body = expr[4:] if expr.startswith("()=>") else expr
    depth, k, last = 0, 0, None
    while k < len(body):
        c = body[k]
        if c in "\"'`":
            k = skip_string(body, k)
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif depth == 0 and body.startswith(".describe(", k):
            p, parts = k + len(".describe("), []
            while p < len(body) and body[p] in "\"'`":
                q = skip_string(body, p)
                parts.append(parse_string(body, p))
                p = q
                while p < len(body) and body[p] in " +":
                    p += 1
            if parts:
                last = "".join(parts)
        k += 1
    return last


def kind_of(expr):
    """The value type from the head of a minified zod expression (best effort)."""
    body = expr[4:] if expr.startswith("()=>") else expr
    m = re.match(r"([A-Za-z_$][\w$]*)\(", body)
    head = m.group(1) if m else ""
    if head == "U":
        lit = re.match(r'U\(\[((?:"[^"]*",?)+)\]\)', body)
        if lit:
            return "enum", json.loads("[" + lit.group(1).rstrip(",") + "]")
        return "string", None
    return {"M": ("boolean", None), "o": ("string", None), "E": ("number", None),
            "T": ("array", None), "u": ("object", None), "pe": ("object", None)}.get(head, ("unknown", None))


def cmd_binary(args):
    schema = json.loads((OUT / "claude-code-settings.schema.json").read_text(encoding="utf-8"))
    known = set(schema.get("properties", {}))
    data = Path(args.path).read_bytes().decode("utf-8", "replace")
    seen = {}
    for m in re.finditer(r"(?<![\w$.])([A-Za-z_$][\w$]*):\(\)=>", data):
        key = m.group(1)
        if key in known or key in seen:
            continue
        expr = entry_expr(data, m.end() - 4)
        d = top_describe(expr)
        if not d or "@internal" in d:
            continue
        # A template expression in the text (`${list.join(", ")}`) is code.
        d = re.sub(r"\$\{[^}]*\}", "…", d)
        kind, values = kind_of(expr)
        item = {"key": key, "type": kind, "description": d}
        if values:
            item["enum"] = values
        seen[key] = item
    keys = sorted(seen.values(), key=lambda x: x["key"].lower())
    print(f"{len(keys)} keys only the binary knows")
    write_json(OUT / "binary-keys.json", {"claude_code_version": args.version, "keys": keys})


def cmd_check(_args):
    schema = json.loads((OUT / "claude-code-settings.schema.json").read_text(encoding="utf-8"))
    binary = json.loads((OUT / "binary-keys.json").read_text(encoding="utf-8"))
    zh = json.loads((OUT / "zh.json").read_text(encoding="utf-8"))
    keys = set(schema["properties"]) | {k["key"] for k in binary["keys"]}
    env = set(schema["properties"]["env"]["properties"])
    missing = sorted(k for k in keys if k not in zh["keys"]) + sorted(
        "env." + e for e in env if e not in zh["env"])
    for m in missing:
        print("missing:", m)
    print(f"{len(keys)} keys, {len(env)} environment variables, {len(missing)} without Chinese text")
    return 1 if missing else 0


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("schema")
    s.add_argument("--from", dest="from_file")
    b = sub.add_parser("binary")
    b.add_argument("path")
    b.add_argument("--version", default=None)
    sub.add_parser("check")
    args = p.parse_args()
    return {"schema": cmd_schema, "binary": cmd_binary, "check": cmd_check}[args.cmd](args) or 0


if __name__ == "__main__":
    sys.exit(main())
