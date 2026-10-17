//! User-facing texts of this crate (keys in the core catalogs, `remote.*`).

use superba_core::LocalizedText;

/// The host has no app `hostId`: no remote hot restart.
pub fn no_host_id(host: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.no_host_id",
        "ChatGPT has no host id for {host}: the change applies after you disconnect and reconnect the host in ChatGPT.",
    )
    .arg("host", host)
}

/// The remote restart kills every `codex app-server --listen` (plan 8.2).
pub fn pkill(host: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.pkill",
        "ChatGPT stops every `codex app-server --listen` process on {host} when it restarts it, including ones you started yourself.",
    )
    .arg("host", host)
}

/// The login shell could not be run.
pub fn login_shell(host: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.login_shell",
        "The login shell on {host} could not be run; CODEX_HOME and the environment were read from a plain shell and may differ from what ChatGPT sees.",
    )
    .arg("host", host)
}

/// No `codex` on the login shell's PATH.
pub fn codex_missing(host: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.codex_missing",
        "No codex binary was found on the PATH of {host}'s login shell.",
    )
    .arg("host", host)
}

/// Unknown Codex version (PAT needs confirmation, plan 9.8).
pub fn version_unknown(host: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.version_unknown",
        "The Codex version on {host} is unknown; PAT needs a recent Codex.",
    )
    .arg("host", host)
}

/// The Codex home was last written from another machine (plan 8.6).
pub fn shared_home(host: &str, other: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.shared_home",
        "The Codex home on {host} was last written from another machine ({other}); it is probably shared (e.g. NFS), so this change affects every machine using it.",
    )
    .arg("host", host)
    .arg("other", other)
}

/// The host still uses the legacy `krill` provider.
pub fn legacy_provider(host: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.legacy_provider",
        "{host} uses the old \"krill\" provider; its existing sessions stay hidden in ChatGPT until they are migrated.",
    )
    .arg("host", host)
}

/// A user line sets the variable to another value.
pub fn foreign_env(path: &str, line: usize, value: &str) -> LocalizedText {
    LocalizedText::new(
        "remote.warn.foreign_env",
        "{path} line {line} sets CODEX_AUTHAPI_BASE_URL to {value}; it may override Superba's value.",
    )
    .arg("path", path)
    .arg("line", line)
    .arg("value", value)
}

/// Where a remote env change takes effect.
pub fn env_scope() -> LocalizedText {
    LocalizedText::new(
        "remote.env.scope",
        "Applies to new SSH sessions and to the app-server ChatGPT starts on this host.",
    )
}

/// A switch with "follow this computer" changes only the host's connection.
pub fn scope_connection(host: &str) -> LocalizedText {
    LocalizedText::new(
        "host_scope.connection",
        "Only the connection of {host} changes (provider, line, key and model); its own Codex settings stay.",
    )
    .arg("host", host)
}

/// A settings sync changes only Codex settings.
pub fn scope_settings(host: &str) -> LocalizedText {
    LocalizedText::new(
        "host_scope.settings",
        "Only Codex settings change on {host}; its connection, key and model stay.",
    )
    .arg("host", host)
}

/// Every text this crate emits, with sample arguments (catalog coverage tests).
pub fn all_samples() -> Vec<LocalizedText> {
    let mut v = crate::sessions::all_samples();
    v.extend([
        no_host_id("h"),
        pkill("h"),
        login_shell("h"),
        codex_missing("h"),
        version_unknown("h"),
        shared_home("h", "m"),
        legacy_provider("h"),
        foreign_env("/p", 3, "v"),
        env_scope(),
        scope_connection("h"),
        scope_settings("h"),
    ]);
    v
}
