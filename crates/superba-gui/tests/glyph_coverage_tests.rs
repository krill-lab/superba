//! Every character Superba draws is in the bundled Geist faces or is CJK (drawn by
//! the system CJK fallback). Anything else renders as the missing-glyph box ("a box
//! with an X") wherever the platform CJK face lacks it, as PingFang and YaHei do
//! for many symbols (user bug: a box in front of "经本地路由" on macOS). Symbols
//! Geist lacks are drawn icons instead (`design::icons`).
//!
//! Checked: every value of every `crates/*/locales/*.toml` (the GUI layers the
//! core, switch, catalog, config and sessions catalogs; the CLI catalog is in too,
//! the same binary runs the CLI) and every string and char literal under
//! `crates/superba-gui/src` (comments skipped, escapes decoded), against
//! [`fonts::bundled_definitions`]: Superba's fonts minus the CJK faces.

#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use egui::{FontFamily, FontId};
use superba_core::i18n::flatten_toml;
use superba_gui::design::fonts;

/// Text Superba writes into Codex rather than draws, as `(path under src, char)`:
/// the reply stats line starts with ⚡ (`superba_core::codex`), and these files
/// build and edit that template. Before drawing, the GUI strips the ⚡ and paints
/// `Icon::Bolt` (the template editor's fixed prefix in `pages/router/settings.rs`,
/// and `QuotePreview`). Nothing else belongs here: draw an icon instead. (The
/// invisible marker U+2060 after the ⚡ needs no entry: egui draws joiners as
/// zero-width glyphs, which `has_glyph` reports as covered.)
const WRITTEN_INTO_CODEX: &[(&str, char)] = &[
    ("pages/router/stats_line.rs", '⚡'),
    ("design/widgets/quote_preview.rs", '⚡'),
];

/// Characters the CJK fallback face draws: CJK symbols and punctuation, the
/// unified ideographs, half-width and full-width forms.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x303F | 0x4E00..=0x9FFF | 0xFF00..=0xFFEF)
}

/// Every family the UI draws with (all five Geist chains).
fn families() -> [FontFamily; 5] {
    [
        FontFamily::Proportional,
        FontFamily::Monospace,
        FontFamily::Name(fonts::family::MEDIUM.into()),
        FontFamily::Name(fonts::family::SEMIBOLD.into()),
        FontFamily::Name(fonts::family::MONO_MEDIUM.into()),
    ]
}

/// An egui context with the bundled fonts only.
struct Coverage {
    ctx: egui::Context,
}

impl Coverage {
    fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::bundled_definitions());
        // The fonts are built on the first frame.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .drop_without_applying_deltas();
        Self { ctx }
    }

    /// Every family has a glyph for `c` (control characters need none).
    fn covers(&self, c: char) -> bool {
        c.is_control()
            || self.ctx.fonts_mut(|f| {
                families()
                    .into_iter()
                    .all(|family| f.has_glyph(&FontId::new(14.0, family), c))
            })
    }

    /// Characters of `text` that would render as the missing-glyph box.
    fn missing(&self, text: &str) -> BTreeSet<char> {
        text.chars()
            .filter(|&c| !is_cjk(c) && !self.covers(c))
            .collect()
    }
}

fn describe(c: char) -> String {
    format!("{c:?} (U+{:04X})", c as u32)
}

/// Path relative to `base` with `/` separators (stable across platforms).
fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .expect("path under base")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the GUI crate is inside crates/")
        .to_path_buf()
}

