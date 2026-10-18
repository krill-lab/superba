//! Syntax highlighting of the Codex file editor (GitHub issue 9): a small
//! tokenizer for TOML and JSON (no extra dependency) and the `LayoutJob` the
//! editor's text field draws, with the problems of the check underlined
//! (errors red, warnings amber) and hidden secrets dimmed.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use superba_core::{CodexFileKind, FileProblem, ProblemSeverity};
use superba_switch::codex_files::MASK_CHAR;

use crate::design::tokens::Palette;

/// What a piece of text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// Anything else (spaces, `=`, `,`).
    Plain,
    /// `# ...` to the end of the line.
    Comment,
    /// A `[table]` / `[[array]]` header.
    Header,
    /// A key (TOML key, JSON member name).
    Key,
    /// A string value.
    String,
    /// A number or a date.
    Number,
    /// `true`, `false`, `null`, `inf`, `nan`.
    Literal,
    /// Brackets and braces.
    Punct,
    /// A hidden secret's placeholder.
    Masked,
}

/// Tokens of `text` as non-overlapping, ordered ranges covering every byte.
pub fn tokens(kind: CodexFileKind, text: &str) -> Vec<(Range<usize>, Token)> {
    let raw = match kind {
        CodexFileKind::ConfigToml => toml_tokens(text),
        CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => json_tokens(text),
    };
    // Fill the gaps with Plain and mark placeholders.
    let mut out = Vec::with_capacity(raw.len() * 2);
    let mut at = 0;
    for (r, t) in raw {
        if r.start < at || r.end > text.len() || r.start >= r.end {
            continue;
        }
        if r.start > at {
            out.push((at..r.start, Token::Plain));
        }
        let t = if t == Token::String && text[r.clone()].contains(MASK_CHAR) {
            Token::Masked
        } else {
            t
        };
        out.push((r.clone(), t));
        at = r.end;
    }
    if at < text.len() {
        out.push((at..text.len(), Token::Plain));
    }
    out
}

/// The end of a quoted string starting at `i` (`"` or `'`, single or
/// triple-quoted); an unterminated one ends at the line end (single) or the
/// end of the text (triple).
fn string_end(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let triple = b.len() >= i + 3 && b[i + 1] == q && b[i + 2] == q;
    let mut j = if triple { i + 3 } else { i + 1 };
    while j < b.len() {
        if q == b'"' && b[j] == b'\\' {
            j += 2;
            continue;
        }
        if triple {
            if b[j] == q && b.get(j + 1) == Some(&q) && b.get(j + 2) == Some(&q) {
                let mut end = j + 3;
                // """" closes with extra quotes inside the string.
                while end < b.len() && b[end] == q && end < j + 5 {
                    end += 1;
                }
                return end;
            }
        } else if b[j] == q {
            return j + 1;
        } else if b[j] == b'\n' {
            return j;
        }
        j += 1;
    }
    b.len()
}

fn is_bare(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

fn toml_tokens(text: &str) -> Vec<(Range<usize>, Token)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // At the start of a line (only spaces before): a key or a header may follow.
    let mut line_start = true;
    // Inside an inline table: a key may follow `{` or `,`.
    let mut brace_depth = 0usize;
    let mut key_next = true;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                i += 1;
                line_start = true;
                key_next = brace_depth == 0;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' => {
                let end = text[i..].find('\n').map_or(b.len(), |n| i + n);
                out.push((i..end, Token::Comment));
                i = end;
            }
            b'[' if line_start && brace_depth == 0 => {
                // A header: to the closing bracket(s) on this line.
                let mut j = i;
                let mut in_str: Option<u8> = None;
                while j < b.len() && b[j] != b'\n' {
                    match (in_str, b[j]) {
                        (None, b'"' | b'\'') => in_str = Some(b[j]),
                        (Some(q), x) if x == q => in_str = None,
                        (None, b'#') => break,
                        _ => {}
                    }
                    j += 1;
                }
                let end = text[i..j].rfind(']').map_or(j, |n| i + n + 1);
                out.push((i..end, Token::Header));
                i = end;
                line_start = false;
                key_next = false;
            }
            b'"' | b'\'' => {
                let end = string_end(b, i);
                let after = text[end..].trim_start_matches([' ', '\t']);
                let is_key = key_next && (after.starts_with('=') || after.starts_with('.'));
                out.push((i..end, if is_key { Token::Key } else { Token::String }));
                i = end;
                line_start = false;
            }
            b'=' => {
                out.push((i..i + 1, Token::Plain));
                i += 1;
                key_next = false;
                line_start = false;
            }
            b'{' => {
                out.push((i..i + 1, Token::Punct));
                brace_depth += 1;
                key_next = true;
                i += 1;
                line_start = false;
            }
            b'}' => {
                out.push((i..i + 1, Token::Punct));
                brace_depth = brace_depth.saturating_sub(1);
                key_next = false;
                i += 1;
            }
            b',' => {
                out.push((i..i + 1, Token::Plain));
                key_next = brace_depth > 0;
                i += 1;
            }
            b'[' | b']' => {
                out.push((i..i + 1, Token::Punct));
                i += 1;
                line_start = false;
            }
            b'.' if key_next => {
                out.push((i..i + 1, Token::Plain));
                i += 1;
            }
            _ if is_bare(c) || c == b'+' || c == b'.' || c == b':' => {
                let mut j = i;
                while j < b.len()
                    && (is_bare(b[j])
                        || matches!(b[j], b'+' | b'.' | b':')
                        || (b[j] == b' '
                            && !key_next
                            && b.get(j + 1).is_some_and(u8::is_ascii_digit)
                            && b[i].is_ascii_digit()))
                {
                    // Keys stop at dots (dotted keys).
                    if key_next && b[j] == b'.' {
                        break;
                    }
                    j += 1;
                }
                let word = &text[i..j];
                let t = if key_next {
                    Token::Key
                } else if matches!(word, "true" | "false" | "inf" | "nan" | "+inf" | "-inf") {
                    Token::Literal
                } else if word
                    .trim_start_matches(['+', '-'])
                    .starts_with(|ch: char| ch.is_ascii_digit())
                {
                    Token::Number
                } else {
                    Token::Plain
                };
                out.push((i..j, t));
                i = j.max(i + 1);
                line_start = false;
            }
            _ => {
                let w = text[i..].chars().next().map_or(1, char::len_utf8);
                i += w;
                line_start = false;
            }
        }
    }
    out
}

