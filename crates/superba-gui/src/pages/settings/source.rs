//! Where the settings model comes from: the settings catalog (official English
//! text, stages, defaults, versions, human overlays and categories), the edited
//! profile's managed settings, the local `config.toml` (read only) and the Codex
//! version of every target. Also saves edits into the profile; applying them to
//! Codex goes through the switch flow.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use superba_catalog::{CatalogStore, ConfigKey, KeyKind, MatchKind, Os, VersionCatalog};
use superba_core::settings::{CodexSettings, SettingEntry, SettingValue, reserved_owner};
use superba_core::{
    CodexVersion, Error, LocalizedText, Overview, Profile, ProfileId, ProfileSet, Result, SiteKind,
    SwitchService, catalog,
};

use super::model::{
    Availability, Category, Change, Entry, EnumOption, Explanation, Item, Kind, Legacy, Model,
    Pending, Stage, TargetInfo, TargetKey, Value,
};
use crate::pages::Cx;
use crate::runtime::ReadTask;

/// Everything the loader needs from the app (cloned into the task).
#[derive(Clone, Debug, Default)]
pub struct Input {
    /// Overview (targets, versions, applied profile, Codex home).
    pub overview: Option<Overview>,
    /// Profiles and custom sites.
    pub profiles: Option<ProfileSet>,
    /// Profile to edit (`None` = the applied key-based profile, else the first one).
    pub profile: Option<ProfileId>,
}

/// Start loading the model in the background.
pub fn load_task(cx: &Cx<'_>, profile: Option<ProfileId>) -> ReadTask<Model> {
    let input = Input {
        overview: cx.data.overview.value.clone(),
        profiles: cx.data.profiles.value.clone(),
        profile,
    };
    // Catalog parsing and `codex features list` (up to 10 s) block: keep them off
    // the async workers.
    crate::runtime::task(async move {
        tokio::task::spawn_blocking(move || load(input))
            .await
            .map_err(|e| Error::other(format!("settings loader: {e}")))?
    })
}

/// Load the catalog the switch service filters with
/// ([`superba_switch::settings_catalog`]: the built-in snapshot, or a verified
/// download) and build the model.
pub fn load(input: Input) -> Result<Model> {
    let ctx = superba_switch::detect_context().ok();
    let cache = ctx.as_ref().map(|ctx| ctx.data_dir.cache_dir());
    let loaded = match &ctx {
        Some(ctx) => superba_switch::settings_catalog(&ctx.data_dir),
        None => superba_catalog::SnapshotSource::builtin()
            .ok()
            .map(|source| superba_switch::SettingsCatalog {
                source: Arc::new(source),
                notices: Vec::new(),
            }),
    }
    .ok_or_else(|| Error::other("the settings catalog cannot be loaded"))?;
    let config = read_config(input.overview.as_ref());
    let mut model = build(
        loaded.source.store(),
        &input,
        config.as_ref().ok(),
        Os::current(),
    )?;
    if let Err(e) = config {
        model.config_error = Some(e);
        model.read_only_reason = Some("codex_settings.read_only_config");
    } else if let (Some(binary), Some(cache)) = (runtime_binary(input.overview.as_ref()), &cache) {
        let config_text = input
            .overview
            .as_ref()
            .and_then(|o| std::fs::read_to_string(o.codex_home.join("config.toml")).ok());
        match features_list(&binary.0, config_text.as_deref(), cache) {
            Ok(listed) => apply_runtime(&mut model, &binary.1, &listed),
            Err(e) => tracing::debug!("codex features list failed: {e}"),
        }
    }
    model
        .notes
        .extend(loaded.notices.into_iter().map(|n| n.text));
    Ok(model)
}

/// The Codex binary whose `features list` is compared with the catalog: the
/// app's bundled Codex, else the CLI first on PATH. Also returns its target label.
fn runtime_binary(overview: Option<&Overview>) -> Option<(std::path::PathBuf, String)> {
    let o = overview?;
    if let Some(path) = o.app.installs.iter().find_map(|i| i.bundled_codex.clone()) {
        return Some((path, "App".into()));
    }
    o.cli
        .binaries
        .iter()
        .min_by_key(|b| b.path_rank.unwrap_or(usize::MAX))
        .map(|b| (b.path.clone(), "CLI".into()))
}

