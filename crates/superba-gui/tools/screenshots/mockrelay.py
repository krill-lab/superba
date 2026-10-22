#!/usr/bin/env python3
"""Deterministic, loopback-only Krill relay for Superba screenshots (Python 3.11).

Example:
    python3 -I mockrelay.py --lines a,b,c --ports-file /tmp/mockrelay-ports.json \
        --api-key screenshot-key --latency a=38,b=96,c=154 --edge-block b
    python3 -I mockrelay.py --self-test

Use --cert/--key with the e2e harness's local CA for PAT screenshots: Superba
deliberately disables PAT on plain HTTP, even when the HTTP endpoints work.
The ports file is published only after every listener is ready. No upstream
connections are made, and the self-test bypasses environment HTTP proxies.

Contracts come from the working-tree superba-net classify/probe/keys/responses,
account/org/fetch modules and superba-core SiteEndpoints. Whoami's mandatory
chatgpt_plan_type is a Codex compatibility constant, not a displayed plan.
Only billing supplies the displayed plan. accounts/check needs a nonempty
accounts array; settings/user has no parser in superba-net, so it serves a
minimal synthetic preference object. The optional compact probe accepts an
empty output array.

Historical data is a pure function of seed, UTC day and model. The clock is
sampled once at startup: today's elapsed fraction, the 45-day history window
and billing expiry stay fixed for the entire screenshot session, including
across pages and lines. Restart to advance that snapshot. Token prices are
$1.25/M input (including cached input) and $10/M output. Integer charge units
of $0.0001 keep billing cents, costs and usage totals exactly reconcilable.

The e2e harness (e2e/lib/scenarios_v2.py) passes --fault-file: a JSON object
{path: status} read on every authenticated request, so one endpoint at a time can
answer 404 (missing) or 429 (rate limited) without restarting the relay.
"""

import argparse
import base64
from datetime import date, datetime, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import signal
import ssl
import sys
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlencode, urlsplit


DAY = 86_400
MODELS = (("gpt-5.5", "GPT-5.5", 0.60),
          ("gpt-6-astra", "GPT-6 Astra", 0.30),
          ("gpt-5.5-mini", "GPT-5.5 Mini", 0.10))
MODEL_PATHS = ("/v1/models", "/backend-api/codex/models", "/codex/v1/models")
RESPONSE_PATHS = tuple(p.removesuffix("models") + "responses" for p in MODEL_PATHS)
WHOAMI = "/v1/user-auth-credential/whoami"
ACCOUNTS = "/backend-api/wham/accounts/check"
SETTINGS = "/backend-api/wham/settings/user"
USAGE = "/v1/organization/usage/completions"
COSTS = "/v1/organization/costs"
ACCOUNT_ID = "acct-mock-dev"
COUNTS = ("input_tokens", "output_tokens", "input_cached_tokens", "num_model_requests")
MAX_TIMESTAMP = 253_402_300_799  # Last second of year 9999, UTC.


# Context windows of the mock models (also served as OpenAI-list metadata).
CONTEXT = {"gpt-5.5": 272_000, "gpt-6-astra": 400_000, "gpt-5.5-mini": 128_000}
# More ids of the OpenAI-style list only (`--extra-model`, e.g. long ids for
# the Models page's custom-site shots).
EXTRA_MODELS = []
FAST = {"id": "priority", "name": "Fast", "description": "2x speed, increased usage"}
ULTRAFAST = {"id": "ultrafast", "name": "Ultrafast",
             "description": "Fastest responses, highest usage"}


def codex_entry(index, slug, name):
    """A Codex-native model entry (`/backend-api/codex/models`): Fast on every
    model, Ultrafast only on gpt-6-astra, like the public list."""
    return {
        "slug": slug,
        "display_name": name,
        "description": f"{name} (mock relay)",
        "visibility": "list",
        "supported_in_api": True,
        "priority": index,
        "context_window": CONTEXT[slug],
        "max_context_window": CONTEXT[slug],
        "effective_context_window_percent": 95,
        "supported_reasoning_levels": [
            {"effort": e, "description": e} for e in ("low", "medium", "high")],
        "default_reasoning_level": "medium",
        "service_tiers": [FAST, ULTRAFAST] if slug == "gpt-6-astra" else [FAST],
        "input_modalities": ["text", "image"],
    }


# Desktop app downloads (SUPERBA_APP_DOWNLOADS points here): an APT index with a
# newer `chatgpt` build. Served without a key, like the vendor's public repository.
APP_VERSION = "26.1020.10000"
APP_DEB = f"pool/main/c/chatgpt/chatgpt_{APP_VERSION}_amd64.deb"


def apt_packages(arch):
    return (f"Package: chatgpt\nVersion: {APP_VERSION}\nArchitecture: {arch}\n"
            f"Filename: {APP_DEB.replace('amd64', arch)}\nSize: 187654321\n"
            f"SHA256: {'ab' * 32}\nDescription: ChatGPT desktop app\n\n").encode()


def json_bytes(value):
    return json.dumps(value, separators=(",", ":"), allow_nan=False).encode("utf-8")


def error(message, code="invalid_request_error", kind="invalid_request_error"):
    return {"error": {"message": message, "type": kind, "code": code}}


