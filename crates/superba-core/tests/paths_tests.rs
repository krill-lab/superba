#![allow(missing_docs)]

use std::path::Path;
use std::sync::Arc;

use superba_core::{CodexHome, DataDir, EnvSource, LocalContext, MapEnv};

#[test]
fn codex_home_defaults_to_the_injected_home_and_ignores_empty_overrides() {
    let home = Path::new("/fixtures/user");
    for env in [
        MapEnv::new(),
        MapEnv::new()
            .with("CODEX_HOME", "")
            .with("CODEX_SQLITE_HOME", ""),
    ] {
        let resolved = CodexHome::resolve(&env, home);
        assert_eq!(resolved.root(), home.join(".codex"));
        assert_eq!(resolved.sqlite_home(), home.join(".codex"));
        assert_eq!(resolved.state_db(), home.join(".codex/state_5.sqlite"));
    }
}

#[test]
fn codex_home_and_sqlite_overrides_are_independent() {
    let home = Path::new("/fixtures/user");
    let env = MapEnv::new().with("CODEX_HOME", "/fixtures/custom codex");
    let resolved = CodexHome::resolve(&env, home);
    assert_eq!(resolved.root(), Path::new("/fixtures/custom codex"));
    assert_eq!(
        resolved.state_db(),
        Path::new("/fixtures/custom codex/state_5.sqlite")
    );
    let env = env.with("CODEX_SQLITE_HOME", "/fixtures/sqlite");
    let resolved = CodexHome::resolve(&env, home);
    assert_eq!(resolved.root(), Path::new("/fixtures/custom codex"));
    assert_eq!(resolved.sqlite_home(), Path::new("/fixtures/sqlite"));
    assert_eq!(
        resolved.state_db(),
        Path::new("/fixtures/sqlite/state_5.sqlite")
    );
    assert_eq!(
        resolved.config_toml(),
        Path::new("/fixtures/custom codex/config.toml")
    );
    let sqlite_only = CodexHome::resolve(
        &MapEnv::new().with("CODEX_SQLITE_HOME", "/fixtures/sqlite"),
        home,
    );
    assert_eq!(sqlite_only.root(), home.join(".codex"));
    assert_eq!(
        sqlite_only.state_db(),
        Path::new("/fixtures/sqlite/state_5.sqlite")
    );
    let empty_sqlite = CodexHome::resolve(
        &MapEnv::new()
            .with("CODEX_HOME", "/fixtures/custom")
            .with("CODEX_SQLITE_HOME", ""),
        home,
    );
    assert_eq!(
        empty_sqlite.state_db(),
        Path::new("/fixtures/custom/state_5.sqlite")
    );
}

#[test]
fn codex_home_layout_keeps_only_the_state_database_in_sqlite_home() {
    let root = Path::new("/fixtures/codex");
    let codex = CodexHome::new(root).with_sqlite_home("/fixtures/db");
    for (actual, relative) in [
        (codex.config_toml(), "config.toml"),
        (codex.auth_json(), "auth.json"),
        (codex.sessions_dir(), "sessions"),
        (codex.archived_sessions_dir(), "archived_sessions"),
        (codex.global_state_json(), ".codex-global-state.json"),
        (codex.app_server_control_dir(), "app-server-control"),
        (codex.tmp_dir(), "tmp"),
        (codex.migrate_lock(), "tmp/superba-migrate.lock"),
    ] {
        assert_eq!(actual, root.join(relative));
    }
    assert_eq!(codex.state_db(), Path::new("/fixtures/db/state_5.sqlite"));
}

#[test]
fn data_directory_exposes_the_complete_persisted_layout() {
    let root = Path::new("/fixtures/superba");
    let data = DataDir::new(root);
    assert_eq!(data.root(), root);
    for (actual, relative) in [
        (data.profiles_json(), "profiles.json"),
        (data.state_json(), "state.json"),
        (data.state_lock(), "state.lock"),
        (data.journal_dir(), "journal"),
        (data.backups_dir(), "backups"),
        (data.first_write_backups_dir(), "backups/first-write"),
        (data.switch_backups_dir(), "backups/switch"),
        (data.login_stash_dir(), "backups/login-stash"),
        (data.rc_backups_dir(), "backups/rc"),
        (
            data.migrate_backup_dir("migration-123"),
            "backups/migrate-migration-123",
        ),
        (data.secrets_dir(), "secrets"),
        (data.app_sessions_json(), "app-sessions.json"),
        (data.remote_dir(), "remote"),
        (data.logs_dir(), "logs"),
    ] {
        assert_eq!(actual, root.join(relative));
    }
}

