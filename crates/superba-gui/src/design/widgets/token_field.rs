//! Token count entry (UX §4, §3.4): a single-line field that accepts `400000`,
//! `400k`, `1m` and, where allowed, a share of the window (`80%`), parsed with the
//! shared parsers of `superba_core::models` ([`parse_token_count`],
//! [`CompactLimit`]). The page owns the text buffer and decides what is valid for
//! a model (bounds, the 90 % compaction clamp); the field shows the error state
//! and reports when the user committed the text (Enter or leaving the field).
//!
//! While the field is not focused it always shows the value it is given (a
//! short `k`/`M` label), never an old buffer: the buffer only lives during an
//! edit, which starts from the exact value with everything selected. Escape
//! cancels the edit.

use egui::text::CCursorRange;
use egui::text_edit::TextEditState;
use egui::{Id, Key, Response, Stroke, StrokeKind, TextEdit, Ui, vec2};
use superba_core::models::{CompactLimit, parse_token_count};

use crate::design::tokens::{Palette, radius};

/// What the user typed into a [`TokenField`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenInput {
    /// Nothing: back to the default.
    Empty,
    /// An absolute token count.
    Tokens(u64),
    /// A share of the context window (1..=100 %).
    Percent(u8),
}

/// Why a typed value cannot be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenError {
    /// Not a token count (or a percentage where one is not allowed).
    Format,
}

/// Parse a typed value: blank = [`TokenInput::Empty`]; `400000`, `400k`, `1m`,
/// `1.5m` = tokens; `80%` = a percentage when `allow_percent`.
pub fn parse_input(text: &str, allow_percent: bool) -> Result<TokenInput, TokenError> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(TokenInput::Empty);
    }
    if t.ends_with('%') {
        if !allow_percent {
            return Err(TokenError::Format);
        }
        return match CompactLimit::parse(t) {
            Some(CompactLimit::Percent(p)) => Ok(TokenInput::Percent(p)),
            _ => Err(TokenError::Format),
        };
    }
    parse_token_count(t)
        .filter(|n| *n > 0)
        .map(TokenInput::Tokens)
        .ok_or(TokenError::Format)
}

/// Short display of a token count: `400K`, `1M`, `1.05M`, `272K`, else grouped
/// digits (`117,283`).
pub fn short_tokens(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(10_000) {
        let whole = n / 1_000_000;
        let frac = (n % 1_000_000) / 10_000;
        return if frac == 0 {
            format!("{whole}M")
        } else if frac.is_multiple_of(10) {
            format!("{whole}.{}M", frac / 10)
        } else {
            format!("{whole}.{frac:02}M")
        };
    }
    if n >= 1_000 && n.is_multiple_of(1_000) {
        return format!("{}K", group(n / 1_000));
    }
    group(n)
}

/// Short label of a token count: exact `K`/`M` when round (`400K`, `1.05M`),
/// else one decimal in `K` (`244.8K`, `117.3K`) or two in `M` (`1.23M`); exact
/// below 1,000. The exact value goes in a tooltip ([`exact_tokens`]).
pub fn token_label(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n.is_multiple_of(1_000) && n < 1_000_000 {
        return format!("{}K", n / 1_000);
    }
    let tenths_k = (n + 50) / 100;
    if tenths_k < 10_000 {
        let (whole, frac) = (tenths_k / 10, tenths_k % 10);
        return if frac == 0 {
            format!("{whole}K")
        } else {
            format!("{whole}.{frac}K")
        };
    }
    let hundredths_m = (n + 5_000) / 10_000;
    let (whole, frac) = (hundredths_m / 100, hundredths_m % 100);
    if frac == 0 {
        format!("{whole}M")
    } else if frac.is_multiple_of(10) {
        format!("{whole}.{}M", frac / 10)
    } else {
        format!("{whole}.{frac:02}M")
    }
}

/// Exact token count with grouped digits: `244,800`.
pub fn exact_tokens(n: u64) -> String {
    group(n)
}

/// Rounded display of a token count for hints and derived values: `245K`,
/// `1.2M`, exact below 10,000 (`9,999`).
pub fn approx_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let tenths = (n + 50_000) / 100_000;
        return if tenths.is_multiple_of(10) {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        };
    }
    if n >= 10_000 {
        return format!("{}K", (n + 500) / 1_000);
    }
    group(n)
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What a [`TokenField`] reported this frame.
#[derive(Debug)]
pub struct TokenFieldResponse {
    /// The text edit.
    pub response: Response,
    /// The user finished editing (Enter or focus left) and the text differs
    /// from the text the field started with.
    pub committed: bool,
}

