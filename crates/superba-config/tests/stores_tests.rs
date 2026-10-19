//! Store contracts using only temporary directories and the plaintext backend.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use superba_config::{
    ENV_SUPERBA_SECRET_BACKEND, JsonProfileStore, KeyringSecretStore, PROFILES_SCHEMA,
};
use superba_core::{
    AuthMode, Background, CloseAction, DataDir, Error, KeyRef, Line, LineId, LineSource,
    LocalContext, Locale, LocalizedText, MapEnv, ModelDefaults, Origin, Profile, ProfileId,
    ProfileSet, ProfileStore, Secret, SecretBackend, SecretStore, Settings, Site, SiteEndpoints,
    SiteId, SiteKind, Theme, catalog,
};

fn context(root: &Path, env: MapEnv) -> LocalContext {
    LocalContext::new(
        root.join("home"),
        DataDir::new(root.join("data")),
        Arc::new(env.with(ENV_SUPERBA_SECRET_BACKEND, "file")),
    )
}

fn private_mode(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn user_set() -> ProfileSet {
    let site_id = SiteId::new("test-compatible");
    let line_id = LineId::new("primary");
    let profile = Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        anthropic: None,
        model_source: None,
        id: ProfileId::new("my-profile"),
        name: "我的 API profile".into(),
        builtin: None,
        site_id: site_id.clone(),
        line_id: Some(line_id.clone()),
        auth_mode: AuthMode::ApiKey,
        key_ref: Some(KeyRef::Env {
            var: "TEST_API_KEY".into(),
        }),
        base_url_override: Some("https://compatible.example/v1".parse().unwrap()),
        model: ModelDefaults {
            auto_compact: None,
            context_window: None,
            speed: None,
            model: Some("test-model".into()),
            reasoning_effort: Some("high".into()),
        },
        auth: superba_core::AuthChoice::from_mode(AuthMode::ApiKey),
        backup_lines: Vec::new(),
        codex_settings: Default::default(),
    };
    let mut set = ProfileSet {
        last_selected: Some(profile.id.clone()),
        ..ProfileSet::default()
    };
    set.profiles.push(profile);
    set.custom_sites.push(Site {
        id: site_id,
        kind: SiteKind::Compatible,
        name: LocalizedText::new("user.site.test", "My compatible site"),
        homepage: Some("https://compatible.example/account".parse().unwrap()),
        lines: vec![Line {
            id: line_id.clone(),
            name: LocalizedText::new("user.line.primary", "Primary"),
            origin: Origin::parse("https://compatible.example").unwrap(),
            source: LineSource::User,
        }],
        capabilities: catalog::compatible_capabilities(),
        endpoints: SiteEndpoints::default(),
        default_line: Some(line_id),
        model_map: Default::default(),
    });
    set.settings = Settings {
        app_codex: Default::default(),
        hosts: Default::default(),
        network: Default::default(),
        countdown_secs: 10,
        safe_mode: true,
        default_auth_mode: AuthMode::ApiKey,
        locale: Some(Locale::ZhCn),
        theme: Theme::Dark,
        background: Background::Lavender,
        auto_probe: false,
        close_action: Some(CloseAction::Tray),
        start_in_tray: true,
        preferred_app_id: Some("test-app".into()),
        router: Default::default(),
        auto_update_check: false,
        update_channel: Some(superba_core::UpdateChannel::Preview),
        reduce_motion: true,
        open_app_on_start: true,
        notify: Default::default(),
    };
    set
}

fn write_set(ctx: &LocalContext, set: &ProfileSet) -> Vec<u8> {
    fs::create_dir_all(ctx.data_dir.root()).unwrap();
    let bytes = serde_json::to_vec_pretty(set).unwrap();
    fs::write(ctx.data_dir.profiles_json(), &bytes).unwrap();
    bytes
}

#[tokio::test]
async fn explicit_file_backend_put_get_replace_delete_and_private_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = KeyringSecretStore::with_backend(ctx.clone(), SecretBackend::PlaintextFile);
    assert_eq!(store.backend(), SecretBackend::PlaintextFile);
    let id = ProfileId::new("test-profile");
    let key = store
        .put(&id, &Secret::new(" \t test-secret-value \r\n"))
        .await
        .unwrap();
    let KeyRef::File { path } = &key else {
        panic!("expected a file key reference")
    };
    assert!(path.starts_with(ctx.data_dir.secrets_dir()));
    assert_eq!(fs::read(path).unwrap(), b"test-secret-value");
    private_mode(path);
    assert_eq!(store.get(&key).await.unwrap().expose(), "test-secret-value");
    let replacement = store
        .put(&id, &Secret::new("replacement-secret"))
        .await
        .unwrap();
    assert_eq!(replacement, key);
    assert_eq!(
        store.get(&key).await.unwrap().expose(),
        "replacement-secret"
    );
    private_mode(path);
    store.delete(&key).await.unwrap();
    assert!(!path.exists());
    assert!(matches!(store.get(&key).await, Err(Error::NotFound { .. })));
    store.delete(&key).await.unwrap();
}

