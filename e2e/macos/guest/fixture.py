#!/usr/bin/env python3
"""Loopback-only model/notification fixture. No auth headers or prompts logged."""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

root = Path(sys.argv[1])


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, body, content_type="application/json"):
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.send({"object": "list", "data": [{"id": "e2e-model", "object": "model", "owned_by": "fixture"}]})

    def do_POST(self):
        size = min(int(self.headers.get("Content-Length", "0")), 2**20)
        body = json.loads(self.rfile.read(size) or b"{}")
        if self.path.endswith("/sendMessage"):
            # The test's own Telegram-shaped sink, no message leaves the VM.
            with (root / "notifications.jsonl").open("a") as stream:
                stream.write(json.dumps({"text": body.get("text", "")}) + "\n")
            self.send({"ok": True, "result": {"message_id": 1}})
            return
        with (root / "requests.jsonl").open("a") as stream:
            stream.write(json.dumps({"path": self.path, "model": body.get("model")}) + "\n")
        response = {"id": "resp_macos_fixture", "object": "response", "status": "completed",
                    "model": body.get("model", "e2e-model"),
                    "output": [{"id": "msg_fixture", "type": "message", "role": "assistant",
                                "status": "completed", "content": [{"type": "output_text", "text": "MACOS_OK",
                                                                      "annotations": []}]}],
                    "usage": {"input_tokens": 12, "output_tokens": 3, "total_tokens": 15}}
        if body.get("stream"):
            events = [{"type": "response.created", "response": dict(response, status="in_progress", output=[])},
                      {"type": "response.output_text.delta", "delta": "MACOS_OK", "output_index": 0,
                       "content_index": 0, "item_id": "msg_fixture"},
                      {"type": "response.completed", "response": response}]
            self.send("".join("event: {}\ndata: {}\n\n".format(event["type"], json.dumps(event))
                              for event in events), "text/event-stream")
        else:
            self.send(response)


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
(root / "fixture-port").write_text(str(server.server_port))
server.serve_forever()
