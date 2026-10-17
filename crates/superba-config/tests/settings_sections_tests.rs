//! A map the user wrote as its own `[section]` (user bug 26) is written back into
//! that section through the public engine: entries changed, added and removed in
//! place, comments and every other byte kept, read back as the same value, and
//! given back to the user's own entries on release and rollback. Inline tables
//! stay inline; a scalar parent is still a skipped shape conflict.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ApplyPlan, CodexSettings, ConfigEngine, KeyChange, ResolvedProfile, SettingValue,
};

const PATH: &str = "shell_environment_policy.set";

/// The user's section, with comments on the header and on an entry.
const SECTION: &str = "[shell_environment_policy.set] # 我的环境变量\n\
PATH = \"/usr/bin\" # keep me\n\
RUST_LOG = \"info\"\n";

fn original() -> String {
    format!("{OFFICIAL_CONFIG}\n[shell_environment_policy]\ninherit = \"core\"\n\n{SECTION}")
}

fn map(entries: &[(&str, SettingValue)]) -> SettingValue {
    SettingValue::Table(
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn text(s: &str) -> SettingValue {
    SettingValue::String(s.to_owned())
}

fn with_setting(path: &str, value: SettingValue) -> ResolvedProfile {
    let mut desired = pat(OVERSEAS);
    desired.settings = CodexSettings::new();
    desired.settings.set(path, value).unwrap();
    desired
}

// A separate TOML reader as the oracle, never the engine's patching helpers.
fn value(config: &str, path: &str) -> Option<toml::Value> {
    let parsed: toml::Value = toml::from_str(config).unwrap();
    path.split('.')
        .try_fold(&parsed, |node, part| node.get(part))
        .cloned()
}

/// A table value from TOML text (the oracle's form of a map).
fn table(text: &str) -> Option<toml::Value> {
    Some(toml::Value::Table(toml::from_str(text).unwrap()))
}

fn change<'a>(plan: &'a ApplyPlan, path: &str) -> Option<&'a KeyChange> {
    plan.key_changes.iter().find(|c| c.path == path)
}

fn shape_conflict_noted(plan: &ApplyPlan, path: &str) -> bool {
    plan.notes.iter().any(|note| {
        note.text.key == "config.note.setting_shape_conflict"
            && note.text.args.iter().any(|(k, v)| k == "key" && v == path)
    })
}