/// Run `codex features list` read-only: Codex gets a private `CODEX_HOME` under
/// Superba's cache dir holding only a copy of `config.toml` (never the user's
/// home, which Codex could write to), at most 10 s, and the directory is removed
/// afterwards.
pub fn features_list(
    binary: &std::path::Path,
    config: Option<&str>,
    cache_dir: &std::path::Path,
) -> Result<superba_core::settings::FeaturesList> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let home = cache_dir.join(format!("features-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| Error::io(&home, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
    }
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(home.clone());
    if let Some(text) = config {
        let file = home.join("config.toml");
        std::fs::write(&file, text).map_err(|e| Error::io(&file, &e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
        }
    }
    let mut child = superba_core::process::command(binary)
        .args(["features", "list"])
        .env("CODEX_HOME", &home)
        .env_remove("CODEX_SQLITE_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::io(binary, &e))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout {
                    what: "codex features list".into(),
                });
            }
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| Error::io(binary, &e))?;
    if !output.status.success() {
        return Err(Error::other(format!(
            "codex features list exited with {}",
            output.status
        )));
    }
    let list =
        superba_core::settings::FeaturesList::parse(&String::from_utf8_lossy(&output.stdout));
    if list.entries.is_empty() {
        return Err(Error::other("codex features list printed no features"));
    }
    Ok(list)
}

/// Record what Codex reports: the effective state of each feature, and a note
/// when the catalog and Codex disagree on which features exist or their stage.
pub fn apply_runtime(
    model: &mut Model,
    target: &str,
    listed: &superba_core::settings::FeaturesList,
) {
    use superba_core::settings::FeatureStage;
    let mut unknown = 0usize;
    let mut stage_differs = 0usize;
    for l in &listed.entries {
        let path = format!("features.{}", l.key);
        match model.entries.iter_mut().find(|e| e.path == path) {
            Some(e) => {
                e.effective = Some(l.enabled);
                let stage = l.stage.map(|s| match s {
                    FeatureStage::Stable => Stage::Stable,
                    FeatureStage::Experimental => Stage::Experimental,
                    FeatureStage::Deprecated => Stage::Deprecated,
                    FeatureStage::Removed => Stage::Removed,
                    _ => Stage::UnderDevelopment,
                });
                if let Some(stage) = stage
                    && stage != e.stage
                {
                    stage_differs += 1;
                    e.stage = stage;
                }
            }
            None => unknown += 1,
        }
    }
    if unknown + stage_differs > 0 {
        model.notes.push(
            LocalizedText::new(
                "codex_settings.note.runtime_differs",
                "{target} Codex reports {n} features differently from the settings catalog; what Codex reports is shown.",
            )
            .arg("target", target)
            .arg("n", unknown + stage_differs),
        );
    }
}

/// The local `config.toml` as a TOML table (empty when absent).
fn read_config(overview: Option<&Overview>) -> std::result::Result<toml::Table, String> {
    let Some(o) = overview else {
        return Ok(toml::Table::new());
    };
    if let Some(e) = o.config.as_ref().and_then(|c| c.parse_error.clone()) {
        return Err(e);
    }
    match std::fs::read_to_string(o.codex_home.join("config.toml")) {
        Ok(text) => text.parse::<toml::Table>().map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Value at a dotted path of a TOML table.
fn toml_at<'a>(table: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut segments = path.split('.');
    let mut value = table.get(segments.next()?)?;
    for seg in segments {
        value = value.as_table()?.get(seg)?;
    }
    Some(value)
}

fn from_toml(v: &toml::Value) -> Value {
    match v {
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Integer(i) => Value::Int(*i),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::String(s) => Value::Str(s.clone()),
        // Arrays and tables in the representation profiles store, so a file value
        // and the same value saved by the page compare equal (and a table read
        // from a `[section]` becomes an inline table).
        other => Value::Other(
            SettingValue::from_toml(other)
                .map(|v| v.to_toml_repr())
                .unwrap_or_else(|| other.to_string()),
        ),
    }
}

fn from_json(v: &serde_json::Value) -> Option<Value> {
    Some(match v {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64()?),
        },
        serde_json::Value::String(s) => Value::Str(s.clone()),
        other => Value::Other(other.to_string()),
    })
}

/// Page value of a profile setting.
pub fn from_setting(v: &SettingValue) -> Value {
    match v {
        SettingValue::Bool(b) => Value::Bool(*b),
        SettingValue::Integer(i) => Value::Int(*i),
        SettingValue::Float(f) => Value::Float(*f),
        SettingValue::String(s) => Value::Str(s.clone()),
        other => Value::Other(other.to_toml_repr()),
    }
}

