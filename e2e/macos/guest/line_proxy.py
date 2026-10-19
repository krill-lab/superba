#!/usr/bin/env python3
"""Two HTTPS test lines through a loopback CONNECT proxy.

TLS stays end-to-end with the designated relay. Virtual origin ports distinguish
the lines without installing a CA or modifying DNS, hosts or trust settings.
Only connection counts are recorded; TLS bytes and credentials are never logged.
"""

import json
import select
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit


class LineProxy(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, upstream, log):
        parsed = urlsplit(upstream)
        if (parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password
                or parsed.path not in ("", "/") or parsed.query or parsed.fragment):
            raise ValueError("The test relay must be an HTTPS origin without credentials")
        self.upstream = (parsed.hostname.lower(), parsed.port or 443)
        self.ports = {18443: "a", 19443: "b"}
        self.log = Path(log)
        self.log_lock = threading.Lock()
        super().__init__(("127.0.0.1", 0), Handler)

    def record(self, line):
        with self.log_lock, self.log.open("a") as stream:
            stream.write(json.dumps({"line": line, "event": "connected", "time": time.time()}) + "\n")

    def origins(self):
        hostname = self.upstream[0]
        if ":" in hostname:
            hostname = "[" + hostname + "]"
        return {line: "https://{}:{}".format(hostname, port) for port, line in self.ports.items()}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_CONNECT(self):
        try:
            authority = urlsplit("https://" + self.path)
            line = self.server.ports.get(authority.port)
            if (authority.hostname != self.server.upstream[0] or not line
                    or authority.username or authority.password or authority.path
                    or authority.query or authority.fragment):
                self.send_error(403)
                return
            upstream = socket.create_connection(self.server.upstream, timeout=15)
        except (ValueError, OSError):
            self.send_error(502)
            return
        self.send_response(200, "Connection established")
        self.end_headers()
        self.wfile.flush()
        self.server.record(line)
        self.close_connection = True
        try:
            with upstream:
                peers = [self.connection, upstream]
                while True:
                    readable, _, _ = select.select(peers, [], [], 120)
                    if not readable:
                        return
                    for source in readable:
                        data = source.recv(65536)
                        if not data:
                            return
                        (upstream if source is self.connection else self.connection).sendall(data)
        except OSError:
            pass


def main():
    root = Path(sys.argv[1])
    settings = json.loads((root / "home/line-proxy.json").read_text())
    proxy = LineProxy(settings["upstream"], root / "home/line-connections.jsonl")
    (root / "home/line-proxy-ready.json").write_text(json.dumps({
        "proxy": "http://127.0.0.1:" + str(proxy.server_port), "origins": proxy.origins()}))
    proxy.serve_forever()


if __name__ == "__main__":
    main()
