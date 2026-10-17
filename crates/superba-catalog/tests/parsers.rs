//! Overlay ids and files, curation rules.

use pretty_assertions::assert_eq;
use superba_catalog::curation::{Curation, path_has_prefix};
use superba_catalog::overlay::{OverlayFile, text_hash};
use superba_catalog::{EntryId, Explanation, Os, Overlay, OverlayEntry, ZhState};

#[test]
fn entry_ids_round_trip() {
    for (s, id) in [
        ("feature:fast_mode", EntryId::Feature("fast_mode".into())),
        (
            "feature:guardianv2.thread_context",
            EntryId::Feature("guardianv2.thread_context".into()),
        ),
        (
            "config:mcp_servers.<name>.command",
            EntryId::Config("mcp_servers.<name>.command".into()),
        ),
        (
            "config:approval_policy=on-request",
            EntryId::Value {
                path: "approval_policy".into(),
                value: "on-request".into(),
            },
        ),
    ] {
        assert_eq!(EntryId::parse(s).unwrap(), id);
        assert_eq!(id.to_string(), s);
    }
    for bad in ["fast_mode", "feature:", "config:", "x:y"] {
        assert!(EntryId::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn zh_state_follows_the_basis_hash() {
    let en_hash = text_hash(None, "Official text.");
    let mut e = OverlayEntry {
        en: "Official text.".into(),
        en_hash: en_hash.clone(),
        ..OverlayEntry::default()
    };
    assert_eq!(e.zh_state(), ZhState::Missing);
    e.zh = "人工撰写".into();
    e.zh_basis = Some(en_hash.clone());
    assert_eq!(e.zh_state(), ZhState::Current);
    e.zh_basis = Some("0000000000000000".into());
    assert_eq!(e.zh_state(), ZhState::Stale);
    e.zh_basis = None;
    assert_eq!(e.zh_state(), ZhState::Stale);
    assert_eq!(text_hash(None, ""), "");
    assert_ne!(text_hash(Some("a"), "b"), text_hash(None, "ab"));
    assert_eq!(text_hash(None, "x").len(), 16);
}

#[test]
fn explanation_prefers_official_english_and_hides_notes_then() {
    let entry = OverlayEntry {
        en_note: Some("Human note.".into()),
        zh: "中文".into(),
        ..OverlayEntry::default()
    };
    let e = Explanation::new(None, Some("Official."), Some(&entry));
    assert_eq!(e.official_en.as_deref(), Some("Official."));
    assert_eq!(e.en_note, None);
    assert_eq!(e.zh.as_deref(), Some("中文"));
    let e = Explanation::new(None, Some("  "), Some(&entry));
    assert!(e.en_missing());
    assert_eq!(e.en_note.as_deref(), Some("Human note."));
    let e = Explanation::new(None, None, None);
    assert!(e.en_missing());
    assert_eq!(e.zh_state, ZhState::Missing);
}

#[test]
fn overlay_files_are_validated() {
    let ok = "format = 1\n[\"feature:x\"]\nen = \"\"\nen_hash = \"\"\nzh = \"\"\n";
    let f = OverlayFile::parse("a.toml", ok).unwrap();
    assert_eq!(f.entries.len(), 1);
    assert!(OverlayFile::parse("a.toml", "format = 2\n").is_err());
    assert!(OverlayFile::parse("a.toml", "format = 1\n[\"bogus\"]\nen = \"\"\n").is_err());
    let dup = Overlay::from_files([("a.toml", ok), ("b.toml", ok)]);
    assert!(dup.is_err());
    let one = Overlay::from_files([("a.toml", ok)]).unwrap();
    assert!(one.get(&EntryId::Feature("x".into())).is_some());
}

#[test]
fn curation_prefixes_match_whole_segments() {
    assert!(path_has_prefix("tui.theme", "tui"));
    assert!(path_has_prefix("tui", "tui"));
    assert!(!path_has_prefix("tui_x", "tui"));
    assert!(!path_has_prefix("tu", "tui"));
    let c = Curation::parse(
        r#"format = 1
[[category]]
id = "a"
label = "A"
keys = ["model"]
features = ["fast_mode"]
[[category]]
id = "b"
label = "B"
keys = ["model.sub"]
[[platform]]
prefix = "windows"
os = ["windows"]
[[platform]]
prefix = "windows.x"
os = ["windows", "linux"]
"#,
    )
    .unwrap();
    assert_eq!(c.category_of_key("model"), "a");
    assert_eq!(c.category_of_key("model.sub.leaf"), "b");
    assert_eq!(c.category_of_key("model_x"), "other");
    assert_eq!(c.category_of_key("features.fast_mode"), "a");
    assert_eq!(c.category_of_key("features.unknown"), "features");
    assert_eq!(
        c.platforms_of("windows.x.y"),
        Some(&[Os::Windows, Os::Linux][..])
    );
    assert_eq!(c.platforms_of("windows.y"), Some(&[Os::Windows][..]));
    assert!(
        Curation::parse(
            "format = 1\n[[category]]\nid=\"a\"\nlabel=\"A\"\n[[category]]\nid=\"a\"\nlabel=\"B\"\n"
        )
        .is_err()
    );
}
