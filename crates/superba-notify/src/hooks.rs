//! Harness hooks: what each harness hands to `spb notify` and what Superba
//! writes into each harness's configuration.
//!
//! **Codex** (every version Superba supports, 0.140 – 0.163, checked against
//! the binaries):
//!
//! - `notify = [program, args...]` in `config.toml`: after each finished turn
//!   Codex starts `program args... <json>` without waiting, the JSON being
//!   `{"type":"agent-turn-complete","thread-id","turn-id","cwd","client",
//!   "input-messages","last-assistant-message"}`. Only finished turns: a turn
//!   that ends with an error runs no `notify` (its rollout's `task_complete`
//!   carries `error`; see [`crate::watch`]). Superba writes
//!   `[spb, "notify", "codex"]`, or `[spb, "notify", "codex", "--", <the
//!   user's own notify program>...]` so an existing program keeps running.
//! - Claude-style lifecycle hooks (`[[hooks.<Event>]]`, `hooks.json`):
//!   `PermissionRequest` (an approval is asked), `Stop`, ... with JSON on
//!   stdin (`session_id`, `turn_id`, `transcript_path`, `cwd`,
//!   `hook_event_name`, `model`, `tool_name`, `tool_input`, ...). Codex runs a
//!   hook only after the user reviewed and trusted it (`/hooks`, stored as
//!   `hooks.state."<file>:<event>:<i>:<j>".trusted_hash`).
//!
//! **Claude Code** (2.1.29x): `hooks` in `settings.json`; `Stop`
//! (`last_assistant_message`), `StopFailure` (`error`, `error_details`) and
//! `Notification` (`message`, `notification_type`: `permission_prompt`,
//! `idle_prompt`, `elicitation_dialog`, `auth_success`) with JSON on stdin;
//! `"async": true` runs a hook without blocking the session.
//!
//! **Other harnesses** (opencode, pi, dsh, …) can call `spb notify hook
//! <harness>` with Superba's own small JSON ([`GenericPayload`]).

use serde::Deserialize;
use serde_json::{Value, json};
use superba_core::notify::harness;
use superba_core::{EventKind, NotifyEvent, Timestamp};

mod windows;

/// The words after the program in Codex's `notify` (and in Claude Code's
/// hook command).
pub const CODEX_NOTIFY_ARGS: [&str; 2] = ["notify", "codex"];
/// `spb notify hook <harness>`.
pub const HOOK_ARGS: [&str; 2] = ["notify", "hook"];

/// Superba's Codex `notify` value: `[program, "notify", "codex"]`, followed by
/// `--` and the program it replaces, if any.
pub fn codex_notify_argv(program: &str, chained: &[String]) -> Vec<String> {
    let mut argv = vec![program.to_owned()];
    argv.extend(CODEX_NOTIFY_ARGS.iter().map(|s| (*s).to_owned()));
    if !chained.is_empty() {
        argv.push("--".into());
        argv.extend(chained.iter().cloned());
    }
    argv
}

/// What a Codex `notify` value is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyShape {
    /// Not set (or empty).
    Missing,
    /// Superba's hook, possibly chaining another program.
    Ours {
        /// The program Superba's hook runs (`spb`, possibly an older path).
        program: String,
        /// The chained program and its arguments.
        chained: Vec<String>,
    },
    /// Another program.
    Foreign(Vec<String>),
}

/// Classify a Codex `notify` value. Superba's hook is any program whose file
/// name is `spb` / `superba` (with `.exe`) followed by `notify codex`.
pub fn classify_notify(argv: &[String]) -> NotifyShape {
    let Some((program, rest)) = argv.split_first() else {
        return NotifyShape::Missing;
    };
    if program.trim().is_empty() {
        return NotifyShape::Missing;
    }
    if is_notify_program(program) && rest.len() >= 2 && rest[0] == "notify" && rest[1] == "codex" {
        let chained = match rest.get(2).map(String::as_str) {
            Some("--") => rest[3..].to_vec(),
            _ => Vec::new(),
        };
        return NotifyShape::Ours {
            program: program.clone(),
            chained,
        };
    }
    NotifyShape::Foreign(argv.to_vec())
}

fn is_notify_program(program: &str) -> bool {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    matches!(name.as_str(), "spb" | "spb.exe" | "superba" | "superba.exe")
}

/// Split `spb notify codex` arguments: `[--, chained...] <json>` (Codex
/// appends the JSON as the last argument).
pub fn split_codex_args(args: &[String]) -> (Vec<String>, Option<String>) {
    let Some((payload, before)) = args.split_last() else {
        return (Vec::new(), None);
    };
    let chained = match before.first().map(String::as_str) {
        Some("--") => before[1..].to_vec(),
        _ => before.to_vec(),
    };
    (chained, Some(payload.clone()))
}

