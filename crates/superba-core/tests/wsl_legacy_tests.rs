//! "Run Codex in WSL" (user bug 54 (b)): the app's setting, the distribution it
//! uses and the `WSLENV` edits that forward a variable into WSL.

use pretty_assertions::assert_eq;
use superba_core::wsl::{
    RUN_CODEX_IN_WSL_KEY, WslDistro, legacy_distro, runs_codex_in_wsl, wslenv_names, wslenv_with,
    wslenv_without,
};

#[test]
fn the_desktop_table_wins_over_the_global_state_it_migrates_from() {
    let on = format!("model = \"gpt-5\"\n\n[desktop]\n{RUN_CODEX_IN_WSL_KEY} = true\n");
    let off = format!("[desktop]\n{RUN_CODEX_IN_WSL_KEY} = false\n");
    let global_on = serde_json::json!({ RUN_CODEX_IN_WSL_KEY: true });
    assert!(runs_codex_in_wsl(Some(&on), None));
    assert!(!runs_codex_in_wsl(Some(&off), Some(&global_on)));
    // Not yet migrated into config.toml: the global state says so.
    assert!(runs_codex_in_wsl(
        Some("model = \"gpt-5\"\n"),
        Some(&global_on)
    ));
    assert!(runs_codex_in_wsl(None, Some(&global_on)));
    // Unset, not a boolean, unparseable: off (the app drops such values too).
    assert!(!runs_codex_in_wsl(None, None));
    assert!(!runs_codex_in_wsl(
        Some(&format!("[desktop]\n{RUN_CODEX_IN_WSL_KEY} = \"yes\"\n")),
        None
    ));
    assert!(!runs_codex_in_wsl(Some("[desktop"), None));
    assert!(!runs_codex_in_wsl(
        None,
        Some(&serde_json::json!({ RUN_CODEX_IN_WSL_KEY: "true" }))
    ));
}

#[test]
fn the_app_uses_the_default_distribution_else_the_first_ubuntu() {
    let named = |n: &str, default: bool| WslDistro {
        default,
        ..WslDistro::named(n)
    };
    let list = [named("Debian", false), named("Ubuntu-24.04", false)];
    assert_eq!(
        legacy_distro(&list).map(|d| d.name.as_str()),
        Some("Ubuntu-24.04")
    );
    let list = [named("Ubuntu", false), named("Debian", true)];
    assert_eq!(
        legacy_distro(&list).map(|d| d.name.as_str()),
        Some("Debian")
    );
    assert_eq!(legacy_distro(&[named("Debian", false)]), None);
}

#[test]
fn wslenv_entries_are_added_once_and_removed_by_name() {
    const VAR: &str = "CODEX_AUTHAPI_BASE_URL";
    assert_eq!(wslenv_with(None, VAR).as_deref(), Some(VAR));
    assert_eq!(wslenv_with(Some(""), VAR).as_deref(), Some(VAR));
    assert_eq!(
        wslenv_with(Some("USERPROFILE/p:"), VAR).as_deref(),
        Some("USERPROFILE/p:CODEX_AUTHAPI_BASE_URL")
    );
    // Named already (any flags, any case): nothing to add.
    assert_eq!(wslenv_with(Some("codex_authapi_base_url/u"), VAR), None);
    assert!(wslenv_names("PATH/l:CODEX_AUTHAPI_BASE_URL", VAR));
    assert!(!wslenv_names("CODEX_AUTHAPI_BASE_URL_X", VAR));
    assert_eq!(
        wslenv_without("USERPROFILE/p:CODEX_AUTHAPI_BASE_URL:PATH/l", VAR).as_deref(),
        Some("USERPROFILE/p:PATH/l")
    );
    assert_eq!(wslenv_without("CODEX_AUTHAPI_BASE_URL", VAR), None);
}
