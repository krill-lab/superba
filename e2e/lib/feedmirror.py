"""Local https mirrors of Superba's signed feeds, with fault modes (scenario s17),
plus a desktop-app download fixture (scenario s23).

    python3 feedmirror.py --valid DIR --stale DIR --app DIR --ports-file F \\
        --cert CERT --key KEY --log LOG --control CONTROL

Four mirror listeners (`m1` .. `m4`) and one app-download listener (`app`), each
on its own random loopback port, so Superba sees four mirrors with different
names (`127.0.0.1:<port>`; the race remembers the winner by name). A mirror's
behaviour is read from the CONTROL JSON file on every request
(`{"m1": "404", "m2": "html", ...}`, a missing entry = `valid`):

    valid      serve the feed tree --valid (the newer serial)
    stale      serve the feed tree --stale (a lower serial, e.g. a stale CDN copy)
    404        answer 404 to everything
    html       answer 200 with an HTML landing page (captive portal / CDN error page)
    truncated  serve --valid, but every file except index.json cut in half
    down       drop the connection without an answer

Feed files are served under `/feeds/<path>` (the mirror base Superba gets is
`https://127.0.0.1:<port>/feeds/`). The `app` listener serves the --app directory
as the desktop app download base (SUPERBA_APP_DOWNLOADS): an APT `Packages` index
and a small fixture package; anything else is 404 (no RPM repository, so the
Linux check uses the deb index only).

Every request is one JSON line in LOG (listener, mode, path, status, bytes). The
ports file `{"m1": port, ..., "app": port}` is written once every listener is up.
Nothing here holds or logs a secret; the trees contain only public, signed data.
"""

import argparse
import json
import os
import ssl
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

MIRRORS = ("m1", "m2", "m3", "m4")
LANDING = (b"<!DOCTYPE html><html><head><title>Welcome</title></head>"
           b"<body><h1>It works!</h1><p>This mirror is not configured.</p></body></html>\n")
TYPES = {".json": "application/json", ".sig": "application/octet-stream", ".md": "text/markdown",
         ".deb": "application/vnd.debian.binary-package"}


def parse_args(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--valid", required=True, type=Path)
    p.add_argument("--stale", required=True, type=Path)
    p.add_argument("--app", required=True, type=Path)
    p.add_argument("--ports-file", required=True, type=Path)
    p.add_argument("--cert", required=True)
    p.add_argument("--key", required=True)
    p.add_argument("--log", required=True, type=Path)
    p.add_argument("--control", required=True, type=Path)
    return p.parse_args(argv)


class Mirror(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, name, cfg, ctx):
        super().__init__(("127.0.0.1", 0), Handler)
        self.name = name
        self.cfg = cfg
        self.ctx = ctx

    def get_request(self):
        sock, addr = super().get_request()
        sock.settimeout(30)
        return self.ctx.wrap_socket(sock, server_side=True), addr

    def handle_error(self, request, client_address):
        pass  # TLS handshakes cut short by a racing client are expected

    def mode(self):
        if self.name == "app":
            return "app"
        try:
            control = json.loads(self.cfg.control.read_text())
        except (OSError, json.JSONDecodeError):
            control = {}
        return control.get(self.name, "valid")


LOCK = threading.Lock()


def log(cfg, **kw):
    entry = {"ts": round(time.time(), 3), **kw}
    with LOCK, open(cfg.log, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry) + "\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "feedmirror"

    def log_message(self, format, *args):
        pass

    def do_HEAD(self):
        self.serve(head=True)

    def do_GET(self):
        self.serve(head=False)

    def reply(self, status, body, ctype="text/plain", head=False):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        if not head:
            self.wfile.write(body)
        return status, len(body)

    def serve(self, head):
        srv = self.server
        mode = srv.mode()
        path = self.path.split("?", 1)[0]
        status, size = self.answer(srv, mode, path, head)
        log(srv.cfg, listener=srv.name, mode=mode, method=self.command, path=path, status=status, bytes=size)

    def answer(self, srv, mode, path, head):
        if mode == "down":
            self.close_connection = True
            try:
                self.connection.shutdown(2)
            except OSError:
                pass
            return "dropped", 0
        if mode == "404":
            return self.reply(404, b"not found\n", head=head)
        if mode == "html":
            return self.reply(200, LANDING, "text/html; charset=utf-8", head)
        if mode == "app":
            root, rel = srv.cfg.app, path.lstrip("/")
        else:
            if not path.startswith("/feeds/"):
                return self.reply(404, b"not found\n", head=head)
            root = srv.cfg.stale if mode == "stale" else srv.cfg.valid
            rel = path[len("/feeds/"):]
        target = (root / rel).resolve()
        if not rel or ".." in Path(rel).parts or root.resolve() not in target.parents or not target.is_file():
            return self.reply(404, b"not found\n", head=head)
        body = target.read_bytes()
        if mode == "truncated" and target.name != "index.json":
            body = body[: len(body) // 2]
        return self.reply(200, body, TYPES.get(target.suffix, "application/octet-stream"), head)


def main(argv=None):
    cfg = parse_args(argv)
    ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    ctx.load_cert_chain(cfg.cert, cfg.key)
    servers = {name: Mirror(name, cfg, ctx) for name in (*MIRRORS, "app")}
    for srv in servers.values():
        threading.Thread(target=srv.serve_forever, daemon=True).start()
    ports = {name: srv.server_address[1] for name, srv in servers.items()}
    tmp = cfg.ports_file.with_suffix(".tmp")
    tmp.write_text(json.dumps(ports))
    os.replace(tmp, cfg.ports_file)
    log(cfg, listener="all", started=ports)
    threading.Event().wait()


if __name__ == "__main__":
    main()
