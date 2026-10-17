"""Logging line forwarder: plays one Superba "line" in front of an upstream relay.

    python3 linefwd.py LABEL PORT UPSTREAM LOGFILE [--tls CERT KEY] [--fault-file PATH]

Every request is forwarded to UPSTREAM (path and query kept, hop-by-hop headers
dropped, bodies streamed) and summarized as one JSON line in LOGFILE: method,
path, upstream status, whether the request carried the PAT header
(`chatgpt-account-id`), the originator, and for POST .../responses the model,
the number of input items, the start of the last user text, the requested
`service_tier` (and whether the field was sent at all), whether a
`prompt_cache_key` was sent (never its value) and how many Superba stats-line
markers (STATS_MARKER) the request's input still carries. Secrets and bodies are
never logged (no header values except the originator and user agent).

WebSocket upgrades get 426 so Codex falls back to HTTP streaming, which keeps
every turn visible in the log.

Busy turns on demand: for a Responses request whose last user message starts with
HOLD_MARKER ("E2E-LONG"), the end of the stream (the chunk carrying
`response.completed` / `response.failed` and everything after it) is held until
HOLD_SECS (env LINEFWD_HOLD_SECS, default 40) after the request started, so the
turn stays running in Codex for a known time whatever the model answers. With --tls the forwarder serves HTTPS with the
given certificate (the e2e harness makes a private CA and points Codex and
Superba at it through SSL_CERT_FILE / CODEX_CA_CERTIFICATE).

Fault injection: with --fault-file, a file whose first word is an HTTP status
(e.g. `401`) makes the forwarder answer every request (or, with a second word,
only paths containing it) with that status and an OpenAI-style JSON error instead
of forwarding; the file is read per request, so the harness flips a line's
behaviour without restarting it. A missing or empty file means normal forwarding.
"""

import asyncio
import json
import os
import ssl
import sys
import time

import aiohttp
from aiohttp import web

try:
    import zstandard
except ImportError:  # optional: only needed when Codex compresses request bodies
    zstandard = None

HOP = {
    "host", "connection", "keep-alive", "transfer-encoding", "upgrade",
    "content-length", "te", "trailer", "proxy-connection",
}


def parse_args(argv):
    if len(argv) < 5:
        sys.exit(__doc__)
    tls, fault, rest = None, None, argv[5:]
    while rest:
        if rest[0] == "--tls" and len(rest) >= 3:
            tls, rest = (rest[1], rest[2]), rest[3:]
        elif rest[0] == "--fault-file" and len(rest) >= 2:
            fault, rest = rest[1], rest[2:]
        else:
            sys.exit(__doc__)
    return argv[1], int(argv[2]), argv[3].rstrip("/"), argv[4], tls, fault


LABEL, PORT, UPSTREAM, LOGFILE, TLS, FAULT_FILE = parse_args(sys.argv)
HOLD_MARKER = "E2E-LONG"
# Prefix of the line Superba's router appends under replies (superba-core
# codex::STATS_LINE_MARKER: the bolt and a WORD JOINER). The router strips it from
# every later request, so the count in a request's input should be 0.
STATS_MARKER = "\u26a1\u2060"
HOLD_SECS = float(os.environ.get("LINEFWD_HOLD_SECS", "40"))
END_EVENTS = (b"response.completed", b"response.failed", b"response.incomplete")


