//! Minimal i18n: locales, localizable text values and layered catalogs.
//!
//! Domain code never formats user-facing English directly. It returns
//! [`LocalizedText`] (a key, an English fallback and named arguments) and the UI
//! (GUI or `spb`) renders it with a [`Catalog`].
//!
//! Catalogs are flat `key = "text"` maps loaded from TOML (nested tables are
//! flattened with `.`). This crate ships the catalog for the keys it defines
//! (`locales/*.toml`: sites, lines, auth modes, restart/block reasons, errors ...).
//! Each binary crate keeps its own UI catalog files and layers them on top with
//! [`Catalog::add_toml`]; later layers win. Arguments use `{name}` placeholders.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Supported UI languages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Locale {
    /// English (also the fallback language).
    #[default]
    #[serde(rename = "en")]
    En,
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    ZhCn,
}

impl Locale {
    /// All supported locales.
    pub const ALL: [Locale; 2] = [Locale::En, Locale::ZhCn];

    /// BCP 47 code: `en`, `zh-CN`.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::ZhCn => "zh-CN",
        }
    }

    /// Parse loose locale strings: `en`, `en_US.UTF-8`, `zh`, `zh-CN`, `zh_Hans` ...
    /// Unknown languages return `None`.
    pub fn parse(s: &str) -> Option<Locale> {
        let lower = s.trim().to_ascii_lowercase();
        let lang = lower.split(['-', '_', '.', '@']).next().unwrap_or("");
        match lang {
            "en" | "c" | "posix" => Some(Locale::En),
            "zh" => Some(Locale::ZhCn),
            _ => None,
        }
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// A user-facing message: i18n key, English fallback and `{name}` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizedText {
    /// Catalog key, e.g. `restart.reason.no_cdp`.
    pub key: String,
    /// English text used when no catalog has the key (may contain placeholders).
    pub fallback: String,
    /// Named arguments substituted into `{name}` placeholders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<(String, String)>,
    /// Named arguments that are themselves localizable (a line or site name inside a
    /// sentence). Rendered with the same catalog before substitution, so a Chinese
    /// sentence never embeds an English line name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested: Vec<(String, LocalizedText)>,
}

impl LocalizedText {
    /// New text without arguments.
    pub fn new(key: impl Into<String>, fallback: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            fallback: fallback.into(),
            args: Vec::new(),
            nested: Vec::new(),
        }
    }

    /// Add (or replace) an argument.
    pub fn arg(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        let name = name.into();
        let value = value.to_string();
        self.nested.retain(|(n, _)| *n != name);
        if let Some(slot) = self.args.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            self.args.push((name, value));
        }
        self
    }

    /// Add (or replace) a localizable argument (see [`LocalizedText::nested`]).
    pub fn arg_text(mut self, name: impl Into<String>, value: LocalizedText) -> Self {
        let name = name.into();
        self.args.retain(|(n, _)| *n != name);
        if let Some(slot) = self.nested.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = value;
        } else {
            self.nested.push((name, value));
        }
        self
    }

    /// Render the English fallback without a catalog (logs, tests).
    pub fn render_fallback(&self) -> String {
        if self.nested.is_empty() {
            return interpolate(&self.fallback, &self.args);
        }
        let mut args = self.args.clone();
        args.extend(
            self.nested
                .iter()
                .map(|(n, t)| (n.clone(), t.render_fallback())),
        );
        interpolate(&self.fallback, &args)
    }
}

impl fmt::Display for LocalizedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render_fallback())
    }
}

/// Severity of a [`Notice`].
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational.
    #[default]
    Info,
    /// Needs attention but does not stop anything.
    Warning,
    /// Destructive or risky; render with the danger style.
    Danger,
}

/// A severity-tagged message (plan warnings, banners, doctor findings).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// Severity.
    pub severity: Severity,
    /// Message.
    pub text: LocalizedText,
}

impl Notice {
    /// Info notice.
    pub fn info(text: LocalizedText) -> Self {
        Self {
            severity: Severity::Info,
            text,
        }
    }

    /// Warning notice.
    pub fn warning(text: LocalizedText) -> Self {
        Self {
            severity: Severity::Warning,
            text,
        }
    }

