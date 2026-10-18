//! The file editor's pure parts and its encrypted backups (GitHub issue 9):
//! formatting keeps comments and data, the check names line and column,
//! unknown / deprecated keys are warnings and wrong types errors (from the
//! settings catalog), `auth.json` secrets are masked and a partly changed
//! placeholder is refused, diffs, backup / restore round trips (sealed on
//! disk), the switch pre-images and originals in the list, and the
//! compare-and-swap write.

mod common;

use std::sync::Arc;

use common::{Env, LEGACY_CONFIG, PAT_KEY};
use pretty_assertions::assert_eq;
use superba_config::codex_files::{
    self, CheckContext, DiffTag, EditRecord, EditorStore, Masker, diff_lines, format_json,
    format_toml, hunks, unified, validate, write_file,
};
use superba_core::{
    AuthMode, AuthShape, BackupSource, CatalogFeature, CatalogKey, CatalogSnapshot,
    CodexFileBackupQuery, CodexFileKind, CodexVersion, ConfigEngine, ConsumerKind, FeatureStage,
    FileSchema, LegacyKind, LegacyRule, OwnedKey, ProblemKind, ProblemSeverity, SettingEnumValue,
    SettingKind, SettingsConsumer, StaticCatalog, TargetId, Timestamp,
};

const T0: Timestamp = Timestamp::from_millis(1_790_000_000_000);

fn catalog() -> Arc<StaticCatalog> {
    let mut effort = CatalogKey::new("model_reasoning_effort", SettingKind::Enum);
    effort.enum_values = ["low", "medium", "high"]
        .iter()
        .map(|v| SettingEnumValue {
            value: (*v).into(),
            description: None,
        })
        .collect();
    let mut old = CatalogKey::new("experimental_old_knob", SettingKind::Boolean);
    old.deprecated_hint = true;
    let mut mcp = CatalogKey::new("mcp_servers.<name>.command", SettingKind::String);
    mcp.under_map = true;
    let snap = CatalogSnapshot::new(CodexVersion::new(0, 162, 0))
        .with_key(CatalogKey::new("model", SettingKind::String))
        .with_key(CatalogKey::new("model_provider", SettingKind::String))
        .with_key(CatalogKey::new("openai_base_url", SettingKind::String))
        .with_key(effort)
        .with_key(old)
        .with_key(CatalogKey::new("mcp_servers", SettingKind::Map))
        .with_key(CatalogKey::new("mcp_servers.<name>", SettingKind::Table))
        .with_key(mcp)
        .with_key(CatalogKey::new(
            "mcp_servers.<name>.args",
            SettingKind::Array,
        ))
        .with_key(CatalogKey::new("mcp_servers.<name>.env", SettingKind::Map))
        .with_key(CatalogKey::new(
            "mcp_servers.<name>.env.<name>",
            SettingKind::String,
        ))
        .with_key(CatalogKey::new("projects", SettingKind::Map))
        .with_key(CatalogKey::new(
            "projects.<name>.trust_level",
            SettingKind::String,
        ))
        .with_key(CatalogKey::new("model_providers", SettingKind::Map))
        .with_key(CatalogKey::new("model_providers.<name>", SettingKind::Any))
        .with_key(CatalogKey::new("features", SettingKind::Table))
        .with_feature(CatalogFeature {
            key: "fast_mode".into(),
            config_path: "features.fast_mode".into(),
            stage: FeatureStage::Stable,
            default_enabled: false,
            name: None,
            menu_description: None,
        })
        .with_feature(CatalogFeature {
            key: "multi_agent".into(),
            config_path: "features.multi_agent".into(),
            stage: FeatureStage::Stable,
            default_enabled: false,
            name: None,
            menu_description: None,
        })
        .with_legacy(LegacyRule {
            path: "features.collab".into(),
            kind: LegacyKind::Alias {
                canonical: "features.multi_agent".into(),
            },
        });
    Arc::new(StaticCatalog::new([snap]))
}

fn schema() -> FileSchema {
    FileSchema {
        consumers: vec![SettingsConsumer::new(
            ConsumerKind::DesktopApp,
            "ChatGPT app",
            Some(CodexVersion::new(0, 162, 0)),
        )],
        catalog: Some(catalog()),
    }
}

fn en(t: &superba_core::LocalizedText) -> String {
    t.render_fallback()
}

// ------------------------------------------------------------------ format

