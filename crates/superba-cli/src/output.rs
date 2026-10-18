//! Output sinks and small, localized rendering primitives.

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use superba_core::{Catalog, Error, Locale, LocalizedText, Notice, Result, Severity};

use crate::text::{catalog, text};

/// A cloneable in-memory writer for embedding and tests.
#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    /// Snapshot of captured UTF-8 output.
    pub fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|p| p.into_inner())).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Output owned by one invocation. JSON is written once, after the command finishes.
pub struct Output {
    pub json: bool,
    pub catalog: Catalog,
    pub(crate) verbose: bool,
    pub(crate) binary: &'static str,
    locale: Locale,
    stdout: Box<dyn Write + Send>,
    stderr: Box<dyn Write + Send>,
    stdout_terminal: bool,
    stderr_terminal: bool,
    progress_open: bool,
    document_written: bool,
}

impl Output {
    /// Create output with injectable writers.
    pub fn new(
        json: bool,
        locale: Locale,
        stdout: Box<dyn Write + Send>,
        stderr: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            json,
            catalog: catalog(locale),
            verbose: false,
            binary: "spb",
            locale,
            stdout,
            stderr,
            stdout_terminal: false,
            stderr_terminal: false,
            progress_open: false,
            document_written: false,
        }
    }

    /// Select terminal presentation for each injected stream. Captures and pipes
    /// default to plain lines; production supplies `IsTerminal` observations.
    pub fn with_terminals(mut self, stdout: bool, stderr: bool) -> Self {
        self.stdout_terminal = stdout;
        self.stderr_terminal = stderr;
        self
    }

    /// Both streams are terminals: a person can be asked a question.
    pub(crate) fn interactive(&self) -> bool {
        self.stdout_terminal && self.stderr_terminal
    }

    /// Capture both streams; `run` selects the invocation's JSON mode and locale.
    pub fn capture() -> (Self, Capture, Capture) {
        let stdout = Capture::default();
        let stderr = Capture::default();
        (
            Self::new(
                false,
                Locale::En,
                Box::new(stdout.clone()),
                Box::new(stderr.clone()),
            ),
            stdout,
            stderr,
        )
    }

    pub(crate) fn configure(&mut self, json: bool, verbose: bool, locale: Locale) {
        self.json = json;
        self.verbose = verbose;
        self.catalog = catalog(locale);
        self.locale = locale;
        self.document_written = false;
    }

    /// Locale of human output.
    pub(crate) fn locale(&self) -> Locale {
        self.locale
    }

    /// Render a localizable value through all catalog layers.
    pub fn render(&self, value: &LocalizedText) -> String {
        self.catalog.text(value)
    }

    pub(crate) fn label(&self, key: &str) -> String {
        self.render(&text(key))
    }

    pub(crate) fn none(&self) -> String {
        self.label("cli.value.none")
    }

    pub(crate) fn optional(&self, value: Option<impl ToString>) -> String {
        value.map(|v| v.to_string()).unwrap_or_else(|| self.none())
    }

    /// A time for people to read: local wall-clock time
    /// (`2026-10-18 07:10:23`), with a ` UTC` suffix where the local offset
    /// is unknown (user bug 62(b): times were shown as RFC 3339 UTC). JSON
    /// output keeps the timestamps as they are.
    pub(crate) fn time(&self, t: superba_core::Timestamp) -> String {
        superba_switch::localtime::local(t).to_string()
    }

    /// [`Self::time`] or the "none" label.
    pub(crate) fn optional_time(&self, t: Option<superba_core::Timestamp>) -> String {
        t.map(|t| self.time(t)).unwrap_or_else(|| self.none())
    }

    /// The local calendar date of `t` (`2026-10-18`).
    pub(crate) fn date(&self, t: superba_core::Timestamp) -> String {
        superba_switch::localtime::local(t).date()
    }

    pub(crate) fn boolean(&self, value: bool) -> String {
        self.label(if value {
            "cli.value.yes"
        } else {
            "cli.value.no"
        })
    }

    pub(crate) fn severity(&self, severity: Severity) -> String {
        self.label(match severity {
            Severity::Info => "cli.severity.info",
            Severity::Warning => "cli.severity.warning",
            Severity::Danger => "cli.severity.danger",
        })
    }

    /// Write one complete JSON document. Serialize before writing any bytes.
    pub fn value(&mut self, value: &impl Serialize) -> Result<()> {
        if !self.json {
            return Ok(());
        }
        if self.document_written {
            return Err(Error::other(self.label("cli.error.duplicate_json")));
        }
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|_| Error::other(self.label("cli.error.serialize")))?;
        bytes.push(b'\n');
        self.document_written = true;
        self.stdout
            .write_all(&bytes)
            .and_then(|()| self.stdout.flush())
            .map_err(|e| Error::io("stdout", &e))
    }

    /// Write localized human output to stdout.
    pub fn line(&mut self, value: LocalizedText) -> Result<()> {
        self.data_line(&self.render(&value))
    }

    pub(crate) fn data_line(&mut self, line: &str) -> Result<()> {
        if self.json {
            return Ok(());
        }
        writeln!(self.stdout, "{line}").map_err(|e| Error::io("stdout", &e))
    }

    pub(crate) fn field(&mut self, key: &str, value: impl ToString) -> Result<()> {
        self.line(
            text("cli.format.field")
                .arg("label", self.label(key))
                .arg("value", value),
        )
    }

    pub(crate) fn table(
        &mut self,
        headers: &[&str],
        rows: impl IntoIterator<Item = Vec<String>>,
    ) -> Result<()> {
        if self.json {
            return Ok(());
        }
        let mut table = vec![
            headers
                .iter()
                .map(|key| self.label(key))
                .collect::<Vec<_>>(),
        ];
        table.extend(rows);
        let widths = (0..headers.len())
            .map(|column| {
                table
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| display_width(cell))
                    .max()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        // Columns of numbers (and empty markers) are right-aligned.
        let none = self.none();
        let numeric = (0..headers.len())
            .map(|column| {
                let mut cells = table.iter().skip(1).filter_map(|row| row.get(column));
                let mut any = false;
                let all = cells.all(|c| {
                    let number = is_number(c);
                    any |= number;
                    number || *c == none
                });
                all && any
            })
            .collect::<Vec<_>>();
        for (index, row) in table.into_iter().enumerate() {
            let cells = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    let padding = widths
                        .get(column)
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(display_width(cell));
                    let right = index > 0 && numeric.get(column).copied().unwrap_or(false);
                    if !self.stdout_terminal {
                        cell.clone()
                    } else if right {
                        format!("{}{cell}", " ".repeat(padding))
                    } else if column + 1 < row.len() {
                        format!("{cell}{}", " ".repeat(padding))
                    } else {
                        cell.clone()
                    }
                })
                .collect::<Vec<_>>();
            self.data_line(&cells.join(" | "))?;
        }
        Ok(())
    }

    pub(crate) fn section_table(
        &mut self,
        title: &str,
        headers: &[&str],
        rows: Vec<Vec<String>>,
    ) -> Result<()> {
        self.line(text("cli.format.section").arg("title", self.label(title)))?;
        if rows.is_empty() {
            self.line(text("cli.value.empty"))
        } else {
            self.table(headers, rows)
        }
    }

    pub(crate) fn notice(&mut self, notice: &Notice) -> Result<()> {
        self.line(
            text("cli.format.notice")
                .arg("severity", self.severity(notice.severity))
                .arg_text("text", notice.text.clone()),
        )
    }

    /// Write progress to stderr. JSON invocations suppress progress entirely.
    pub fn progress(&mut self, value: LocalizedText) -> Result<()> {
        if self.json {
            return Ok(());
        }
        let line = self.render(&value);
        self.finish_progress()?;
        writeln!(self.stderr, "{line}")
            .and_then(|()| self.stderr.flush())
            .map_err(|e| Error::io("stderr", &e))
    }

    pub(crate) fn countdown_tick(&mut self, value: LocalizedText) -> Result<()> {
        if self.json {
            return Ok(());
        }
        if !self.stderr_terminal {
            return self.progress(value);
        }
        let line = self.render(&value);
        self.progress_open = true;
        write!(self.stderr, "\r{line}\x1b[K")
            .and_then(|()| self.stderr.flush())
            .map_err(|e| Error::io("stderr", &e))
    }

    /// A progress line that keeps changing (the session migration): rewritten in
    /// place on a terminal; elsewhere written only at `milestone`s (a new stage,
    /// the end) so logs stay short.
    pub(crate) fn live_progress(&mut self, value: LocalizedText, milestone: bool) -> Result<()> {
        if self.json || (!self.stderr_terminal && !milestone) {
            return Ok(());
        }
        self.countdown_tick(value)
    }

    pub(crate) fn finish_progress(&mut self) -> Result<()> {
        if self.progress_open {
            self.progress_open = false;
            writeln!(self.stderr)
                .and_then(|()| self.stderr.flush())
                .map_err(|e| Error::io("stderr", &e))?;
        }
        Ok(())
    }

    pub(crate) fn report_error(&mut self, error: &Error) {
        if self.json {
            // A failed pipe cannot be repaired by appending a second JSON document.
            if !self.document_written {
                let _ = self.value(&serde_json::json!({ "error": error }));
            }
        } else {
            let _ = self.progress(text("cli.error.prefix").arg_text("error", error.to_text()));
            if let Error::Blocked { reasons } = error {
                for reason in reasons {
                    let _ = self.progress(reason.text());
                }
            }
            if let Error::NotFound { what } = error {
                // Service errors identify these resources by a stable leading
                // noun. CLI-local errors match their localized catalog prefix.
                let hint = if what.starts_with("profile ")
                    || what
                        .starts_with(&self.render(&text("cli.error.profile_missing").arg("id", "")))
                {
                    Some("cli.hint.profiles")
                } else if what.starts_with("line ") {
                    Some("cli.hint.lines")
                } else if what.starts_with("site ")
                    || what.starts_with(&self.render(&text("cli.error.site_missing").arg("id", "")))
                {
                    Some("cli.hint.sites")
                } else {
                    None
                };
                if let Some(hint) = hint {
                    let _ = self.progress(text(hint).arg("binary", self.binary));
                }
            }
        }
    }

    pub(crate) fn help(&mut self, help: &str, usage_error: bool) -> Result<()> {
        if self.json {
            self.value(&serde_json::json!({ "help": help }))
        } else if usage_error {
            writeln!(self.stderr, "{help}")
                .and_then(|()| self.stderr.flush())
                .map_err(|e| Error::io("stderr", &e))
        } else {
            self.data_line(help)
        }
    }
}