/// Codex's `notify` JSON.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexNotify {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "thread-id")]
    thread_id: Option<String>,
    #[serde(rename = "turn-id")]
    turn_id: Option<String>,
    cwd: Option<String>,
    client: Option<String>,
    #[serde(rename = "last-assistant-message")]
    last_assistant_message: Option<String>,
}

/// A hook payload that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadError;

/// Parse what a harness passed (`harness` = [`harness`] id) into an event.
/// `Ok(None)`: a valid payload Superba does not report (a subagent stopped, a
/// login succeeded, an unknown Codex notification type).
pub fn parse(
    harness_id: &str,
    payload: &str,
    now: Timestamp,
) -> Result<Option<NotifyEvent>, PayloadError> {
    let value: Value = serde_json::from_str(payload.trim()).map_err(|_| PayloadError)?;
    if !value.is_object() {
        return Err(PayloadError);
    }
    // Codex's `notify` JSON has a `type`; hooks have `hook_event_name`.
    if value.get("hook_event_name").is_some() {
        return Ok(parse_lifecycle_hook(harness_id, &value, now));
    }
    if harness_id == harness::CODEX || value.get("type").is_some() {
        let n: CodexNotify = serde_json::from_value(value.clone()).map_err(|_| PayloadError)?;
        if n.kind != "agent-turn-complete" {
            return Ok(None);
        }
        let mut e = NotifyEvent::new(EventKind::TurnComplete, harness::CODEX, now);
        e.thread_id = n.thread_id;
        e.turn_id = n.turn_id;
        e.cwd = n.cwd;
        e.client = n.client;
        e.summary = n.last_assistant_message;
        return Ok(Some(e));
    }
    let g: GenericPayload = serde_json::from_value(value).map_err(|_| PayloadError)?;
    Ok(g.into_event(harness_id, now))
}

/// Claude-style lifecycle hook input (Claude Code, Codex hooks).
fn parse_lifecycle_hook(harness_id: &str, v: &Value, now: Timestamp) -> Option<NotifyEvent> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let name = s("hook_event_name")?;
    let kind = match name.as_str() {
        "Stop" => EventKind::TurnComplete,
        "StopFailure" => EventKind::TurnFailed,
        "PermissionRequest" => EventKind::NeedsApproval,
        "Notification" => match s("notification_type").as_deref() {
            Some("permission_prompt") => EventKind::NeedsApproval,
            Some("idle_prompt") | Some("elicitation_dialog") => EventKind::NeedsInput,
            // Login messages and future types are not worth a phone message.
            _ => return None,
        },
        // SubagentStop, SessionStart, ... are not reported.
        _ => return None,
    };
    let mut e = NotifyEvent::new(kind, harness_id, now);
    e.thread_id = s("session_id");
    e.turn_id = s("turn_id");
    e.cwd = s("cwd");
    e.model = s("model");
    e.summary = s("last_assistant_message");
    match name.as_str() {
        "StopFailure" => {
            e.detail = s("error").or_else(|| s("error_details"));
        }
        "PermissionRequest" => {
            e.detail = s("tool_name");
            e.summary = v.get("tool_input").map(tool_input_text);
        }
        "Notification" => {
            e.detail = s("message").or_else(|| s("title"));
        }
        _ => {}
    }
    Some(e)
}

/// A one-line rendering of a tool's input (the command of a shell tool, the
/// path of an edit, else compact JSON).
fn tool_input_text(input: &Value) -> String {
    for key in ["command", "cmd", "file_path", "path", "url", "query"] {
        match input.get(key) {
            Some(Value::String(s)) => return s.clone(),
            Some(Value::Array(a)) => {
                let words: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
                if !words.is_empty() {
                    return words.join(" ");
                }
            }
            _ => {}
        }
    }
    serde_json::to_string(input).unwrap_or_default()
}

/// The transcript file a lifecycle hook names (`transcript_path`).
pub fn transcript_path(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload.trim()).ok()?;
    v.get("transcript_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Superba's own hook JSON for harnesses without a hook format of their own
/// (`spb notify hook opencode < event.json`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GenericPayload {
    /// [`EventKind`] id (`turn_complete`, `needs_approval`, ...).
    pub event: String,
    /// Session / thread id.
    pub thread_id: Option<String>,
    /// Turn id (deduplication).
    pub turn_id: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Session title.
    pub title: Option<String>,
    /// Last reply.
    pub summary: Option<String>,
    /// Error / request text.
    pub detail: Option<String>,
    /// Model.
    pub model: Option<String>,
    /// Duration.
    pub duration_ms: Option<u64>,
}