#[test]
fn formatting_keeps_every_comment_and_value() {
    let messy = "# top comment\n\n\n   model   =    \"gpt-5.5\"    # the model\nmodel_reasoning_effort=\"high\"\n\n\n\n# about the server\n[mcp_servers.docs]\n  command=\"npx\"   \nargs = [\"-y\",   \"docs-mcp\"] # keep spacing inside\n[ projects . \"/home/u/项目\" ]\ntrust_level = \"trusted\"\n\n\n# trailing comment\n\n";
    let formatted = format_toml(messy).unwrap();
    assert_eq!(
        formatted,
        "# top comment\n\nmodel = \"gpt-5.5\" # the model\nmodel_reasoning_effort = \"high\"\n\n# about the server\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\",   \"docs-mcp\"] # keep spacing inside\n\n[projects.\"/home/u/项目\"]\ntrust_level = \"trusted\"\n\n# trailing comment\n"
    );
    // Same data, idempotent.
    let a: toml::Table = toml::from_str(messy).unwrap();
    let b: toml::Table = toml::from_str(&formatted).unwrap();
    assert_eq!(a, b);
    assert_eq!(format_toml(&formatted).unwrap(), formatted);
}

#[test]
fn formatting_the_legacy_tutorial_config_keeps_its_comments() {
    let formatted = format_toml(LEGACY_CONFIG).unwrap();
    for comment in ["# Codex 配置（Krill 教程）", "# inline secret"] {
        assert!(formatted.contains(comment), "{comment} lost:\n{formatted}");
    }
    let a: toml::Table = toml::from_str(LEGACY_CONFIG).unwrap();
    let b: toml::Table = toml::from_str(&formatted).unwrap();
    assert_eq!(a, b);
}

#[test]
fn formatting_keeps_crlf_dotted_keys_and_multiline_strings() {
    let src = "a.b   =  1\r\nnote = \"\"\"\r\n  keep   this  \r\n\"\"\"\r\n[t]\r\nx=1\r\n";
    let out = format_toml(src).unwrap();
    assert_eq!(
        out,
        "a.b = 1\r\nnote = \"\"\"\r\n  keep   this  \r\n\"\"\"\r\n\r\n[t]\r\nx = 1\r\n"
    );
}

#[test]
fn formatting_refuses_broken_toml_with_its_position() {
    let p = format_toml("model = \"a\"\nbroken = \n").unwrap_err();
    assert_eq!(p.kind, ProblemKind::Syntax);
    assert_eq!(p.line, 2);
    assert!(en(&p.message).starts_with("TOML syntax error"));
}

#[test]
fn json_is_pretty_printed_in_key_order() {
    let out = format_json("{\"b\":1,\"a\":{\"z\":true}}").unwrap();
    assert_eq!(out, "{\n  \"b\": 1,\n  \"a\": {\n    \"z\": true\n  }\n}\n");
    assert!(format_json("{\"a\":}").is_err());
}

// ------------------------------------------------------------------ validate

#[test]
fn toml_syntax_errors_name_line_and_column_without_quoting_the_file() {
    let src = "model = \"gpt\"\nkey = \"secret-value\nother = 1\n";
    let ps = validate(CodexFileKind::ConfigToml, src, &CheckContext::default());
    assert_eq!(ps.len(), 1, "{ps:?}");
    let p = &ps[0];
    assert_eq!(
        (p.severity, p.kind),
        (ProblemSeverity::Error, ProblemKind::Syntax)
    );
    assert_eq!(p.line, 2);
    assert!(p.blocks_save());
    assert!(
        !en(&p.message).contains("secret-value"),
        "{}",
        en(&p.message)
    );
}

#[test]
fn catalog_checks_warn_unknown_and_deprecated_keys_and_reject_wrong_types() {
    let src = "model = 5\nmodel_reasoning_effort = \"extreme\"\nmodle = \"typo\"\nexperimental_old_knob = true\n\n[features]\nfast_mode = true\ncollab = true\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\"]\n\n[mcp_servers.docs.env]\nTOKEN = \"x\"\n\n[model_providers.anything]\nwhatever = 1\n\n[nonsense]\na = 1\nb = 2\n";
    let schema = schema();
    let cx = CheckContext {
        schema: Some(&schema),
        ..CheckContext::default()
    };
    let ps = validate(CodexFileKind::ConfigToml, src, &cx);
    let msgs: Vec<(usize, ProblemSeverity, String)> = ps
        .iter()
        .map(|p| (p.line, p.severity, en(&p.message)))
        .collect();
    assert_eq!(
        msgs,
        vec![
            (1, ProblemSeverity::Error, "model: expected a string.".into()),
            (
                2,
                ProblemSeverity::Error,
                "model_reasoning_effort: expected one of: \"low\", \"medium\", \"high\".".into()
            ),
            (
                3,
                ProblemSeverity::Warning,
                "modle is not a setting of ChatGPT app 0.162.0: Codex ignores it (check the spelling).".into()
            ),
            (
                4,
                ProblemSeverity::Warning,
                "experimental_old_knob is deprecated in ChatGPT app 0.162.0.".into()
            ),
            (
                8,
                ProblemSeverity::Warning,
                "features.collab is an old name: use features.multi_agent.".into()
            ),
            (
                20,
                ProblemSeverity::Warning,
                "nonsense is not a setting of ChatGPT app 0.162.0: Codex ignores it (check the spelling).".into()
            ),
        ]
    );
    // Type errors can be saved anyway; nothing here blocks the save.
    assert!(ps.iter().all(|p| !p.blocks_save()));
    assert!(ps.iter().any(|p| p.needs_force()));
    assert!(!codex_files::clean(&ps));
}

