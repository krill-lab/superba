//! Export a Codex thread (its rollout) as Markdown or as a new session of
//! another tool (Claude Code, pi), so it can be read or resumed there.
//!
//! The conversion is deliberately lossy and safe to resume:
//! - user and assistant text are kept; Codex's injected context
//!   (`<environment_context>`, AGENTS.md instructions, ...) and developer
//!   messages are dropped;
//! - tool calls and their outputs become text (the target tools have other
//!   tools, and a history of tool calls they never made would not validate);
//! - reasoning: `encrypted_content` cannot be read and is dropped; a readable
//!   summary is kept in Markdown only;
//! - a compaction replaces everything before it with its summary, as Codex
//!   itself resumes;
//! - images become `[image]`; usage is not carried.
//!
//! Nothing is written into a Codex store: Markdown is returned as text, session
//! files are created new (`create_new`) in the target tool's own store.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use superba_core::{CodexHome, Error, Result, SessionExport, SessionExportFormat, Timestamp};

use crate::util::{io_err, write_new};

/// Longest tool input or output kept (characters).
pub const MAX_TOOL_TEXT: usize = 2000;
/// Longest rollout line read (bytes).
const MAX_LINE: usize = 16 * 1024 * 1024;
/// Claude Code version recorded in exported lines.
const CLAUDE_VERSION: &str = "2.0.0";

/// Who said it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The user.
    User,
    /// The model.
    Assistant,
}

/// One piece of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Text.
    Text(String),
    /// A readable reasoning summary.
    Reasoning(String),
    /// A tool call and, when found, its output.
    Tool {
        /// Tool name.
        name: String,
        /// Arguments or input.
        input: String,
        /// Output.
        output: Option<String>,
        /// Codex call id (to attach the output).
        call_id: String,
    },
    /// An image the user attached.
    Image,
}

/// One message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Role.
    pub role: Role,
    /// Content.
    pub blocks: Vec<Block>,
    /// The rollout timestamp (RFC 3339).
    pub at: Option<String>,
}

/// A parsed thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    /// Thread id.
    pub id: String,
    /// Working directory.
    pub cwd: String,
    /// Last model.
    pub model: Option<String>,
    /// `session_meta` timestamp.
    pub created: Option<String>,
    /// Messages.
    pub messages: Vec<Message>,
    /// Items that were dropped because they cannot be read
    /// (encrypted reasoning).
    pub encrypted: usize,
    /// Compactions applied.
    pub compactions: usize,
}

/// Where the other tools keep their sessions.
#[derive(Clone, Debug)]
pub struct Targets {
    /// Claude Code's config dir (`$CLAUDE_CONFIG_DIR`, else `~/.claude`).
    pub claude_dir: PathBuf,
    /// pi's agent dir (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`).
    pub pi_dir: PathBuf,
}

impl Targets {
    /// The default locations under `home_dir`, with the tools' env overrides.
    pub fn from_env(home_dir: &Path) -> Self {
        let env = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Targets {
            claude_dir: env("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home_dir.join(".claude")),
            pi_dir: env("PI_CODING_AGENT_DIR").unwrap_or_else(|| home_dir.join(".pi/agent")),
        }
    }
}

/// Context Codex injects as user messages.
const INJECTED: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "<permissions instructions>",
    "<turn_aborted>",
    "<collaboration_mode>",
    "<skill",
    "# AGENTS.md instructions",
];

fn injected(text: &str) -> bool {
    let t = text.trim_start();
    INJECTED.iter().any(|p| t.starts_with(p))
}

fn trim_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…");
    out
}

/// Text of a `function_call_output.output` (a string, or `{content}` /
/// a list of content items).
fn output_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => o.get("content").map(output_text).unwrap_or_default(),
        _ => String::new(),
    }
}

impl Transcript {
    fn push(&mut self, role: Role, block: Block, at: Option<&str>) {
        match self.messages.last_mut() {
            Some(last) if last.role == role => last.blocks.push(block),
            _ => self.messages.push(Message {
                role,
                blocks: vec![block],
                at: at.map(str::to_owned),
            }),
        }
    }

    fn attach_output(&mut self, call_id: &str, out: String) {
        for m in self.messages.iter_mut().rev() {
            for b in m.blocks.iter_mut().rev() {
                if let Block::Tool {
                    call_id: id,
                    output,
                    ..
                } = b
                    && id == call_id
                {
                    *output = Some(trim_to(&out, MAX_TOOL_TEXT));
                    return;
                }
            }
        }
    }

