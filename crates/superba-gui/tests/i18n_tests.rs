#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use superba_core::i18n::{core_catalog_source, flatten_toml};
use superba_core::{AuthMode, Locale};
use superba_gui::i18n::{Tr, UI_AREAS, ui_files};

fn placeholders(text: &str) -> BTreeSet<&str> {
    text.split('{')
        .skip(1)
        .map(|tail| {
            let name = tail.split_once('}').expect("unclosed placeholder").0;
            assert!(
                !name.is_empty() && !name.contains('{'),
                "invalid placeholder in {text:?}"
            );
            name.split('|').next().unwrap()
        })
        .collect()
}

fn area_source(locale: Locale, area: &str) -> &'static str {
    ui_files(locale)
        .iter()
        .find(|(a, _)| *a == area)
        .unwrap_or_else(|| panic!("no {area} file for {locale:?}"))
        .1
}

#[test]
fn every_area_file_has_identical_keys_and_placeholders_in_both_languages() {
    let mut total = 0;
    for area in UI_AREAS {
        let en = flatten_toml(area_source(Locale::En, area))
            .unwrap_or_else(|e| panic!("{area}.en.toml: {e}"));
        let zh = flatten_toml(area_source(Locale::ZhCn, area))
            .unwrap_or_else(|e| panic!("{area}.zh-CN.toml: {e}"));
        total += en.len();
        assert_eq!(
            en.keys().collect::<BTreeSet<_>>(),
            zh.keys().collect::<BTreeSet<_>>(),
            "keys of {area}.en.toml and {area}.zh-CN.toml differ"
        );
        for (key, english) in &en {
            assert!(
                !english.is_empty() && !zh[key].is_empty(),
                "empty translation for {key} ({area})"
            );
            assert_eq!(
                placeholders(english),
                placeholders(&zh[key]),
                "placeholder mismatch for {key} ({area})"
            );
        }
    }
    assert!(total > 500, "only {total} UI keys");
}

#[test]
fn no_key_is_defined_in_two_area_files() {
    for locale in Locale::ALL {
        let mut owner: std::collections::BTreeMap<String, &str> = Default::default();
        for (area, source) in ui_files(locale) {
            for key in flatten_toml(source).expect("catalog parses").into_keys() {
                if let Some(first) = owner.insert(key.clone(), area) {
                    panic!("{key} is defined in {first} and {area} ({locale:?})");
                }
            }
        }
    }
}

#[test]
fn the_area_list_matches_the_locale_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut on_disk = BTreeSet::new();
    for entry in fs::read_dir(&dir).expect("read locales") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        if let Some(stem) = name.strip_suffix(".toml") {
            on_disk.insert(stem.to_owned());
        }
    }
    let listed: BTreeSet<String> = UI_AREAS
        .iter()
        .flat_map(|a| [format!("{a}.en"), format!("{a}.zh-CN")])
        .collect();
    assert_eq!(on_disk, listed, "every locale file is loaded, nothing else");
    for locale in Locale::ALL {
        let areas: Vec<&str> = ui_files(locale).iter().map(|(a, _)| *a).collect();
        assert_eq!(areas, UI_AREAS, "{locale:?} files follow UI_AREAS");
    }
}

fn rust_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read GUI source directory") {
        let entry = entry.expect("read directory entry");
        let path = entry.path();
        let kind = entry.file_type().expect("read entry type");
        if kind.is_dir() {
            rust_sources(&path, files);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

fn looks_like_key(text: &str) -> bool {
    // Numbers (including versions), URLs, paths and empty segments fail the
    // alphabet check below. Bare file names need an explicit suffix exclusion.
    if [
        ".json", ".toml", ".ttc", ".ttf", ".jsonl", ".sqlite", ".com",
    ]
    .iter()
    .any(|suffix| text.ends_with(suffix))
    {
        return false;
    }
    text.contains('.')
        && text.split('.').all(|segment| {
            !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
}

// Walk quoted strings without a regex. Skip comments and escaped quotes so
// prose and code between literals cannot accidentally become catalog keys.
fn string_literals(source: &str) -> Vec<(usize, &str)> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"//") {
            i += source[i..].find('\n').unwrap_or(bytes.len() - i);
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i..].starts_with(b"'\"'") {
            i += 3;
        } else if bytes[i] == b'r' {
            let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
            let quote = i + 1 + hashes;
            if bytes.get(quote) == Some(&b'"') {
                let start = quote + 1;
                let closing = format!("\"{}", "#".repeat(hashes));
                let end = start + source[start..].find(&closing).expect("closed raw string");
                literals.push((start, &source[start..end]));
                i = end + closing.len();
            } else {
                i += 1;
            }
        } else if bytes[i] == b'"' {
            let start = i + 1;
            i = start;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => break,
                    _ => i += 1,
                }
            }
            assert!(i < bytes.len(), "unclosed source string");
            literals.push((start, &source[start..i]));
            i += 1;
        } else {
            i += 1;
        }
    }
    literals
}