/// Profile setting of a page value (`Other` is never produced by the editors).
pub fn to_setting(v: &Value) -> Option<SettingValue> {
    Some(match v {
        Value::Bool(b) => SettingValue::Bool(*b),
        Value::Int(i) => SettingValue::Integer(*i),
        Value::Float(f) => SettingValue::float(*f)?,
        Value::Str(s) => SettingValue::String(s.clone()),
        Value::Other(repr) => SettingValue::from_toml_repr(repr)?,
    })
}

/// The profile whose settings the page edits: the requested one, else the applied
/// key-based profile, else the first key-based profile.
pub fn pick_profile<'a>(
    profiles: &'a ProfileSet,
    overview: Option<&Overview>,
    wanted: Option<&ProfileId>,
) -> Option<&'a Profile> {
    let keyed = |p: &&Profile| p.builtin.is_none() && p.auth_mode.uses_key();
    let applied = overview
        .and_then(|o| o.active.as_ref())
        .and_then(|a| a.profile_id.as_ref());
    wanted
        .and_then(|id| profiles.profile(id))
        .filter(keyed)
        .or_else(|| applied.and_then(|id| profiles.profile(id)).filter(keyed))
        .or_else(|| profiles.profiles.iter().find(keyed))
}

fn stage_of(s: superba_catalog::Stage) -> Stage {
    match s {
        superba_catalog::Stage::Stable => Stage::Stable,
        superba_catalog::Stage::Experimental => Stage::Experimental,
        superba_catalog::Stage::Deprecated => Stage::Deprecated,
        superba_catalog::Stage::Removed => Stage::Removed,
        _ => Stage::UnderDevelopment,
    }
}

fn number(n: &Option<serde_json::Number>) -> Option<f64> {
    n.as_ref().and_then(serde_json::Number::as_f64)
}

fn kind_of(store: &CatalogStore, catalog: &VersionCatalog, key: &ConfigKey) -> Kind {
    match key.kind {
        KeyKind::Boolean => Kind::Bool,
        KeyKind::Enum => Kind::Enum {
            options: key
                .enum_values
                .iter()
                .map(|v| {
                    let ex = store.explain_value(catalog, &key.path, &v.value);
                    EnumOption {
                        value: v.value.clone(),
                        en: ex.official_en.or(ex.en_note),
                        zh: ex.zh,
                    }
                })
                .collect(),
            table_form: key.enum_has_table_form,
        },
        KeyKind::Integer => Kind::Integer {
            min: number(&key.minimum).map(|x| x.ceil() as i64),
            max: number(&key.maximum).map(|x| x.floor() as i64),
        },
        KeyKind::Number => Kind::Number {
            min: number(&key.minimum),
            max: number(&key.maximum),
        },
        KeyKind::String => Kind::Text {
            path: is_path_key(key),
        },
        KeyKind::Array => match key.items.as_deref() {
            Some("string") => Kind::List {
                item: Item::Text {
                    path: key.path.ends_with("_roots") || key.path.ends_with("_dirs"),
                },
            },
            Some("AbsolutePathBuf" | "PathBuf") => Kind::List {
                item: Item::Text { path: true },
            },
            Some("integer") => Kind::List {
                item: Item::Integer,
            },
            Some("number") => Kind::List { item: Item::Number },
            _ => Kind::ReadOnly,
        },
        KeyKind::Map => match map_value_item(catalog, key) {
            Some(value) => Kind::Map { value },
            None => Kind::ReadOnly,
        },
        _ => Kind::ReadOnly,
    }
}

fn is_path_key(key: &ConfigKey) -> bool {
    key.path.ends_with("_file")
        || key.path.ends_with("_path")
        || key.path.ends_with("_dir")
        || key.format.as_deref() == Some("path")
}

/// The scalar value type of a map (`<path>.<name>` is a string, number, boolean
/// or enum); `None` for maps of tables.
fn map_value_item(catalog: &VersionCatalog, key: &ConfigKey) -> Option<Item> {
    let child = catalog.key(&format!("{}.<name>", key.path))?;
    Some(match child.kind {
        KeyKind::String => Item::Text {
            path: is_path_key(child),
        },
        KeyKind::Integer => Item::Integer,
        KeyKind::Number => Item::Number,
        KeyKind::Boolean => Item::Bool,
        KeyKind::Enum if !child.enum_has_table_form => {
            Item::Enum(child.enum_values.iter().map(|v| v.value.clone()).collect())
        }
        _ => return None,
    })
}

