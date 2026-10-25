//! Pure text operations on shell rc files, `environment.d` files and the macOS
//! LaunchAgent plist: Superba's marker block (shared format from
//! `superba_core::env`), legacy / foreign line detection and (un)commenting.

use superba_core::ShellKind;
use superba_core::env::{
    LEGACY_COMMENT_PREFIX, MARKER_BEGIN, MARKER_END, MARKER_NOTE, export_value, find_block,
    legacy_krill_export_value, render_block,
};

/// Syntax of a file Superba writes a variable into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// POSIX shell / fish marker block.
    Shell(ShellKind),
    /// systemd `environment.d` (`NAME=value` inside the marker block).
    EnvironmentD,
}

/// Where a new block goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Top of the file (after a shebang): `~/.bashrc`, so the value is set before
    /// the usual "return if not interactive" guard.
    Top,
    /// End of the file.
    Bottom,
}

/// Render a complete block for `vars` in `dialect`.
pub(crate) fn render(dialect: &Dialect, vars: &[(String, String)]) -> String {
    match dialect {
        Dialect::Shell(shell) => {
            let refs: Vec<(&str, &str)> =
                vars.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            render_block(shell, &refs)
        }
        Dialect::EnvironmentD => {
            let mut s = format!("{MARKER_BEGIN}\n{MARKER_NOTE}\n");
            for (k, v) in vars {
                s.push_str(&format!("{k}={v}\n"));
            }
            s.push_str(MARKER_END);
            s.push('\n');
            s
        }
    }
}

/// Variables set inside Superba's block (`None` = no block).
pub(crate) fn block_vars(text: &str, dialect: &Dialect) -> Option<Vec<(String, String)>> {
    let range = find_block(text)?;
    let mut vars = Vec::new();
    for line in text[range].lines() {
        let t = line.trim();
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let name = match dialect {
            Dialect::EnvironmentD => t.split_once('=').map(|(n, _)| n.trim()),
            Dialect::Shell(_) => t
                .strip_prefix("export ")
                .and_then(|r| r.split_once('=').map(|(n, _)| n.trim()))
                .or_else(|| {
                    t.strip_prefix("set -gx ")
                        .and_then(|r| r.split_whitespace().next())
                }),
        };
        let Some(name) = name.filter(|n| !n.is_empty()) else {
            continue;
        };
        let value = match dialect {
            Dialect::EnvironmentD => t.split_once('=').map(|(_, v)| v.trim().to_owned()),
            Dialect::Shell(_) => export_value(t, name),
        };
        if let Some(v) = value {
            vars.push((name.to_owned(), v));
        }
    }
    Some(vars)
}

/// The value of `var` inside Superba's block.
pub(crate) fn block_value(text: &str, dialect: &Dialect, var: &str) -> Option<String> {
    block_vars(text, dialect)?
        .into_iter()
        .find(|(n, _)| n == var)
        .map(|(_, v)| v)
}

/// Insert, replace (`block = Some`) or remove (`None`) Superba's block.
pub(crate) fn put_block(text: &str, block: Option<&str>, placement: Placement) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let block = block.map(|b| {
        if nl == "\r\n" {
            b.replace('\n', "\r\n")
        } else {
            b.to_owned()
        }
    });
    if let Some(range) = find_block(text) {
        let mut out = String::with_capacity(text.len());
        let (head, tail) = (&text[..range.start], &text[range.end..]);
        match &block {
            Some(b) => {
                out.push_str(head);
                out.push_str(b);
                out.push_str(tail);
            }
            None => {
                // Drop the separator blank line inserted together with the block.
                let shebang_only = head.starts_with("#!")
                    && head.ends_with(nl)
                    && !head[..head.len() - nl.len()].contains('\n');
                if tail.is_empty() && head.ends_with(&format!("{nl}{nl}")) {
                    out.push_str(&head[..head.len() - nl.len()]);
                } else if head.is_empty() || shebang_only {
                    out.push_str(head);
                    out.push_str(tail.strip_prefix(nl).unwrap_or(tail));
                } else {
                    out.push_str(head);
                    out.push_str(tail);
                }
            }
        }
        return out;
    }
    let Some(b) = block else {
        return text.to_owned();
    };
    match placement {
        Placement::Bottom => {
            let mut out = text.to_owned();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push_str(nl);
                }
                out.push_str(nl);
            }
            out.push_str(&b);
            out
        }
        Placement::Top => {
            // Keep a shebang on the first line.
            let (head, rest) = match text.strip_prefix("#!") {
                Some(_) => match text.find('\n') {
                    Some(i) => text.split_at(i + 1),
                    None => (text, ""),
                },
                None => ("", text),
            };
            let mut out = String::with_capacity(text.len() + b.len() + 2);
            out.push_str(head);
            if !head.is_empty() && !head.ends_with('\n') {
                out.push_str(nl);
            }
            out.push_str(&b);
            if !rest.is_empty() {
                out.push_str(nl);
            }
            out.push_str(rest);
            out
        }
    }
}

