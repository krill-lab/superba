use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use superba_config::catalog_sources;
use superba_core::Locale;
use superba_core::i18n::flatten_toml;

fn catalogs() -> Vec<(Locale, BTreeMap<String, String>)> {
    catalog_sources()
        .into_iter()
        .map(|(locale, source)| (locale, flatten_toml(source).unwrap().into_iter().collect()))
        .collect()
}

fn placeholders(text: &str) -> BTreeSet<&str> {
    text.split('{')
        .skip(1)
        .map(|rest| {
            let (name, _) = rest.split_once('}').expect("unclosed catalog placeholder");
            let name = name.split('|').next().unwrap();
            assert!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "invalid placeholder in {text:?}"
            );
            name
        })
        .collect()
}

#[test]
fn both_catalogs_parse_and_have_identical_key_sets() {
    let all = catalogs();
    assert_eq!(
        all.iter().map(|(locale, _)| *locale).collect::<Vec<_>>(),
        vec![Locale::En, Locale::ZhCn]
    );
    assert!(!all[0].1.is_empty());
    assert_eq!(
        all[0].1.keys().collect::<BTreeSet<_>>(),
        all[1].1.keys().collect::<BTreeSet<_>>()
    );
    for (_, messages) in all {
        assert!(messages.keys().any(|k| k.starts_with("config.note.")));
        assert!(messages.keys().any(|k| k.starts_with("env.scope.")));
        assert!(messages.values().all(|s| !s.trim().is_empty()));
    }
}

#[test]
fn catalog_placeholder_names_match_for_every_key() {
    let all = catalogs();
    for (key, english) in &all[0].1 {
        let chinese = all[1]
            .1
            .get(key)
            .unwrap_or_else(|| panic!("missing Chinese key {key}"));
        assert_eq!(
            placeholders(english),
            placeholders(chinese),
            "placeholder mismatch for {key}"
        );
    }
    // Guard against a vacuous extractor if catalog formatting changes.
    assert_eq!(
        placeholders(&all[0].1["config.note.provider_base_url_missing"]),
        BTreeSet::from(["provider"])
    );
}

fn rust_sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push((
                path.to_string_lossy().into_owned(),
                fs::read_to_string(path).unwrap(),
            ));
        }
    }
}

// A small lexer suffices for source-text scanning without adding dependencies.
// It skips comments, preserves string literals as single tokens, and recognizes
// raw strings as well as normal strings. That keeps commas in fallback messages
// from looking like helper arguments.
fn tokens(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if source[i..].starts_with("//") {
            i += source[i..].find('\n').unwrap_or(bytes.len() - i);
        } else if source[i..].starts_with("/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth != 0 {
                if source[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if source[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += source[i..].chars().next().unwrap().len_utf8();
                }
            }
        } else if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            out.push(source[start..i].to_owned());
        } else if bytes[i] == b'r' && matches!(bytes.get(i + 1), Some(b'#' | b'"')) {
            let start = i;
            i += 1;
            while bytes.get(i) == Some(&b'#') {
                i += 1;
            }
            let hashes = i - start - 1;
            if bytes.get(i) == Some(&b'"') {
                i += 1;
                let end = format!("\"{}", "#".repeat(hashes));
                let len = source[i..]
                    .find(&end)
                    .expect("unterminated raw source string");
                out.push(format!("\"{}\"", &source[i..i + len]));
                i += len + end.len();
            } else {
                out.push(source[start..i].to_owned());
            }
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(source[start..i].to_owned());
        } else {
            let ch = source[i..].chars().next().unwrap();
            out.push(ch.to_string());
            i += ch.len_utf8();
        }
    }
    out
}

fn literal(token: &str) -> Option<&str> {
    token.strip_prefix('"')?.strip_suffix('"')
}

fn source_keys(source: &str) -> BTreeSet<String> {
    let tokens = tokens(source);
    let mut keys = BTreeSet::new();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(text) = literal(token)
            && [
                "config.note.",
                "config.settings.",
                "env.scope.",
                "codex_files.",
            ]
            .iter()
            .any(|prefix| text.starts_with(prefix))
            && !text.contains(['{', '}', ' ', '\n', '\\'])
        {
            keys.insert(text.to_owned());
        }
        // note(severity, "suffix", fallback) and t("suffix", fallback).
        if tokens.get(i + 1).is_none_or(|s| s != "(") {
            continue;
        }
        let key_index = match token.as_str() {
            "note" => {
                let Some(comma) = tokens[i + 2..].iter().position(|s| s == ",") else {
                    continue;
                };
                i + 2 + comma + 1
            }
            "t" => i + 2,
            _ => continue,
        };
        if let Some(suffix) = tokens.get(key_index).and_then(|t| literal(t)) {
            let prefix = if token == "note" {
                "config.note"
            } else {
                "env.scope"
            };
            keys.insert(format!("{prefix}.{suffix}"));
        }
    }
    keys
}

#[test]
fn every_literal_and_helper_generated_message_key_exists_in_both_catalogs() {
    let mut sources = Vec::new();
    rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    assert!(!sources.is_empty());
    let all = catalogs();
    let mut used = BTreeSet::new();
    for (path, source) in sources {
        for key in source_keys(&source) {
            for (locale, messages) in &all {
                assert!(
                    messages.contains_key(&key),
                    "{path} references {key}, missing from {locale:?}"
                );
            }
            used.insert(key);
        }
    }
    // Ensure both dynamic helper patterns were actually scanned.
    for key in [
        "config.note.provider_base_url_missing",
        "config.note.drift_kept",
        "env.scope.new_shells",
        "env.scope.macos_launchd",
        "env.scope.windows_terminal",
    ] {
        assert!(used.contains(key), "source scanner missed {key}");
    }
    assert_eq!(used, all[0].1.keys().cloned().collect::<BTreeSet<_>>());
    // Exercise unseen keys so the check cannot pass solely by looking up known
    // catalog suffixes in the source.
    assert_eq!(
        source_keys(
            r#"note(Notice::warning, "future_note", "Fallback"); t("future_scope", "Fallback"); LocalizedText::new("env.scope.literal", "Fallback");"#
        ),
        BTreeSet::from([
            "config.note.future_note".into(),
            "env.scope.future_scope".into(),
            "env.scope.literal".into()
        ])
    );
}
