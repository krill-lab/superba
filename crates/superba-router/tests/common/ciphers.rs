//! Codex 0.163-shaped request items that carry ciphertexts (GitHub issue 11),
//! and the refusals the Krill relay answers them with. Shapes were captured
//! from the real Codex 0.163 binary against a provider that is not an OpenAI
//! backend (a sub-agent spawned there stores its task as an
//! `encrypted_content` part holding plain text) and from the relay's answers
//! (2026-10-18); every ciphertext here is synthetic.

use serde_json::{Value, json};

/// Turn id stamped on the items.
pub const TURN: &str = "01a14f90-2885-7ff1-a4f1-faa50e326449";
/// A sub-agent's task as Codex stores it after a third-party provider.
pub const TASK: &str = "Count the files in the repository and report the number.";
/// The agent message carrying [`TASK`].
pub const TASK_ID: &str = "amsg_01a14f90-4274-7462-ab32-fd7a6bb914ec";
/// The NEW_TASK header Codex puts before the payload.
pub const TASK_HEADER: &str =
    "Message Type: NEW_TASK\nTask name: /root/worker\nSender: /root\nPayload:\n";

/// A Fernet-like ciphertext (`gAAAAAB…`, base64url) unique to `tag`.
pub fn cipher(tag: &str) -> String {
    let mut body = String::new();
    while body.len() < 160 {
        body.push_str(tag);
        body.push('-');
        body.push_str(&"Q2lwaGVy".repeat(2));
    }
    let clean: String = body
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // The last characters differ per tag, as a real ciphertext's do.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let h = tag
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let end: String = [h % 64, (h / 64) % 64]
        .iter()
        .map(|i| char::from(alphabet[*i as usize]))
        .collect();
    format!("gAAAAABq{clean}{end}==")
}

/// The first and last four characters of `encrypted` (how upstreams name it).
pub fn ends(encrypted: &str) -> (String, String) {
    let head: String = encrypted.chars().take(4).collect();
    let tail: String = encrypted
        .chars()
        .skip(encrypted.chars().count() - 4)
        .collect();
    (head, tail)
}

fn meta() -> Value {
    json!({"turn_id": TURN})
}

/// A user message.
pub fn user(text: &str) -> Value {
    json!({"type": "message", "id": format!("msg_u_{}", text.len()), "role": "user",
           "content": [{"type": "input_text", "text": text}],
           "internal_chat_message_metadata_passthrough": {"turn_id": TURN, "content_item_kinds": ["user.text"]}})
}

/// A developer message (Codex's instructions and context fragments).
pub fn developer(text: &str) -> Value {
    json!({"type": "message", "id": "msg_01a14f90-2830-7000-8000-000000000001", "role": "developer",
           "content": [{"type": "input_text", "text": text}]})
}

/// A reasoning item.
pub fn reasoning(id: &str, encrypted: &str) -> Value {
    json!({"type": "reasoning", "id": id, "summary": [], "content": null,
           "encrypted_content": encrypted, "internal_chat_message_metadata_passthrough": meta()})
}

/// An assistant message, with `encrypted_content` when the provider replays
/// output tokens.
pub fn assistant(text: &str, encrypted: Option<&str>) -> Value {
    let mut v = json!({"type": "message", "id": "msg_r03", "status": "completed", "role": "assistant",
                       "content": [{"type": "output_text", "text": text, "annotations": []}]});
    if let Some(e) = encrypted {
        v["encrypted_content"] = json!(e);
    }
    v
}

/// The `spawn_agent` call that made [`agent_task`]; `encrypted` names the
/// arguments an OpenAI backend encrypted.
pub fn spawn_call(message: &str, encrypted: Option<&[&str]>) -> Value {
    let mut v = json!({"type": "function_call", "status": "completed", "id": "fc_r01", "name": "spawn_agent",
                       "namespace": "collaboration",
                       "arguments": json!({"task_name": "worker", "message": message}).to_string(),
                       "call_id": "call_r01", "internal_chat_message_metadata_passthrough": meta()});
    if let Some(names) = encrypted {
        v["encrypted_function_args"] = json!(names);
    }
    v
}

/// The sub-agent's task message with `payload` in its encrypted part.
pub fn agent_task(id: &str, payload: &str) -> Value {
    json!({"type": "agent_message", "id": id, "author": "/root", "recipient": "/root/worker",
           "content": [{"type": "input_text", "text": TASK_HEADER},
                       {"type": "encrypted_content", "encrypted_content": payload}],
           "internal_chat_message_metadata_passthrough": {"turn_id": "01a14f90-4265-7c63-b6d6-0daeff51d800"}})
}

/// A `notes.write_file` call.
pub fn notes_call(call_id: &str) -> Value {
    json!({"type": "function_call", "id": "fc_0807", "name": "write_file", "namespace": "notes",
           "arguments": "{\"path\":\"e2e/reorg.md\",\"text\":\"x\"}", "call_id": call_id,
           "internal_chat_message_metadata_passthrough": meta()})
}

/// Its encrypted output.
pub fn notes_output(id: &str, call_id: &str, encrypted: &str) -> Value {
    json!({"type": "function_call_output", "id": id, "call_id": call_id,
           "output": [{"type": "encrypted_content", "encrypted_content": encrypted}],
           "internal_chat_message_metadata_passthrough": meta()})
}

/// A compaction item.
pub fn compaction(encrypted: &str) -> Value {
    json!({"type": "compaction", "id": "cmp_02c6", "encrypted_content": encrypted})
}

/// A Codex 0.163 Responses body around `input` (instructions in `input[0]`).
pub fn body(input: Vec<Value>) -> Value {
    json!({
        "model": "gpt-5.5",
        "stream": true,
        "input": input,
        "tools": [{"type": "function", "name": "exec_command", "strict": false,
                   "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}}}],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "reasoning": {"effort": "medium", "summary": "auto"},
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": "01a14f90-4244-78c1-be27-38dce8200357",
        "text": {"verbosity": "low"},
        "client_metadata": {"x-codex-turn-metadata": "{\"turn_id\":\"01a14f90-4265-7c63-b6d6-0daeff51d800\"}"}
    })
}

/// The relay's HTTP 400 naming an item (measured 2026-10-18).
pub fn refusal_for_item(id: &str) -> String {
    json!({"error": {"message": format!("The encrypted content for item {id} could not be verified. Reason: encrypted content hydration failed: Encrypted content could not be decrypted or parsed."),
                     "type": "invalid_request_error", "param": "input", "code": "invalid_encrypted_content"}})
    .to_string()
}

/// The relay's HTTP 400 naming a ciphertext by its ends (its own refusal of a
/// ciphertext it saw refused before has this shape too).
pub fn refusal_for_ends(encrypted: &str) -> String {
    let (head, tail) = ends(encrypted);
    json!({"type": "error", "error": {"type": "invalid_request_error", "code": "invalid_encrypted_content",
           "message": format!("The encrypted content {head}...{tail} could not be verified. Reason: Encrypted content could not be decrypted or parsed."),
           "param": null}, "status": 400})
    .to_string()
}

/// The relay's HTTP 400 for an encrypted tool output (measured with the real
/// Codex 0.163 sub-agent request).
pub const REFUSAL_FUNCTION_OUTPUT: &str = r#"{"error":{"type":"invalid_request_error","code":"invalid_encrypted_content","message":"Encrypted function output content could not be decrypted or decoded.","param":null}}"#;

/// A string that lives for the rest of the test process (fake upstream
/// answers are `&'static str`).
pub fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}
