//! UI translations: the core catalog (sites, lines, auth modes, reasons, errors)
//! and the switch catalog (plan warnings, steps, questions produced by
//! `superba-switch`), layered with this crate's UI catalog.
//!
//! The UI catalog is split into one file per area (`locales/<area>.<locale>.toml`,
//! [`UI_AREAS`]), so the owner of an area adds keys without touching a shared
//! file. Pages call [`Tr::t`] with literal keys; tests check every area file for
//! parity (same keys and placeholders in both languages), that no key is defined
//! in two files, and that every literal key used in the sources exists.

use std::fmt::Display;

use superba_core::{Catalog, Locale, LocalizedText};

/// UI catalog areas, in loading order.
pub const UI_AREAS: [&str; 13] = [
    "shell",
    "overview",
    "wizard",
    "providers",
    "models",
    "router",
    "settings",
    "sessions",
    "hosts",
    "updates",
    "diagnostics",
    "preferences",
    "palette",
];

/// English UI catalog files as `(area, TOML source)`.
const UI_EN: [(&str, &str); 13] = [
    ("shell", include_str!("../locales/shell.en.toml")),
    ("overview", include_str!("../locales/overview.en.toml")),
    ("wizard", include_str!("../locales/wizard.en.toml")),
    ("providers", include_str!("../locales/providers.en.toml")),
    ("models", include_str!("../locales/models.en.toml")),
    ("router", include_str!("../locales/router.en.toml")),
    ("settings", include_str!("../locales/settings.en.toml")),
    ("sessions", include_str!("../locales/sessions.en.toml")),
    ("hosts", include_str!("../locales/hosts.en.toml")),
    ("updates", include_str!("../locales/updates.en.toml")),
    (
        "diagnostics",
        include_str!("../locales/diagnostics.en.toml"),
    ),
    (
        "preferences",
        include_str!("../locales/preferences.en.toml"),
    ),
    ("palette", include_str!("../locales/palette.en.toml")),
];

/// Simplified Chinese UI catalog files as `(area, TOML source)`.
const UI_ZH_CN: [(&str, &str); 13] = [
    ("shell", include_str!("../locales/shell.zh-CN.toml")),
    ("overview", include_str!("../locales/overview.zh-CN.toml")),
    ("wizard", include_str!("../locales/wizard.zh-CN.toml")),
    ("providers", include_str!("../locales/providers.zh-CN.toml")),
    ("models", include_str!("../locales/models.zh-CN.toml")),
    ("router", include_str!("../locales/router.zh-CN.toml")),
    ("settings", include_str!("../locales/settings.zh-CN.toml")),
    ("sessions", include_str!("../locales/sessions.zh-CN.toml")),
    ("hosts", include_str!("../locales/hosts.zh-CN.toml")),
    ("updates", include_str!("../locales/updates.zh-CN.toml")),
    (
        "diagnostics",
        include_str!("../locales/diagnostics.zh-CN.toml"),
    ),
    (
        "preferences",
        include_str!("../locales/preferences.zh-CN.toml"),
    ),
    ("palette", include_str!("../locales/palette.zh-CN.toml")),
];

/// The UI catalog files of a locale as `(area, TOML source)`, in [`UI_AREAS`]
/// order.
pub fn ui_files(locale: Locale) -> &'static [(&'static str, &'static str)] {
    match locale {
        Locale::En => &UI_EN,
        Locale::ZhCn => &UI_ZH_CN,
    }
}

/// Translator for one display locale.
#[derive(Clone, Debug)]
pub struct Tr {
    catalog: Catalog,
}

impl Tr {
    /// Build the layered catalog for `locale`.
    pub fn new(locale: Locale) -> Self {
        let mut catalog = superba_switch::messages::catalog(locale);
        for l in [Locale::En, locale] {
            // Compiled-in catalogs are covered by tests; a parse error is a bug and
            // only drops that file (keys then fall back to English text).
            for (_, source) in ui_files(l) {
                let _ = catalog.add_toml(l, source);
            }
            // Category names and catalog notices of the settings catalog.
            let _ = catalog.add_toml(l, superba_catalog::i18n_source(l));
        }
        Self { catalog }
    }

    /// Display locale.
    pub fn locale(&self) -> Locale {
        self.catalog.locale()
    }

    /// Translate a UI key (missing keys render as the key itself, which the
    /// catalog test prevents).
    pub fn t(&self, key: &str) -> String {
        self.catalog.get(key).unwrap_or(key).to_owned()
    }

    /// Translate a UI key and substitute `{name}` arguments.
    pub fn f(&self, key: &str, args: &[(&str, &dyn Display)]) -> String {
        let mut text = LocalizedText::new(key, key);
        for (name, value) in args {
            text = text.arg(*name, value);
        }
        self.catalog.text(&text)
    }

    /// Render a domain text.
    pub fn text(&self, text: &LocalizedText) -> String {
        self.catalog.text(text)
    }
}

/// The OS locale mapped onto a supported locale (English when unsupported).
pub fn system_locale() -> Locale {
    sys_locale::get_locale()
        .and_then(|l| Locale::parse(&l))
        .unwrap_or_default()
}