#[tokio::test]
async fn constructor_uses_file_backend_from_context_and_survives_reopening() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = KeyringSecretStore::new(ctx.clone());
    assert_eq!(store.backend(), SecretBackend::PlaintextFile);
    let key = store
        .put(
            &ProfileId::new("from-context"),
            &Secret::new("context-secret"),
        )
        .await
        .unwrap();
    let KeyRef::File { path } = &key else {
        panic!("environment backend selection did not produce a file")
    };
    private_mode(path);
    let reopened = KeyringSecretStore::new(ctx);
    assert_eq!(reopened.get(&key).await.unwrap().expose(), "context-secret");
    reopened.delete(&key).await.unwrap();
    assert!(matches!(store.get(&key).await, Err(Error::NotFound { .. })));
}

#[tokio::test]
async fn reading_file_secrets_trims_pasted_whitespace() {
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyringSecretStore::new(context(tmp.path(), MapEnv::new()));
    let path = tmp.path().join("pasted-key");
    fs::write(&path, b" \t\r\npasted-secret\n  ").unwrap();
    assert_eq!(
        store.get(&KeyRef::File { path }).await.unwrap().expose(),
        "pasted-secret"
    );
}

#[tokio::test]
async fn env_key_uses_only_injected_environment_and_delete_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    // PATH is deliberately unrelated to a secret. Never mutate/read the process env.
    let ctx = context(
        tmp.path(),
        MapEnv::new().with("PATH", "  injected-secret-only  "),
    );
    let store = KeyringSecretStore::new(ctx);
    let key = KeyRef::Env { var: "PATH".into() };
    assert_eq!(
        store.get(&key).await.unwrap().expose(),
        "injected-secret-only"
    );
    store.delete(&key).await.unwrap();
    assert_eq!(
        store.get(&key).await.unwrap().expose(),
        "injected-secret-only"
    );
    let empty = KeyringSecretStore::new(context(tmp.path(), MapEnv::new()));
    assert!(matches!(empty.get(&key).await, Err(Error::NotFound { .. })));
    assert!(!tmp.path().join("data").exists());
}

#[tokio::test]
async fn missing_file_and_absent_or_blank_env_entries_are_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyringSecretStore::new(context(tmp.path(), MapEnv::new().with("BLANK", " \t\n")));
    for key in [
        KeyRef::File {
            path: tmp.path().join("missing"),
        },
        KeyRef::Env {
            var: "MISSING".into(),
        },
        KeyRef::Env {
            var: "BLANK".into(),
        },
    ] {
        assert!(
            matches!(store.get(&key).await, Err(Error::NotFound { .. })),
            "{key:?}"
        );
    }
}

#[tokio::test]
async fn delete_leaves_external_files_and_parent_traversal_targets_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    fs::create_dir_all(ctx.data_dir.secrets_dir()).unwrap();
    let store = KeyringSecretStore::new(ctx.clone());
    for path in [
        tmp.path().join("external-key"),
        ctx.data_dir.root().join("secrets-sibling/key"),
        ctx.data_dir.secrets_dir().join("../outside-key"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"user-owned secret\n").unwrap();
        store
            .delete(&KeyRef::File { path: path.clone() })
            .await
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"user-owned secret\n");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn delete_does_not_follow_a_secrets_directory_symlink_to_an_external_key() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    fs::create_dir_all(ctx.data_dir.secrets_dir()).unwrap();
    let external = tmp.path().join("external-secret");
    fs::write(&external, b"external-secret").unwrap();
    let link = ctx.data_dir.secrets_dir().join("linked-key");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    let store = KeyringSecretStore::new(ctx);
    store
        .delete(&KeyRef::File { path: link.clone() })
        .await
        .unwrap();
    assert_eq!(fs::read(&external).unwrap(), b"external-secret");
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn empty_put_is_invalid_and_does_not_destroy_an_existing_secret() {
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyringSecretStore::new(context(tmp.path(), MapEnv::new()));
    let id = ProfileId::new("existing");
    let key = store.put(&id, &Secret::new("keep-me")).await.unwrap();
    for value in ["", " \t\r\n"] {
        assert!(matches!(
            store.put(&id, &Secret::new(value)).await,
            Err(Error::Invalid { .. })
        ));
        assert_eq!(store.get(&key).await.unwrap().expose(), "keep-me");
    }
}

#[tokio::test]
async fn missing_profiles_returns_default_with_two_builtins_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    let set = store.load().await.unwrap();
    assert_eq!(set, ProfileSet::default());
    assert_eq!(set.profiles.len(), 2);
    assert_eq!(set.profiles, catalog::builtin_profiles());
    assert!(!ctx.data_dir.root().exists());
}