    /// Apply one `response_item` payload.
    fn item(&mut self, p: &Value, at: Option<&str>) {
        let kind = p.get("type").and_then(Value::as_str).unwrap_or("");
        let call_id = || {
            p.get("call_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match kind {
            "message" => {
                let role = match p.get("role").and_then(Value::as_str) {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => return,
                };
                for c in p
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match c.get("type").and_then(Value::as_str) {
                        Some("input_text" | "output_text" | "text") => {
                            let text = c.get("text").and_then(Value::as_str).unwrap_or("");
                            if text.trim().is_empty() || (role == Role::User && injected(text)) {
                                continue;
                            }
                            self.push(role, Block::Text(text.to_owned()), at);
                        }
                        Some("input_image" | "image") => self.push(role, Block::Image, at),
                        _ => {}
                    }
                }
            }
            "reasoning" => {
                if p.get("encrypted_content").is_some_and(|v| !v.is_null()) {
                    self.encrypted += 1;
                }
                let summary: Vec<&str> = p
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.get("text").and_then(Value::as_str))
                    .filter(|s| !s.trim().is_empty())
                    .collect();
                if !summary.is_empty() {
                    self.push(Role::Assistant, Block::Reasoning(summary.join("\n\n")), at);
                }
            }
            "function_call" | "custom_tool_call" | "local_shell_call" | "web_search_call"
            | "tool_search_call" => {
                let name = p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(match kind {
                        "local_shell_call" => "shell",
                        "web_search_call" => "web_search",
                        _ => "tool",
                    })
                    .to_owned();
                let input = match p
                    .get("arguments")
                    .or_else(|| p.get("input"))
                    .or_else(|| p.get("action"))
                {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => String::new(),
                };
                self.push(
                    Role::Assistant,
                    Block::Tool {
                        name,
                        input: trim_to(&input, MAX_TOOL_TEXT),
                        output: None,
                        call_id: call_id(),
                    },
                    at,
                );
            }
            "function_call_output" | "custom_tool_call_output" | "tool_search_output" => {
                let out = p.get("output").map(output_text).unwrap_or_default();
                self.attach_output(&call_id(), out);
            }
            _ => {}
        }
    }
}

/// Parse a rollout (uncompressed JSONL).
pub fn parse(plain: &[u8]) -> Transcript {
    let mut t = Transcript::default();
    for line in plain.split(|b| *b == b'\n') {
        if line.is_empty() || line.len() > MAX_LINE {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let at = v.get("timestamp").and_then(Value::as_str);
        let p = v.get("payload").unwrap_or(&Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if t.id.is_empty() {
                    t.id = p.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
                    t.cwd = p
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    t.created = p
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .or(at)
                        .map(str::to_owned);
                }
            }
            Some("turn_context") => {
                if let Some(m) = p.get("model").and_then(Value::as_str) {
                    t.model = Some(m.to_owned());
                }
                if t.cwd.is_empty()
                    && let Some(c) = p.get("cwd").and_then(Value::as_str)
                {
                    t.cwd = c.to_owned();
                }
            }
            Some("response_item") => t.item(p, at),
            Some("compacted") => {
                t.compactions += 1;
                t.messages.clear();
                if let Some(history) = p.get("replacement_history").and_then(Value::as_array) {
                    for item in history {
                        t.item(item, at);
                    }
                }
                if let Some(summary) = p
                    .get("message")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    t.messages.insert(
                        0,
                        Message {
                            role: Role::User,
                            blocks: vec![Block::Text(format!(
                                "[Summary of the earlier conversation]\n\n{summary}"
                            ))],
                            at: at.map(str::to_owned),
                        },
                    );
                }
            }
            _ => {}
        }
    }
    t
}

fn tool_text(name: &str, input: &str, output: Option<&str>) -> String {
    let mut s = format!("[tool call: {name}]\n```\n{input}\n```");
    if let Some(out) = output {
        s.push_str(&format!("\n[tool output]\n```\n{out}\n```"));
    }
    s
}