def log(**kw):
    now = time.time()
    entry = {
        "t": time.strftime("%H:%M:%S", time.localtime(now)) + f".{int(now * 1000) % 1000:03d}",
        "ts": round(now, 3),
        "line": LABEL,
        **kw,
    }
    with open(LOGFILE, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def summarize(body, encoding):
    try:
        if encoding == "zstd":
            if zstandard is None:
                return {"parse_err": "zstd body, zstandard not installed"}
            body = zstandard.ZstdDecompressor().decompressobj().decompress(body)
        j = json.loads(body)
        items = j.get("input") or []
        last_user = None
        for item in reversed(items):
            if isinstance(item, dict) and item.get("role") == "user":
                content = item.get("content")
                if isinstance(content, list):
                    for c in content:
                        if isinstance(c, dict) and c.get("text"):
                            last_user = c["text"][:120]
                            break
                elif isinstance(content, str):
                    last_user = content[:120]
                break
        return {
            "model": j.get("model"),
            "n_input": len(items),
            "last_user": last_user,
            "stream": j.get("stream"),
            "has_prev": bool(j.get("previous_response_id")),
            # The tier value is not a secret; `tier_sent` tells an absent field
            # from an explicit null.
            "service_tier": j.get("service_tier"),
            "tier_sent": "service_tier" in j,
            "has_cache_key": bool(j.get("prompt_cache_key")),
            "markers": count_markers(items),
        }
    except Exception as e:  # a summary is best effort
        return {"parse_err": str(e)[:120]}


def count_markers(value):
    """Occurrences of STATS_MARKER in every string inside `value` (the input
    items: earlier assistant replies, user text, tool output)."""
    if isinstance(value, str):
        return value.count(STATS_MARKER)
    if isinstance(value, list):
        return sum(count_markers(v) for v in value)
    if isinstance(value, dict):
        return sum(count_markers(v) for v in value.values())
    return 0


def fault_for(path):
    """The injected status for `path`, if the fault file asks for one."""
    if not FAULT_FILE:
        return None
    try:
        with open(FAULT_FILE, encoding="utf-8") as f:
            words = f.read().split()
    except OSError:
        return None
    if not words or not words[0].isdigit():
        return None
    if len(words) > 1 and words[1] not in path:
        return None
    return int(words[0])


async def handle(req: web.Request):
    fault = fault_for(req.path)
    if fault is not None:
        names = {k.lower() for k in req.headers}
        auth = "pat" if "chatgpt-account-id" in names else ("bearer" if "authorization" in names else "none")
        log(kind="http", method=req.method, path=req.path_qs, status=fault, fault=True, auth_kind=auth,
            originator=req.headers.get("originator"))
        err = {"error": {"message": f"injected by the e2e forwarder ({fault})", "type": "invalid_request_error",
                         "code": "invalid_api_key" if fault == 401 else "e2e_fault"}}
        return web.json_response(err, status=fault)
    if req.headers.get("Upgrade", "").lower() == "websocket":
        log(kind="ws-upgrade", path=req.path_qs, status=426)
        return web.Response(status=426, text="upgrade required")
    body = await req.read()
    info = {}
    if req.method == "POST" and req.path.endswith("/responses"):
        info = summarize(body, req.headers.get("Content-Encoding", ""))
    names = {k.lower() for k in req.headers}
    meta = {
        "auth_kind": "pat" if "chatgpt-account-id" in names else ("bearer" if "authorization" in names else "none"),
        "originator": req.headers.get("originator"),
        "ua": (req.headers.get("User-Agent") or "")[:80],
        # Codex's conversation key (the thread id; not a secret): router stickiness.
        "session": (req.headers.get("session-id") or req.headers.get("thread-id") or "")[:64] or None,
    }
    hdr = {k: v for k, v in req.headers.items() if k.lower() not in HOP}
    started = time.time()
    try:
        async with req.app["sess"].request(
            req.method, UPSTREAM + req.path_qs, headers=hdr, data=body, allow_redirects=False
        ) as up:
            log(kind="http", method=req.method, path=req.path_qs, status=up.status, **meta, **info)
            resp = web.StreamResponse(
                status=up.status,
                headers={k: v for k, v in up.headers.items() if k.lower() not in HOP},
            )
            await resp.prepare(req)
            hold = (info.get("last_user") or "").startswith(HOLD_MARKER)
            held = []
            try:
                async for chunk in up.content.iter_any():
                    if hold and (held or any(m in chunk for m in END_EVENTS)):
                        held.append(chunk)
                        continue
                    await resp.write(chunk)
                if held:
                    wait = started + HOLD_SECS - time.time()
                    log(kind="hold", path=req.path, secs=round(max(wait, 0), 1))
                    await asyncio.sleep(max(wait, 0))
                    for chunk in held:
                        await resp.write(chunk)
                await resp.write_eof()
            except ConnectionResetError as e:
                # The client went away (e.g. its app-server was restarted mid-turn).
                log(kind="client-gone", path=req.path, error=type(e).__name__,
                    ms=int((time.time() - started) * 1000))
                return resp
            log(kind="done", method=req.method, path=req.path, status=up.status,
                ms=int((time.time() - started) * 1000))
            return resp
    except (aiohttp.ClientError, asyncio.TimeoutError) as e:
        log(kind="upstream-error", method=req.method, path=req.path_qs, error=str(e)[:200], **meta)
        return web.Response(status=502, text="upstream error")


async def main():
    app = web.Application(client_max_size=256 * 1024 * 1024)
    app["sess"] = aiohttp.ClientSession(
        auto_decompress=False, timeout=aiohttp.ClientTimeout(total=None, sock_connect=15)
    )
    app.router.add_route("*", "/{tail:.*}", handle)
    runner = web.AppRunner(app, auto_decompress=False, access_log=None)
    await runner.setup()
    ctx = None
    if TLS:
        ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
        ctx.load_cert_chain(TLS[0], TLS[1])
    await web.TCPSite(runner, "127.0.0.1", PORT, ssl_context=ctx).start()
    log(kind="started", port=PORT, tls=bool(TLS))
    await asyncio.Event().wait()


if __name__ == "__main__":
    asyncio.run(main())