/// A variable assignment found outside Superba's block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FoundLine {
    /// 1-based line number.
    pub line_no: usize,
    /// Full line (without the line break).
    pub text: String,
    /// Value it sets.
    pub value: String,
    /// True for a legacy Krill tutorial line (`CODEX_AUTHAPI_BASE_URL` pointing at
    /// a Krill host); false for anything else (the user's own value).
    pub legacy: bool,
}

/// Lines that set `var` outside Superba's block.
pub(crate) fn scan(text: &str, var: &str) -> Vec<FoundLine> {
    let block = find_block(text);
    let mut out = Vec::new();
    let mut offset = 0;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += raw.len();
        if block.as_ref().is_some_and(|r| r.contains(&start)) {
            continue;
        }
        let line = raw.trim_end_matches(['\n', '\r']);
        let Some(value) = export_value(line, var) else {
            continue;
        };
        let legacy = var == superba_core::codex::ENV_AUTHAPI_BASE_URL
            && legacy_krill_export_value(line).is_some();
        out.push(FoundLine {
            line_no: i + 1,
            text: line.to_owned(),
            value,
            legacy,
        });
    }
    out
}

/// Comment out line `line_no` if it still reads `original`; otherwise find the
/// first line equal to `original` (the file shifted). `None` if not found.
pub(crate) fn comment_line(text: &str, line_no: usize, original: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let matches = |l: &str| l.trim_end_matches(['\n', '\r']) == original;
    let idx = if lines
        .get(line_no.wrapping_sub(1))
        .is_some_and(|l| matches(l))
    {
        line_no - 1
    } else {
        lines.iter().position(|l| matches(l))?
    };
    let mut out = String::with_capacity(text.len() + LEGACY_COMMENT_PREFIX.len());
    for (i, l) in lines.iter().enumerate() {
        if i == idx {
            out.push_str(LEGACY_COMMENT_PREFIX);
        }
        out.push_str(l);
    }
    Some(out)
}

/// Restore a line commented by [`comment_line`]. `None` if not found.
pub(crate) fn uncomment_line(text: &str, original: &str) -> Option<String> {
    let commented = format!("{LEGACY_COMMENT_PREFIX}{original}");
    let mut found = false;
    let mut out = String::with_capacity(text.len());
    for l in text.split_inclusive('\n') {
        if !found && l.trim_end_matches(['\n', '\r']) == commented {
            found = true;
            out.push_str(&l[LEGACY_COMMENT_PREFIX.len()..]);
        } else {
            out.push_str(l);
        }
    }
    found.then_some(out)
}

/// True if `text` still contains `original` commented out by Superba.
pub(crate) fn has_commented(text: &str, original: &str) -> bool {
    let commented = format!("{LEGACY_COMMENT_PREFIX}{original}");
    text.lines().any(|l| l.trim_end_matches('\r') == commented)
}

/// Escape XML text.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// LaunchAgent label for `var`.
pub(crate) fn launch_agent_label(var: &str) -> String {
    format!(
        "com.krill-code.superba.env.{}",
        var.to_ascii_lowercase().replace('_', "-")
    )
}

/// macOS LaunchAgent that runs `launchctl setenv <var> <value>` at every login.
pub(crate) fn render_plist(var: &str, value: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Managed by Superba: sets {var} for apps started from the Dock / Finder. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/launchctl</string>
    <string>setenv</string>
    <string>{var}</string>
    <string>{value}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#,
        label = xml_escape(&launch_agent_label(var)),
        var = xml_escape(var),
        value = xml_escape(value),
    )
}

