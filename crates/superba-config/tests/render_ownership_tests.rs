//! The pure render's ownership bookkeeping (`RenderedFiles::owned` /
//! `newly_owned`): remote hosts keep their own record from it and extend their
//! baseline with the user's value of every key a render takes over, so a key
//! Superba releases later gets that value back (review platform-security #3).

mod common;

use common::*;
use superba_core::{
    BaselineExtras, CodexFiles, ConfigEngine, KeySnapshot, RenderInput, RenderedFiles,
    ResolvedMode, ResolvedProfile, SecretBytes, Timestamp,
};

const HOST_CONFIG: &str = "model = \"gpt-5.5\"\napproval_policy = \"on-request\"\n\n[mcp_servers.fs]\ncommand = \"mcp-fs\"\n";

fn files(config: &str) -> CodexFiles {
    CodexFiles {
        config_toml: Some(SecretBytes::new(config.as_bytes().to_vec())),
        auth_json: None,
    }
}

fn after(env: &Env, files: &CodexFiles, r: &RenderedFiles) -> String {
    match &r.config_toml {
        superba_core::FileChange::Write { contents, .. } => {
            String::from_utf8(contents.expose().to_vec()).unwrap()
        }
        superba_core::FileChange::Unchanged => {
            String::from_utf8(files.config_toml.as_ref().unwrap().expose().to_vec()).unwrap()
        }
        superba_core::FileChange::Remove => {
            panic!("{}: config.toml removed", env.home.root().display())
        }
    }
}

fn value<'a>(keys: &'a [KeySnapshot], path: &str) -> Option<Option<&'a str>> {
    keys.iter()
        .find(|k| k.path == path)
        .map(|k| k.value.as_deref())
}

fn render(
    env: &Env,
    files: &CodexFiles,
    desired: &ResolvedProfile,
    baseline: &superba_core::Baseline,
    last_written: &[KeySnapshot],
) -> RenderedFiles {
    env.engine
        .render(&RenderInput {
            files,
            desired,
            baseline: Some(baseline),
            login_stash: None,
            original_files: None,
            last_written: Some(last_written),
        })
        .unwrap()
}

#[test]
fn a_render_reports_what_it_owns_and_what_it_takes_over() {
    let env = Env::new();
    let host = files(HOST_CONFIG);
    let baseline = env
        .engine
        .capture_baseline(
            env.home.root(),
            &host,
            BaselineExtras::default(),
            Timestamp::from_millis(0),
        )
        .unwrap();
    // The captured baseline has no model keys and no settings.
    assert_eq!(value(&baseline.keys, "model"), None);

    let mut desired = apikey(OVERSEAS);
    desired.model.model = Some("gpt-6-astra".into());
    desired
        .settings
        .set("approval_policy", "on-request")
        .unwrap();
    let r = render(&env, &host, &desired, &baseline, &[]);
    // Owned: every key set, unchanged ones included (approval_policy already
    // had the profile's value).
    assert_eq!(value(&r.owned, "model"), Some(Some("\"gpt-6-astra\"")));
    assert_eq!(value(&r.owned, "model_provider"), Some(Some("\"openai\"")));
    assert_eq!(
        value(&r.owned, "approval_policy"),
        Some(Some("\"on-request\""))
    );
    // Newly owned: the value each key had just before.
    assert_eq!(value(&r.newly_owned, "model"), Some(Some("\"gpt-5.5\"")));
    assert_eq!(value(&r.newly_owned, "model_provider"), Some(None));
    assert_eq!(
        value(&r.newly_owned, "approval_policy"),
        Some(Some("\"on-request\""))
    );
    assert!(!r.key_changes.iter().any(|k| k.path == "approval_policy"));

    // Taken over again: nothing is newly owned.
    let host2 = files(&after(&env, &host, &r));
    let again = render(&env, &host2, &desired, &baseline, &r.owned);
    assert!(again.newly_owned.is_empty(), "{:?}", again.newly_owned);
    assert_eq!(again.owned, r.owned);

    // Released by the official login: owns nothing. With the caller's baseline
    // extended by the takeover's values, `model` returns to the user's value;
    // without it, a released key would be deleted.
    let official = restore(ResolvedMode::Official);
    let mut extended = baseline.clone();
    extended.keys.extend(
        r.newly_owned
            .iter()
            .filter(|k| !baseline.keys.iter().any(|b| b.path == k.path))
            .cloned(),
    );
    let released = render(&env, &host2, &official, &extended, &r.owned);
    assert!(released.owned.is_empty(), "{:?}", released.owned);
    assert!(released.newly_owned.is_empty());
    let cfg = after(&env, &host2, &released);
    assert!(cfg.contains("model = \"gpt-5.5\""), "{cfg}");
    assert!(cfg.contains("approval_policy = \"on-request\""), "{cfg}");
    assert!(!cfg.contains("model_provider"), "{cfg}");

    let lost = render(&env, &host2, &official, &baseline, &r.owned);
    assert!(!after(&env, &host2, &lost).contains("model ="));
}