#[tokio::test]
async fn a_section_is_updated_in_place_read_back_and_released_to_the_user() {
    let env = Env::new();
    let original = original();
    env.write_config(&original);
    let wanted = map(&[
        ("PATH", text("/opt/bin:/usr/bin")),
        ("CARGO_TARGET_DIR", text("/srv/target")),
    ]);
    let mut desired = with_setting(PATH, wanted.clone());

    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert!(!shape_conflict_noted(&plan, PATH), "{:?}", plan.notes);
    let c = change(&plan, PATH).expect("the section is written");
    assert_eq!(
        c.before.as_deref(),
        Some(r#"{ PATH = "/usr/bin", RUST_LOG = "info" }"#)
    );
    assert_eq!(c.after.as_deref(), Some(wanted.to_toml_repr().as_str()));
    env.engine.apply(&plan).await.unwrap();

    // Changed, added and removed inside the section; comments and the rest stay.
    let applied = env.config();
    assert!(
        applied.ends_with(
            "# Desktop app settings\n[desktop]\ntheme = \"dark\"\n\n\
             [mcp_servers.fs]\ncommand = \"mcp-fs\"\n\n\
             [shell_environment_policy]\ninherit = \"core\"\n\n\
             [shell_environment_policy.set] # 我的环境变量\n\
             PATH = \"/opt/bin:/usr/bin\" # keep me\n\
             CARGO_TARGET_DIR = \"/srv/target\"\n"
        ),
        "{applied}"
    );
    assert!(!applied.contains("set = {"), "{applied}");
    // Read back: the same map, no drift, and a second plan changes nothing.
    assert_eq!(
        value(&applied, PATH),
        table("PATH = '/opt/bin:/usr/bin'\nCARGO_TARGET_DIR = '/srv/target'")
    );
    assert!(
        env.engine
            .read_state(&env.home)
            .await
            .unwrap()
            .drift
            .is_empty()
    );
    let again = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_eq!(change(&again, PATH), None);

    // Released: the user's own entries come back into the same section.
    desired.settings.forget(PATH);
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
    let released = env.config();
    assert!(released.ends_with(SECTION), "{released}");
    assert_eq!(value(&released, PATH), value(&original, PATH));
}

#[tokio::test]
async fn rollback_puts_the_users_section_back() {
    let env = Env::new();
    let original = original();
    env.write_config(&original);
    let desired = with_setting(PATH, map(&[("PATH", text("/opt/bin"))]));
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    let report = env.engine.apply(&plan).await.unwrap();
    assert_eq!(value(&env.config(), PATH), table("PATH = '/opt/bin'"));
    env.engine.rollback(&report).await.unwrap();
    let config = env.config();
    assert!(config.contains(SECTION), "{config}");
    assert_eq!(value(&config, PATH), value(&original, PATH));
}

#[tokio::test]
async fn a_nested_table_under_a_dotted_section_is_updated_in_place() {
    let env = Env::new();
    let tail = "\n[mcp_servers.docs]\ncommand = \"docs-mcp\"\n\n\
                [mcp_servers.docs.env] # 令牌\nTOKEN = \"a\" # rotate monthly\n";
    env.write_config(&format!("{OFFICIAL_CONFIG}{tail}"));
    let desired = with_setting(
        "mcp_servers.docs",
        map(&[
            ("command", text("docs-mcp")),
            ("env", map(&[("TOKEN", text("b")), ("REGION", text("eu"))])),
        ]),
    );
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert!(!shape_conflict_noted(&plan, "mcp_servers.docs"));
    env.engine.apply(&plan).await.unwrap();
    let applied = env.config();
    assert!(
        applied.ends_with(
            "\n[mcp_servers.docs]\ncommand = \"docs-mcp\"\n\n\
             [mcp_servers.docs.env] # 令牌\nTOKEN = \"b\" # rotate monthly\nREGION = \"eu\"\n"
        ),
        "{applied}"
    );
    assert_eq!(
        value(&applied, "mcp_servers.docs.env"),
        table("TOKEN = 'b'\nREGION = 'eu'")
    );
    // The other server is untouched.
    assert_eq!(
        value(&applied, "mcp_servers.fs.command"),
        Some("mcp-fs".into())
    );
}

#[tokio::test]
async fn a_new_map_and_an_inline_map_are_written_inline() {
    for (config, expected) in [
        (OFFICIAL_CONFIG.to_owned(), "set = { A = \"1\" }".to_owned()),
        (
            format!("{OFFICIAL_CONFIG}\n[shell_environment_policy]\nset = {{ B = \"2\" }}\n"),
            "[shell_environment_policy]\nset = { A = \"1\" }\n".to_owned(),
        ),
    ] {
        let env = Env::new();
        env.write_config(&config);
        let desired = with_setting(PATH, map(&[("A", text("1"))]));
        let plan = env.engine.plan(&env.home, &desired).await.unwrap();
        env.engine.apply(&plan).await.unwrap();
        let applied = env.config();
        assert!(applied.contains(&expected), "{applied}");
        assert_eq!(value(&applied, PATH), table("A = '1'"));
    }
}

#[tokio::test]
async fn a_scalar_parent_is_still_a_skipped_shape_conflict() {
    let env = Env::new();
    let original = format!("shell_environment_policy = \"core\" # 用户的值\n{OFFICIAL_CONFIG}");
    env.write_config(&original);
    let mut desired = with_setting(PATH, map(&[("A", text("1"))]));
    desired.settings.set("web_search", "cached").unwrap();
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert!(shape_conflict_noted(&plan, PATH), "{:?}", plan.notes);
    assert_eq!(change(&plan, PATH), None);
    env.engine.apply(&plan).await.unwrap();
    let config = env.config();
    assert!(config.contains("shell_environment_policy = \"core\" # 用户的值\n"));
    assert_eq!(value(&config, "web_search"), Some("cached".into()));
}