#[test]
fn without_a_known_codex_only_the_syntax_is_checked() {
    let cx = CheckContext::default();
    let ps = validate(CodexFileKind::ConfigToml, "modle = 5\n", &cx);
    assert!(ps.is_empty(), "{ps:?}");
}

#[test]
fn owned_keys_are_marked_as_information() {
    let owned = [OwnedKey {
        path: "openai_base_url".into(),
        written: Some("\"https://a\"".into()),
    }];
    let cx = CheckContext {
        owned: &owned,
        owner: Some("Krill"),
        ..CheckContext::default()
    };
    let ps = validate(
        CodexFileKind::ConfigToml,
        "model = \"x\"\nopenai_base_url = \"https://b\"\n",
        &cx,
    );
    assert_eq!(ps.len(), 1);
    assert_eq!((ps[0].line, ps[0].kind), (2, ProblemKind::Owned));
    assert!(en(&ps[0].message).contains("Krill"));
}

#[test]
fn auth_json_shape_is_checked() {
    let cx = CheckContext::default();
    let ps = validate(
        CodexFileKind::AuthJson,
        "{\n  \"OPENAI_API_KEY\": 5,\n  \"extra\": true,\n  \"last_refresh\": \"yesterday\"\n}\n",
        &cx,
    );
    let msgs: Vec<(usize, String)> = ps.iter().map(|p| (p.line, en(&p.message))).collect();
    assert_eq!(
        msgs,
        vec![
            (
                1,
                "auth.json holds no credential (no API key, PAT or ChatGPT login): Codex will ask you to sign in.".into()
            ),
            (2, "OPENAI_API_KEY: expected string or null, found number.".into()),
            (3, "Codex does not read extra.".into()),
            (4, "last_refresh is not a date and time like 2026-10-18T09:30:00Z.".into()),
        ]
    );
    let empty = validate(CodexFileKind::AuthJson, "  \n", &cx);
    assert!(empty[0].blocks_save());
    let broken = validate(CodexFileKind::AuthJson, "{\n  \"a\": 1,\n}\n", &cx);
    assert_eq!((broken[0].line, broken[0].kind), (3, ProblemKind::Syntax));
}

// ------------------------------------------------------------------ masking

#[test]
fn secrets_are_masked_kept_apart_and_put_back() {
    let real = format!(
        "{{\n  \"personal_access_token\": \"{PAT_KEY}\",\n  \"OPENAI_API_KEY\": \"{PAT_KEY}\",\n  \"tokens\": {{\"access_token\": \"short\", \"account_id\": \"acc-1\"}}\n}}\n"
    );
    let mut m = Masker::new();
    let masked = m.mask(&real);
    assert!(!masked.contains(PAT_KEY), "{masked}");
    assert!(!masked.contains("\"short\""));
    assert!(masked.contains("acc-1"), "non-secrets stay visible");
    // The same key gets the same placeholder; its last four characters show.
    assert_eq!(masked.matches("••••••••••••0001").count(), 2, "{masked}");
    assert_eq!(m.len(), 2);
    assert_eq!(m.unmask(&masked), real);
    // Masking again is stable.
    assert_eq!(m.mask(&masked), masked);
    // The masked text checks clean; a partly changed placeholder is refused.
    let cx = CheckContext {
        masker: Some(&m),
        ..CheckContext::default()
    };
    assert!(codex_files::clean(&validate(
        CodexFileKind::AuthJson,
        &masked,
        &cx
    )));
    let broken = masked.replacen("••••••••••••0001", "••••••••0001", 1);
    let ps = validate(CodexFileKind::AuthJson, &broken, &cx);
    let secret: Vec<_> = ps
        .iter()
        .filter(|p| p.kind == ProblemKind::Secret)
        .collect();
    assert_eq!(secret.len(), 1, "{ps:?}");
    assert!(secret[0].blocks_save());
    assert!(!en(&secret[0].message).contains(PAT_KEY));
    // Without the session's placeholders, any placeholder is a leftover.
    assert!(!codex_files::clean(&validate(
        CodexFileKind::AuthJson,
        &masked,
        &CheckContext::default()
    )));
}