fn explanation(ex: superba_catalog::Explanation) -> (Option<String>, Option<String>, Explanation) {
    let stale = ex.zh_state == superba_catalog::ZhState::Stale;
    (
        ex.official_name,
        ex.zh_name,
        Explanation {
            en: ex.official_en,
            en_note: ex.en_note,
            zh: ex.zh,
            zh_stale: stale,
        },
    )
}

/// Config paths that are listed (leaves Codex reads directly, and maps of
/// scalars as one key-value setting; map entries, per-profile copies and union
/// variants are not settings of their own).
fn listed(catalog: &VersionCatalog, key: &ConfigKey) -> bool {
    let depth = key.path.split('.').count();
    (key.kind.is_leaf() || (key.kind == KeyKind::Map && map_value_item(catalog, key).is_some()))
        && !key.under_map
        && key.variant_of.is_empty()
        && !key.path.starts_with("profiles.")
        && key.path != "profiles"
        // `features.<key>` comes from the registry; its sub-keys come from here.
        && !(key.path.starts_with("features.") && depth == 2)
}

/// Large families kept behind Advanced.
fn advanced_only(path: &str) -> bool {
    ["tui.keymap.", "otel.", "notice."]
        .iter()
        .any(|p| path.starts_with(p))
}

/// One catalog per local consumer and per remote host, plus the catalog the
/// list is built from.
struct Targets {
    infos: Vec<TargetInfo>,
    catalogs: Vec<(TargetKey, Option<Arc<VersionCatalog>>)>,
    hosts: Vec<TargetInfo>,
    host_catalogs: Vec<(TargetKey, Option<Arc<VersionCatalog>>)>,
    primary: Arc<VersionCatalog>,
}

fn targets(store: &CatalogStore, overview: Option<&Overview>) -> Result<Targets> {
    type Found = Vec<(TargetKey, String, Option<CodexVersion>)>;
    type Catalogs = Vec<(TargetKey, Option<Arc<VersionCatalog>>)>;
    let mut local: Found = Vec::new();
    let mut remote: Found = Vec::new();
    if let Some(o) = overview {
        if let Some(install) = o.app.installs.iter().find(|i| i.bundled_codex.is_some()) {
            local.push((
                TargetKey::App,
                "App".into(),
                install.bundled_codex_version.clone(),
            ));
        }
        let cli = o
            .cli
            .binaries
            .iter()
            .min_by_key(|b| b.path_rank.unwrap_or(usize::MAX));
        if let Some(b) = cli {
            local.push((TargetKey::Cli, "CLI".into(), b.version.clone()));
        }
        // Only hosts the desktop app is connected to (never ssh-config entries).
        for r in o.remotes.iter().filter(|r| r.host.is_app_known()) {
            remote.push((
                TargetKey::Remote(r.host.target_id.to_string()),
                r.host.label.clone(),
                r.state.as_ref().and_then(|s| s.codex_version.clone()),
            ));
        }
    }
    let resolve = |found: Found| -> Result<(Vec<TargetInfo>, Catalogs)> {
        let mut infos = Vec::new();
        let mut catalogs = Vec::new();
        for (key, label, version) in found {
            let matched = match &version {
                Some(v) => Some(
                    store
                        .for_version(v)
                        .map_err(|e| Error::other(e.to_string()))?,
                ),
                None => None,
            };
            infos.push(TargetInfo {
                key: key.clone(),
                label,
                version: version.as_ref().map(ToString::to_string),
                approximate: matched.as_ref().is_some_and(|m| {
                    matches!(m.kind, MatchKind::Nearest | MatchKind::OlderThanCatalog)
                }),
                catalog_outdated: matched
                    .as_ref()
                    .is_some_and(|m| m.kind == MatchKind::NewerThanCatalog),
            });
            catalogs.push((key, matched.map(|m| m.catalog)));
        }
        Ok((infos, catalogs))
    };
    let (infos, catalogs) = resolve(local)?;
    let (hosts, host_catalogs) = resolve(remote)?;
    let primary = match catalogs.iter().find_map(|(_, c)| c.clone()) {
        Some(c) => c,
        None => store.latest().map_err(|e| Error::other(e.to_string()))?,
    };
    Ok(Targets {
        infos,
        catalogs,
        hosts,
        host_catalogs,
        primary,
    })
}