/// Whether a literal is UI copy: a dotted lower-case literal that is not one of
/// Codex's own `config.toml` paths. `looks_like_key` alone cannot tell the two
/// apart — `sandbox_workspace_write.network_access` is a config path (user bug
/// 41-level: the settings page named it in the intent chips), and the shipped
/// catalog is the authority on which paths exist.
fn is_message_key(text: &str, config_paths: &BTreeSet<String>) -> bool {
    looks_like_key(text) && !config_paths.contains(text)
}

/// Every `config.toml` path of the shipped catalog: the settings page's rows,
/// the keys the page writes and the feature registry's own paths.
fn config_paths() -> BTreeSet<String> {
    let store = superba_catalog::CatalogStore::builtin().expect("the shipped catalog");
    let catalog = store.latest().expect("the latest catalog version");
    catalog
        .config_keys
        .iter()
        .map(|k| k.path.clone())
        .chain(catalog.features.iter().map(|f| f.config_path.clone()))
        .collect()
}

#[test]
fn every_literal_gui_message_key_exists_in_an_english_catalog() {
    let mut keys = BTreeSet::new();
    for source in ui_files(Locale::En).iter().map(|(_, s)| *s).chain([
        superba_catalog::i18n_source(Locale::En),
        core_catalog_source(Locale::En),
        superba_switch::messages::catalog_source(Locale::En),
    ]) {
        keys.extend(flatten_toml(source).expect("catalog parses").into_keys());
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    files.sort();
    assert!(files.iter().any(|path| path == &src.join("shell.rs")));
    assert!(files.iter().any(|path| path.starts_with(src.join("pages"))));
    let mut found = BTreeSet::new();
    let mut missing = Vec::new();
    let paths = config_paths();
    for path in files {
        let source = fs::read_to_string(&path).expect("read GUI Rust source");
        for (offset, key) in string_literals(&source) {
            if is_message_key(key, &paths) {
                found.insert(key.to_owned());
                if !keys.contains(key) {
                    let line = source[..offset].bytes().filter(|&b| b == b'\n').count() + 1;
                    missing.push(format!("{}:{line}: {key}", path.display()));
                }
            }
        }
    }
    assert!(
        found.contains("nav.overview"),
        "source scan must find real GUI keys"
    );
    assert!(
        missing.is_empty(),
        "missing catalog keys:\n{}",
        missing.join("\n")
    );
}

#[test]
fn key_scanner_accepts_dotted_keys_and_excludes_files_urls_and_versions() {
    for key in [
        "nav.overview",
        "banner.waiting.thread_local",
        "under_score.other_key",
    ] {
        assert!(looks_like_key(key), "missed {key}");
    }
    for value in [
        "nav",
        "",
        ".nav",
        "nav.",
        "nav..overview",
        "Nav.overview",
        "nav.bad-key",
        "auth.json",
        "config.toml",
        "font.ttc",
        "font.ttf",
        "ledger.jsonl",
        "state.sqlite",
        "example.com",
        "https://example.org",
        "app://index.html",
        "0.162.0",
        "v1.2.3",
        "alpha.17.2",
        "../locales/en.toml",
    ] {
        assert!(!looks_like_key(value), "misidentified {value}");
    }
    let sample = r###"
        // "comment.key"
        /* "block.key" /* "nested.key" */ */
        let quote = '"';
        let text = "nav.overview";
        let escaped = "a \"fake.key\" quote";
        let raw = r#"banner.waiting.title"#;
        let raw_prose = r#"contains "fake.raw" quotes"#;
        let next = "action.cancel";
    "###;
    let scanned: Vec<_> = string_literals(sample)
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| looks_like_key(text))
        .collect();
    assert_eq!(
        scanned,
        ["nav.overview", "banner.waiting.title", "action.cancel"]
    );
    // The scan alone reads a Codex config path as a message key; the catalog's
    // path set is what excludes it — and only the paths that really exist:
    // a dotted name that is no path stays missing from the catalog.
    let paths = config_paths();
    assert!(
        paths.contains("sandbox_workspace_write.network_access"),
        "the catalog lists the network-access path"
    );
    assert!(looks_like_key("sandbox_workspace_write.network_access"));
    assert!(!is_message_key(
        "sandbox_workspace_write.network_access",
        &paths
    ));
    assert!(is_message_key("not.a.real.path", &paths));
    assert!(is_message_key("nav.overview", &paths));
}