class BadRequest(ValueError):
    """A client error whose message contains no request values or secrets."""


def one(query, name, default=None):
    values = query.get(name)
    if values is None:
        if default is not None:
            return default
        raise BadRequest(f"Missing {name}")
    if len(values) != 1 or not values[0]:
        raise BadRequest(f"Expected one nonempty {name}")
    return values[0]


def integer(text, name, minimum=0, maximum=MAX_TIMESTAMP):
    if not text.isascii() or not text.isdecimal() or len(text) > 16:
        raise BadRequest(f"Invalid integer for {name}")
    number = int(text)
    if not minimum <= number <= maximum:
        raise BadRequest(f"{name} is out of range")
    return number


def fields(query, allowed):
    if query.keys() - set(allowed):
        raise BadRequest("Unsupported query parameter")


def utc_date(text):
    try:
        value = date.fromisoformat(text)
        if value.isoformat() != text:
            raise ValueError
        return (value - date(1970, 1, 1)).days * DAY
    except ValueError:
        raise BadRequest("Dates must be valid YYYY-MM-DD values") from None


class SyntheticData:
    def __init__(self, seed, now=None):
        self.seed = seed
        self.now = int(time.time() if now is None else now)
        self.today = self.now // DAY * DAY

    def noise(self, day, model, purpose):
        material = json_bytes([self.seed, day, model, purpose])
        return int.from_bytes(hashlib.sha256(material).digest()[:8], "big") / 2**64

    def records(self, day, start, end):
        """Integer counts and micro-dollars for an intersection with one UTC day."""
        left, right = max(day, start), min(day + DAY, end, self.now)
        if day < self.today - 45 * DAY or right <= left:
            return []
        weekend = datetime.fromtimestamp(day, timezone.utc).weekday() >= 5
        activity = 0.48 if weekend else 1.0
        trend = math.sin((day // DAY + self.seed % 10_000) / 4.7)
        jitter = sum(self.noise(day - i * DAY, "", "daily") for i in range(3)) / 3
        daily_dollars = (8.8 + 3.3 * trend + 2.0 * (jitter - 0.5)) * activity
        weights = [share * (0.94 + 0.12 * self.noise(day, model, "share"))
                   for model, _, share in MODELS]
        weight_total = sum(weights)

        def portion(count):
            # Cumulative rounding makes adjacent partial ranges additive.
            return count * (right - day) // DAY - count * (left - day) // DAY

        out = []
        for (model, _, _), weight in zip(MODELS, weights):
            dollars = daily_dollars * weight / weight_total
            output_share = 0.60 + 0.16 * self.noise(day, model, "output")
            # 80 input tokens or 10 output tokens each cost exactly $0.0001.
            inputs = portion(round(dollars * (1 - output_share) * 10_000)) * 80
            outputs = portion(round(dollars * output_share * 10_000)) * 10
            cached_share = 0.40 + 0.30 * self.noise(day, model, "cached")
            requests = max(80, round((120 + 260 * self.noise(day, model, "requests"))
                                     * activity))
            out.append({"model": model, "input_tokens": inputs,
                        "output_tokens": outputs,
                        "input_cached_tokens": int(inputs * cached_share),
                        "num_model_requests": portion(requests),
                        "cost_micros": inputs * 5 // 4 + outputs * 10})
        return out

    def billing_usage(self, query):
        fields(query, {"start_date", "end_date"})
        start = utc_date(one(query, "start_date"))
        end = utc_date(one(query, "end_date"))
        if not 0 < end - start <= 100 * DAY:
            raise BadRequest("Billing ranges must contain 1 to 100 days (end exclusive)")
        micros = sum(row["cost_micros"] for day in range(start, end, DAY)
                     for row in self.records(day, day, day + DAY))
        return {"object": "list", "total_usage": round(micros / 10_000, 2)}

    @staticmethod
    def usage_results(records, grouped):
        if records and not grouped:
            records = [{"model": None,
                        **{k: sum(r[k] for r in records) for k in (*COUNTS, "cost_micros")}}]
        return [{"object": "organization.usage.completions.result",
                 **{k: r[k] for k in COUNTS}, "input_audio_tokens": 0,
                 "output_audio_tokens": 0, "project_id": None, "user_id": None,
                 "api_key_id": None, "model": r["model"], "batch": None,
                 "cost_usd": r["cost_micros"] / 1_000_000} for r in records]

    @staticmethod
    def cost_results(records, grouped):
        if records and not grouped:
            records = [{"model": None, "cost_micros": sum(r["cost_micros"] for r in records)}]
        return [{"object": "organization.costs.result",
                 "amount": {"value": r["cost_micros"] / 1_000_000, "currency": "usd"},
                 "line_item": r["model"], "project_id": None} for r in records]

    def page(self, path, query):
        fields(query, {"start_time", "end_time", "bucket_width", "group_by",
                       "group_by[]", "limit", "page"})
        start = integer(one(query, "start_time"), "start_time")
        end = integer(one(query, "end_time", str(self.now)), "end_time")
        if end <= start:
            raise BadRequest("end_time must be after start_time")
        if one(query, "bucket_width", "1d") != "1d":
            raise BadRequest("Only bucket_width=1d is supported")
        limit = integer(one(query, "limit", "7"), "limit", 1, 31)
        groups = {part for value in query.get("group_by", []) + query.get("group_by[]", [])
                  for part in value.split(",")}
        allowed = {"model"} if path == USAGE else {"line_item", "project_id"}
        if groups - allowed:
            raise BadRequest("Unsupported group_by value")
        first_day = start // DAY * DAY
        count = (end - 1) // DAY - start // DAY + 1
        # The cursor is opaque and bound to this snapshot, API, range and grouping.
        tag = hashlib.sha256(json_bytes([self.seed, self.now, path, start, end,
                                        sorted(groups)])).hexdigest()[:24]
        offset = 0
        if "page" in query:
            token = one(query, "page")
            try:
                if len(token) > 256:
                    raise ValueError
                raw = base64.b64decode(token + "=" * (-len(token) % 4),
                                       altchars=b"-_", validate=True)
                cursor = json.loads(raw)
                if (not isinstance(cursor, list) or len(cursor) != 2 or cursor[0] != tag
                        or type(cursor[1]) is not int or not 0 < cursor[1] < count):
                    raise ValueError
                offset = cursor[1]
            except (ValueError, UnicodeError):
                raise BadRequest("Invalid page cursor for this query") from None
        stop = min(offset + limit, count)
        buckets = []
        for index in range(offset, stop):
            day = first_day + index * DAY
            left, right = max(day, start), min(day + DAY, end)
            records = self.records(day, left, right)
            results = (self.usage_results(records, "model" in groups) if path == USAGE
                       else self.cost_results(records, "line_item" in groups))
            buckets.append({"object": "bucket", "start_time": left, "end_time": right,
                            "results": results})
        next_page = (base64.urlsafe_b64encode(json_bytes([tag, stop])).decode().rstrip("=")
                     if stop < count else None)
        return {"object": "page", "data": buckets, "has_more": next_page is not None,
                "next_page": next_page}


def responses_stream(model, created_at):
    response = {"id": "resp_mock_pong", "object": "response", "created_at": created_at,
                "model": model, "status": "in_progress", "output": [], "error": None}
    completed = {**response, "status": "completed", "output": [
        {"id": "msg_mock_pong", "type": "message", "role": "assistant",
         "status": "completed", "content": [
             {"type": "output_text", "text": "pong", "annotations": []}]}],
        "usage": {"input_tokens": 12, "output_tokens": 1, "total_tokens": 13,
                  "input_tokens_details": {"cached_tokens": 0},
                  "output_tokens_details": {"reasoning_tokens": 0}}}
    events = [
        {"type": "response.created", "sequence_number": 0, "response": response},
        {"type": "response.output_text.delta", "sequence_number": 1,
         "item_id": "msg_mock_pong", "output_index": 0, "content_index": 0, "delta": "pong"},
        {"type": "response.completed", "sequence_number": 2, "response": completed},
    ]
    return b"".join(b"event: " + e["type"].encode() + b"\ndata: " + json_bytes(e) + b"\n\n"
                    for e in events)


class LineServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, relay, line, context):
        self.relay, self.line, self.context = relay, line, context
        super().__init__(("127.0.0.1", 0), Handler)

    def get_request(self):
        connection, address = super().get_request()
        try:
            connection.settimeout(10)
            if self.context is not None:
                # Handshake on the worker's first read, never in the accept loop.
                connection = self.context.wrap_socket(
                    connection, server_side=True, do_handshake_on_connect=False)
            return connection, address
        except Exception:
            connection.close()
            raise

    def handle_error(self, request, client_address):
        # A disconnected client or failed TLS handshake must not dump request data.
        print("mockrelay: connection handling failed", file=sys.stderr)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "MockKrill/1"
    sys_version = ""

    def log_message(self, format, *args):
        pass  # http.server's access log includes query strings; use our JSON log.

    def send_error(self, code, message=None, explain=None):
        # Also keep HTTP parser errors JSON and avoid reflecting untrusted input.
        if code == 501:  # A syntactically valid but unsupported HTTP method.
            self.handle_api()
            return
        time.sleep(self.server.relay.config.latency.get(self.server.line, 0) / 1000)
        self.reply(code, error("Invalid HTTP request"))

    def reply(self, status, payload, headers=None):
        headers = headers or {}
        body = payload if isinstance(payload, bytes) else json_bytes(payload)
        try:
            path = urlsplit(getattr(self, "path", "")).path
        except ValueError:
            path = "/<invalid-request-target>"
        path = path.replace(self.server.relay.config.api_key, "[redacted]")
        self.server.relay.log(self.server.line, getattr(self, "command", ""), path, status)
        self.close_connection = True
        try:
            self.send_response(status)
            self.send_header("Content-Type", headers.get("Content-Type", "application/json"))
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.send_header("Access-Control-Allow-Origin", "*")
            for name, value in headers.items():
                if name != "Content-Type":
                    self.send_header(name, value)
            self.end_headers()
            if getattr(self, "command", "") != "HEAD":
                self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError, TimeoutError, ssl.SSLError):
            pass

    def read_json(self):
        lengths = self.headers.get_all("Content-Length", [])
        if len(lengths) != 1 or self.headers.get("Transfer-Encoding"):
            raise BadRequest("A single Content-Length is required")
        size = integer(lengths[0], "Content-Length", 1, 1_048_576)
        body = self.rfile.read(size)
        if len(body) != size:
            raise BadRequest("Incomplete request body")
        try:
            value = json.loads(body)
        except (ValueError, UnicodeError):
            raise BadRequest("Request body must be JSON") from None
        if not isinstance(value, dict):
            raise BadRequest("Request body must be a JSON object")
        return value

    def route(self):
        relay = self.server.relay
        parsed = urlsplit(self.path)
        path = parsed.path
        if self.command == "GET" and path.startswith("/linux/deb/dists/stable/main/binary-") \
                and path.endswith("/Packages"):
            arch = path.split("/binary-", 1)[1].split("/", 1)[0]
            return 200, apt_packages(arch), {"Content-Type": "text/plain"}
        if self.command == "GET" and path.startswith(("/linux/rpm/", "/windows-release.json")):
            return 404, error("Endpoint not found", "not_found_error"), {}
        # Interception precedes origin authentication, including unauth probes.
        if self.server.line in relay.config.edge_block and (
                path == WHOAMI or path.startswith("/backend-api/wham/")):
            return 200, b"", {"l-block": "Filter|MockKrill"}
        if self.headers.get_all("Authorization", []) != ["Bearer " + relay.config.api_key]:
            # The latency probe expects this JSON 401 (not a successful catalog).
            return 401, error("Invalid API key", "invalid_api_key"), {}
        fault = relay.fault(path)
        if fault is not None:
            if fault == 429:
                return 429, error("Usage rate limit exceeded", "rate_limit_exceeded",
                                  "rate_limit_error"), {"Retry-After": "1800"}
            return fault, error("Endpoint not found" if fault == 404 else "Injected failure",
                                "not_found_error" if fault == 404 else "server_error"), {}
        if self.headers.get("Upgrade", "").lower() == "websocket":
            return 426, error("Use HTTP streaming for Responses", "unsupported_protocol"), {
                "Upgrade": "HTTP/1.1"}
        try:
            query = parse_qs(parsed.query, keep_blank_values=True, strict_parsing=True,
                             max_num_fields=64, errors="strict")
        except ValueError:
            raise BadRequest("Invalid query string") from None
        if self.command == "POST":
            if path in RESPONSE_PATHS or path.removesuffix("/compact") in RESPONSE_PATHS:
                body = self.read_json()
                model = body.get("model", MODELS[0][0])
                if not isinstance(model, str) or not model.strip():
                    raise BadRequest("model must be a nonempty string")
                if path.endswith("/compact"):
                    return 200, {"object": "response.compaction", "output": []}, {}
                return 200, responses_stream(model, relay.data.now), {
                    "Content-Type": "text/event-stream", "Cache-Control": "no-cache"}
        elif self.command == "GET":
            if path in MODEL_PATHS:
                fields(query, {"client_version"})
                if "client_version" in query:
                    one(query, "client_version")
                if path == "/backend-api/codex/models":
                    return 200, {"models": [codex_entry(i, m, n) for i, (m, n, _)
                                            in enumerate(MODELS)]}, {}
                return 200, {"object": "list", "data": [
                    {"id": m, "object": "model", "created": 0, "owned_by": "mock-krill",
                     "context_length": CONTEXT.get(m, 200_000)}
                    for m in [m for m, _, _ in MODELS] + EXTRA_MODELS]}, {}
            if path == WHOAMI:
                return 200, {"email": "dev@example.com", "chatgpt_user_id": "user-mock-dev",
                             "chatgpt_account_id": ACCOUNT_ID, "chatgpt_plan_type": "pro",
                             "chatgpt_account_is_fedramp": False}, {}
            if path == ACCOUNTS:
                return 200, {"workspace_backend_origin": "NO_CONSTRAINT", "accounts": [
                    {"id": ACCOUNT_ID, "name": "Personal",
                     "workspace_backend_origin": "NO_CONSTRAINT"}]}, {}
            if path == SETTINGS:
                return 200, {"training_opt_out": True}, {}
            billing_path = path.removeprefix("/v1")
            if relay.config.billing and billing_path == "/dashboard/billing/subscription":
                return 200, {"object": "billing_subscription", "has_payment_method": True,
                             "soft_limit_usd": 240.0, "hard_limit_usd": 300.0,
                             "system_hard_limit_usd": 300.0,
                             "access_until": relay.data.now + 23 * DAY,
                             "plan": {"title": "Pro", "id": "pro-monthly"}}, {}
            if relay.config.billing and billing_path == "/dashboard/billing/usage":
                return 200, relay.data.billing_usage(query), {}
            if path in (USAGE, COSTS):
                # One shared counter across APIs and listeners, independent of paging.
                if not relay.take_usage_request():
                    return 429, error("Usage rate limit exceeded", "rate_limit_exceeded",
                                      "rate_limit_error"), {"Retry-After": "1800"}
                if path != COSTS or relay.config.costs:
                    return 200, relay.data.page(path, query), {}
        return 404, error("Endpoint not found", "not_found_error"), {}

    def handle_api(self):
        time.sleep(self.server.relay.config.latency.get(self.server.line, 0) / 1000)
        try:
            status, payload, headers = self.route()
        except BadRequest as exc:
            status, payload, headers = 400, error(str(exc)), {}
        except (ValueError, UnicodeError):
            status, payload, headers = 400, error("Invalid request"), {}
        self.reply(status, payload, headers)

    do_GET = do_POST = do_HEAD = do_OPTIONS = handle_api
    do_PUT = do_PATCH = do_DELETE = do_TRACE = do_CONNECT = handle_api


class Relay:
    def __init__(self, config, *, now=None):
        self.config = config
        self.data = SyntheticData(config.seed, now)
        self.servers = []
        self.threads = []
        self.ports = {}
        self.usage_count = 0
        self.usage_lock = threading.Lock()
        self.log_lock = threading.Lock()
        self.log_file = None

    def fault(self, path):
        """Status forced for `path` by --fault-file (read per request), if any.
        Keys are paths; a key matches with or without its "/v1" prefix."""
        if not self.config.fault_file:
            return None
        try:
            faults = json.loads(Path(self.config.fault_file).read_text(encoding="utf-8") or "{}")
        except (OSError, ValueError):
            return None
        bare = path.removeprefix("/v1")
        for key, status in faults.items() if isinstance(faults, dict) else ():
            if (key == path or key.removeprefix("/v1") == bare) and isinstance(status, int):
                return status
        return None

    def take_usage_request(self):
        with self.usage_lock:
            self.usage_count += 1
            return (self.config.usage_limit is None
                    or self.usage_count <= self.config.usage_limit)

    def log(self, line, method, path, status):
        entry = {"ts": round(time.time(), 3), "line": line, "method": method,
                 "path": path, "status": status}
        with self.log_lock:
            if self.log_file is not None:
                self.log_file.write(json.dumps(entry, separators=(",", ":")) + "\n")
                self.log_file.flush()

    def publish_ports(self):
        if self.config.ports_file is None:  # In-memory self-test.
            return
        path = Path(self.config.ports_file)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                             prefix="." + path.name + ".", delete=False) as out:
                temporary = Path(out.name)
                json.dump(self.ports, out)
                out.write("\n")
                out.flush()
                os.fsync(out.fileno())
            os.replace(temporary, path)
        finally:
            if temporary is not None and temporary.exists():
                temporary.unlink()

    def __enter__(self):
        try:
            context = None
            if self.config.cert:
                context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                context.load_cert_chain(self.config.cert, self.config.key)
                context.set_alpn_protocols(["http/1.1"])
            if self.config.log:
                self.log_file = open(self.config.log, "a", encoding="utf-8")
            for line in self.config.lines:
                server = LineServer(self, line, context)
                self.servers.append(server)
                self.ports[line] = server.server_address[1]
            for server in self.servers:
                thread = threading.Thread(target=server.serve_forever,
                                          kwargs={"poll_interval": 0.05}, daemon=True)
                thread.start()
                self.threads.append(thread)
            self.publish_ports()
            return self
        except BaseException:
            self.close()
            raise

    def close(self):
        for server in self.servers[:len(self.threads)]:
            server.shutdown()
        for server in self.servers:
            server.server_close()
        for thread in self.threads:
            thread.join()
        self.servers.clear()
        self.threads.clear()
        with self.log_lock:
            if self.log_file is not None:
                self.log_file.close()
                self.log_file = None
        # Leave the ports file and append-only request log for the caller to inspect.

    def __exit__(self, *exc):
        self.close()


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--lines", help="comma-separated Superba line IDs")
    parser.add_argument("--ports-file", help="atomically published JSON port map")
    parser.add_argument("--host", choices=["127.0.0.1"], default="127.0.0.1")
    parser.add_argument("--cert")
    parser.add_argument("--key", help="TLS certificate private key file")
    parser.add_argument("--api-key", help="required bearer key (never logged)")
    parser.add_argument("--latency", default="", help="milliseconds, e.g. a=38,b=96,c=154")
    parser.add_argument("--edge-block", action="append", default=[], metavar="LINE")
    parser.add_argument("--usage-limit", type=int)
    parser.add_argument("--costs", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--billing", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--log", help="append request metadata as JSON lines")
    parser.add_argument("--fault-file", help="JSON {path: status} read per request (e2e: "
                        "404 hides an endpoint, 429 rate-limits it)")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--extra-model", action="append", default=[], metavar="ID",
                        help="another id of the OpenAI-style /v1/models list")
    args = parser.parse_args(argv)
    if args.self_test:
        return args
    if not args.lines or not args.ports_file or not args.api_key:
        parser.error("--lines, --ports-file and --api-key are required")
    if any(not 33 <= ord(c) <= 126 for c in args.api_key):
        parser.error("--api-key must contain only printable ASCII without whitespace")
    args.lines = [line.strip() for line in args.lines.split(",")]
    for line in args.lines:
        if (not line or len(line) > 64 or line[0] not in "abcdefghijklmnopqrstuvwxyz0123456789"
                or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789._-" for c in line)):
            parser.error("line IDs must match Superba's lowercase ASCII ID format")
    if len(set(args.lines)) != len(args.lines):
        parser.error("--lines contains duplicate IDs")
    if set(args.edge_block) - set(args.lines):
        parser.error("--edge-block must name a configured line")
    if bool(args.cert) != bool(args.key):
        parser.error("--cert and --key must be supplied together")
    if args.usage_limit is not None and args.usage_limit < 0:
        parser.error("--usage-limit must be nonnegative")
    if args.log and Path(args.log).resolve() == Path(args.ports_file).resolve():
        parser.error("--log and --ports-file must be different files")
    latency = {}
    for pair in args.latency.split(",") if args.latency else []:
        try:
            line, raw = pair.split("=", 1)
            line, ms = line.strip(), float(raw)
            if line not in args.lines or line in latency or not math.isfinite(ms) or ms < 0:
                raise ValueError
            latency[line] = ms
        except ValueError:
            parser.error("--latency must list unique configured lines with finite nonnegative ms")
    args.latency = latency
    return args


def self_test():
    from urllib.error import HTTPError
    from urllib.request import ProxyHandler, Request, build_opener

    test_key = "sk-mockrelay-self-test"
    now = int(datetime(2026, 10, 14, 12, tzinfo=timezone.utc).timestamp())
    today = now // DAY * DAY
    start, end = today - 6 * DAY, today + DAY
    opener = build_opener(ProxyHandler({}))

    def check(condition, message):
        if not condition:
            raise AssertionError(message)

    def config(*extra):
        result = parse_args(["--lines", "a,b", "--ports-file", "unused-self-test-ports",
                             "--api-key", test_key, *extra])
        result.ports_file = None
        return result

    def request(relay, path, *, line="a", key=test_key, status=200, body=None, headers=None):
        hdr = dict(headers or {})
        if key is not None:
            hdr["Authorization"] = "Bearer " + key
        data = None if body is None else json_bytes(body)
        if data is not None:
            hdr["Content-Type"] = "application/json"
        req = Request(f"http://127.0.0.1:{relay.ports[line]}{path}", data=data, headers=hdr)
        try:
            response = opener.open(req, timeout=5)
        except HTTPError as exc:
            response = exc
        with response:
            content = response.read()
            check(response.status == status,
                  f"{req.get_method()} {path}: expected HTTP {status}, got {response.status}")
            check(int(response.headers["Content-Length"]) == len(content), "Content-Length mismatch")
            check(response.headers["Access-Control-Allow-Origin"] == "*", "missing CORS header")
            return response.headers, content

    def get(relay, path, **kwargs):
        headers, body = request(relay, path, **kwargs)
        check(headers.get_content_type() == "application/json", "expected JSON content type")
        return json.loads(body)

    def page_url(path, *, limit=7, page=None, groups=None, left=start, right=end):
        pairs = [("start_time", left), ("end_time", right), ("bucket_width", "1d"), ("limit", limit)]
        if page is not None:
            pairs.append(("page", page))
        if groups is not None:
            pairs.extend(("group_by", group) for group in groups)
        return path + "?" + urlencode(pairs)

    def all_pages(relay, path, *, limit, groups=None, left=start, right=end):
        rows, sizes, seen = [], [], set()
        cursor = None
        while True:
            page = get(relay, page_url(path, limit=limit, page=cursor, groups=groups,
                                       left=left, right=right))
            check(page["object"] == "page", "missing page object")
            rows.extend(page["data"])
            sizes.append(len(page["data"]))
            if not page["has_more"]:
                check(page["next_page"] is None, "final page has a cursor")
                return rows, sizes
            cursor = page["next_page"]
            check(isinstance(cursor, str) and cursor and cursor not in seen, "invalid/repeated cursor")
            seen.add(cursor)
            check(len(seen) < 30, "pagination did not finish")

    def cost_micros(bucket, api):
        return sum(round((r["cost_usd"] if api == USAGE else r["amount"]["value"]) * 1_000_000)
                   for r in bucket["results"])

    with Relay(config("--edge-block", "b", "--latency", "a=5"), now=now) as relay:
        check(len(set(relay.ports.values())) == 2, "lines must bind distinct ports")
        for line in ("a", "b"):
            for path in MODEL_PATHS:
                t0 = time.monotonic()
                headers, body = request(relay, path, line=line, key=None, status=401)
                check("l-block" not in headers and isinstance(json.loads(body), dict),
                      "latency probe must be healthy JSON 401")
                if line == "a":
                    check(time.monotonic() - t0 >= 0.005, "configured latency was not applied")
        protected = (*MODEL_PATHS, WHOAMI, ACCOUNTS, SETTINGS, USAGE, COSTS,
                     "/v1/dashboard/billing/subscription", "/v1/dashboard/billing/usage")
        for path in protected:
            for key in (None, "wrong-key"):
                denied = get(relay, path, key=key, status=401)
                check(denied == error("Invalid API key", "invalid_api_key"), "wrong auth error shape")
        who = get(relay, WHOAMI)
        check(who["email"] == "dev@example.com" and who["chatgpt_account_id"] == ACCOUNT_ID,
              "wrong whoami identity")
        check(all(isinstance(who[k], str) and who[k] for k in (
            "chatgpt_user_id", "chatgpt_account_id", "chatgpt_plan_type")), "missing whoami strings")
        check(who["chatgpt_account_is_fedramp"] is False and "plan" not in who
              and "plan_name" not in who, "wrong whoami compatibility fields")
        accounts = get(relay, ACCOUNTS)
        check(accounts["accounts"] and accounts["workspace_backend_origin"] == "NO_CONSTRAINT",
              "accounts/check contract mismatch")
        get(relay, SETTINGS)
        for path in MODEL_PATHS:
            catalog = get(relay, path + "?client_version=0.162.0", line="b")
            names = {m.get("slug", m.get("id")) for m in catalog.get("models", catalog.get("data", []))}
            check(names == {m for m, _, _ in MODELS}, "model catalog mismatch")
        for path in (WHOAMI, ACCOUNTS, SETTINGS, "/backend-api/wham/other"):
            for key in (None, test_key):
                headers, body = request(relay, path, line="b", key=key)
                check(body == b"" and headers["l-block"], "edge interception mismatch")

        whole = get(relay, page_url(USAGE, groups=["model"]))["data"]
        paged, sizes = all_pages(relay, USAGE, limit=3, groups=["model"])
        check(sizes == [3, 3, 1] and paged == whole, "usage pagination changed data")
        check(get(relay, page_url(USAGE, groups=["model"]), line="b")["data"] == whole,
              "usage data differs between lines")
        costs, sizes = all_pages(relay, COSTS, limit=3)
        check(sizes == [3, 3, 1], "cost pagination mismatch")
        ungrouped = get(relay, page_url(USAGE))["data"]
        repeated = get(relay, page_url(USAGE, groups=["model", "model"]))["data"]
        comma = get(relay, page_url(USAGE, groups=["model,model"]))["data"]
        check(repeated == whole == comma, "repeatable/comma-separated grouping mismatch")
        grouped_costs = get(relay, page_url(COSTS, groups=["line_item", "project_id"]))["data"]
        check(grouped_costs == get(relay, page_url(COSTS, groups=["line_item,project_id"]))["data"],
              "cost grouping forms disagree")
        for index, (usage, cost, combined, itemized) in enumerate(zip(whole, costs, ungrouped, grouped_costs)):
            check(usage["start_time"] == start + index * DAY
                  and usage["end_time"] == start + (index + 1) * DAY, "incorrect UTC bucket")
            check(cost_micros(usage, USAGE) == cost_micros(cost, COSTS)
                  == cost_micros(combined, USAGE) == cost_micros(itemized, COSTS),
                  "daily costs disagree between APIs/groupings")
            check(len(combined["results"]) == 1 and combined["results"][0]["model"] is None,
                  "ungrouped usage must aggregate models")
            check(cost["results"][0]["line_item"] is None, "ungrouped line_item must be null")
            for field in COUNTS:
                check(sum(r[field] for r in usage["results"]) == combined["results"][0][field],
                      "ungrouped token/request totals differ")
            for row in usage["results"]:
                check(all(type(row[k]) is int and row[k] >= 0 for k in COUNTS), "invalid counts")
                check(0.39 <= row["input_cached_tokens"] / row["input_tokens"] <= 0.70,
                      "cached input share out of range")
                check(round(row["cost_usd"] * 1_000_000)
                      == row["input_tokens"] * 5 // 4 + row["output_tokens"] * 10,
                      "token prices do not reconcile")
        for prefix in ("/v1", ""):
            sub = get(relay, prefix + "/dashboard/billing/subscription", line="b")
            check(sub["hard_limit_usd"] == 300.0 and sub["soft_limit_usd"] == 240.0
                  and sub["plan"] == {"title": "Pro", "id": "pro-monthly"}
                  and sub["access_until"] == now + 23 * DAY, "billing subscription mismatch")
            query = urlencode({"start_date": datetime.fromtimestamp(start, timezone.utc).date().isoformat(),
                               "end_date": datetime.fromtimestamp(end, timezone.utc).date().isoformat()})
            bill = get(relay, prefix + "/dashboard/billing/usage?" + query)
            check(bill["total_usage"] == round(sum(cost_micros(b, COSTS) for b in costs) / 10_000, 2),
                  "billing cents do not equal range costs")

        history, sizes = all_pages(relay, USAGE, limit=31, groups=["model"], left=today - 59 * DAY)
        check(sizes == [31, 29] and all(not b["results"] for b in history[:14])
              and all(b["results"] for b in history[14:]), "45-day history boundary mismatch")
        future = get(relay, page_url(USAGE, left=end, right=end + DAY))["data"]
        check(future[0]["results"] == [], "future days must be empty")
        partial = get(relay, page_url(USAGE, groups=["model"], left=start + 3600, right=start + DAY - 3600))
        partial_cost = get(relay, page_url(COSTS, left=start + 3600, right=start + DAY - 3600))
        check(partial["data"][0]["start_time"] == start + 3600
              and partial["data"][0]["end_time"] == start + DAY - 3600
              and cost_micros(partial["data"][0], USAGE) == cost_micros(partial_cost["data"][0], COSTS),
              "partial-day bounds/cost mismatch")
        first = get(relay, page_url(USAGE, limit=3, groups=["model"]))
        bad = [USAGE, USAGE + "?start_time=nope", USAGE + "?start_time=-1",
               page_url(USAGE, left=end, right=start), page_url(USAGE, limit=0),
               page_url(USAGE, limit=32), page_url(USAGE).replace("1d", "1h"),
               page_url(USAGE, groups=["unknown"]), page_url(COSTS, groups=["model"]),
               page_url(USAGE, page="broken!"), page_url(USAGE) + "&start_time=1",
               page_url(COSTS, page=first["next_page"]),
               page_url(USAGE, page=first["next_page"], groups=["model"], left=start + DAY),
               "/v1/dashboard/billing/usage?start_date=2026-02-30&end_date=2026-03-01",
               "/v1/dashboard/billing/usage?start_date=20260101&end_date=2026-01-02",
               "/v1/dashboard/billing/usage?start_date=2026-01-01&end_date=2026-04-12"]
        for path in bad:
            check(get(relay, path, status=400)["error"]["type"] == "invalid_request_error",
                  "bad queries must get an OpenAI-style error")
        get(relay, "/not-a-route", status=404)
        for path in RESPONSE_PATHS:
            body = {"model": "gpt-5.5", "instructions": "Reply with exactly one word: pong.",
                    "input": [{"type": "message", "role": "user", "content": [
                        {"type": "input_text", "text": "ping"}]}], "stream": True, "store": False}
            headers, stream = request(relay, path, line="b", body=body)
            check(headers.get_content_type() == "text/event-stream" and stream.endswith(b"\n\n"),
                  "invalid SSE framing")
            events = [json.loads(line[6:]) for line in stream.splitlines() if line.startswith(b"data: ")]
            check([e["type"] for e in events] == ["response.created", "response.output_text.delta",
                                                 "response.completed"], "wrong SSE event sequence")
            check(events[1]["delta"] == "pong" and events[-1]["response"]["usage"]["total_tokens"] == 13,
                  "missing pong/completion usage")
            check(get(relay, path + "/compact", body=body)["output"] == [], "compact probe failed")
            request(relay, path, headers={"Upgrade": "websocket", "Connection": "Upgrade"}, status=426)

    with Relay(config("--usage-limit", "2"), now=now) as relay:
        get(relay, page_url(USAGE), key="wrong-key", status=401)
        get(relay, "/v1/dashboard/billing/subscription")
        get(relay, page_url(USAGE), line="a")
        get(relay, page_url(COSTS), line="b")
        for path, line in ((USAGE, "b"), (COSTS, "a")):
            headers, body = request(relay, page_url(path), line=line, status=429)
            check(headers["Retry-After"] == "1800"
                  and json.loads(body)["error"]["code"] == "rate_limit_exceeded",
                  "shared usage rate limit mismatch")
        get(relay, "/v1/models")

    with Relay(config(), now=now) as relay:
        # Codex-native list with tiers; OpenAI list with context metadata.
        native = get(relay, "/backend-api/codex/models?client_version=0.162.0")["models"]
        check([m["slug"] for m in native] == [m for m, _, _ in MODELS], "native slugs")
        tiers = {m["slug"]: [t["id"] for t in m["service_tiers"]] for m in native}
        check(tiers["gpt-6-astra"] == ["priority", "ultrafast"], "astra tiers")
        check(tiers["gpt-5.5"] == ["priority"], "fast only elsewhere")
        listed = get(relay, "/v1/models")["data"]
        check(all(m["context_length"] == CONTEXT[m["id"]] for m in listed), "list metadata")
        # App downloads: public, no key.
        _, text = request(relay, "/linux/deb/dists/stable/main/binary-amd64/Packages", key=None)
        check(f"Version: {APP_VERSION}" in text.decode(), "APT index")
        request(relay, "/linux/rpm/x86_64/repodata/repomd.xml", key=None, status=404)

    with Relay(config("--no-costs", "--no-billing"), now=now) as relay:
        get(relay, page_url(COSTS), status=404)
        for prefix in ("/v1", ""):
            for endpoint in ("subscription", "usage"):
                get(relay, prefix + "/dashboard/billing/" + endpoint, status=404)
        check(get(relay, page_url(USAGE, groups=["model"]))["data"] == whole,
              "fallback data changed with costs/billing disabled or after restart")
    print("self-test ok")


def main(argv=None):
    args = parse_args(argv)
    EXTRA_MODELS.extend(args.extra_model)
    if args.self_test:
        try:
            self_test()
            return 0
        except Exception as exc:
            print(f"self-test failed: {type(exc).__name__}: {exc}", file=sys.stderr)
            return 1
    stop = threading.Event()
    previous = {sig: signal.signal(sig, lambda _signum, _frame: stop.set())
                for sig in (signal.SIGTERM, signal.SIGINT)}
    try:
        with Relay(args):
            stop.wait()
        return 0
    except (OSError, ValueError) as exc:
        print(f"mockrelay: {exc}", file=sys.stderr)
        return 1
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    sys.exit(main())