/// A message as plain text for the session formats (reasoning dropped).
fn plain_text(m: &Message) -> String {
    m.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(t) => Some(t.clone()),
            Block::Tool {
                name,
                input,
                output,
                ..
            } => Some(tool_text(name, input, output.as_deref())),
            Block::Image => Some("[image]".to_owned()),
            Block::Reasoning(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Messages for a chat history: non-empty, alternating, starting with the user.
fn alternating(t: &Transcript) -> Vec<(Role, String, Option<String>)> {
    let mut out: Vec<(Role, String, Option<String>)> = Vec::new();
    for m in &t.messages {
        let text = plain_text(m);
        if text.trim().is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.0 == m.role => {
                last.1.push_str("\n\n");
                last.1.push_str(&text);
            }
            _ => out.push((m.role, text, m.at.clone())),
        }
    }
    if out.first().is_some_and(|m| m.0 == Role::Assistant) {
        out.insert(0, (Role::User, "[Continued from Codex]".to_owned(), None));
    }
    out
}

/// Markdown transcript.
pub fn markdown(t: &Transcript, title: &str) -> String {
    let mut s = String::new();
    let title = if title.trim().is_empty() {
        &t.id
    } else {
        title
    };
    s.push_str(&format!("# {title}\n\n"));
    s.push_str(&format!("- Codex thread: `{}`\n", t.id));
    if !t.cwd.is_empty() {
        s.push_str(&format!("- Directory: `{}`\n", t.cwd));
    }
    if let Some(m) = &t.model {
        s.push_str(&format!("- Model: {m}\n"));
    }
    if let Some(c) = &t.created {
        s.push_str(&format!("- Started: {c}\n"));
    }
    s.push('\n');
    for m in &t.messages {
        s.push_str(match m.role {
            Role::User => "## User\n\n",
            Role::Assistant => "## Assistant\n\n",
        });
        for b in &m.blocks {
            match b {
                Block::Text(text) => s.push_str(text.trim_end()),
                Block::Reasoning(r) => {
                    for line in r.lines() {
                        s.push_str("> ");
                        s.push_str(line);
                        s.push('\n');
                    }
                }
                Block::Tool {
                    name,
                    input,
                    output,
                    ..
                } => {
                    s.push_str(&format!("**Tool `{name}`**\n\n```\n{input}\n```\n"));
                    if let Some(out) = output {
                        s.push_str(&format!("\n```text\n{out}\n```\n"));
                    }
                }
                Block::Image => s.push_str("*[image]*"),
            }
            s.push_str("\n\n");
        }
    }
    s.trim_end().to_owned() + "\n"
}

/// Claude Code's project folder name: every character other than an ASCII
/// letter or digit becomes `-`.
pub fn claude_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// pi's session folder name: `--<cwd without its leading separator, / \ : → ->--`.
pub fn pi_slug(cwd: &str) -> String {
    let trimmed = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let body: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{body}--")
}

fn time_or(at: &Option<String>, now: Timestamp) -> String {
    at.clone().unwrap_or_else(|| now.to_rfc3339())
}

/// Claude Code session JSONL (one line per message, a `parentUuid` chain).
pub fn claude_jsonl(t: &Transcript, session_id: &str, now: Timestamp) -> String {
    let mut out = String::new();
    let mut parent: Option<String> = None;
    for (role, text, at) in alternating(t) {
        let uuid = uuid::Uuid::new_v4().to_string();
        let message = match role {
            Role::User => json!({"role": "user", "content": text}),
            Role::Assistant => json!({
                "id": format!("msg_codex_{}", uuid.replace('-', "")),
                "type": "message",
                "role": "assistant",
                "model": t.model.clone().unwrap_or_else(|| "codex".to_owned()),
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            }),
        };
        let line = json!({
            "parentUuid": parent,
            "isSidechain": false,
            "userType": "external",
            "cwd": t.cwd,
            "sessionId": session_id,
            "version": CLAUDE_VERSION,
            "type": match role { Role::User => "user", Role::Assistant => "assistant" },
            "message": message,
            "uuid": uuid,
            "timestamp": time_or(&at, now),
        });
        out.push_str(&line.to_string());
        out.push('\n');
        parent = Some(uuid);
    }
    out
}

fn ms_of(at: &Option<String>, now: Timestamp) -> i64 {
    at.as_deref()
        .and_then(crate::util::parse_rfc3339_ms)
        .unwrap_or(now.0)
}

/// pi session JSONL (format v3: a header, then a tree of entries).
pub fn pi_jsonl(t: &Transcript, session_id: &str, now: Timestamp) -> String {
    let short = || uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let mut out = String::new();
    let header = json!({
        "type": "session",
        "version": 3,
        "id": session_id,
        "timestamp": now.to_rfc3339(),
        "cwd": t.cwd,
    });
    out.push_str(&header.to_string());
    out.push('\n');
    let mut parent: Option<String> = None;
    for (role, text, at) in alternating(t) {
        let id = short();
        let ms = ms_of(&at, now);
        let message = match role {
            Role::User => json!({
                "role": "user",
                "content": [{"type": "text", "text": text}],
                "timestamp": ms,
            }),
            Role::Assistant => json!({
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
                "api": "openai-responses",
                "provider": "codex-import",
                "model": t.model.clone().unwrap_or_else(|| "codex".to_owned()),
                "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0,
                    "totalTokens": 0,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
                "stopReason": "stop",
                "timestamp": ms,
            }),
        };
        let line = json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "timestamp": time_or(&at, now),
            "message": message,
        });
        out.push_str(&line.to_string());
        out.push('\n');
        parent = Some(id);
    }
    out
}

