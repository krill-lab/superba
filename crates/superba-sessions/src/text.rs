//! Localized notices produced by this crate (keys under `sessions.`).
//!
//! Binaries add [`crate::catalog_source`] to their `i18n::Catalog`; the English
//! fallback travels with every text, so rendering works without it.

use std::path::Path;

use superba_core::{Locale, LocalizedText, Notice};

const EN: &str = include_str!("../locales/en.toml");
const ZH_CN: &str = include_str!("../locales/zh-CN.toml");

/// Raw TOML source of this crate's catalog for `locale`.
pub(crate) fn source(locale: Locale) -> &'static str {
    match locale {
        Locale::En => EN,
        Locale::ZhCn => ZH_CN,
    }
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

pub(crate) fn unreadable(path: &Path, error: &str) -> Notice {
    Notice::warning(
        LocalizedText::new("sessions.warn.unreadable", "Could not read {path}: {error}")
            .arg("path", path_arg(path))
            .arg("error", error),
    )
}

pub(crate) fn symlink_skipped(path: &Path) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "sessions.warn.symlink_skipped",
            "Skipped the symbolic link {path}; session files must be regular files.",
        )
        .arg("path", path_arg(path)),
    )
}

pub(crate) fn no_session_meta(count: usize) -> Notice {
    Notice::info(
        LocalizedText::new(
            "sessions.warn.no_session_meta",
            "{count} session files have no session metadata and were left alone.",
        )
        .arg("count", count),
    )
}

pub(crate) fn db_unreadable(path: &Path, error: &str) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "sessions.warn.db_unreadable",
            "Could not read the database {path}: {error}",
        )
        .arg("path", path_arg(path))
        .arg("error", error),
    )
}

pub(crate) fn db_on_share(path: &Path) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "sessions.warn.db_on_share",
            "Skipped the database {path}: SQLite cannot lock files on network or WSL shares.",
        )
        .arg("path", path_arg(path)),
    )
}

pub(crate) fn sqlite_home_unresolved(value: &str) -> Notice {
    Notice::warning(
        LocalizedText::new(
            "sessions.warn.sqlite_home_unresolved",
            "Could not use sqlite_home = \"{value}\" from config.toml.",
        )
        .arg("value", value),
    )
}

pub(crate) fn session_index_provider() -> Notice {
    Notice::info(LocalizedText::new(
        "sessions.warn.session_index_provider",
        "session_index.jsonl contains provider information; Superba leaves it unchanged.",
    ))
}

pub(crate) fn stale_foreign_lock(path: &Path) -> Notice {
    Notice::info(
        LocalizedText::new(
            "sessions.warn.stale_foreign_lock",
            "Ignored an old lock left by another tool: {path}",
        )
        .arg("path", path_arg(path)),
    )
}