/// The value set by a plist written by [`render_plist`].
pub(crate) fn plist_value(text: &str, var: &str) -> Option<String> {
    let strings: Vec<String> = text
        .split("<string>")
        .skip(1)
        .filter_map(|s| s.split_once("</string>").map(|(v, _)| xml_unescape(v)))
        .collect();
    // Label, /bin/launchctl, setenv, VAR, VALUE
    let i = strings.iter().position(|s| s == "setenv")?;
    (strings.get(i + 1)? == var).then(|| strings.get(i + 2).cloned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAR: &str = "CODEX_AUTHAPI_BASE_URL";

    fn posix() -> Dialect {
        Dialect::Shell(ShellKind::Sh)
    }

    #[test]
    fn block_roundtrip_bottom_and_top() {
        let original = "# my rc\nalias ll='ls -l'\n";
        let block = render(&posix(), &[(VAR.into(), "https://api.kbsfdxc.net".into())]);
        let with = put_block(original, Some(&block), Placement::Bottom);
        assert!(with.starts_with(original));
        assert_eq!(
            block_value(&with, &posix(), VAR).as_deref(),
            Some("https://api.kbsfdxc.net")
        );
        assert_eq!(put_block(&with, None, Placement::Bottom), original);

        let top = put_block(original, Some(&block), Placement::Top);
        assert!(top.starts_with(MARKER_BEGIN));
        assert_eq!(put_block(&top, None, Placement::Top), original);

        let shebang = "#!/bin/bash\necho hi\n";
        let top = put_block(shebang, Some(&block), Placement::Top);
        assert!(top.starts_with("#!/bin/bash\n# >>> superba >>>"));
        assert_eq!(put_block(&top, None, Placement::Top), shebang);

        // Replace in place.
        let b2 = render(&posix(), &[(VAR.into(), "https://api.kbsfdxc.com".into())]);
        let replaced = put_block(&with, Some(&b2), Placement::Top);
        assert!(replaced.starts_with(original));
        assert_eq!(
            block_value(&replaced, &posix(), VAR).as_deref(),
            Some("https://api.kbsfdxc.com")
        );
        // Empty file.
        let fresh = put_block("", Some(&block), Placement::Bottom);
        assert_eq!(fresh, block);
        assert_eq!(put_block(&fresh, None, Placement::Bottom), "");
    }

    #[test]
    fn fish_and_envd_blocks() {
        let fish = Dialect::Shell(ShellKind::Fish);
        let b = render(&fish, &[(VAR.into(), "https://x.krill-code.net".into())]);
        assert!(b.contains("set -gx CODEX_AUTHAPI_BASE_URL \"https://x.krill-code.net\""));
        assert_eq!(
            block_value(&b, &fish, VAR).as_deref(),
            Some("https://x.krill-code.net")
        );
        let d = render(
            &Dialect::EnvironmentD,
            &[(VAR.into(), "https://x.krill-code.net".into())],
        );
        assert!(d.contains("\nCODEX_AUTHAPI_BASE_URL=https://x.krill-code.net\n"));
        assert_eq!(
            block_value(&d, &Dialect::EnvironmentD, VAR).as_deref(),
            Some("https://x.krill-code.net")
        );
    }

    #[test]
    fn scan_comment_uncomment() {
        let text = "export PATH=$PATH:~/bin\nexport CODEX_AUTHAPI_BASE_URL=\"https://api-slb.kbsfdxc.net\"\nexport CODEX_AUTHAPI_BASE_URL=https://example.com\n# export CODEX_AUTHAPI_BASE_URL=https://api.kbsfdxc.net\n";
        let found = scan(text, VAR);
        assert_eq!(found.len(), 2);
        assert!(found[0].legacy);
        assert_eq!(found[0].line_no, 2);
        assert!(!found[1].legacy);
        let commented = comment_line(text, 2, &found[0].text).unwrap();
        assert!(
            commented.contains(
                "# [superba] export CODEX_AUTHAPI_BASE_URL=\"https://api-slb.kbsfdxc.net\""
            )
        );
        assert!(has_commented(&commented, &found[0].text));
        assert_eq!(scan(&commented, VAR).len(), 1);
        assert_eq!(uncomment_line(&commented, &found[0].text).unwrap(), text);
        // Shifted lines are found by content.
        let shifted = format!("# new first line\n{text}");
        assert!(comment_line(&shifted, 2, &found[0].text).is_some());
        assert!(comment_line(text, 2, "nope").is_none());
        // Lines inside our block are not reported.
        let block = render(&posix(), &[(VAR.into(), "https://api.kbsfdxc.net".into())]);
        assert!(scan(&block, VAR).is_empty());
    }

    #[test]
    fn crlf_preserved() {
        let text = "a=1\r\nb=2\r\n";
        let block = render(&posix(), &[(VAR.into(), "https://api.kbsfdxc.net".into())]);
        let with = put_block(text, Some(&block), Placement::Bottom);
        assert!(!with.replace("\r\n", "").contains('\n'));
        assert_eq!(put_block(&with, None, Placement::Bottom), text);
    }

    #[test]
    fn plist() {
        let p = render_plist(VAR, "https://a.krill-code.net/?x=1&y=2");
        assert!(p.contains("<string>https://a.krill-code.net/?x=1&amp;y=2</string>"));
        assert_eq!(
            plist_value(&p, VAR).as_deref(),
            Some("https://a.krill-code.net/?x=1&y=2")
        );
        assert_eq!(plist_value(&p, "OTHER"), None);
        assert_eq!(
            launch_agent_label(VAR),
            "com.krill-code.superba.env.codex-authapi-base-url"
        );
    }
}