/// The rollout of `thread_id`: `hint` when it exists, else the newest
/// `rollout-*-<thread id>.jsonl[.zst]` under `sessions/` or `archived_sessions/`.
pub fn find_rollout(home: &CodexHome, thread_id: &str, hint: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = hint.filter(|p| p.is_file()) {
        return Some(p.to_path_buf());
    }
    if thread_id.is_empty() {
        return None;
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for dir in [home.sessions_dir(), home.archived_sessions_dir()] {
        for e in walkdir::WalkDir::new(dir)
            .max_depth(5)
            .into_iter()
            .flatten()
        {
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_file()
                && crate::rollout::is_rollout_name(&name)
                && name.contains(thread_id)
            {
                found.push(e.path().to_path_buf());
            }
        }
    }
    found.sort();
    found.pop()
}

/// Read and parse a rollout file (`.zst` decompressed).
pub fn read(path: &Path) -> Result<Transcript> {
    let raw = fs::read(path).map_err(|e| io_err(path, &e))?;
    let plain = if crate::rollout::is_compressed(path) {
        zstd::stream::decode_all(raw.as_slice()).map_err(|e| io_err(path, &e))?
    } else {
        raw
    };
    Ok(parse(&plain))
}

/// Export `thread_id` of `home` as `format`. Session formats are written as
/// new files under `targets`; Markdown is returned.
pub fn export(
    home: &CodexHome,
    thread_id: &str,
    rollout: Option<&Path>,
    title: &str,
    format: SessionExportFormat,
    targets: &Targets,
    now: Timestamp,
) -> Result<SessionExport> {
    let path = find_rollout(home, thread_id, rollout)
        .ok_or_else(|| Error::invalid(format!("no rollout file for thread {thread_id}")))?;
    let mut t = read(&path)?;
    if t.id.is_empty() {
        t.id = thread_id.to_owned();
    }
    let messages = t.messages.len();
    match format {
        SessionExportFormat::Markdown => Ok(SessionExport {
            text: markdown(&t, title),
            messages,
            ..SessionExport::default()
        }),
        SessionExportFormat::ClaudeCode => {
            let id = uuid::Uuid::new_v4().to_string();
            let file = targets
                .claude_dir
                .join("projects")
                .join(claude_slug(&t.cwd))
                .join(format!("{id}.jsonl"));
            write_new(&file, claude_jsonl(&t, &id, now).as_bytes())?;
            let cd = if t.cwd.is_empty() {
                String::new()
            } else {
                format!("cd {} && ", shell_quote(&t.cwd))
            };
            Ok(SessionExport {
                text: format!("{cd}claude --resume {id}"),
                path: Some(file),
                session_id: Some(id),
                messages,
            })
        }
        SessionExportFormat::Pi => {
            let id = uuid::Uuid::new_v4().to_string();
            let stamp = now.to_rfc3339().replace([':', '.'], "-");
            let file = targets
                .pi_dir
                .join("sessions")
                .join(pi_slug(&t.cwd))
                .join(format!("{stamp}_{id}.jsonl"));
            write_new(&file, pi_jsonl(&t, &id, now).as_bytes())?;
            let cd = if t.cwd.is_empty() {
                String::new()
            } else {
                format!("cd {} && ", shell_quote(&t.cwd))
            };
            Ok(SessionExport {
                text: format!("{cd}pi --session {}", shell_quote(&file.to_string_lossy())),
                path: Some(file),
                session_id: Some(id),
                messages,
            })
        }
    }
}

/// Quote for a POSIX shell when needed.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/-_.:~+@%".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
