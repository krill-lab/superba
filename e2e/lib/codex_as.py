"""Create real Codex threads through `codex app-server` over stdio.

    python3 codex_as.py CODEX_BIN CWD PROMPT [PROMPT...]
    python3 codex_as.py --model-list CODEX_BIN CWD

The second form prints the app-server's `model/list` answer (what the desktop
app's model picker is built from) as one JSON document; nothing is sent upstream.

Used to build a legacy Codex home: with a config whose model_provider is a custom
id (e.g. "krill"), each PROMPT becomes one thread with one completed turn,
recorded by Codex itself in state_5.sqlite and sessions/**/rollout-*.jsonl. The
client identifies as the VS Code extension so the threads have the "vscode"
source the desktop app lists in its recents (it hides "exec" threads).
Prints one JSON line per thread: {"thread_id", "prompt", "reply"}.
"""

import json
import queue
import subprocess
import sys
import threading
import time


class AppServer:
    def __init__(self, codex, cwd):
        self.p = subprocess.Popen(
            [codex, "app-server"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            cwd=cwd,
            text=True,
            bufsize=1,
        )
        self.next_id = 0
        # A reader thread: select() on the pipe misses lines already pulled into
        # Python's buffer by an earlier readline() (seen as a 180 s hang).
        self.lines = queue.Queue()
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        for line in self.p.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def request(self, method, params, timeout=60):
        self.next_id += 1
        rid = self.next_id
        self.send({"id": rid, "method": method, "params": params})
        return self.wait(lambda m: m.get("id") == rid, timeout)

    def wait(self, pred, timeout):
        end = time.time() + timeout
        while time.time() < end:
            try:
                line = self.lines.get(timeout=max(0.1, end - time.time()))
            except queue.Empty:
                break
            if line is None:
                raise RuntimeError("app-server exited")
            msg = json.loads(line)
            if pred(msg):
                if "error" in msg:
                    raise RuntimeError(json.dumps(msg["error"]))
                return msg
        raise TimeoutError("app-server did not answer")

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=10)
        except Exception:
            self.p.kill()


def model_list(codex, cwd):
    s = AppServer(codex, cwd)
    try:
        s.request("initialize", {"clientInfo": {"name": "codex_vscode", "title": "Codex", "version": "0.162.0"}})
        s.send({"method": "initialized"})
        models, cursor = [], None
        for _ in range(20):
            params = {"includeHidden": False}
            if cursor:
                params["cursor"] = cursor
            res = s.request("model/list", params)["result"]
            models += res.get("data") or res.get("models") or []
            cursor = res.get("nextCursor")
            if not cursor:
                break
        print(json.dumps({"models": models}), flush=True)
    finally:
        s.close()


def main(argv):
    if len(argv) == 4 and argv[1] == "--model-list":
        model_list(argv[2], argv[3])
        return
    if len(argv) < 4:
        sys.exit(__doc__)
    codex, cwd, prompts = argv[1], argv[2], argv[3:]
    s = AppServer(codex, cwd)
    try:
        s.request("initialize", {"clientInfo": {"name": "codex_vscode", "title": "Codex", "version": "0.162.0"}})
        s.send({"method": "initialized"})
        for prompt in prompts:
            tid = s.request("thread/start", {"cwd": cwd})["result"]["thread"]["id"]
            s.request("turn/start", {"threadId": tid, "input": [{"type": "text", "text": prompt}]})
            reply = []

            def done(m):
                if m.get("method") == "item/completed":
                    item = m["params"]["item"]
                    if item.get("type") == "agentMessage":
                        reply.append(item.get("text", ""))
                return m.get("method") == "turn/completed" and m["params"].get("threadId") == tid

            s.wait(done, 180)
            print(json.dumps({"thread_id": tid, "prompt": prompt, "reply": "".join(reply)}), flush=True)
    finally:
        s.close()


if __name__ == "__main__":
    main(sys.argv)