    /// Danger notice.
    pub fn danger(text: LocalizedText) -> Self {
        Self {
            severity: Severity::Danger,
            text,
        }
    }
}

/// Core catalog files: the main file, then one file per v0.3 area
/// (`locales/core.<area>.<locale>.toml`), so each area owner adds keys without
/// touching a shared file. Every area file starts with a table header and never
/// repeats a key of another core file, so the files concatenate into one valid
/// TOML catalog (tests check both).
const CORE_EN_FILES: &[(&str, &str)] = &[
    ("en.toml", include_str!("../locales/en.toml")),
    (
        "core.models.en.toml",
        include_str!("../locales/core.models.en.toml"),
    ),
    (
        "core.requests.en.toml",
        include_str!("../locales/core.requests.en.toml"),
    ),
    (
        "core.versions.en.toml",
        include_str!("../locales/core.versions.en.toml"),
    ),
    (
        "core.hosts.en.toml",
        include_str!("../locales/core.hosts.en.toml"),
    ),
    (
        "core.selfupdate.en.toml",
        include_str!("../locales/core.selfupdate.en.toml"),
    ),
    (
        "core.import.en.toml",
        include_str!("../locales/core.import.en.toml"),
    ),
];
const CORE_ZH_CN_FILES: &[(&str, &str)] = &[
    ("zh-CN.toml", include_str!("../locales/zh-CN.toml")),
    (
        "core.models.zh-CN.toml",
        include_str!("../locales/core.models.zh-CN.toml"),
    ),
    (
        "core.requests.zh-CN.toml",
        include_str!("../locales/core.requests.zh-CN.toml"),
    ),
    (
        "core.versions.zh-CN.toml",
        include_str!("../locales/core.versions.zh-CN.toml"),
    ),
    (
        "core.hosts.zh-CN.toml",
        include_str!("../locales/core.hosts.zh-CN.toml"),
    ),
    (
        "core.selfupdate.zh-CN.toml",
        include_str!("../locales/core.selfupdate.zh-CN.toml"),
    ),
    (
        "core.import.zh-CN.toml",
        include_str!("../locales/core.import.zh-CN.toml"),
    ),
];

/// The whole core catalog per locale: the files above, concatenated at compile
/// time (keep both lists in the same order).
const CORE_EN: &str = concat!(
    include_str!("../locales/en.toml"),
    "\n",
    include_str!("../locales/core.models.en.toml"),
    "\n",
    include_str!("../locales/core.requests.en.toml"),
    "\n",
    include_str!("../locales/core.versions.en.toml"),
    "\n",
    include_str!("../locales/core.hosts.en.toml"),
    "\n",
    include_str!("../locales/core.selfupdate.en.toml"),
    "\n",
    include_str!("../locales/core.import.en.toml"),
);
const CORE_ZH_CN: &str = concat!(
    include_str!("../locales/zh-CN.toml"),
    "\n",
    include_str!("../locales/core.models.zh-CN.toml"),
    "\n",
    include_str!("../locales/core.requests.zh-CN.toml"),
    "\n",
    include_str!("../locales/core.versions.zh-CN.toml"),
    "\n",
    include_str!("../locales/core.hosts.zh-CN.toml"),
    "\n",
    include_str!("../locales/core.selfupdate.zh-CN.toml"),
    "\n",
    include_str!("../locales/core.import.zh-CN.toml"),
);

/// Raw TOML source of the whole core catalog for `locale` (every core file,
/// concatenated; for parity tests in other crates).
pub fn core_catalog_source(locale: Locale) -> &'static str {
    match locale {
        Locale::En => CORE_EN,
        Locale::ZhCn => CORE_ZH_CN,
    }
}

/// The core catalog files for `locale` as `(file name, TOML source)`, main file
/// first (per-file parity and layout tests).
pub fn core_catalog_files(locale: Locale) -> &'static [(&'static str, &'static str)] {
    match locale {
        Locale::En => CORE_EN_FILES,
        Locale::ZhCn => CORE_ZH_CN_FILES,
    }
}

/// Layered message catalog for one display locale with English fallback.
#[derive(Clone, Debug)]
pub struct Catalog {
    locale: Locale,
    selected: HashMap<String, String>,
    english: HashMap<String, String>,
}

