//! Secrets of `auth.json` hidden in the editor until the user reveals them.
//!
//! [`Masker::mask`] replaces the string value of every secret member (a name
//! with `token`, `key`, `secret` or `password` in it: the API key, the PAT,
//! the ChatGPT login's tokens) by a placeholder such as `"••••••••••••a1b2"`
//! and remembers the original; the same secret always gets the same
//! placeholder. [`Masker::unmask`] puts the originals back, so a masked text
//! can be edited (other fields, formatting) and saved without ever showing a
//! secret. A placeholder the user changed only partly is no secret any more:
//! the check reports it ([`superba_core::ProblemKind::Secret`]) and saving is
//! refused until the user reveals the secrets or restores the placeholder.

use superba_core::Secret;

use super::json;

/// The character placeholders are made of.
pub const MASK_CHAR: char = '•';
/// Bullets in a placeholder before the last four characters.
const BULLETS: usize = 12;
/// Secrets shorter than this show no characters at all.
const SHOW_TAIL_FROM: usize = 16;

/// A member name that holds a secret (`OPENAI_API_KEY`,
/// `personal_access_token`, `tokens.access_token`, `refresh_token`, ...).
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["token", "key", "secret", "password"]
        .iter()
        .any(|w| n.contains(w))
}

/// A string contains placeholder characters (a masked value, or one the user
/// changed only partly).
pub fn has_mask(s: &str) -> bool {
    s.contains(MASK_CHAR)
}

/// The placeholders of one editing session and the secrets behind them.
#[derive(Default)]
pub struct Masker {
    /// (placeholder without quotes, original raw string contents).
    entries: Vec<(String, Secret)>,
}

impl std::fmt::Debug for Masker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Masker")
            .field("secrets", &self.entries.len())
            .finish()
    }
}

impl Masker {
    /// No secrets known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of secrets hidden so far.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No secret hidden.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `placeholder` is one of this session's.
    pub fn knows(&self, placeholder: &str) -> bool {
        self.entries.iter().any(|(p, _)| p == placeholder)
    }

    fn placeholder_for(&mut self, raw: &str) -> String {
        if let Some((p, _)) = self.entries.iter().find(|(_, s)| s.expose() == raw) {
            return p.clone();
        }
        let chars: Vec<char> = raw.chars().collect();
        let mut base: String = std::iter::repeat_n(MASK_CHAR, BULLETS).collect();
        if chars.len() >= SHOW_TAIL_FROM {
            base.extend(&chars[chars.len() - 4..]);
        }
        let mut p = base.clone();
        let mut n = 2;
        while self.entries.iter().any(|(q, _)| *q == p) {
            p = format!("{base}#{n}");
            n += 1;
        }
        self.entries.push((p.clone(), Secret::new(raw)));
        p
    }

    /// `text` (JSON) with every secret string value replaced by its
    /// placeholder. Values that already are placeholders, empty strings and
    /// text that cannot be scanned are left as they are.
    pub fn mask(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for m in json::members(text) {
            if m.kind != json::Kind::String || !is_secret_name(m.name()) {
                continue;
            }
            let (start, end) = (m.value.start + 1, m.value.end - 1);
            if start > end || m.value.start < at {
                continue;
            }
            let raw = &text[start..end];
            if raw.is_empty() || has_mask(raw) {
                continue;
            }
            let p = self.placeholder_for(raw);
            out.push_str(&text[at..start]);
            out.push_str(&p);
            at = end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// `text` with every known placeholder (as a whole JSON string) replaced
    /// by the original secret.
    pub fn unmask(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (p, raw) in &self.entries {
            let quoted = format!("\"{p}\"");
            if out.contains(&quoted) {
                out = out.replace(&quoted, &format!("\"{}\"", raw.expose()));
            }
        }
        out
    }
}
