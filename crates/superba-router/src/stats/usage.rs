//! Token usage of one response (`response.usage`), normalised to OpenAI
//! semantics: `input` includes cached and cache-write tokens, `output` includes
//! reasoning tokens. Unknown counts stay `None`, never 0.
//!
//! Accepted shapes, in order:
//!
//! | Field | Semantics |
//! |---|---|
//! | `input_tokens_details.cached_tokens` | cache read, subset of `input_tokens` |
//! | `input_tokens_details.cache_write_tokens` (Codex ≥ 0.150), else `cache_creation_tokens` | cache write, subset |
//! | top-level `cache_read_input_tokens` / `cache_creation_input_tokens` (Anthropic style) | additive: added to `input_tokens` |
//! | `prompt_tokens` / `completion_tokens` (+ `prompt_tokens_details.cached_tokens`) | Chat Completions fallback |

use serde::Deserialize;

/// Normalised usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens, cached and cache-write included.
    pub input: u64,
    /// Cache read tokens.
    pub cached: Option<u64>,
    /// Cache write tokens.
    pub cache_write: Option<u64>,
    /// Output tokens, reasoning included.
    pub output: u64,
    /// Reasoning tokens.
    pub reasoning: Option<u64>,
}

impl Usage {
    /// Non-cached input: `input - cached - cache_write`.
    pub fn fresh_input(&self) -> u64 {
        self.input
            .saturating_sub(self.cached.unwrap_or(0))
            .saturating_sub(self.cache_write.unwrap_or(0))
    }

    /// Parse a `usage` object; `None` when it has neither input nor output
    /// counts (or is not an object).
    pub fn from_json(bytes: &[u8]) -> Option<Usage> {
        serde_json::from_slice::<UsageWire>(bytes)
            .ok()
            .and_then(UsageWire::normalise)
    }

    /// Normalise an already deserialized wire object.
    pub(crate) fn from_wire(wire: UsageWire) -> Option<Usage> {
        wire.normalise()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Details {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}

/// The wire shape of `usage` (every field optional; unknown fields ignored).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct UsageWire {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    input_tokens_details: Option<Details>,
    output_tokens_details: Option<Details>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_tokens_details: Option<Details>,
    completion_tokens_details: Option<Details>,
}

impl UsageWire {
    fn normalise(self) -> Option<Usage> {
        let (input, output, in_details, out_details) = match (self.input_tokens, self.output_tokens)
        {
            (None, None) => match (self.prompt_tokens, self.completion_tokens) {
                (None, None) => return None,
                (i, o) => (
                    i,
                    o,
                    self.prompt_tokens_details,
                    self.completion_tokens_details,
                ),
            },
            (i, o) => (i, o, self.input_tokens_details, self.output_tokens_details),
        };
        let in_details = in_details.unwrap_or_default();
        let mut usage = Usage {
            input: input.unwrap_or(0),
            cached: in_details.cached_tokens,
            cache_write: in_details
                .cache_write_tokens
                .or(in_details.cache_creation_tokens),
            output: output.unwrap_or(0),
            reasoning: out_details.and_then(|d| d.reasoning_tokens),
        };
        // Anthropic-style counts are additive: `input_tokens` excludes them.
        if self.cache_read_input_tokens.is_some() || self.cache_creation_input_tokens.is_some() {
            let read = self.cache_read_input_tokens.unwrap_or(0);
            let created = self.cache_creation_input_tokens.unwrap_or(0);
            if usage.cached.is_none() && usage.cache_write.is_none() {
                usage.input = usage.input.saturating_add(read).saturating_add(created);
                usage.cached = self.cache_read_input_tokens;
                usage.cache_write = self.cache_creation_input_tokens;
            }
        }
        Some(usage)
    }
}
