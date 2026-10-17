//! CLI-only catalog layered on the shared core and switch wording.

use std::collections::HashMap;
use std::sync::LazyLock;
use superba_core::{Catalog, Locale, LocalizedText, i18n::flatten_toml};

/// English CLI catalog, also the source of fallback text and clap help.
pub const EN: &str = include_str!("../locales/en.toml");
/// Simplified Chinese CLI catalog.
pub const ZH_CN: &str = include_str!("../locales/zh-CN.toml");

static ENGLISH: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    // Embedded catalogs are parsed and checked for parity by the integration tests.
    flatten_toml(EN).expect("the embedded English CLI catalog is valid")
});

/// English template for clap's static help metadata.
pub fn english(key: &str) -> &'static str {
    ENGLISH.get(key).map(String::as_str).unwrap_or("")
}

/// Localizable CLI text. Call sites use literal keys, checked against both catalogs.
pub fn text(key: &str) -> LocalizedText {
    LocalizedText::new(key, english(key))
}

pub(crate) fn name_id(name: LocalizedText, id: impl ToString) -> LocalizedText {
    text("cli.format.name_id")
        .arg_text("name", name)
        .arg("id", id)
}

pub(crate) fn line_label(
    sites: &[superba_core::Site],
    site_id: Option<&superba_core::SiteId>,
    id: &superba_core::LineId,
) -> LocalizedText {
    sites
        .iter()
        .filter(|site| site_id.is_none_or(|id| &site.id == id))
        .find_map(|site| site.line(id))
        .map(|line| name_id(line.name.clone(), id))
        .unwrap_or_else(|| LocalizedText::new("", id.to_string()))
}

pub(crate) fn destination_label(plan: &superba_core::SwitchPlan) -> LocalizedText {
    let label = plan.to.label.clone();
    if let Some(id) = &plan.to.line_id {
        if let Some((_, name)) = label.nested.iter().find(|(key, _)| key == "line") {
            let name = name_id(name.clone(), id);
            return label.arg_text("line", name);
        }
        if let Some((_, name)) = label.args.iter().find(|(key, _)| key == "line") {
            let name = name_id(LocalizedText::new("", name), id);
            return label.arg_text("line", name);
        }
    }
    label
}

/// Catalog containing all domain and CLI keys.
pub fn catalog(locale: Locale) -> Catalog {
    let mut c = superba_switch::messages::catalog(locale);
    // Both embedded catalogs are parsed by the parity test; failures are invariants.
    c.add_toml(Locale::En, EN)
        .expect("the embedded English CLI catalog is valid");
    c.add_toml(Locale::ZhCn, ZH_CN)
        .expect("the embedded Chinese CLI catalog is valid");
    c
}

/// Translate a clap description from the same English catalog that created it.
pub(crate) fn translate_help(catalog: &Catalog, english: &str) -> String {
    ENGLISH
        .iter()
        .find(|(_, value)| value.as_str() == english)
        .map(|(key, _)| catalog.text(&text(key)))
        .unwrap_or_else(|| english.to_owned())
}