/// A token count field bound to a text buffer owned by the page.
#[must_use = "call .show(ui)"]
pub struct TokenField<'a> {
    id: Id,
    buffer: &'a mut String,
    initial: String,
    display: Option<String>,
    hint: String,
    width: f32,
    error: bool,
    enabled: bool,
    muted: bool,
}

impl<'a> TokenField<'a> {
    /// A field over `buffer`; `initial` is the text that counts as unchanged
    /// (the saved value), so leaving the field without edits commits nothing.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        buffer: &'a mut String,
        initial: impl Into<String>,
    ) -> Self {
        Self {
            id: Id::unique(("superba-token-field", id_salt)),
            buffer,
            initial: initial.into(),
            display: None,
            hint: String::new(),
            width: 96.0,
            error: false,
            enabled: true,
            muted: false,
        }
    }

    /// Text shown while the field is not being edited (a short label such as
    /// `244.8K`; default: the initial text).
    pub fn display(mut self, display: impl Into<String>) -> Self {
        self.display = Some(display.into());
        self
    }

    /// Draw the value in the muted text colour (a default, not the user's own).
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    /// Placeholder shown while empty (the default value).
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Width in points.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Draw the field with a red outline (the page shows the message).
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    /// Enable or disable editing.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// The widget's id (to focus it from a command).
    pub fn id(&self) -> Id {
        self.id
    }

    /// Draw.
    pub fn show(self, ui: &mut Ui) -> TokenFieldResponse {
        let pal = Palette::of(ui.ctx());
        let focused = ui.memory(|m| m.has_focus(self.id));
        let editing_id = self.id.with("editing");
        let was_editing = ui.data(|d| d.get_temp::<bool>(editing_id)).unwrap_or(false);
        if focused && !was_editing {
            // An edit starts from the exact value, all of it selected.
            self.buffer.clone_from(&self.initial);
            let mut state = TextEditState::load(ui.ctx(), self.id).unwrap_or_default();
            state
                .cursor
                .set_char_range(Some(CCursorRange::select_all_str(self.buffer)));
            state.store(ui.ctx(), self.id);
            ui.data_mut(|d| d.insert_temp(editing_id, true));
        }
        // The buffer holds the user's text only during an edit (and the frame
        // that ends it); otherwise it follows the value, never a stale copy.
        let editing = focused || was_editing;
        if !editing {
            self.buffer.clone_from(&self.initial);
        }
        // egui drops the focus on Escape before any widget runs, so the edit
        // that just lost it counts too.
        let escape = editing && ui.input(|i| i.key_pressed(Key::Escape));
        let mut shown =
            (!editing).then(|| self.display.clone().unwrap_or_else(|| self.initial.clone()));
        let color = if self.muted && !editing {
            pal.text_subtle
        } else {
            pal.text
        };
        let text: &mut String = match shown.as_mut() {
            Some(s) => s,
            None => &mut *self.buffer,
        };
        let edit = TextEdit::singleline(text)
            .id(self.id)
            .font(egui::TextStyle::Monospace)
            .text_color(color)
            .hint_text(self.hint)
            .desired_width(self.width)
            .margin(vec2(8.0, 4.0));
        let response = ui.add_enabled(self.enabled, edit);
        if self.error {
            ui.painter().rect_stroke(
                response.rect,
                radius::CONTROL,
                Stroke::new(1.0, pal.red),
                StrokeKind::Inside,
            );
        }
        let enter = ui.input(|i| i.key_pressed(Key::Enter));
        let finished = editing && (response.lost_focus() || (response.has_focus() && enter));
        if escape {
            // Cancel: the value stays as it was.
            self.buffer.clone_from(&self.initial);
        }
        let committed = finished && !escape && self.buffer.trim() != self.initial.trim();
        if (committed || escape) && response.has_focus() {
            response.surrender_focus();
        }
        if editing && (!response.has_focus() || committed || escape) {
            ui.data_mut(|d| d.remove::<bool>(editing_id));
        }
        TokenFieldResponse {
            response,
            committed,
        }
    }
}
