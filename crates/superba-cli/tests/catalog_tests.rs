use std::collections::BTreeSet;
use std::path::Path;

use superba_cli::text::{EN, ZH_CN, catalog, text};
use superba_core::{Locale, i18n::flatten_toml};

fn placeholders(value: &str) -> BTreeSet<&str> {
    value
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(key, _)| key))
        .map(|key| key.split('|').next().unwrap())
        .collect()
}

#[test]
fn catalogs_have_matching_nonempty_keys_and_placeholders() {
    let en = flatten_toml(EN).unwrap();
    let zh = flatten_toml(ZH_CN).unwrap();
    assert_eq!(en.keys().collect::<BTreeSet<_>>(), zh.keys().collect());
    for (key, value) in &en {
        assert!(key.starts_with("cli."), "{key}");
        assert!(
            !value.trim().is_empty() && !zh[key].trim().is_empty(),
            "{key}"
        );
        assert_eq!(placeholders(value), placeholders(&zh[key]), "{key}");
    }
}

fn visit_sources(path: &Path, visit: &mut impl FnMut(&str)) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            visit_sources(&path, visit);
        } else if path.extension().is_some_and(|e| e == "rs") {
            visit(&std::fs::read_to_string(&path).unwrap());
        }
    }
}

#[test]
fn every_cli_key_used_by_code_exists_in_both_catalogs() {
    let en = flatten_toml(EN).unwrap();
    let zh = flatten_toml(ZH_CN).unwrap();
    let mut used = BTreeSet::new();
    visit_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut |source| {
            // All CLI key arguments are string literals, including clap help metadata.
            for fragment in source.split('"').skip(1).step_by(2) {
                if fragment.starts_with("cli.")
                    && fragment
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
                {
                    used.insert(fragment.to_owned());
                }
            }
        },
    );
    assert!(used.len() > 100);
    for key in used {
        assert!(
            en.contains_key(&key) && zh.contains_key(&key),
            "missing {key}"
        );
    }
}

#[test]
fn shared_wording_is_layered_and_not_copied_into_cli_catalogs() {
    let switch = flatten_toml(superba_switch::messages::SWITCH_EN).unwrap();
    for key in flatten_toml(EN).unwrap().keys() {
        assert!(!switch.contains_key(key));
    }
    let zh = catalog(Locale::ZhCn);
    assert_eq!(zh.text(&text("cli.label.destination")), "目标配置");
    assert_ne!(
        zh.text(&superba_switch::messages::outcome(
            superba_core::OutcomeStatus::Switched
        )),
        "Switched"
    );
    assert_ne!(
        zh.text(&superba_core::AuthMode::Pat.text()),
        superba_core::AuthMode::Pat.text().render_fallback()
    );
}

#[test]
fn counted_cli_strings_handle_zero_one_and_many_in_both_languages() {
    let en = catalog(Locale::En);
    let zh = catalog(Locale::ZhCn);
    for (n, process, session, row, file, operation) in [
        (0, "processes", "sessions", "rows", "files", "operations"),
        (1, "process", "session", "row", "file", "operation"),
        (2, "processes", "sessions", "rows", "files", "operations"),
    ] {
        let consumer = text("cli.format.consumer")
            .arg("consumer", "CLI")
            .arg("restart", "next launch")
            .arg("count", n);
        assert_eq!(
            en.text(&consumer),
            format!("CLI: next launch ({n} {process})")
        );
        assert!(zh.text(&consumer).contains(&format!("{n} 个进程")));
        let migration = text("cli.format.migration")
            .arg("sessions", n)
            .arg("rows", n)
            .arg("rollouts", n)
            .arg("provider", "openai");
        assert_eq!(
            en.text(&migration),
            format!("Migration: {n} {session} ({n} database {row}, {n} rollout {file}) → openai")
        );
        assert!(!zh.text(&migration).contains('{'));
        let recovery = en.text(&text("cli.recovery.auto").arg("n", n));
        assert!(recovery.contains(&format!("{n} interrupted {operation} before")));
    }
}