/// `crates/*/locales/*.toml`, sorted.
fn locale_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(crates_dir()).expect("read crates/") {
        let dir = entry.expect("crates/ entry").path().join("locales");
        if !dir.is_dir() {
            continue;
        }
        for file in fs::read_dir(&dir).expect("read a locales directory") {
            let path = file.expect("locales entry").path();
            if path.extension().is_some_and(|e| e == "toml") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read a GUI source directory") {
        let entry = entry.expect("source directory entry");
        let path = entry.path();
        if entry.file_type().expect("entry type").is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

/// Decode a cooked literal body starting at `s[i]` up to the closing `quote`.
/// Returns the text, the index after the closing quote and the newlines crossed.
fn cooked(s: &[char], mut i: usize, quote: char) -> (String, usize, usize) {
    let mut text = String::new();
    let mut lines = 0;
    while i < s.len() && s[i] != quote {
        if s[i] != '\\' {
            if s[i] == '\n' {
                lines += 1;
            }
            text.push(s[i]);
            i += 1;
            continue;
        }
        i += 1;
        match s[i] {
            'n' => text.push('\n'),
            't' => text.push('\t'),
            'r' => text.push('\r'),
            '0' => text.push('\0'),
            '\\' | '"' | '\'' => text.push(s[i]),
            'x' => {
                let hex: String = s[i + 1..i + 3].iter().collect();
                text.push(char::from(
                    u8::from_str_radix(&hex, 16).expect("\\x escape"),
                ));
                i += 2;
            }
            'u' => {
                let close = i + s[i..].iter().position(|&c| c == '}').expect("\\u{..}");
                let hex: String = s[i + 2..close].iter().collect();
                let code = u32::from_str_radix(&hex, 16).expect("\\u escape");
                text.push(char::from_u32(code).expect("\\u scalar value"));
                i = close;
            }
            '\n' | '\r' => {
                // Line continuation: the newline and the next line's indent vanish.
                while i < s.len() && s[i].is_whitespace() {
                    if s[i] == '\n' {
                        lines += 1;
                    }
                    i += 1;
                }
                continue;
            }
            other => panic!("unknown escape \\{other}"),
        }
        i += 1;
    }
    assert!(i < s.len(), "unclosed literal");
    (text, i + 1, lines)
}

/// The string and char literals of Rust `source` as `(line, text)`: comments
/// skipped, escapes decoded, byte literals left out (bytes are never drawn).
fn literals(source: &str) -> Vec<(usize, String)> {
    let s: Vec<char> = source.chars().collect();
    let at = |k: usize| s.get(k).copied();
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if c == '/' && at(i + 1) == Some('/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            let mut depth = 0;
            while i < s.len() {
                if s[i] == '/' && at(i + 1) == Some('*') {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && at(i + 1) == Some('/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if s[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if c == '"' {
            let (text, end, lines) = cooked(&s, i + 1, '"');
            out.push((line, text));
            line += lines;
            i = end;
        } else if c == '\'' {
            if at(i + 1) == Some('\\') {
                let (text, end, _) = cooked(&s, i + 1, '\'');
                out.push((line, text));
                i = end;
            } else if at(i + 2) == Some('\'') {
                out.push((line, s[i + 1].to_string()));
                i += 3;
            } else {
                // A lifetime or a loop label.
                i += 1;
            }
        } else if ident(c) {
            // An identifier, keyword or number, or the prefix of a byte, raw or C
            // string.
            let start = i;
            while i < s.len() && ident(s[i]) {
                i += 1;
            }
            let word: String = s[start..i].iter().collect();
            match (word.as_str(), at(i)) {
                ("r" | "br" | "cr", Some('"' | '#')) => {
                    let mut hashes = 0;
                    while at(i + hashes) == Some('#') {
                        hashes += 1;
                    }
                    if at(i + hashes) != Some('"') {
                        // A raw identifier (`r#type`).
                        i += hashes;
                        continue;
                    }
                    let body = i + hashes + 1;
                    let mut end = body;
                    while !(s[end] == '"' && (1..=hashes).all(|k| at(end + k) == Some('#'))) {
                        end += 1;
                    }
                    let text: String = s[body..end].iter().collect();
                    let lines = text.matches('\n').count();
                    if word != "br" {
                        out.push((line, text));
                    }
                    line += lines;
                    i = end + 1 + hashes;
                }
                ("b" | "c", Some('"')) => {
                    let (text, end, lines) = cooked(&s, i + 1, '"');
                    if word == "c" {
                        out.push((line, text));
                    }
                    line += lines;
                    i = end;
                }
                ("b", Some('\'')) => {
                    i = cooked(&s, i + 1, '\'').1;
                }
                _ => {}
            }
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn the_literal_scanner_skips_comments_and_decodes_escapes() {
    let source = r##"// ⇄ in a line comment
/* ∅ /* nested */ ✗ */ let a = "x\u{2713}";
fn f<'a>(s: &'a str) -> char { 'y' }
let r = r#"raw "⋯" "#; let b = b"\xff"; let q = '\''; let d = '"';
'outer: loop { let e = "two \
    lines"; let f = br"▾"; let g = r#type; }
let h = "after";"##;
    assert_eq!(
        literals(source),
        vec![
            (2, "x✓".to_owned()),
            (3, "y".to_owned()),
            (4, "raw \"⋯\" ".to_owned()),
            (4, "'".to_owned()),
            (4, "\"".to_owned()),
            (5, "two lines".to_owned()),
            (7, "after".to_owned()),
        ]
    );
}

#[test]
fn the_check_tells_geist_symbols_from_the_ones_drawn_as_icons() {
    let coverage = Coverage::new();
    for c in "Aa0 ·•—–→↑↓↵…≈≥×“”‹›●○§°".chars() {
        assert!(coverage.covers(c), "Geist draws {}", describe(c));
    }
    // What the UI drew before the fix (now icons), and what is left to the CJK
    // fallback.
    for c in "⇄ⓘ∅▾⋯✓✗⚡中，".chars() {
        assert!(!coverage.covers(c), "Geist has no {}", describe(c));
    }
    assert!(is_cjk('中') && is_cjk('，') && is_cjk('「') && !is_cjk('⇄'));
}

#[test]
fn every_locale_value_is_drawn_by_the_bundled_fonts_or_the_cjk_fallback() {
    let coverage = Coverage::new();
    let crates = crates_dir();
    let files = locale_files();
    for expected in [
        "superba-gui/locales/overview.zh-CN.toml",
        "superba-core/locales/zh-CN.toml",
        "superba-switch/locales/zh-CN.toml",
    ] {
        assert!(
            files.iter().any(|f| relative(f, &crates) == expected),
            "the scan finds {expected}"
        );
    }
    let mut problems = Vec::new();
    for path in &files {
        let source = fs::read_to_string(path).expect("read a locale file");
        let values = flatten_toml(&source).expect("locale file parses");
        let mut keys: Vec<&String> = values.keys().collect();
        keys.sort();
        for key in keys {
            for c in coverage.missing(&values[key]) {
                problems.push(format!(
                    "crates/{}: {key}: {}",
                    relative(path, &crates),
                    describe(c)
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "characters neither Geist nor the CJK fallback draws (use a drawn icon or a \
         Geist character):\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_gui_string_literal_is_drawn_by_the_bundled_fonts_or_the_cjk_fallback() {
    let coverage = Coverage::new();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    files.sort();
    assert!(
        files
            .iter()
            .any(|f| relative(f, &src) == "pages/overview/mod.rs")
    );
    let mut problems = Vec::new();
    let mut allowed = BTreeSet::new();
    for path in &files {
        let rel = relative(path, &src);
        let source = fs::read_to_string(path).expect("read a GUI source file");
        for (line, text) in literals(&source) {
            for c in coverage.missing(&text) {
                if WRITTEN_INTO_CODEX.contains(&(rel.as_str(), c)) {
                    allowed.insert((rel.clone(), c));
                } else {
                    problems.push(format!("src/{rel}:{line}: {} in {text:?}", describe(c)));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "characters neither Geist nor the CJK fallback draws (use a drawn icon or a \
         Geist character):\n{}",
        problems.join("\n")
    );
    for &(file, c) in WRITTEN_INTO_CODEX {
        assert!(
            allowed.contains(&(file.to_owned(), c)),
            "stale allowlist entry: {file} no longer has {}",
            describe(c)
        );
    }
}