#[test]
fn test_context_uses_only_its_injected_root_and_empty_environment() {
    let root = Path::new("/fixtures/isolated");
    let context = LocalContext::for_tests(root);
    assert_eq!(context.home_dir, root.join("home"));
    assert_eq!(context.data_dir.root(), root.join("data"));
    assert_eq!(context.codex_home().root(), root.join("home/.codex"));
    for key in ["HOME", "CODEX_HOME", "CODEX_SQLITE_HOME", "PATH"] {
        assert_eq!(context.env.var_os(key), None);
    }
}

#[test]
fn local_context_honors_an_injected_environment() {
    let context = LocalContext::new(
        "/fixtures/home",
        DataDir::new("/fixtures/data"),
        Arc::new(MapEnv::new().with("CODEX_HOME", "/fixtures/override")),
    );
    assert_eq!(context.codex_home().root(), Path::new("/fixtures/override"));
    assert_eq!(context.data_dir.root(), Path::new("/fixtures/data"));
}

#[test]
fn map_environment_distinguishes_missing_empty_and_nonempty_values() {
    let env = MapEnv::new()
        .with("EMPTY", "")
        .with("SPACE", " \t ")
        .with("VALUE", " retained ");
    assert_eq!(env.var("MISSING"), None);
    assert_eq!(env.var("EMPTY").as_deref(), Some(""));
    assert_eq!(env.var_non_empty("EMPTY"), None);
    assert_eq!(env.var_non_empty("SPACE"), None);
    assert_eq!(env.var_non_empty("VALUE").as_deref(), Some(" retained "));
}

#[test]
fn display_paths_drop_the_windows_verbatim_prefix() {
    use superba_core::paths::display_path;
    // User bug 43: the wizard showed `\\?\C:\Program Files\WindowsApps\...`.
    for (raw, shown) in [
        (
            r"\\?\C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0",
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0",
        ),
        (
            r"\\?\UNC\server\share\codex.exe",
            r"\\server\share\codex.exe",
        ),
        (r"\\?\unc\server\share", r"\\server\share"),
        // Verbatim forms without a shorter name, and plain paths, stay as they are.
        (r"\\?\Volume{0b1c}\dir", r"\\?\Volume{0b1c}\dir"),
        (r"C:\Users\me\codex.exe", r"C:\Users\me\codex.exe"),
        (r"\\server\share", r"\\server\share"),
        ("/home/me/.codex", "/home/me/.codex"),
    ] {
        assert_eq!(display_path(Path::new(raw)), shown, "{raw}");
    }
}

#[test]
fn display_paths_use_one_separator_on_windows() {
    use superba_core::paths::display_path_text;
    // User bugs 55 / 62(c): `LOCALAPPDATA` joined with "OpenAI/Codex/bin".
    for (raw, shown) in [
        (
            r"C:\Users\superba\AppData\Local\OpenAI/Codex/bin\896a0166b774037a\codex.exe",
            r"C:\Users\superba\AppData\Local\OpenAI\Codex\bin\896a0166b774037a\codex.exe",
        ),
        (r"c:/Users/me/.codex", r"c:\Users\me\.codex"),
        (
            r"\\?\C:\Program Files\App/resources\codex.exe",
            r"C:\Program Files\App\resources\codex.exe",
        ),
        (r"\\?\UNC\server\share/dir", r"\\server\share\dir"),
        (
            r"\\wsl.localhost\Ubuntu/home/me/.codex",
            r"\\wsl.localhost\Ubuntu\home\me\.codex",
        ),
        ("//server/share/x", r"\\server\share\x"),
        // A WSL distribution's or remote host's own paths, home-relative text
        // and verbatim forms without a shorter name stay as they are.
        ("/home/me/.codex", "/home/me/.codex"),
        ("~/.codex/config.toml", "~/.codex/config.toml"),
        (r"\\?\Volume{0b1c}/dir", r"\\?\Volume{0b1c}/dir"),
        (r"\\.\pipe/x", r"\\.\pipe/x"),
    ] {
        assert_eq!(display_path_text(raw, true), shown, "{raw}");
    }
    // Elsewhere a backslash is part of a file name.
    assert_eq!(display_path_text(r"C:\x/y", false), r"C:\x/y");
    assert_eq!(display_path_text("/home/a\\b", false), "/home/a\\b");
}