#[tokio::test]
async fn profiles_round_trip_user_site_settings_and_write_private_json() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    let set = user_set();
    store.save(&set).await.unwrap();
    private_mode(&ctx.data_dir.profiles_json());
    assert_eq!(
        JsonProfileStore::new(ctx.clone()).load().await.unwrap(),
        set
    );
    let stored: ProfileSet =
        serde_json::from_slice(&fs::read(ctx.data_dir.profiles_json()).unwrap()).unwrap();
    assert_eq!(stored, set);
    // An update also keeps the file private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            ctx.data_dir.profiles_json(),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    store.save(&set).await.unwrap();
    private_mode(&ctx.data_dir.profiles_json());
}

#[tokio::test]
async fn stored_builtins_are_replaced_from_catalog_and_ordered_before_users() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    let expected = user_set();
    let mut stale = expected.clone();
    stale.profiles[0].name = "stale original".into();
    stale.profiles[1].name = "stale official".into();
    stale.profiles[1].builtin = None; // A reserved id is still replaced.
    stale.profiles.reverse();
    let bytes = write_set(&ctx, &stale);
    assert_eq!(store.load().await.unwrap(), expected);
    assert_eq!(
        fs::read(ctx.data_dir.profiles_json()).unwrap(),
        bytes,
        "load must not rewrite the file"
    );
    store.save(&stale).await.unwrap();
    let saved: ProfileSet =
        serde_json::from_slice(&fs::read(ctx.data_dir.profiles_json()).unwrap()).unwrap();
    assert_eq!(saved, expected);
}

#[tokio::test]
async fn newer_profile_schema_is_unsupported_and_left_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let set = ProfileSet {
        schema: PROFILES_SCHEMA + 1,
        ..user_set()
    };
    let bytes = write_set(&ctx, &set);
    let store = JsonProfileStore::new(ctx.clone());
    assert!(matches!(store.load().await, Err(Error::Unsupported { .. })));
    assert_eq!(fs::read(ctx.data_dir.profiles_json()).unwrap(), bytes);
}

#[tokio::test]
async fn duplicate_profile_ids_are_rejected_on_save_without_overwriting_good_data() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    let mut set = user_set();
    store.save(&set).await.unwrap();
    let before = fs::read(ctx.data_dir.profiles_json()).unwrap();
    set.profiles.push(set.profiles.last().unwrap().clone());
    assert!(matches!(store.save(&set).await, Err(Error::Invalid { .. })));
    assert_eq!(fs::read(ctx.data_dir.profiles_json()).unwrap(), before);
}

#[tokio::test]
async fn custom_sites_cannot_reuse_either_builtin_site_id() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    for builtin in catalog::builtin_sites() {
        let mut set = user_set();
        set.custom_sites[0].id = builtin.id;
        assert!(matches!(store.save(&set).await, Err(Error::Invalid { .. })));
    }
    assert!(!ctx.data_dir.profiles_json().exists());
}

#[tokio::test]
async fn duplicate_custom_site_ids_are_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    let mut set = user_set();
    set.custom_sites.push(set.custom_sites[0].clone());
    assert!(matches!(store.save(&set).await, Err(Error::Invalid { .. })));
    assert!(!ctx.data_dir.profiles_json().exists());
}

#[tokio::test]
async fn corrupt_profiles_json_returns_parse_error_without_changing_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    fs::create_dir_all(ctx.data_dir.root()).unwrap();
    let store = JsonProfileStore::new(ctx.clone());
    for bytes in [
        b"{ truncated JSON\n".as_slice(),
        b"{\"schema\":1,\"profiles\":42}\n".as_slice(),
    ] {
        fs::write(ctx.data_dir.profiles_json(), bytes).unwrap();
        assert!(matches!(store.load().await, Err(Error::Parse { .. })));
        assert_eq!(fs::read(ctx.data_dir.profiles_json()).unwrap(), bytes);
    }
}

#[tokio::test]
async fn loading_profiles_rejects_duplicate_ids_and_builtin_site_collisions() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path(), MapEnv::new());
    let store = JsonProfileStore::new(ctx.clone());
    let mut duplicate = user_set();
    duplicate
        .profiles
        .push(duplicate.profiles.last().unwrap().clone());
    let mut collision = user_set();
    collision.custom_sites[0].id = catalog::builtin_sites()[0].id.clone();
    for invalid in [duplicate, collision] {
        let before = write_set(&ctx, &invalid);
        assert!(matches!(store.load().await, Err(Error::Invalid { .. })));
        assert_eq!(fs::read(ctx.data_dir.profiles_json()).unwrap(), before);
    }
}