/// First catalog version that lists each config path (features by their path),
/// for paths added after the oldest catalog version (older ones get no badge).
fn first_seen(store: &CatalogStore) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut versions: Vec<&CodexVersion> = store.versions().collect();
    versions.sort();
    let oldest = versions.first().map(|v| v.to_string());
    for v in versions {
        let Ok(Some(c)) = store.exact(v) else {
            continue;
        };
        let paths = c
            .features
            .iter()
            .map(|f| f.config_path.clone())
            .chain(c.config_keys.iter().map(|k| k.path.clone()));
        for path in paths {
            out.entry(path).or_insert_with(|| v.to_string());
        }
    }
    out.retain(|_, v| Some(&*v) != oldest.as_ref());
    out
}

/// Build the model from a catalog store (pure apart from the store's lazy parsing).
pub fn build(
    store: &CatalogStore,
    input: &Input,
    config: Option<&toml::Table>,
    os: Os,
) -> Result<Model> {
    let overview = input.overview.as_ref();
    let t = targets(store, overview)?;
    let empty = ProfileSet::default();
    let profiles = input.profiles.as_ref().unwrap_or(&empty);
    let profile = pick_profile(profiles, overview, input.profile.as_ref());
    let managed: CodexSettings = profile
        .map(|p| p.codex_settings.clone())
        .unwrap_or_default();
    let applied = profile.is_some_and(|p| {
        overview
            .and_then(|o| o.active.as_ref())
            .and_then(|a| a.profile_id.as_ref())
            == Some(&p.id)
    });
    let krill = profile.is_some_and(|p| {
        p.site_id.as_str() == catalog::KRILL_SITE_ID
            || profiles
                .custom_sites
                .iter()
                .any(|s| s.id == p.site_id && s.kind == SiteKind::Krill)
    });
    let recommended = if krill {
        CodexSettings::krill_recommended()
    } else {
        CodexSettings::new()
    };
    let curation = store.curation();
    let file = |path: &str| config.and_then(|c| toml_at(c, path)).map(from_toml);
    let support = |catalogs: &[(TargetKey, Option<Arc<VersionCatalog>>)],
                   has: &dyn Fn(&VersionCatalog) -> bool|
     -> Vec<(TargetKey, Availability)> {
        catalogs
            .iter()
            .map(|(k, c)| {
                let a = match c {
                    Some(c) if has(c) => Availability::Yes,
                    Some(_) => Availability::No,
                    None => Availability::Unknown,
                };
                (k.clone(), a)
            })
            .collect()
    };
    let availability = |has: &dyn Fn(&VersionCatalog) -> bool| support(&t.catalogs, has);
    let host_support = |has: &dyn Fn(&VersionCatalog) -> bool| support(&t.host_catalogs, has);
    let common: BTreeMap<&str, usize> = curation
        .common()
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i))
        .collect();
    let current = |path: &str| -> (Option<Value>, bool) {
        match managed.get(path) {
            Some(SettingEntry::Set(v)) => (Some(from_setting(v)), true),
            Some(SettingEntry::Unset) => (None, true),
            None => (file(path), false),
        }
    };
    let platform = |path: &str| {
        curation.platforms_of(path).map(|oses| {
            oses.iter()
                .map(|o| match o {
                    Os::Macos => "macOS",
                    Os::Linux => "Linux",
                    Os::Windows => "Windows",
                })
                .collect::<Vec<_>>()
                .join(" / ")
        })
    };

    // Union of every target's catalog, primary first: an entry listed only by an
    // older or newer target still shows (with its availability).
    let mut all: Vec<Arc<VersionCatalog>> = vec![t.primary.clone()];
    for (_, c) in &t.catalogs {
        if let Some(c) = c
            && !all.iter().any(|a| a.codex_version == c.codex_version)
        {
            all.push(c.clone());
        }
    }
    let first_seen = first_seen(store);
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for catalog in &all {
        for f in &catalog.features {
            let path = f.config_path.clone();
            if !seen.insert(path.clone()) {
                continue;
            }
            let (name, name_zh, text) = explanation(store.explain_feature(catalog, &f.key));
            let (current, is_managed) = current(&path);
            let key = f.key.clone();
            let since = first_seen.get(&path).cloned();
            entries.push(Entry {
                id: format!("feature:{key}"),
                category: curation.category_of_feature(&key).to_owned(),
                stage: stage_of(f.stage_on(os)),
                kind: Kind::Bool,
                text,
                default: Some(Value::Bool(f.default_on(os))),
                file_value: file(&path),
                current,
                effective: None,
                since,
                availability: availability(&|c: &VersionCatalog| c.feature(&key).is_some()),
                host_support: host_support(&|c: &VersionCatalog| c.feature(&key).is_some()),
                common: common.get(path.as_str()).copied(),
                legacy: None,
                recommended: recommended
                    .get(&path)
                    .and_then(SettingEntry::value)
                    .map(from_setting),
                platform: platform(&path),
                managed: is_managed,
                advanced_only: false,
                owner: reserved_owner(&path),
                name,
                name_zh,
                path,
            });
        }
        for k in catalog.config_keys.iter().filter(|k| listed(catalog, k)) {
            if !seen.insert(k.path.clone()) {
                continue;
            }
            let (_, name_zh, text) = explanation(store.explain_key(catalog, &k.path));
            let (current, is_managed) = current(&k.path);
            let path = k.path.clone();
            let since = first_seen.get(&path).cloned();
            entries.push(Entry {
                id: format!("config:{path}"),
                name: None,
                name_zh,
                common: common.get(path.as_str()).copied(),
                host_support: host_support(&|c: &VersionCatalog| c.key(&path).is_some()),
                category: curation.category_of_key(&path).to_owned(),
                stage: if k.deprecated_hint {
                    Stage::Deprecated
                } else {
                    Stage::Stable
                },
                kind: kind_of(store, catalog, k),
                text,
                default: k.default.as_ref().and_then(from_json),
                file_value: file(&path),
                current,
                effective: None,
                since,
                availability: availability(&|c: &VersionCatalog| c.key(&path).is_some()),
                legacy: None,
                recommended: recommended
                    .get(&path)
                    .and_then(SettingEntry::value)
                    .map(from_setting),
                platform: platform(&path),
                managed: is_managed,
                advanced_only: advanced_only(&path),
                owner: reserved_owner(&path),
                path,
            });
        }
    }

    // Legacy keys: only those present in the file or the profile (otherwise noise).
    let legacy = &t.primary.legacy;
    let mut legacy_paths: Vec<(String, Legacy)> = Vec::new();
    for a in &legacy.feature_aliases {
        legacy_paths.push((
            format!("features.{}", a.legacy_key),
            Legacy {
                canonical: Some(format!("features.{}", a.canonical)),
                canonical_value: None,
                ignored: false,
            },
        ));
    }
    for a in &legacy.top_level_aliases {
        legacy_paths.push((
            a.legacy_key.clone(),
            Legacy {
                canonical: Some(format!("features.{}", a.canonical)),
                canonical_value: None,
                ignored: false,
            },
        ));
    }
    for k in &legacy.ignored_feature_keys {
        legacy_paths.push((
            format!("features.{k}"),
            Legacy {
                canonical: None,
                canonical_value: None,
                ignored: true,
            },
        ));
    }
    for k in &legacy.notice_feature_keys {
        legacy_paths.push((
            format!("features.{k}"),
            Legacy {
                canonical: None,
                canonical_value: None,
                ignored: false,
            },
        ));
    }
    // Hand-written semantic migrations (curation.toml) win over generated rules,
    // when the configured value matches the rule's `when`.
    for m in &curation.migrations {
        let (value, _) = current(&m.path);
        let when = m.when.as_ref().map(from_toml);
        if when.is_some() && value.is_some() && when != value {
            continue;
        }
        let rule = Legacy {
            canonical: Some(m.canonical.clone()),
            canonical_value: Some(from_toml(&m.value)),
            ignored: false,
        };
        match legacy_paths.iter_mut().find(|(p, _)| p == &m.path) {
            Some((_, r)) => *r = rule,
            None => legacy_paths.push((m.path.clone(), rule)),
        }
    }
    for (path, rule) in legacy_paths {
        let (current, is_managed) = current(&path);
        if current.is_none() && !is_managed {
            continue;
        }
        let file_value = file(&path);
        if let Some(e) = entries.iter_mut().find(|e| e.path == path) {
            e.legacy = Some(rule);
            continue;
        }
        entries.push(Entry {
            id: format!("config:{path}"),
            name: None,
            name_zh: None,
            category: curation.category_of_key(&path).to_owned(),
            stage: Stage::Deprecated,
            kind: Kind::Bool,
            text: Explanation::default(),
            default: None,
            file_value,
            current,
            effective: None,
            since: None,
            availability: Vec::new(),
            legacy: Some(rule),
            recommended: None,
            platform: None,
            managed: is_managed,
            advanced_only: false,
            owner: None,
            common: None,
            host_support: Vec::new(),
            path,
        });
    }

    // Categories in curation order, then any others that have entries.
    let mut categories: Vec<Category> = curation
        .categories
        .iter()
        .map(|c| Category {
            id: c.id.clone(),
            label: c.label.clone(),
            label_zh: None,
        })
        .collect();
    for id in entries
        .iter()
        .map(|e| e.category.clone())
        .collect::<BTreeSet<_>>()
    {
        if !categories.iter().any(|c| c.id == id) {
            categories.push(Category {
                label: id.clone(),
                id,
                label_zh: None,
            });
        }
    }
    let order: BTreeMap<&str, usize> = categories
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    entries.sort_by(|a, b| {
        let ca = order
            .get(a.category.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        let cb = order
            .get(b.category.as_str())
            .copied()
            .unwrap_or(usize::MAX);
        ca.cmp(&cb)
            .then_with(|| a.is_feature().cmp(&b.is_feature()))
            .then_with(|| a.path.cmp(&b.path))
    });
    let categories = categories
        .into_iter()
        .filter(|c| entries.iter().any(|e| e.category == c.id))
        .collect();

    let mut notes: Vec<LocalizedText> = Vec::new();
    for (info, (_, c)) in t.infos.iter().zip(&t.catalogs) {
        if c.is_none() {
            notes.push(
                LocalizedText::new(
                    "codex_settings.note.version_unknown",
                    "The Codex version of {target} is unknown; availability is not shown for it.",
                )
                .arg("target", &info.label),
            );
        }
    }
    Ok(Model {
        profile_id: profile.map(|p| p.id.clone()),
        profile_applied: applied,
        read_only_reason: profile.is_none().then_some("codex_settings.no_profile"),
        notes,
        targets: t.infos,
        hosts: t.hosts,
        categories,
        entries,
        catalog_version: Some(t.primary.codex_version.clone()),
        profile: profile.map(|p| p.name.clone()),
        config_error: None,
    })
}

