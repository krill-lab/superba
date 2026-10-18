//! Anthropic Messages SSE events the router writes itself (the stats line).

use bytes::Bytes;
use serde_json::{Value, json};

/// One SSE event `event: <name>` with `data` as JSON.
pub(crate) fn event(name: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
}

/// A `content_block_start` event.
pub(crate) fn block_start(index: u64, block: Value) -> Bytes {
    event(
        "content_block_start",
        &json!({"type": "content_block_start", "index": index, "content_block": block}),
    )
}

/// A `content_block_delta` event.
pub(crate) fn block_delta(index: u64, delta: Value) -> Bytes {
    event(
        "content_block_delta",
        &json!({"type": "content_block_delta", "index": index, "delta": delta}),
    )
}

/// A `content_block_stop` event.
pub(crate) fn block_stop(index: u64) -> Bytes {
    event(
        "content_block_stop",
        &json!({"type": "content_block_stop", "index": index}),
    )
}
