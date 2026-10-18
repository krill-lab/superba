//! A strict Anthropic Messages stream reader, as an SDK accumulates it: the
//! event order is checked (message_start first, blocks started in index
//! order, one open block at a time, deltas only to the open block, stop
//! reason and usage from message_delta, message_stop last).

use serde_json::{Value, json};

/// The events of an SSE text: `(event name, data)`.
pub fn events(sse: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for raw in sse.split("\n\n") {
        let mut name = None;
        let mut data = String::new();
        for line in raw.lines() {
            if let Some(n) = line.strip_prefix("event:") {
                name = Some(n.trim().to_owned());
            } else if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.strip_prefix(' ').unwrap_or(d));
            }
        }
        if data.is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(&data).unwrap_or_else(|e| panic!("bad data {data}: {e}"));
        let name = name.unwrap_or_else(|| v["type"].as_str().unwrap_or_default().to_owned());
        assert_eq!(
            Some(name.as_str()),
            v["type"].as_str(),
            "event name and type differ: {raw}"
        );
        out.push((name, v));
    }
    out
}

/// The message a client builds from a stream; panics on an ill-formed one.
/// An `error` event yields `{"error": …}`.
pub fn accumulate(sse: &str) -> Value {
    let mut msg: Option<Value> = None;
    let mut open: Option<usize> = None;
    let mut json_buf: Vec<String> = Vec::new();
    let mut stopped = false;
    for (name, v) in events(sse) {
        assert!(!stopped, "event after message_stop: {name}");
        match name.as_str() {
            "error" => return json!({"error": v["error"]}),
            "ping" => {}
            "message_start" => {
                assert!(msg.is_none(), "second message_start");
                let mut m = v["message"].clone();
                assert_eq!(m["content"], json!([]));
                m["content"] = json!([]);
                msg = Some(m);
            }
            "content_block_start" => {
                let m = msg.as_mut().expect("block before message_start");
                assert!(open.is_none(), "block started while another is open");
                let content = m["content"].as_array_mut().unwrap();
                let index = v["index"].as_u64().unwrap() as usize;
                assert_eq!(index, content.len(), "block index out of order");
                content.push(v["content_block"].clone());
                json_buf.push(String::new());
                open = Some(index);
            }
            "content_block_delta" => {
                let m = msg.as_mut().expect("delta before message_start");
                let index = v["index"].as_u64().unwrap() as usize;
                assert_eq!(Some(index), open, "delta to a block that is not open");
                let block = &mut m["content"][index];
                let d = &v["delta"];
                match d["type"].as_str().unwrap() {
                    "text_delta" => {
                        let t = block["text"].as_str().unwrap_or_default().to_owned()
                            + d["text"].as_str().unwrap();
                        block["text"] = t.into();
                    }
                    "thinking_delta" => {
                        let t = block["thinking"].as_str().unwrap_or_default().to_owned()
                            + d["thinking"].as_str().unwrap();
                        block["thinking"] = t.into();
                    }
                    "signature_delta" => {
                        block["signature"] = d["signature"].clone();
                    }
                    "input_json_delta" => {
                        json_buf[index].push_str(d["partial_json"].as_str().unwrap());
                    }
                    other => panic!("unknown delta {other}"),
                }
            }
            "content_block_stop" => {
                let m = msg.as_mut().unwrap();
                let index = v["index"].as_u64().unwrap() as usize;
                assert_eq!(Some(index), open, "stop of a block that is not open");
                let block = &mut m["content"][index];
                if block["type"] == "tool_use" {
                    let buf = &json_buf[index];
                    block["input"] = if buf.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(buf)
                            .unwrap_or_else(|e| panic!("tool input {buf}: {e}"))
                    };
                }
                open = None;
            }
            "message_delta" => {
                let m = msg.as_mut().unwrap();
                assert!(open.is_none(), "message_delta with an open block");
                m["stop_reason"] = v["delta"]["stop_reason"].clone();
                for (k, val) in v["usage"].as_object().unwrap() {
                    m["usage"][k] = val.clone();
                }
            }
            "message_stop" => stopped = true,
            other => panic!("unknown event {other}"),
        }
    }
    assert!(stopped, "no message_stop");
    msg.unwrap()
}