/// The edited profile with `pending` written into its Codex settings.
pub fn updated_profile(profiles: &ProfileSet, model: &Model, pending: &Pending) -> Result<Profile> {
    let id = model
        .profile_id
        .as_ref()
        .ok_or_else(|| Error::invalid("no profile to save settings into"))?;
    let mut profile = profiles
        .profile(id)
        .cloned()
        .ok_or_else(|| Error::NotFound {
            what: id.to_string(),
        })?;
    for (path, change) in pending.iter() {
        let entry = match change {
            Change::Set(v) => SettingEntry::Set(
                to_setting(v).ok_or_else(|| Error::invalid(format!("invalid value for {path}")))?,
            ),
            Change::Unset => SettingEntry::Unset,
        };
        profile.codex_settings.insert(path, entry)?;
    }
    Ok(profile)
}

/// What saving the pending edits does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterSave {
    /// The profile is applied: plan a switch to it (the switch flow previews the
    /// configuration changes before anything is written).
    Apply(ProfileId),
    /// Saved only; used the next time the profile is applied.
    Saved,
}

/// Save the pending edits into the profile.
pub async fn save(
    service: Arc<dyn SwitchService>,
    model: Model,
    pending: Pending,
) -> Result<AfterSave> {
    let profiles = service.profile_set().await?;
    let profile = updated_profile(&profiles, &model, &pending)?;
    let id = profile.id.clone();
    service.save_profile(profile, None).await?;
    Ok(if model.profile_applied {
        AfterSave::Apply(id)
    } else {
        AfterSave::Saved
    })
}

/// Why the pending edits cannot be saved, as an i18n key (`None` = they can).
pub fn blocked_reason(cx: &Cx<'_>, model: &Model) -> Option<&'static str> {
    if cx.busy {
        return Some("codex_settings.pending.busy");
    }
    if model.config_error.is_some() {
        return Some("codex_settings.read_only_config");
    }
    if model.profile_id.is_none() {
        return Some("codex_settings.no_profile");
    }
    None
}