#[test]
fn masker_debug_never_shows_a_secret() {
    let mut m = Masker::new();
    m.mask(&format!("{{\"OPENAI_API_KEY\": \"{PAT_KEY}\"}}"));
    assert!(!format!("{m:?}").contains(PAT_KEY));
}

// --------------------------------------------------------------------- diff

#[test]
fn diffs_show_only_changed_lines_with_context() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
    let new = "a\nb\nC\nd\ne\nf\ng\nh\ni\n";
    let lines = diff_lines(old, new);
    let changed: Vec<(DiffTag, String)> = lines
        .iter()
        .filter(|l| l.tag != DiffTag::Equal)
        .map(|l| (l.tag, l.text.clone()))
        .collect();
    assert_eq!(
        changed,
        vec![
            (DiffTag::Delete, "c".into()),
            (DiffTag::Insert, "C".into()),
            (DiffTag::Insert, "i".into()),
        ]
    );
    assert_eq!(hunks(&lines, 1).len(), 2);
    assert_eq!(hunks(&lines, 3).len(), 1);
    assert_eq!(
        unified("old", "new", "x\n", "y\n", 3),
        "--- old\n+++ new\n@@ -1,1 +1,1 @@\n-x\n+y\n"
    );
    assert!(unified("a", "b", "same\n", "same\n", 3).is_empty());
}

// ---------------------------------------------------------------- ownership

#[test]
fn owned_changes_and_credential_changes_are_named() {
    let owned = [
        OwnedKey {
            path: "openai_base_url".into(),
            written: Some("\"https://a/backend-api/codex\"".into()),
        },
        OwnedKey {
            path: "model_provider".into(),
            written: Some("\"openai\"".into()),
        },
    ];
    let old = "model_provider = \"openai\"\nopenai_base_url = \"https://a/backend-api/codex\"\n";
    let new = "model_provider = 'openai'\nopenai_base_url = \"https://b/codex/v1\"\n";
    let ch = codex_files::owned_changes(old, new, &owned);
    assert_eq!(ch.len(), 1);
    assert_eq!(ch[0].path, "openai_base_url");
    assert_eq!(ch[0].after.as_deref(), Some("\"https://b/codex/v1\""));

    let pat =
        format!("{{\"personal_access_token\":\"{PAT_KEY}\",\"OPENAI_API_KEY\":\"{PAT_KEY}\"}}");
    let reformatted = format_json(&pat).unwrap();
    assert!(
        codex_files::credential_change(Some(pat.as_bytes()), Some(reformatted.as_bytes()))
            .is_none()
    );
    let other = "{\"OPENAI_API_KEY\":\"sk-other-key-000000000009\",\"auth_mode\":\"apikey\"}";
    let d = codex_files::credential_change(Some(pat.as_bytes()), Some(other.as_bytes())).unwrap();
    assert!(matches!(d.written, AuthShape::Pat { .. }));
    assert!(matches!(d.live, AuthShape::ApiKey { ref key_last4 } if key_last4 == "0009"));
}

// ------------------------------------------------------------------ backups

#[test]
fn backups_are_sealed_listed_and_read_back() {
    let env = Env::new();
    let store = EditorStore::new(env.ctx.clone());
    let secret = format!("{{\"OPENAI_API_KEY\": \"{PAT_KEY}\"}}\n");
    let local = TargetId::local();
    let b = store
        .backup(
            &local,
            "This computer",
            CodexFileKind::AuthJson,
            secret.as_bytes(),
            BackupSource::BeforeEdit,
            T0,
        )
        .unwrap();
    assert!(b.id.ends_with(&b.id[b.id.len() - 4..]));
    assert!(b.id.contains("-auth-"), "{}", b.id);
    // Sealed on disk: the key is nowhere in the data directory.
    for entry in walk(env.data()) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(PAT_KEY),
            "{} holds the key",
            entry.display()
        );
    }
    let listed = store
        .list(
            &CodexFileBackupQuery::default(),
            env.home.root(),
            "This computer",
        )
        .unwrap();
    assert_eq!(listed, vec![b.clone()]);
    let read = store.read(&b.id, env.home.root(), "This computer").unwrap();
    assert_eq!(read.contents.expose(), secret.as_bytes());
    assert!(!format!("{read:?}").contains(PAT_KEY));
    assert!(store.read("nope", env.home.root(), "x").is_err());
}