/// Terminal cell width for labels and paths. East Asian wide/fullwidth ranges
/// occupy two cells; common combining marks and variation selectors occupy none.
/// This deliberately avoids a Unicode dependency for our small table renderer.
pub(crate) fn display_width(value: &str) -> usize {
    value
        .chars()
        .map(|ch| match ch {
            '\u{0000}'..='\u{001f}'
            | '\u{007f}'..='\u{009f}'
            | '\u{0300}'..='\u{036f}'
            | '\u{1ab0}'..='\u{1aff}'
            | '\u{1dc0}'..='\u{1dff}'
            | '\u{200b}'..='\u{200f}'
            | '\u{20d0}'..='\u{20ff}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{fe20}'..='\u{fe2f}'
            | '\u{e0100}'..='\u{e01ef}' => 0,
            '\u{1100}'..='\u{115f}'
            | '\u{2329}'..='\u{232a}'
            | '\u{2e80}'..='\u{303e}'
            | '\u{3040}'..='\u{a4cf}'
            | '\u{a960}'..='\u{a97c}'
            | '\u{ac00}'..='\u{d7a3}'
            | '\u{f900}'..='\u{faff}'
            | '\u{fe10}'..='\u{fe19}'
            | '\u{fe30}'..='\u{fe6f}'
            | '\u{ff01}'..='\u{ff60}'
            | '\u{ffe0}'..='\u{ffe6}'
            | '\u{1b000}'..='\u{1b2ff}'
            | '\u{1f300}'..='\u{1f64f}'
            | '\u{1f680}'..='\u{1f6ff}'
            | '\u{1f900}'..='\u{1faff}'
            | '\u{20000}'..='\u{3fffd}' => 2,
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_align_cjk_and_combining_text_on_terminals_only() {
        for terminal in [false, true] {
            let (out, stdout, _) = Output::capture();
            let mut out = out.with_terminals(terminal, false);
            out.table(
                &["cli.label.name", "cli.label.id"],
                [
                    vec!["海外线路".into(), "a".into()],
                    vec!["Tokyo".into(), "b".into()],
                    vec!["e\u{301}".into(), "c".into()],
                    vec!["Ａ𠀀".into(), "d".into()],
                ],
            )
            .unwrap();
            if terminal {
                assert_eq!(
                    stdout.contents(),
                    "Name     | ID\n海外线路 | a\nTokyo    | b\ne\u{301}        | c\nＡ𠀀     | d\n"
                );
            } else {
                assert_eq!(
                    stdout.contents(),
                    "Name | ID\n海外线路 | a\nTokyo | b\ne\u{301} | c\nＡ𠀀 | d\n"
                );
            }
        }
    }
}

/// A number as tables print it: digits with separators, a sign, a currency or
/// percent sign.
fn is_number(cell: &str) -> bool {
    cell.bytes().any(|b| b.is_ascii_digit())
        && cell
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b',' | b'.' | b'$' | b'%' | b'-' | b'+'))
}

/// `1084640` -> `1,084,640`.
pub(crate) fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod number_tests {
    use super::{grouped, is_number};

    #[test]
    fn numbers_are_grouped_and_recognised() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_084_640), "1,084,640");
        for n in ["12", "1,084,640", "$1.50", "$0.0042", "-3", "45%"] {
            assert!(is_number(n), "{n}");
        }
        for s in ["gpt-5.5", "-", "", "Overseas"] {
            assert!(!is_number(s), "{s}");
        }
    }
}