impl GenericPayload {
    fn into_event(self, harness_id: &str, now: Timestamp) -> Option<NotifyEvent> {
        let kind = EventKind::parse(&self.event)?;
        let mut e = NotifyEvent::new(kind, harness_id, now);
        e.thread_id = self.thread_id;
        e.turn_id = self.turn_id;
        e.cwd = self.cwd;
        e.title = self.title;
        e.summary = self.summary;
        e.detail = self.detail;
        e.model = self.model;
        e.duration_ms = self.duration_ms;
        Some(e)
    }
}

/// Quote a program path. Quoting alone does not make a Windows hook executable
/// in every shell; use [`hook_command`] to build a complete hook command.
pub fn shell_quote(program: &str) -> String {
    // A plain path needs no quotes in any shell (and quotes would make
    // PowerShell read the path as a string, not a command).
    let plain = |extra: &[u8]| {
        program
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+".contains(&b) || extra.contains(&b))
    };
    if cfg!(windows) {
        if plain(b"\\:") {
            program.to_owned()
        } else {
            format!("\"{}\"", program.replace('"', "\\\""))
        }
    } else if plain(b"") {
        program.to_owned()
    } else {
        format!("'{}'", program.replace('\'', "'\\''"))
    }
}

/// The command line of Superba's hook for `harness` (`spb notify hook <id>`).
pub fn hook_command(program: &str, harness_id: &str) -> String {
    if cfg!(windows) {
        return windows::command(program, harness_id);
    }
    format!(
        "{} {} {} {harness_id}",
        shell_quote(program),
        HOOK_ARGS[0],
        HOOK_ARGS[1]
    )
}

/// Claude Code `settings.json` `hooks` entries Superba owns (merge each event's
/// group into the user's `hooks.<Event>` arrays; the Claude Code settings
/// writer does that). Every hook runs in the background (`async`) so a slow
/// channel never delays Claude Code; `spb` decides what to send.
pub fn claude_code_hooks(program: &str) -> Value {
    let command = hook_command(program, harness::CLAUDE_CODE);
    let handler = json!({
        "type": "command",
        "command": command,
        "async": true,
        "timeout": 30,
    });
    json!({
        "Stop": [ { "hooks": [handler.clone()] } ],
        "StopFailure": [ { "hooks": [handler.clone()] } ],
        "Notification": [ {
            "matcher": "permission_prompt|idle_prompt|elicitation_dialog",
            "hooks": [handler],
        } ],
    })
}

/// Whether a Claude Code hook handler (`{"type":"command","command":..}`) is
/// Superba's: its command runs `spb`/`superba` with `notify hook`.
pub fn is_our_hook_command(command: &str) -> bool {
    let command = command.trim();
    let our_args = |words: &[&str]| {
        matches!(
            words,
            ["notify", "hook", harness::CODEX | harness::CLAUDE_CODE]
        )
    };
    if let Some([program, notify, hook, harness_id]) = windows::argv(command) {
        return is_notify_program(&program) && our_args(&[&notify, &hook, &harness_id]);
    }
    // The program may be quoted ('…' or "…") because of spaces in its path.
    let (program, rest) = match command.chars().next() {
        Some(q @ ('\'' | '"')) => match command[1..].find(q) {
            Some(end) if command[end + 2..].starts_with(char::is_whitespace) => {
                (&command[1..=end], &command[end + 2..])
            }
            _ => return false,
        },
        _ => {
            let (program, rest) = command
                .split_once(char::is_whitespace)
                .unwrap_or((command, ""));
            // Legacy generated unquoted paths contain no shell operators.
            if !program
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-+\\:".contains(&b))
            {
                return false;
            }
            (program, rest)
        }
    };
    let words: Vec<&str> = rest.split_whitespace().collect();
    // User commands that chain, redirect or customize a hook are not ours to
    // replace on reinstall (or remove on uninstall).
    is_notify_program(program) && our_args(&words)
}

/// The Codex `config.toml` table for Superba's `PermissionRequest` hook
/// (`[[hooks.PermissionRequest]]`, one handler). Codex runs it only after the
/// user trusted it in Codex (`/hooks`).
pub fn codex_permission_hook(program: &str) -> Value {
    json!({
        "hooks": [ {
            "type": "command",
            "command": hook_command(program, harness::CODEX),
            "timeout": 30,
            "async": true,
        } ],
    })
}
