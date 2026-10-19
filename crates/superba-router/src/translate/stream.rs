//! Bounded, incremental SSE framing and codec composition. A TCP chunk need
//! not be UTF-8 or contain a complete line/event. No stream is collected whole.

use super::ir::{Event, Protocol};
use super::{Invalid, anthropic, chat, gemini, responses};
use serde_json::Value;

const MAX_EVENT: usize = 16 * 1024 * 1024;

/// A stateful native-protocol decoder.
pub enum Decoder {
    /// Anthropic Messages.
    Messages(anthropic::Decoder),
    /// OpenAI Responses.
    Responses(responses::Decoder),
    /// OpenAI Chat Completions.
    Chat(chat::Decoder),
    /// Google Gemini.
    Gemini(gemini::Decoder),
}
impl Decoder {
    /// Construct the selected protocol codec or codec composition.
    pub fn new(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Messages => Self::Messages(Default::default()),
            Protocol::Responses => Self::Responses(Default::default()),
            Protocol::Chat => Self::Chat(Default::default()),
            Protocol::Gemini => Self::Gemini(Default::default()),
        }
    }
    /// Decode one native event into the IR.
    pub fn decode(&mut self, v: &Value) -> Result<Vec<Event>, Invalid> {
        match self {
            Self::Messages(d) => d.decode(v),
            Self::Responses(d) => d.decode(v),
            Self::Chat(d) => d.decode(v),
            Self::Gemini(d) => d.decode(v),
        }
    }
    /// Finish a native event stream at its terminal marker or HTTP EOF.
    pub fn finish(&mut self) -> Vec<Event> {
        match self {
            Self::Chat(d) => d.finish(),
            Self::Gemini(d) => d.finish(),
            _ => vec![],
        }
    }
}
/// A stateful native-protocol encoder.
pub enum Encoder {
    /// Anthropic Messages.
    Messages(anthropic::Encoder),
    /// OpenAI Responses.
    Responses(responses::Encoder),
    /// OpenAI Chat Completions.
    Chat(chat::Encoder),
    /// Google Gemini.
    Gemini(gemini::Encoder),
}
impl Encoder {
    /// Construct the selected protocol codec or codec composition.
    pub fn new(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Messages => Self::Messages(Default::default()),
            Protocol::Responses => Self::Responses(Default::default()),
            Protocol::Chat => Self::Chat(Default::default()),
            Protocol::Gemini => Self::Gemini(Default::default()),
        }
    }
    /// Encode one IR event into native events.
    pub fn encode(&mut self, e: &Event) -> Result<Vec<Value>, Invalid> {
        match self {
            Self::Messages(d) => d.encode(e),
            Self::Responses(d) => d.encode(e),
            Self::Chat(d) => d.encode(e),
            Self::Gemini(d) => d.encode(e),
        }
    }
}

/// Standalone SSE parser shared by the HTTP and WebSocket bridges.
#[derive(Default)]
pub struct Sse {
    buffer: Vec<u8>,
    data: Vec<u8>,
}
impl Sse {
    /// Consume an arbitrary byte chunk and return all newly completed output.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, Invalid> {
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut consumed = 0;
        while let Some(offset) = self.buffer[consumed..].iter().position(|b| *b == b'\n') {
            let end = consumed + offset;
            let line = self.buffer[consumed..end]
                .strip_suffix(b"\r")
                .unwrap_or(&self.buffer[consumed..end]);
            if line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop();
                    out.push(std::mem::take(&mut self.data));
                }
            } else if let Some(data) = line.strip_prefix(b"data:") {
                self.data
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                self.data.push(b'\n');
            }
            consumed = end + 1;
            if self.data.len() > MAX_EVENT {
                return Err(Invalid::new(
                    "upstream SSE event exceeds the translation limit",
                ));
            }
        }
        self.buffer.drain(..consumed);
        if self.buffer.len() + self.data.len() > MAX_EVENT {
            return Err(Invalid::new(
                "upstream SSE event exceeds the translation limit",
            ));
        }
        Ok(out)
    }
    /// Finish framing at EOF; a transcoder also rejects incomplete responses.
    pub fn finish(&mut self) -> Result<Vec<Vec<u8>>, Invalid> {
        self.push(b"\n\n")
    }
}

/// Write one native JSON event with the required SSE event name.
pub fn frame(protocol: Protocol, v: &Value, out: &mut Vec<u8>) {
    if matches!(protocol, Protocol::Messages | Protocol::Responses)
        && let Some(kind) = v["type"].as_str()
    {
        out.extend_from_slice(b"event: ");
        out.extend_from_slice(kind.as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(b"data: ");
    out.extend_from_slice(v.to_string().as_bytes());
    out.extend_from_slice(b"\n\n");
}

/// Compose native codecs through the IR while retaining incremental state.
pub struct Transcoder {
    parser: Sse,
    decoder: Decoder,
    encoder: Encoder,
    target: Protocol,
    ended: bool,
}
impl Transcoder {
    /// Restore Responses text tools and namespaces using the request's tools.
    pub fn with_response_tools(mut self, tools: &[super::ir::Tool]) -> Self {
        if let Encoder::Responses(encoder) = self.encoder {
            self.encoder = Encoder::Responses(encoder.with_tools(tools));
        }
        self
    }
    /// Construct the selected protocol codec or codec composition.
    pub fn new(source: Protocol, target: Protocol) -> Self {
        Self {
            parser: Default::default(),
            decoder: Decoder::new(source),
            encoder: Encoder::new(target),
            target,
            ended: false,
        }
    }
    /// Consume an arbitrary byte chunk and return all newly completed output.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<u8>, Invalid> {
        let frames = self.parser.push(bytes)?;
        self.frames(frames)
    }
    fn events(&mut self, events: Vec<Event>, out: &mut Vec<u8>) -> Result<(), Invalid> {
        for event in events {
            if self.ended {
                return Err(Invalid::new("upstream sent events after stream completion"));
            }
            let ending = matches!(event, Event::End | Event::Error(_));
            for value in self.encoder.encode(&event)? {
                frame(self.target, &value, out);
            }
            if ending {
                self.ended = true;
                if self.target == Protocol::Chat {
                    out.extend_from_slice(b"data: [DONE]\n\n");
                }
            }
        }
        Ok(())
    }
    fn frames(&mut self, frames: Vec<Vec<u8>>) -> Result<Vec<u8>, Invalid> {
        let mut out = Vec::new();
        for data in frames {
            if data == b"[DONE]" {
                let events = self.decoder.finish();
                self.events(events, &mut out)?;
                continue;
            }
            let value: Value = serde_json::from_slice(&data)
                .map_err(|_| Invalid::new("upstream SSE data is not JSON"))?;
            let events = self.decoder.decode(&value)?;
            self.events(events, &mut out)?;
        }
        Ok(out)
    }
    /// Finish framing at EOF; a transcoder also rejects incomplete responses.
    pub fn finish(&mut self) -> Result<Vec<u8>, Invalid> {
        let frames = self.parser.finish()?;
        let mut out = self.frames(frames)?;
        if !self.ended {
            let events = self.decoder.finish();
            self.events(events, &mut out)?;
        }
        if !self.ended {
            return Err(Invalid::new("upstream stream ended before completion"));
        }
        Ok(out)
    }
}
