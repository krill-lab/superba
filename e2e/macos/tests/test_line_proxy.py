import json
import socket
import socketserver
import tempfile
import threading
import unittest
from pathlib import Path

from e2e.macos.guest.line_proxy import LineProxy


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.sendall(self.request.recv(4096))


class LineProxyTests(unittest.TestCase):
    def test_both_virtual_lines_tunnel_bytes_without_logging_them(self):
        with tempfile.TemporaryDirectory() as directory, socketserver.ThreadingTCPServer(("127.0.0.1", 0), Echo) as relay:
            threading.Thread(target=relay.serve_forever, daemon=True).start()
            log = Path(directory) / "connections.jsonl"
            proxy = LineProxy("https://127.0.0.1:" + str(relay.server_address[1]), log)
            threading.Thread(target=proxy.serve_forever, daemon=True).start()
            try:
                for port in (18443, 19443):
                    with socket.create_connection(proxy.server_address, timeout=3) as client:
                        client.sendall(f"CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: ignored\r\n\r\n".encode())
                        response = b""
                        while b"\r\n\r\n" not in response:
                            response += client.recv(4096)
                        self.assertIn(b"200 Connection established", response)
                        client.sendall(b"opaque-tls-test-credential")
                        self.assertEqual(client.recv(4096), b"opaque-tls-test-credential")
                entries = [json.loads(line) for line in log.read_text().splitlines()]
                self.assertEqual([entry["line"] for entry in entries], ["a", "b"])
                self.assertNotIn("credential", log.read_text())
            finally:
                proxy.shutdown()
                proxy.server_close()
                relay.shutdown()

    def test_proxy_refuses_a_different_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "connections.jsonl"
            proxy = LineProxy("https://relay.invalid", log)
            threading.Thread(target=proxy.serve_forever, daemon=True).start()
            try:
                with socket.create_connection(proxy.server_address, timeout=3) as client:
                    client.sendall(b"CONNECT other.invalid:18443 HTTP/1.1\r\n\r\n")
                    self.assertIn(b"403", client.recv(4096))
                self.assertFalse(log.exists())
            finally:
                proxy.shutdown()
                proxy.server_close()

    def test_only_an_https_origin_can_be_the_upstream(self):
        for upstream in ("http://relay.invalid", "https://key" + chr(64) + "relay.invalid", "https://relay.invalid/path"):
            with self.subTest(upstream=upstream), self.assertRaises(ValueError):
                LineProxy(upstream, "unused")
