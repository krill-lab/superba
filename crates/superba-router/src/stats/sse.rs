//! Server-sent events: an incremental event splitter that works on any chunk
//! boundaries (LF and CRLF line ends), and type sniffing without parsing.
//!
//! An event ends at an empty line: `\n\n` or `\n\r\n` (which covers `\r\n\r\n`).
//! The scanner never allocates for events that lie inside one chunk; only the
//! bytes of an event split across chunks are copied (and at most
//! [`MAX_EVENT_BYTES`] of them).

use memchr::memchr;

/// Largest event kept for inspection; bigger events pass uninspected.
pub const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// Boundary search state carried across chunks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Boundary {
    /// 0 = inside a line, 1 = just after `\n`, 2 = after `\n\r`.
    state: u8,
}

impl Boundary {
    /// The end (exclusive) of the first event boundary in `data[from..]`,
    /// continuing the state of the previous bytes. Updates the state up to the
    /// returned position, or through the end of `data` when none is found.
    pub fn find(&mut self, data: &[u8], from: usize) -> Option<usize> {
        let mut i = from;
        // Finish a delimiter that started in the previous bytes.
        if self.state != 0 && i < data.len() {
            match (self.state, data[i]) {
                (1 | 2, b'\n') => {
                    self.state = 0;
                    return Some(i + 1);
                }
                (1, b'\r') => {
                    if i + 1 < data.len() {
                        if data[i + 1] == b'\n' {
                            self.state = 0;
                            return Some(i + 2);
                        }
                        self.state = 0;
                        i += 1;
                    } else {
                        self.state = 2;
                        return None;
                    }
                }
                _ => self.state = 0,
            }
        }
        while i < data.len() {
            let Some(off) = memchr(b'\n', &data[i..]) else {
                self.state = 0;
                return None;
            };
            let nl = i + off;
            match data.get(nl + 1) {
                None => {
                    self.state = 1;
                    return None;
                }
                Some(b'\n') => {
                    self.state = 0;
                    return Some(nl + 2);
                }
                Some(b'\r') => match data.get(nl + 2) {
                    None => {
                        self.state = 2;
                        return None;
                    }
                    Some(b'\n') => {
                        self.state = 0;
                        return Some(nl + 3);
                    }
                    Some(_) => i = nl + 2,
                },
                Some(_) => i = nl + 1,
            }
        }
        self.state = 0;
        None
    }

    /// True while a delimiter may be half seen.
    pub fn pending(&self) -> bool {
        self.state != 0
    }
}

/// Kinds of Responses events the tap reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// `response.created`.
    Created,
    /// `response.output_item.added`.
    ItemAdded,
    /// `response.output_item.done`.
    ItemDone,
    /// `response.content_part.added`.
    PartAdded,
    /// `response.content_part.done`.
    PartDone,
    /// `response.output_text.delta`.
    TextDelta,
    /// `response.output_text.done`.
    TextDone,
    /// Another output delta (reasoning, refusal, tool input): counts as output.
    OutputDelta,
    /// `response.completed`.
    Completed,
    /// `response.incomplete`.
    Incomplete,
    /// `response.failed`.
    Failed,
    /// `error` (WebSocket error frames, some relays over SSE).
    Error,
    /// Anything else (`response.in_progress`, metadata, comments, `[DONE]`).
    Other,
}

impl EventKind {
    /// Classify an event type name.
    pub fn of(name: &str) -> EventKind {
        match name {
            "response.created" => EventKind::Created,
            "response.output_item.added" => EventKind::ItemAdded,
            "response.output_item.done" => EventKind::ItemDone,
            "response.content_part.added" => EventKind::PartAdded,
            "response.content_part.done" => EventKind::PartDone,
            "response.output_text.delta" => EventKind::TextDelta,
            "response.output_text.done" => EventKind::TextDone,
            "response.completed" => EventKind::Completed,
            "response.incomplete" => EventKind::Incomplete,
            "response.failed" => EventKind::Failed,
            "error" | "response.error" => EventKind::Error,
            n if n.starts_with("response.") && n.ends_with(".delta") => EventKind::OutputDelta,
            _ => EventKind::Other,
        }
    }

    /// True for the events that end a response.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            EventKind::Completed | EventKind::Incomplete | EventKind::Failed | EventKind::Error
        )
    }
}

/// The parts of one complete SSE event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent<'a> {
    /// The `event:` name, if any.
    pub name: Option<&'a str>,
    /// The `data:` payload (several data lines joined with `\n`).
    pub data: std::borrow::Cow<'a, [u8]>,
}

/// Split one complete event's bytes into its name and data.
pub fn parse_event(raw: &[u8]) -> SseEvent<'_> {
    let mut name = None;
    let mut data: Option<std::borrow::Cow<'_, [u8]>> = None;
    for line in raw.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(v) = line.strip_prefix(b"event:") {
            name = std::str::from_utf8(trim_space(v)).ok();
        } else if let Some(v) = line.strip_prefix(b"data:") {
            let v = v.strip_prefix(b" ").unwrap_or(v);
            data = Some(match data {
                None => std::borrow::Cow::Borrowed(v),
                Some(prev) => {
                    let mut joined = prev.into_owned();
                    joined.push(b'\n');
                    joined.extend_from_slice(v);
                    std::borrow::Cow::Owned(joined)
                }
            });
        }
    }
    SseEvent {
        name,
        data: data.unwrap_or(std::borrow::Cow::Borrowed(&[])),
    }
}

fn trim_space(v: &[u8]) -> &[u8] {
    let start = v.iter().position(|b| *b != b' ').unwrap_or(v.len());
    let end = v
        .iter()
        .rposition(|b| *b != b' ' && *b != b'\r')
        .map_or(start, |e| e + 1);
    &v[start..end.max(start)]
}

/// The type of a JSON event payload: its top-level `"type"` member, read
/// without parsing the payload (fast path: `{"type":"…"` first, as OpenAI and
/// Codex write it; otherwise a depth-aware scan).
pub fn sniff_type(json: &[u8]) -> Option<&str> {
    let start = json.iter().position(|b| !b" \t\r\n".contains(b))?;
    if json[start] != b'{' {
        return None;
    }
    let mut depth = 0usize;
    let mut i = start;
    while i < json.len() {
        match json[i] {
            b'"' => {
                let end = string_end(json, i)?;
                if depth == 1 && &json[i..end] == b"\"type\"" {
                    let j = skip_ws(json, end);
                    if json.get(j) == Some(&b':') {
                        let v = skip_ws(json, j + 1);
                        if json.get(v) != Some(&b'"') {
                            return None;
                        }
                        let close = string_end(json, v)?;
                        return std::str::from_utf8(&json[v + 1..close - 1]).ok();
                    }
                }
                i = end;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return None;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// End (exclusive) of the JSON string starting at `start` (a `"`).
fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// The kind of a complete SSE event: its `event:` name, else the payload's type.
pub fn event_kind(ev: &SseEvent<'_>) -> EventKind {
    match ev.name {
        Some(n) if !n.is_empty() && n != "message" => EventKind::of(n),
        _ => sniff_type(&ev.data).map_or(EventKind::Other, EventKind::of),
    }
}