#[test]
fn only_the_newest_backups_of_a_file_are_kept() {
    let env = Env::new();
    let store = EditorStore::new(env.ctx.clone());
    let local = TargetId::local();
    for i in 0..(codex_files_keep() + 3) {
        store
            .backup(
                &local,
                "This computer",
                CodexFileKind::ConfigToml,
                format!("n = {i}\n").as_bytes(),
                BackupSource::BeforeEdit,
                Timestamp::from_millis(T0.as_millis() + i as i64 * 1000),
            )
            .unwrap();
    }
    let listed = store
        .list(&CodexFileBackupQuery::default(), env.home.root(), "x")
        .unwrap();
    assert_eq!(listed.len(), codex_files_keep());
    let newest = store.read(&listed[0].id, env.home.root(), "x").unwrap();
    assert_eq!(
        newest.contents.expose(),
        format!("n = {}\n", codex_files_keep() + 2).as_bytes()
    );
    let sealed = walk(&env.data().join("backups/editor"))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "enc"))
        .count();
    assert_eq!(sealed, codex_files_keep(), "pruned copies are deleted");
}

fn codex_files_keep() -> usize {
    codex_files::KEEP_PER_FILE
}

#[tokio::test]
async fn switch_pre_images_and_the_original_are_listed_and_restorable() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    let desired = common::token(common::OVERSEAS, AuthMode::Pat, PAT_KEY);
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
    let store = EditorStore::new(env.ctx.clone());
    let listed = store
        .list(
            &CodexFileBackupQuery {
                target: Some(TargetId::local()),
                kind: Some(CodexFileKind::ConfigToml),
            },
            env.home.root(),
            "This computer",
        )
        .unwrap();
    let ids: Vec<&str> = listed.iter().map(|b| b.id.as_str()).collect();
    assert!(ids.contains(&"switch-g1-config"), "{ids:?}");
    assert!(ids.contains(&"original-config"), "{ids:?}");
    let before = listed.iter().find(|b| b.id == "switch-g1-config").unwrap();
    assert!(matches!(
        before.source,
        BackupSource::BeforeSwitch { generation: 1, .. }
    ));
    assert_eq!(before.size, LEGACY_CONFIG.len() as u64);
    for id in ["switch-g1-config", "original-config"] {
        let read = store.read(id, env.home.root(), "This computer").unwrap();
        assert_eq!(read.contents.expose(), LEGACY_CONFIG.as_bytes(), "{id}");
    }
    // The engine reports what it owns now.
    let owned = env.engine.owned_keys(&env.home).await.unwrap();
    assert!(owned.iter().any(|k| k.path == "openai_base_url"));
}

#[test]
fn edits_are_recorded_per_file_and_home() {
    let env = Env::new();
    let store = EditorStore::new(env.ctx.clone());
    let rec = |at: i64, sha: &str| EditRecord {
        target: TargetId::local(),
        kind: CodexFileKind::ConfigToml,
        path: "/x/config.toml".into(),
        at: Timestamp::from_millis(at),
        before_sha256: None,
        after_sha256: sha.into(),
    };
    store.record_edit(rec(1, "a")).unwrap();
    store.record_edit(rec(2, "b")).unwrap();
    let last = store
        .last_edit(&TargetId::local(), CodexFileKind::ConfigToml)
        .unwrap()
        .unwrap();
    assert_eq!(last.after_sha256, "b");
    assert!(
        store
            .last_edit(&TargetId::local(), CodexFileKind::AuthJson)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.edits(&TargetId::local()).unwrap().len(), 1);
}

// -------------------------------------------------------------------- write

#[test]
fn write_replaces_only_the_file_that_was_opened() {
    let env = Env::new();
    env.write_config("a = 1\n");
    let sha = sha("a = 1\n");
    write_file(
        &env.config_path(),
        CodexFileKind::ConfigToml,
        b"a = 2\n",
        Some(&sha),
    )
    .unwrap();
    assert_eq!(env.config(), "a = 2\n");
    // Opened before that write: refused, file untouched.
    let err = write_file(
        &env.config_path(),
        CodexFileKind::ConfigToml,
        b"a = 3\n",
        Some(&sha),
    )
    .unwrap_err();
    assert!(matches!(err, superba_core::Error::Conflict { .. }), "{err}");
    assert_eq!(env.config(), "a = 2\n");
    // A new auth.json is private.
    write_file(&env.auth_path(), CodexFileKind::AuthJson, b"{}", None).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(env.auth_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

fn sha(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
