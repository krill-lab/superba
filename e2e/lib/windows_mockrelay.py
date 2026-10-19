#!/usr/bin/env python3
"""Windows e2e relay with a complete Responses stream."""
import importlib.util
import json
from pathlib import Path

source = Path(__file__).resolve().parents[2] / "crates/superba-gui/tools/screenshots/mockrelay.py"
spec = importlib.util.spec_from_file_location("screenshot_mockrelay", source)
relay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(relay)
json_bytes = relay.json_bytes

# The event sequence of a one-message answer as the real Responses API streams it (and as the
# router's own fixtures model it, crates/superba-router/tests/common/responses.rs): the router's
# stats line waits for the final answer's output_text.done after its output_item.added.
RESPONSE_EVENTS = ("response.created", "response.in_progress", "response.output_item.added",
                   "response.content_part.added", "response.output_text.delta",
                   "response.output_text.done", "response.content_part.done",
                   "response.output_item.done", "response.completed")


def responses_stream(model, created_at):
    item_id, text = "msg_mock_pong", "pong"
    response = {"id": "resp_mock_pong", "object": "response", "created_at": created_at,
                "model": model, "status": "in_progress", "output": [], "error": None,
                "usage": None}
    part = {"type": "output_text", "text": text, "annotations": [], "logprobs": []}
    item = {"id": item_id, "type": "message", "role": "assistant", "status": "completed",
            "phase": "final_answer", "content": [part]}
    completed = {**response, "status": "completed", "output": [item],
                 "usage": {"input_tokens": 12, "output_tokens": 1, "total_tokens": 13,
                           "input_tokens_details": {"cached_tokens": 0},
                           "output_tokens_details": {"reasoning_tokens": 0}}}
    at = {"item_id": item_id, "output_index": 0, "content_index": 0}
    payloads = [
        {"response": response},
        {"response": response},
        {"output_index": 0, "item": {**item, "status": "in_progress", "content": []}},
        {**at, "part": {**part, "text": ""}},
        {**at, "delta": text, "logprobs": []},
        {**at, "text": text, "logprobs": []},
        {**at, "part": part},
        {"output_index": 0, "item": item},
        {"response": completed},
    ]
    events = [{"type": kind, "sequence_number": n, **payload}
              for n, (kind, payload) in enumerate(zip(RESPONSE_EVENTS, payloads))]
    return b"".join(b"event: " + e["type"].encode() + b"\ndata: " + json_bytes(e) + b"\n\n"
                    for e in events)

if __name__ == "__main__":
    relay.responses_stream = responses_stream
    raise SystemExit(relay.main())