impl Catalog {
    /// A catalog for `locale` preloaded with the core keys.
    pub fn new(locale: Locale) -> Self {
        let mut c = Catalog {
            locale,
            selected: HashMap::new(),
            english: HashMap::new(),
        };
        // The core catalogs are compiled in and covered by tests; failure is a bug.
        c.add_toml(Locale::En, CORE_EN)
            .expect("core en catalog parses");
        c.add_toml(Locale::ZhCn, CORE_ZH_CN)
            .expect("core zh-CN catalog parses");
        c
    }

    /// Display locale.
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// Merge a TOML catalog for `locale`. Entries for other locales than the display
    /// locale and English are ignored. Later layers override earlier ones.
    pub fn add_toml(&mut self, locale: Locale, src: &str) -> Result<()> {
        let flat = flatten_toml(src)?;
        if locale == Locale::En {
            self.english.extend(flat.clone());
        }
        if locale == self.locale {
            self.selected.extend(flat);
        }
        Ok(())
    }

    /// Raw template for `key` in the display locale, else English.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.selected
            .get(key)
            .or_else(|| self.english.get(key))
            .map(String::as_str)
    }

    /// Translate a key with a fallback and no arguments.
    pub fn tr(&self, key: &str, fallback: &str) -> String {
        self.get(key).unwrap_or(fallback).to_owned()
    }

    /// Render a [`LocalizedText`] (catalog template, else its fallback; then args).
    pub fn text(&self, t: &LocalizedText) -> String {
        let template = self.get(&t.key).unwrap_or(&t.fallback);
        if t.nested.is_empty() {
            return interpolate(template, &t.args);
        }
        let mut args = t.args.clone();
        args.extend(t.nested.iter().map(|(n, nt)| (n.clone(), self.text(nt))));
        interpolate(template, &args)
    }
}

/// Parse a TOML catalog into a flat `dotted.key -> text` map.
pub fn flatten_toml(src: &str) -> Result<HashMap<String, String>> {
    let table: toml::Table = src.parse().map_err(|e| Error::parse("i18n catalog", e))?;
    let mut out = HashMap::new();
    flatten_into(&mut out, "", &table)?;
    Ok(out)
}

fn flatten_into(out: &mut HashMap<String, String>, prefix: &str, t: &toml::Table) -> Result<()> {
    for (k, v) in t {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::String(s) => {
                out.insert(key, s.clone());
            }
            toml::Value::Table(inner) => flatten_into(out, &key, inner)?,
            other => {
                return Err(Error::parse(
                    "i18n catalog",
                    format!("{key}: expected string or table, got {}", other.type_str()),
                ));
            }
        }
    }
    Ok(())
}

/// Replace `{name}` placeholders with argument values; unknown placeholders stay.
///
/// Plural selector: `{name|one|other}` becomes `one` when the argument `name` is
/// exactly `1` and `other` otherwise (English needs it; Chinese templates simply do
/// not use it). Example: `"{n} {n|conversation is|conversations are} running"`.
/// The two forms are plain text (no nested braces or `|`).
pub fn interpolate(template: &str, args: &[(String, String)]) -> String {
    if args.is_empty() || !template.contains('{') {
        return template.to_owned();
    }
    let mut out = template.to_owned();
    if out.contains('|') {
        out = select_plurals(&out, args);
    }
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Resolve `{name|one|other}` selectors whose `name` is a known argument.
fn select_plurals(template: &str, args: &[(String, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let Some(close) = tail.find('}') else {
            out.push_str(tail);
            return out;
        };
        let inner = &tail[1..close];
        let mut parts = inner.splitn(3, '|');
        let (name, one, other) = (parts.next(), parts.next(), parts.next());
        match (name, one, other) {
            (Some(name), Some(one), Some(other)) if !inner.contains('{') => {
                match args.iter().find(|(n, _)| n == name) {
                    Some((_, v)) => out.push_str(if v.trim() == "1" { one } else { other }),
                    None => out.push_str(&tail[..=close]),
                }
            }
            _ => out.push_str(&tail[..=close]),
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}
