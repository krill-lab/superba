"""Local https source of Codex release packages for the e2e run (scenarios s21,
s22), standing in for GitHub releases (SUPERBA_CODEX_PACKAGES=<base>).

    python3 pkgcache.py --cache DIR --ports-file F --cert CERT --key KEY \\
        --log LOG --control CONTROL

Serves `<base>/rust-v<version>/codex-package-<target>.tar.gz` and
`<base>/rust-v<version>/codex-package_SHA256SUMS` from a cache directory that
holds the files as `codex-package-<version>-<target>.tar.gz` and
`codex-package_SHA256SUMS-<version>` (see `ensure_cached`, which downloads a
missing package from GitHub once and checks it against the release's
SHA256SUMS). Range requests are answered (Superba resumes `.part` downloads).

CONTROL is a JSON file read on every request: `{"mode": "normal"}`, `"corrupt"`
(the package bytes with one byte flipped in every 1 MiB block, same size, so only
the checksum can tell) or `"down"` (404 for everything). Every request is one JSON
line in LOG (path, mode, status, range, bytes). Packages are public release
artifacts; nothing secret is served or logged.
"""

import argparse
import hashlib
import json
import os
import re
import ssl
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

GITHUB = "https://github.com/openai/codex/releases/download"
ROUTE = re.compile(r"^/rust-v(?P<ver>[0-9A-Za-z.+-]+)/(?P<file>codex-package(?:-(?P<target>[a-z0-9_-]+)\.tar\.gz|_SHA256SUMS))$")
LOCK = threading.Lock()


def cache_name(version, file):
    """Cache file of a release asset."""
    if file == "codex-package_SHA256SUMS":
        return f"codex-package_SHA256SUMS-{version}"
    target = file[len("codex-package-"):-len(".tar.gz")]
    return f"codex-package-{version}-{target}.tar.gz"


def ensure_cached(cache, version, target, log=print):
    """Make sure the cache holds `version`'s package for `target` and its
    SHA256SUMS (downloaded from GitHub once, verified); returns the package path."""
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    pkg_file = f"codex-package-{target}.tar.gz"
    sums = cache / cache_name(version, "codex-package_SHA256SUMS")
    if not sums.exists():
        log(f"pkgcache: downloading SHA256SUMS of {version}")
        with urllib.request.urlopen(f"{GITHUB}/rust-v{version}/codex-package_SHA256SUMS", timeout=60) as r:
            sums.write_bytes(r.read())
    want = None
    for line in sums.read_text().splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == pkg_file:
            want = parts[0].lower()
    if not want:
        raise RuntimeError(f"{sums} lists no {pkg_file}")
    pkg = cache / cache_name(version, pkg_file)
    if pkg.exists() and sha256(pkg) == want:
        return pkg
    log(f"pkgcache: downloading {pkg_file} {version} (verified against SHA256SUMS)")
    tmp = pkg.with_suffix(".part")
    with urllib.request.urlopen(f"{GITHUB}/rust-v{version}/{pkg_file}", timeout=600) as r, open(tmp, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    if sha256(tmp) != want:
        tmp.unlink()
        raise RuntimeError(f"{pkg_file} {version}: checksum mismatch")
    os.replace(tmp, pkg)
    return pkg


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def corrupt(body):
    b = bytearray(body)
    for i in range(4096, len(b), 1 << 20):
        b[i] ^= 0xFF
    return bytes(b)


def parse_args(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--cache", required=True, type=Path)
    p.add_argument("--ports-file", required=True, type=Path)
    p.add_argument("--cert", required=True)
    p.add_argument("--key", required=True)
    p.add_argument("--log", required=True, type=Path)
    p.add_argument("--control", required=True, type=Path)
    return p.parse_args(argv)


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, cfg, ctx):
        super().__init__(("127.0.0.1", 0), Handler)
        self.cfg = cfg
        self.ctx = ctx
        self.bodies = {}

    def get_request(self):
        sock, addr = super().get_request()
        sock.settimeout(60)
        return self.ctx.wrap_socket(sock, server_side=True), addr

    def handle_error(self, request, client_address):
        pass

    def mode(self):
        try:
            return json.loads(self.cfg.control.read_text()).get("mode", "normal")
        except (OSError, json.JSONDecodeError):
            return "normal"

    def body(self, path, mode):
        key = (str(path), mode)
        with LOCK:
            if key not in self.bodies:
                data = path.read_bytes()
                self.bodies[key] = corrupt(data) if mode == "corrupt" and path.suffix == ".gz" else data
            return self.bodies[key]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "pkgcache"

    def log_message(self, format, *args):
        pass

    def do_HEAD(self):
        self.serve(head=True)

    def do_GET(self):
        self.serve(head=False)

    def send(self, status, body, headers=(), head=False):
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head:
            self.wfile.write(body)

    def serve(self, head):
        srv = self.server
        mode = srv.mode()
        path = self.path.split("?", 1)[0]
        rng = self.headers.get("Range")
        status, size = self.answer(srv, mode, path, rng, head)
        entry = {"ts": round(time.time(), 3), "method": self.command, "path": path, "mode": mode,
                 "status": status, "range": rng, "bytes": size}
        with LOCK, open(srv.cfg.log, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry) + "\n")

    def answer(self, srv, mode, path, rng, head):
        m = ROUTE.match(path)
        target = srv.cfg.cache / cache_name(m["ver"], m["file"]) if m else None
        if mode == "down" or not target or not target.is_file():
            self.send(404, b"not found\n", [("Content-Type", "text/plain")], head)
            return 404, 0
        body = srv.body(target, mode)
        ctype = "application/gzip" if target.suffix == ".gz" else "text/plain"
        start, end = 0, len(body) - 1
        status = 200
        if rng:
            r = re.match(r"bytes=(\d+)-(\d*)$", rng.strip())
            if not r or int(r[1]) >= len(body):
                self.send(416, b"", [("Content-Range", f"bytes */{len(body)}")], head)
                return 416, 0
            start = int(r[1])
            end = min(int(r[2]), end) if r[2] else end
            status = 206
        part = body[start:end + 1]
        headers = [("Content-Type", ctype), ("Accept-Ranges", "bytes")]
        if status == 206:
            headers.append(("Content-Range", f"bytes {start}-{end}/{len(body)}"))
        self.send(status, part, headers, head)
        return status, len(part)


def main(argv=None):
    cfg = parse_args(argv)
    ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    ctx.load_cert_chain(cfg.cert, cfg.key)
    srv = Server(cfg, ctx)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    tmp = cfg.ports_file.with_suffix(".tmp")
    tmp.write_text(json.dumps({"port": srv.server_address[1]}))
    os.replace(tmp, cfg.ports_file)
    threading.Event().wait()


if __name__ == "__main__":
    main()
