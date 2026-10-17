//! Fitting long values into fixed widths (user bug 15): one line, an ellipsis at
//! the end for prose and in the middle for paths, URLs and commands, so the file
//! name or the host stays visible. Widths come from a per-character measure: the
//! glyph advances of the font when drawing, display columns in tests (East Asian
//! wide characters count two).
//!
//! Also the small line and item diff the setting diff dialogs expand into.

/// Where a value that does not fit loses its characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ellipsis {
    /// Keep the start: `Always answer in Chinese and keep…`.
    End,
    /// Keep both ends, the last path segment first: `/Users/me/…/notifier`.
    Path,
    /// Keep both ends, the scheme and host first: `https://api.example.com/…/v1`.
    Url,
    /// Keep both ends evenly: `notify-send --app-name…--urgency low`.
    Middle,
}

/// What a value looks like; decides the font and the ellipsis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A file system path.
    Path,
    /// A URL.
    Url,
    /// A command line (or an argument list).
    Command,
    /// Free text: sentences, instructions, anything multi-line.
    Prose,
    /// A short token: enum value, number, identifier.
    Token,
}

impl Shape {
    /// Classify a string value.
    pub fn of(s: &str) -> Shape {
        let t = s.trim();
        if t.contains('\n') {
            return Shape::Prose;
        }
        let lower = t.to_ascii_lowercase();
        if let Some((scheme, rest)) = lower.split_once("://")
            && !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            && !rest.is_empty()
            && !t.contains(char::is_whitespace)
        {
            return Shape::Url;
        }
        let wide = t.chars().any(|c| char_columns(c) == 2);
        let first = t.split_whitespace().next().unwrap_or("");
        if looks_like_path(first) {
            // `/usr/bin/notifier --flag` is a command; a lone path (spaces included,
            // as in `~/Library/Application Support/x`) is a path.
            let flags = t.split_whitespace().skip(1).any(|w| w.starts_with('-'));
            return if flags { Shape::Command } else { Shape::Path };
        }
        if t.split_whitespace()
            .skip(1)
            .any(|w| w.starts_with("--") || (w.len() == 2 && w.starts_with('-')))
            && !wide
        {
            return Shape::Command;
        }
        if wide || t.split_whitespace().count() >= 3 {
            return Shape::Prose;
        }
        Shape::Token
    }

    /// Drawn in the monospace font (everything but prose).
    pub fn mono(self) -> bool {
        !matches!(self, Shape::Prose)
    }

    /// The ellipsis this shape uses.
    pub fn ellipsis(self) -> Ellipsis {
        match self {
            Shape::Path => Ellipsis::Path,
            Shape::Url => Ellipsis::Url,
            Shape::Command => Ellipsis::Middle,
            Shape::Prose | Shape::Token => Ellipsis::End,
        }
    }
}

fn looks_like_path(s: &str) -> bool {
    let b = s.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    let sep = s.chars().skip(1).any(|c| c == '/' || c == '\\');
    drive
        || s.starts_with("\\\\")
        || ((s.starts_with('/')
            || s.starts_with("~/")
            || s.starts_with("./")
            || s.starts_with("../"))
            && (sep || s.len() > 1))
}

/// Display columns of a character: 2 for East Asian wide and full-width forms,
/// 0 for control characters and combining marks, else 1.
pub fn char_columns(c: char) -> usize {
    let u = c as u32;
    if c.is_control() || (0x0300..=0x036F).contains(&u) || u == 0x200B || u == 0x2060 {
        return 0;
    }
    let wide = matches!(
        u,
        0x1100..=0x115F
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF
            | 0x20000..=0x3FFFD
    );
    if wide { 2 } else { 1 }
}

/// Display columns of a string.
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_columns).sum()
}

/// Shorten `text` to at most `max_cols` display columns (the ellipsis counts one).
pub fn elide_cols(text: &str, max_cols: usize, mode: Ellipsis) -> String {
    elide_by(text, max_cols as f32, mode, |c| char_columns(c) as f32)
}

/// Shorten `text` to at most `max` (in the unit of `width`, which measures one
/// character; the ellipsis `…` is measured too). `text` is returned unchanged
/// when it fits. Line breaks and tabs count as spaces; callers pass one line.
pub fn elide_by(
    text: &str,
    max: f32,
    mode: Ellipsis,
    mut width: impl FnMut(char) -> f32,
) -> String {
    elide_dyn(text, max, mode, &mut width)
}