fn json_tokens(text: &str) -> Vec<(Range<usize>, Token)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let end = string_end(b, i);
                let after = text[end..].trim_start();
                let t = if after.starts_with(':') {
                    Token::Key
                } else {
                    Token::String
                };
                out.push((i..end, t));
                i = end;
            }
            b'{' | b'}' | b'[' | b']' => {
                out.push((i..i + 1, Token::Punct));
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let mut j = i + 1;
                while j < b.len() && matches!(b[j], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    j += 1;
                }
                out.push((i..j, Token::Number));
                i = j;
            }
            b't' | b'f' | b'n' => {
                let word = ["true", "false", "null"]
                    .into_iter()
                    .find(|w| text[i..].starts_with(w));
                match word {
                    Some(w) => {
                        out.push((i..i + w.len(), Token::Literal));
                        i += w.len();
                    }
                    None => i += 1,
                }
            }
            _ => {
                let w = text[i..].chars().next().map_or(1, char::len_utf8);
                i += w;
            }
        }
    }
    out
}

/// The color of a token.
pub fn color(pal: &Palette, t: Token) -> Color32 {
    match t {
        Token::Plain | Token::Punct => pal.text,
        Token::Comment => pal.text_subtle,
        Token::Header => pal.amber,
        Token::Key => pal.link,
        Token::String => pal.green,
        Token::Number | Token::Literal => pal.amber,
        Token::Masked => pal.text_subtle,
    }
}

/// The text as a highlighted layout job, problems underlined.
pub fn layout(
    text: &str,
    kind: CodexFileKind,
    problems: &[FileProblem],
    pal: &Palette,
    font: FontId,
    wrap_width: f32,
) -> LayoutJob {
    let toks = tokens(kind, text);
    // Underlined ranges (errors win over warnings where they overlap).
    let mut marks: Vec<(Range<usize>, Color32)> = problems
        .iter()
        .filter(|p| p.severity != ProblemSeverity::Info)
        .map(|p| {
            // The check may be one edit behind the text: stay on char boundaries.
            let floor = |mut i: usize| {
                i = i.min(text.len());
                while !text.is_char_boundary(i) {
                    i -= 1;
                }
                i
            };
            let start = floor(p.start);
            let mut end = floor(p.end.max(start));
            if end == start {
                // At least the character there (or the line's last one).
                end = text[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(text.len(), |(n, _)| start + n);
            }
            let color = if p.severity == ProblemSeverity::Error {
                pal.red
            } else {
                pal.amber
            };
            (start..end, color)
        })
        .collect();
    marks.sort_by_key(|(r, c)| (r.start, *c != pal.red));
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (r, _) in &toks {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    for (r, _) in &marks {
        cuts.push(r.start);
        cuts.push(r.end);
    }
    cuts.retain(|c| text.is_char_boundary(*c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut job = LayoutJob {
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        keep_trailing_whitespace: true,
        ..Default::default()
    };
    let mut ti = 0;
    for w in cuts.windows(2) {
        let (a, z) = (w[0], w[1]);
        if a >= z {
            continue;
        }
        while ti < toks.len() && toks[ti].0.end <= a {
            ti += 1;
        }
        let tok = toks
            .get(ti)
            .filter(|(r, _)| r.start <= a)
            .map_or(Token::Plain, |(_, t)| *t);
        let underline = marks
            .iter()
            .find(|(r, _)| r.start <= a && a < r.end)
            .map_or(Stroke::NONE, |(_, c)| Stroke::new(1.5, *c));
        job.append(
            &text[a..z],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: color(pal, tok),
                underline,
                ..Default::default()
            },
        );
    }
    if text.is_empty() {
        job.append(
            "",
            0.0,
            TextFormat {
                font_id: font,
                color: pal.text,
                ..Default::default()
            },
        );
    }
    job
}