#[test]
fn translator_uses_the_requested_language() {
    let en = Tr::new(Locale::En);
    let zh = Tr::new(Locale::ZhCn);
    assert_eq!(en.locale(), Locale::En);
    assert_eq!(zh.locale(), Locale::ZhCn);
    assert_eq!(en.t("nav.overview"), "Overview");
    assert_eq!(zh.t("nav.overview"), "概览");
}

#[test]
fn formatting_substitutes_every_named_argument() {
    let args: &[(&str, &dyn std::fmt::Display)] = &[("dest", &"CDN"), ("secs", &5)];
    assert_eq!(
        Tr::new(Locale::En).f("banner.armed.title", args),
        "Switching to “CDN” in 5 s"
    );
    assert_eq!(
        Tr::new(Locale::ZhCn).f("banner.armed.title", args),
        "5 秒后切换到「CDN」"
    );
}

#[test]
fn unknown_key_renders_as_itself_in_both_languages() {
    for locale in Locale::ALL {
        let tr = Tr::new(locale);
        assert_eq!(tr.t("test.nonexistent.message"), "test.nonexistent.message");
        assert_eq!(
            tr.f("test.nonexistent.message", &[]),
            "test.nonexistent.message"
        );
    }
}

#[test]
fn domain_text_uses_the_chinese_core_catalog() {
    let text = AuthMode::Pat.text();
    let translated = Tr::new(Locale::ZhCn).text(&text);
    assert_eq!(translated, "PAT（个人访问令牌）");
    assert_ne!(translated, text.render_fallback());
    assert_eq!(Tr::new(Locale::En).text(&text), text.render_fallback());
}

#[test]
fn counted_gui_copy_has_correct_singular_and_plural_forms() {
    let tr = Tr::new(Locale::En);
    for (key, arg, singular, plural) in [
        (
            "banner.waiting.title",
            "n",
            "1 conversation is running; switch paused",
            "2 conversations are running; switch paused",
        ),
        (
            "banner.show_changes",
            "n",
            "Show 1 change",
            "Show 2 changes",
        ),
        (
            "toast.recovered",
            "n",
            "Recovered 1 interrupted switch",
            "Recovered 2 interrupted switches",
        ),
        ("time.days_ago", "n", "1 day ago", "2 days ago"),
        ("time.in_days", "n", "in 1 day", "in 2 days"),
    ] {
        assert_eq!(tr.f(key, &[(arg, &1)]), singular, "{key}");
        assert_eq!(tr.f(key, &[(arg, &2)]), plural, "{key}");
        assert!(!tr.f(key, &[(arg, &0)]).contains('|'), "{key}");
    }
    assert!(
        tr.f("banner.note.cli", &[("n", &1)])
            .contains("process in a terminal is not affected; it uses")
    );
    assert!(
        tr.f("banner.note.cli", &[("n", &2)])
            .contains("processes in terminals are not affected; they use")
    );
    assert!(
        tr.f("wizard.legacy.skip_body", &[("count", &1)])
            .starts_with("This 1 session is")
    );
    // Hosts the app knows, not "connected" ones (review ux-visual #15).
    assert_eq!(
        tr.f("wizard.detect.remotes", &[("n", &1), ("live", &0)]),
        "The app knows 1 remote host (0 online)"
    );
    assert_eq!(
        tr.f("wizard.detect.remotes", &[("n", &2), ("live", &1)]),
        "The app knows 2 remote hosts (1 online)"
    );
}