fn elide_dyn(text: &str, max: f32, mode: Ellipsis, width: &mut dyn FnMut(char) -> f32) -> String {
    let chars: Vec<char> = text
        .chars()
        .map(|c| {
            if c == '\n' || c == '\t' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let w: Vec<f32> = chars.iter().map(|&c| width(c)).collect();
    let total: f32 = w.iter().sum();
    if total <= max {
        return chars.into_iter().collect();
    }
    let budget = max - width('…');
    if budget <= 0.0 {
        return "…".to_owned();
    }
    let n = chars.len();
    let sum = |r: std::ops::Range<usize>| -> f32 { w[r].iter().sum() };
    let (mut h, mut t) = (0usize, 0usize);
    let mut used = 0.0;
    // Grow the head (or the tail) one character at a time while it fits.
    let grow_head = |h: &mut usize, t: usize, used: &mut f32| {
        while *h + t < n && *used + w[*h] <= budget {
            *used += w[*h];
            *h += 1;
        }
    };
    let grow_tail = |h: usize, t: &mut usize, used: &mut f32| {
        while h + *t < n && *used + w[n - 1 - *t] <= budget {
            *used += w[n - 1 - *t];
            *t += 1;
        }
    };
    match mode {
        Ellipsis::End => grow_head(&mut h, t, &mut used),
        Ellipsis::Path => {
            // The last segment (with its separator), when it leaves room for a head.
            let trimmed = chars
                .iter()
                .rposition(|&c| c != '/' && c != '\\')
                .unwrap_or(0);
            let sep = chars[..trimmed]
                .iter()
                .rposition(|&c| c == '/' || c == '\\');
            if let Some(sep) = sep {
                let tail_w = sum(sep..n);
                if tail_w <= budget * 0.7 {
                    t = n - sep;
                    used = tail_w;
                }
            }
            if t == 0 {
                // A file name longer than the room: a short head, then as much of
                // the end of the name as fits.
                while h + t < n && used + w[h] <= budget * 0.3 {
                    used += w[h];
                    h += 1;
                }
                grow_tail(h, &mut t, &mut used);
            } else {
                grow_head(&mut h, t, &mut used);
                grow_tail(h, &mut t, &mut used);
            }
        }
        Ellipsis::Url => {
            // Scheme and host (up to the first '/' after "://") stay; when they do
            // not fit with room for the rest, the scheme goes first, then the end.
            let s: String = chars.iter().collect();
            let Some(i) = s.find("://") else {
                return elide_dyn(text, max, Ellipsis::Middle, width);
            };
            let start = s[..i + 3].chars().count();
            let end = chars[start..]
                .iter()
                .position(|&c| c == '/' || c == '?' || c == '#')
                .map_or(n, |p| start + p);
            if sum(0..end) <= budget * 0.85 {
                h = end;
                used = sum(0..end);
                grow_tail(h, &mut t, &mut used);
                grow_head(&mut h, t, &mut used);
            } else {
                let rest: String = chars[start..].iter().collect();
                let host_w = sum(start..end);
                let mode = if host_w <= budget * 0.85 {
                    Ellipsis::Url
                } else {
                    Ellipsis::End
                };
                return elide_scheme_less(&rest, max, mode, end - start, width);
            }
        }
        Ellipsis::Middle => balanced(&w, budget, &mut h, &mut t, &mut used),
    }
    let mut head: String = chars[..h].iter().collect();
    let tail: String = chars[n - t..].iter().collect();
    if mode == Ellipsis::End {
        let keep = head.trim_end().len();
        head.truncate(keep);
    }
    format!("{head}…{tail}")
}

/// A URL without its scheme: the host (`host` characters) kept, then the end
/// of the path (`Ellipsis::Url`), or only the start (`Ellipsis::End`).
fn elide_scheme_less(
    rest: &str,
    max: f32,
    mode: Ellipsis,
    host: usize,
    width: &mut dyn FnMut(char) -> f32,
) -> String {
    let total: f32 = rest.chars().map(&mut *width).sum();
    if total <= max {
        return rest.to_owned();
    }
    if mode == Ellipsis::End {
        return elide_dyn(rest, max, Ellipsis::End, width);
    }
    let chars: Vec<char> = rest.chars().collect();
    let w: Vec<f32> = chars.iter().map(|&c| width(c)).collect();
    let budget = max - width('…');
    let n = chars.len();
    let (h, mut t) = (host, 0usize);
    let mut used: f32 = w[..h].iter().sum();
    while h + t < n && used + w[n - 1 - t] <= budget {
        used += w[n - 1 - t];
        t += 1;
    }
    let head: String = chars[..h].iter().collect();
    let tail: String = chars[n - t..].iter().collect();
    format!("{head}…{tail}")
}

/// Alternate head and tail characters while they fit.
fn balanced(w: &[f32], budget: f32, h: &mut usize, t: &mut usize, used: &mut f32) {
    let n = w.len();
    loop {
        let mut grew = false;
        if *h + *t < n && *used + w[*h] <= budget {
            *used += w[*h];
            *h += 1;
            grew = true;
        }
        if *h + *t < n && *used + w[n - 1 - *t] <= budget {
            *used += w[n - 1 - *t];
            *t += 1;
            grew = true;
        }
        if !grew {
            break;
        }
    }
}

/// The first line of `text` with any text in it, and how many lines follow it
/// (a trailing line break adds none).
pub fn first_line(text: &str) -> (&str, usize) {
    let lines: Vec<&str> = text.lines().collect();
    match lines.iter().position(|l| !l.trim().is_empty()) {
        Some(i) => (lines[i], lines.len() - i - 1),
        None => ("", lines.len().saturating_sub(1)),
    }
}

/// One line of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffOp {
    /// In both.
    Same,
    /// Only before.
    Removed,
    /// Only after.
    Added,
}

/// Line (or item) diff of `before` and `after` by longest common subsequence;
/// above 250 000 comparisons everything before counts as removed and everything
/// after as added.
pub fn diff_lines<'a>(before: &[&'a str], after: &[&'a str]) -> Vec<(DiffOp, &'a str)> {
    let (n, m) = (before.len(), after.len());
    if n.saturating_mul(m) > 250_000 {
        return before
            .iter()
            .map(|l| (DiffOp::Removed, *l))
            .chain(after.iter().map(|l| (DiffOp::Added, *l)))
            .collect();
    }
    // lcs[i][j] = LCS length of before[i..] and after[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if before[i] == after[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(n.max(m));
    while i < n || j < m {
        if i < n && j < m && before[i] == after[j] {
            out.push((DiffOp::Same, before[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // A changed line reads as the old one, then the new one.
            out.push((DiffOp::Removed, before[i]));
            i += 1;
        } else {
            out.push((DiffOp::Added, after[j]));
            j += 1;
        }
    }
    out
}
