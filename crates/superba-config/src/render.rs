//! Pure rendering (plan 9.1, 9.3, 9.4, 10.4): desired state + current bytes ->
//! new bytes. No IO; shared by the local engine and (through
//! `ConfigEngine::render`) by `superba-remote`.
//!
//! Ownership model: a key is *owned* while Superba's last apply set or removed it
//! (`last_written`). Keys Superba sets or removes for the new state become owned;
//! owned keys the new state does not manage are restored to the baseline value and
//! released. Keys that are not owned are never touched unless the new state
//! manages them. An owned key whose live value differs from what Superba wrote is
//! drift: it is changed only with an explicit [`KeyResolution`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use superba_core::codex::{PAT_RECOMMENDED_KEYS, PROVIDER_OPENAI, config_keys as k};
use superba_core::{
    AUTH_JSON_PATH, AuthChange, AuthShape, Baseline, BaselineExtras, CodexFiles, ConfigState,
    Error, FileChange, KeyChange, KeyResolution, KeySnapshot, LocalizedText, Notice,
    ProviderRestore, RenderedFiles, ResolvedMode, ResolvedProfile, Result, SecretBytes, Timestamp,
    is_valid_setting_path, reserved_owner,
};

use crate::authjson;
use crate::fsx::sha256_hex;
use crate::toml_keys::{ConfigDoc, reprs_equal, string_repr};

/// Owned routing keys (always managed by token and official modes).
pub(crate) const ROUTING_KEYS: [&str; 3] =
    [k::MODEL_PROVIDER, k::OPENAI_BASE_URL, k::CHATGPT_BASE_URL];
/// Model keys (owned only when a profile sets them).
pub(crate) const MODEL_KEYS: [&str; 2] = [k::MODEL, k::MODEL_REASONING_EFFORT];
/// v0.3 model keys of a profile's Models page (owned only when the resolved
/// profile sets them): the model catalog file, the "all models" context window
/// and compaction threshold, and the speed tier. Like [`MODEL_KEYS`] they are
/// captured lazily when first taken over and restored when released.
pub(crate) const MODEL_EXTRA_KEYS: [&str; 4] = [
    k::MODEL_CATALOG_JSON,
    k::MODEL_CONTEXT_WINDOW,
    k::MODEL_AUTO_COMPACT_TOKEN_LIMIT,
    k::SERVICE_TIER,
];

/// Every path Superba may own.
pub(crate) fn all_owned_paths() -> Vec<&'static str> {
    ROUTING_KEYS
        .iter()
        .chain(MODEL_KEYS.iter())
        .chain(MODEL_EXTRA_KEYS.iter())
        .copied()
        .chain(PAT_RECOMMENDED_KEYS.iter().map(|(p, _)| *p))
        .collect()
}

/// Model keys that follow the user's latest value instead of the takeover-time
/// value ([`MODEL_KEYS`] and [`MODEL_EXTRA_KEYS`]).
pub(crate) fn is_model_key(path: &str) -> bool {
    MODEL_KEYS.contains(&path) || MODEL_EXTRA_KEYS.contains(&path)
}

/// Paths snapshotted when the baseline is captured. Model keys are captured
/// lazily when a profile first takes them over (and refreshed whenever Superba
/// takes them over again), because the app itself changes `model`.
pub(crate) fn baseline_paths() -> Vec<&'static str> {
    all_owned_paths()
        .into_iter()
        .filter(|p| !is_model_key(p))
        .collect()
}

/// A point-in-time state recorded after a generation (engine-internal).
#[derive(Clone, Debug)]
pub(crate) struct GenerationSnapshot {
    /// Owned keys as written by that generation.
    pub keys: Vec<KeySnapshot>,
    /// auth.json right after it (`None` = absent).
    pub auth_json: Option<SecretBytes>,
    /// config.toml right after it (`Some(None)` = absent, `None` = not recorded).
    pub config_toml: Option<Option<SecretBytes>>,
}

/// Everything the renderer needs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inputs<'a> {
    pub files: &'a CodexFiles,
    pub desired: &'a ResolvedProfile,
    pub baseline: Option<&'a Baseline>,
    pub login_stash: Option<&'a SecretBytes>,
    pub original_files: Option<&'a CodexFiles>,
    pub last_written: &'a [KeySnapshot],
    pub generation: Option<&'a GenerationSnapshot>,
    /// The `auth.json` credential Superba wrote was replaced outside Superba
    /// (GH#5): it is changed only with an explicit resolution of
    /// [`superba_core::AUTH_JSON_PATH`].
    pub auth_drift: bool,
    /// Superba owns nothing (its last apply restored or handed off): paths, and
    /// [`superba_core::AUTH_JSON_PATH`], changed since that apply. Restores keep
    /// them unless the desired state answers `Overwrite`.
    pub released_changed: &'a [String],
}

/// Render result plus the ownership bookkeeping the engine persists.
#[derive(Clone, Debug, Default)]
pub(crate) struct Rendered {
    pub files: RenderedFiles,
    /// Owned key values after this render (the next `last_written`).
    pub owned: Vec<KeySnapshot>,
    /// Keys taken over by this render that were not owned before, with their value
    /// before the write (lazy baseline capture).
    pub newly_owned: Vec<KeySnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    Set(String),
    Remove,
    Restore,
}

fn note(severity: fn(LocalizedText) -> Notice, key: &str, fallback: &str) -> Notice {
    severity(LocalizedText::new(format!("config.note.{key}"), fallback))
}

fn opt_eq(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => reprs_equal(x, y),
        _ => false,
    }
}

/// `openai_base_url` as written to config.toml: without a trailing `/`, because
/// Codex appends `/responses`, `/models`, ... itself (a bare origin parsed by
/// `url::Url` always ends in `/`, which would yield `//responses`).
pub(crate) fn openai_base_url_value(url: &url::Url) -> String {
    let s = url.as_str();
    if url.query().is_none() && url.fragment().is_none() {
        s.trim_end_matches('/').to_owned()
    } else {
        s.to_owned()
    }
}

fn parse_config(files: &CodexFiles) -> Result<ConfigDoc> {
    match &files.config_toml {
        Some(b) => ConfigDoc::parse(b.expose()),
        None => Ok(ConfigDoc::empty()),
    }
}

/// Desired value per owned path.
fn targets(
    inp: &Inputs<'_>,
    doc: &ConfigDoc,
    notes: &mut Vec<Notice>,
) -> Result<BTreeMap<String, Target>> {
    let d = inp.desired;
    let mut t: BTreeMap<String, Target> = BTreeMap::new();
    let mut put = |p: &str, v: Target| {
        t.insert(p.to_owned(), v);
    };
    match &d.mode {
        ResolvedMode::Token {
            auth, endpoints, ..
        } => {
            if !auth.uses_key() {
                return Err(Error::invalid(format!("{auth:?} is not a token auth mode")));
            }
            put(k::MODEL_PROVIDER, Target::Set(string_repr(PROVIDER_OPENAI)));
            put(
                k::OPENAI_BASE_URL,
                Target::Set(string_repr(&openai_base_url_value(
                    &endpoints.openai_base_url,
                ))),
            );
            if auth.writes_chatgpt_base_url() {
                let url = endpoints.chatgpt_base_url.as_ref().ok_or_else(|| {
                    Error::invalid(format!("{auth:?} needs chatgpt_base_url in the endpoints"))
                })?;
                put(k::CHATGPT_BASE_URL, Target::Set(string_repr(url.as_str())));
            } else {
                put(k::CHATGPT_BASE_URL, Target::Restore);
            }
            // Krill's recommended keys are released unless the profile manages
            // them as settings (below).
            for (p, _) in PAT_RECOMMENDED_KEYS {
                put(p, Target::Restore);
            }
            // Codex settings of the profile (already filtered by the settings
            // engine).
            for (p, entry) in d.settings.iter() {
                if !is_valid_setting_path(p) || reserved_owner(p).is_some() {
                    return Err(Error::invalid(format!(
                        "{p} cannot be written as a Codex setting"
                    )));
                }
                put(
                    p,
                    match entry.to_toml_repr() {
                        Some(r) => Target::Set(r),
                        None => Target::Remove,
                    },
                );
            }
        }
        ResolvedMode::Official => {
            for p in ROUTING_KEYS {
                put(p, Target::Remove);
            }
            for (p, _) in PAT_RECOMMENDED_KEYS {
                put(p, Target::Restore);
            }
        }
        ResolvedMode::Original { .. } | ResolvedMode::Release { .. } => {
            // A hand-off restores what Superba still owns like a restore of the
            // original configuration that keeps provider `openai` (sessions stay
            // visible); what another tool changed is kept (planner resolutions).
            let provider = match &d.mode {
                ResolvedMode::Original { provider } => provider,
                _ => &ProviderRestore::KeepOpenai,
            };
            let b = inp.baseline.ok_or_else(|| Error::NotFound {
                what: "baseline (no original configuration was recorded)".into(),
            })?;
            for p in all_owned_paths() {
                put(p, Target::Restore);
            }
            let orig_provider = b.provider_key.as_deref().filter(|p| *p != PROVIDER_OPENAI);
            if let (ProviderRestore::KeepOpenai, Some(p)) = (provider, orig_provider) {
                // Plan 10.4 option 1: keep provider "openai" so every session stays
                // visible; route through the original provider's base_url.
                put(k::MODEL_PROVIDER, Target::Set(string_repr(PROVIDER_OPENAI)));
                let orig_base = b
                    .keys
                    .iter()
                    .find(|s| s.path == k::OPENAI_BASE_URL)
                    .and_then(|s| s.value.clone());
                if orig_base.is_none() {
                    match doc.get_str(&format!("{}.{p}.base_url", k::MODEL_PROVIDERS)) {
                        Some(url) => put(k::OPENAI_BASE_URL, Target::Set(string_repr(&url))),
                        None => notes.push(
                            note(
                                Notice::warning,
                                "provider_base_url_missing",
                                "The original provider table [model_providers.{provider}] has no base_url; openai_base_url is restored as it was.",
                            )
                            .with_arg("provider", p),
                        ),
                    }
                }
            }
        }
        ResolvedMode::Generation { generation } => {
            let snap = inp.generation.ok_or_else(|| Error::Unsupported {
                what: format!(
                    "restoring generation {generation} needs the local engine's snapshot (use ConfigEngine::plan)"
                ),
            })?;
            for p in all_owned_paths() {
                put(p, Target::Restore);
            }
            for s in &snap.keys {
                put(
                    &s.path,
                    match &s.value {
                        Some(v) => Target::Set(v.clone()),
                        None => Target::Remove,
                    },
                );
            }
        }
        other => {
            return Err(Error::Unsupported {
                what: format!("config mode {other:?}"),
            });
        }
    }
    if let Some(m) = &d.model.model {
        put(k::MODEL, Target::Set(string_repr(m)));
    }
    if let Some(e) = &d.model.reasoning_effort {
        put(k::MODEL_REASONING_EFFORT, Target::Set(string_repr(e)));
    }
    // v0.3 model keys: written exactly when the resolved profile sets them (the
    // switch decides, e.g. Standard as `service_tier = "default"` for key-based
    // profiles); released ones are restored to the user's value like `model`.
    if let Some(path) = &d.model_catalog {
        let path = path.to_str().ok_or_else(|| {
            Error::invalid(format!(
                "{} is not valid UTF-8: {}",
                k::MODEL_CATALOG_JSON,
                path.display()
            ))
        })?;
        put(k::MODEL_CATALOG_JSON, Target::Set(string_repr(path)));
    }
    if let Some(n) = d.model.context_window {
        put(k::MODEL_CONTEXT_WINDOW, Target::Set(n.to_string()));
    }
    if let Some(n) = d.model.auto_compact {
        put(
            k::MODEL_AUTO_COMPACT_TOKEN_LIMIT,
            Target::Set(n.to_string()),
        );
    }
    if let Some(tier) = d.model.speed {
        put(
            k::SERVICE_TIER,
            Target::Set(string_repr(tier.config_value())),
        );
    }
    Ok(t)
}

trait WithArg {
    fn with_arg(self, name: &str, value: impl ToString) -> Self;
}

impl WithArg for Notice {
    fn with_arg(mut self, name: &str, value: impl ToString) -> Self {
        self.text = self.text.arg(name, value);
        self
    }
}

/// What auth.json should become.
enum AuthTarget {
    Keep,
    Write(SecretBytes),
    Remove,
}

fn official_auth(
    live_shape: &AuthShape,
    stash: Option<&SecretBytes>,
    notes: &mut Vec<Notice>,
) -> AuthTarget {
    if matches!(live_shape, AuthShape::ChatGpt { .. }) {
        // The live login wins: Codex rotates refresh tokens, a stash may be stale.
        return AuthTarget::Keep;
    }
    if let Some(s) = stash.filter(|s| authjson::login_info(s.expose()).is_some()) {
        return AuthTarget::Write(s.clone());
    }
    notes.push(note(
        Notice::warning,
        "sign_in_required",
        "No saved ChatGPT login: sign in again in ChatGPT after switching.",
    ));
    match live_shape {
        AuthShape::Missing => AuthTarget::Keep,
        _ => AuthTarget::Remove,
    }
}

fn auth_target(
    inp: &Inputs<'_>,
    live_shape: &AuthShape,
    notes: &mut Vec<Notice>,
) -> Result<AuthTarget> {
    let live_is_login = matches!(live_shape, AuthShape::ChatGpt { .. });
    let released_auth =
        is_restore(&inp.desired.mode) && inp.released_changed.iter().any(|p| p == AUTH_JSON_PATH);
    if inp.auth_drift || released_auth {
        let resolution = inp.desired.key_resolutions.get(AUTH_JSON_PATH);
        let token = matches!(inp.desired.mode, ResolvedMode::Token { .. });
        match (token, resolution) {
            (_, Some(KeyResolution::Overwrite)) => {}
            // A ChatGPT login (Codex's own sign-in) is stashed before a switch
            // replaces it, and restoring the official login puts it back.
            (true, _) if live_is_login => {}
            // A switch cannot route Codex to a line with another tool's key.
            (true, _) => return Err(auth_conflict()),
            // Restores, uninstall and hand-offs leave the credential to whoever
            // changed it, unless told to take it over.
            (false, _) => {
                notes.push(note(
                    Notice::info,
                    "auth_kept",
                    "Kept auth.json: it was changed outside Superba after Superba wrote it.",
                ));
                return Ok(AuthTarget::Keep);
            }
        }
    }
    Ok(match &inp.desired.mode {
        ResolvedMode::Token { auth, key, .. } => {
            AuthTarget::Write(authjson::render_token(*auth, key)?)
        }
        ResolvedMode::Official => official_auth(live_shape, inp.login_stash, notes),
        // A hand-off restores the credential only while it is Superba's.
        ResolvedMode::Release { .. } if inp.last_written.is_empty() => AuthTarget::Keep,
        ResolvedMode::Original { .. } | ResolvedMode::Release { .. } => {
            let b = inp.baseline.ok_or_else(|| Error::NotFound {
                what: "baseline".into(),
            })?;
            if live_is_login {
                notes.push(note(
                    Notice::info,
                    "live_login_kept",
                    "The ChatGPT login currently in auth.json is kept.",
                ));
                AuthTarget::Keep
            } else {
                match &b.auth {
                    AuthShape::Missing => AuthTarget::Remove,
                    AuthShape::ChatGpt { .. } => {
                        let original = inp
                            .original_files
                            .and_then(|o| o.auth_json.as_ref())
                            .filter(|o| authjson::login_info(o.expose()).is_some());
                        match inp.login_stash.or(original) {
                            Some(s) => official_auth(live_shape, Some(s), notes),
                            None => official_auth(live_shape, None, notes),
                        }
                    }
                    _ => {
                        let o = inp.original_files.ok_or_else(|| Error::NotFound {
                            what: "the auth.json from before the takeover".into(),
                        })?;
                        match &o.auth_json {
                            Some(bytes) => AuthTarget::Write(bytes.clone()),
                            None => AuthTarget::Remove,
                        }
                    }
                }
            }
        }
        ResolvedMode::Generation { .. } => {
            let snap = inp.generation.ok_or_else(|| Error::Unsupported {
                what: "generation restore without snapshot".into(),
            })?;
            match &snap.auth_json {
                Some(bytes) if authjson::login_info(bytes.expose()).is_some() => {
                    // A recorded login is stale by now; prefer live, then the stash.
                    official_auth(live_shape, inp.login_stash.or(Some(bytes)), notes)
                }
                Some(bytes) => AuthTarget::Write(bytes.clone()),
                None if live_is_login => AuthTarget::Keep,
                None => AuthTarget::Remove,
            }
        }
        other => {
            return Err(Error::Unsupported {
                what: format!("auth for mode {other:?}"),
            });
        }
    })
}

/// Destinations that hand the files back (GH#5: they keep what changed
/// outside Superba unless told otherwise).
fn is_restore(mode: &ResolvedMode) -> bool {
    matches!(
        mode,
        ResolvedMode::Original { .. }
            | ResolvedMode::Official
            | ResolvedMode::Generation { .. }
            | ResolvedMode::Release { .. }
    )
}

/// The conflict error for a replaced credential (the planner turns it into
/// [`superba_core::PlanChoice::OwnedKeyConflict`] with the auth part).
fn auth_conflict() -> Error {
    Error::Conflict {
        message: format!("owned keys were changed outside Superba: {AUTH_JSON_PATH}"),
    }
}

fn file_change(current: Option<&[u8]>, new: Option<Vec<u8>>) -> FileChange {
    match (current, new) {
        (Some(c), Some(n)) if c == n.as_slice() => FileChange::Unchanged,
        (None, None) => FileChange::Unchanged,
        (_, Some(n)) => FileChange::Write {
            sha256: sha256_hex(&n),
            contents: SecretBytes::new(n),
        },
        (Some(_), None) => FileChange::Remove,
    }
}

/// The full render.
pub(crate) fn render(inp: &Inputs<'_>) -> Result<Rendered> {
    let mut notes = Vec::new();
    let mut doc = parse_config(inp.files)?;
    let targets = targets(inp, &doc, &mut notes)?;
    let lw: BTreeMap<&str, Option<&str>> = inp
        .last_written
        .iter()
        .map(|s| (s.path.as_str(), s.value.as_deref()))
        .collect();
    let baseline_value = |p: &str| -> Option<Option<String>> {
        inp.baseline
            .and_then(|b| b.keys.iter().find(|s| s.path == p))
            .map(|s| s.value.clone())
    };
    // A hand-off when Superba already owns nothing puts nothing back: only an
    // explicit restore of the original configuration restores released keys.
    let restore_released = matches!(inp.desired.mode, ResolvedMode::Original { .. })
        && inp.baseline.is_some_and(|b| b.released);
    let released_original = if restore_released {
        inp.original_files.map(parse_config).transpose()?
    } else {
        None
    };

    let mut path_set: BTreeSet<String> = targets.keys().cloned().collect();
    path_set.extend(lw.keys().map(|p| (*p).to_owned()));
    // Canonical order (the Krill template's): new keys are inserted in this order.
    let order = all_owned_paths();
    let mut paths: Vec<String> = path_set.iter().cloned().collect();
    paths.sort_by_key(|p| order.iter().position(|o| o == p).unwrap_or(usize::MAX));

    let restore = is_restore(&inp.desired.mode);
    let setting_paths: BTreeSet<&str> = match inp.desired.mode {
        ResolvedMode::Token { .. } => inp.desired.settings.paths().collect(),
        _ => BTreeSet::new(),
    };
    let mut key_changes = Vec::new();
    let mut owned = Vec::new();
    let mut newly_owned = Vec::new();
    let mut conflicts = Vec::new();
    for path in &paths {
        let target = targets.get(path).cloned().unwrap_or(Target::Restore);
        let owned_now = lw.contains_key(path.as_str());
        let current = doc.get_repr(path);
        let want: Option<String> = match &target {
            Target::Set(r) => Some(r.clone()),
            Target::Remove => None,
            Target::Restore if owned_now => baseline_value(path).unwrap_or(None),
            // Official releases ownership but retains the originals. An explicit
            // Original restore still restores those recorded paths. If backups
            // were deleted while released, the engine supplies the current file.
            Target::Restore if restore_released => match baseline_value(path) {
                Some(value) => released_original
                    .as_ref()
                    .map_or(value, |d| d.get_repr(path)),
                None => continue,
            },
            // Not ours: leave the user's value alone.
            Target::Restore => continue,
        };
        let will_own = !matches!(target, Target::Restore)
            && !matches!(
                inp.desired.mode,
                ResolvedMode::Official | ResolvedMode::Release { .. }
            );
        if opt_eq(current.as_deref(), want.as_deref()) {
            if will_own {
                owned.push(KeySnapshot {
                    path: path.clone(),
                    value: current.clone(),
                });
                if !owned_now {
                    newly_owned.push(KeySnapshot {
                        path: path.clone(),
                        value: current.clone(),
                    });
                }
            }
            continue;
        }
        let changed_outside = match lw.get(path.as_str()) {
            Some(written) => !opt_eq(current.as_deref(), *written),
            None => restore && inp.released_changed.contains(path),
        };
        if changed_outside {
            match inp.desired.key_resolutions.get(path) {
                Some(KeyResolution::KeepCurrent) => {
                    notes.push(
                        note(Notice::info, "drift_kept", "Kept your own value of {key}.")
                            .with_arg("key", path),
                    );
                    continue;
                }
                Some(KeyResolution::Overwrite) => {}
                None => {
                    conflicts.push(path.clone());
                    continue;
                }
            }
        }
        match &want {
            // A setting whose parent is a plain value in the user's file (or a
            // value that is not a table where the user has a table) is skipped
            // with a note instead of failing the whole switch; routing keys still
            // fail loudly. A map the user wrote as a `[section]` is updated in
            // place (ConfigDoc::set_repr).
            Some(r) if setting_paths.contains(path.as_str()) => {
                if let Err(Error::Conflict { .. }) = doc.set_repr(path, r) {
                    notes.push(
                        note(
                            Notice::warning,
                            "setting_shape_conflict",
                            "Skipped {key}: config.toml has a different kind of value at or above it.",
                        )
                        .with_arg("key", path),
                    );
                    continue;
                }
            }
            Some(r) => doc.set_repr(path, r)?,
            None => {
                doc.remove(path);
            }
        }
        key_changes.push(KeyChange {
            path: path.clone(),
            before: current.clone(),
            after: want.clone(),
        });
        if will_own {
            owned.push(KeySnapshot {
                path: path.clone(),
                value: want.clone(),
            });
            if !owned_now {
                newly_owned.push(KeySnapshot {
                    path: path.clone(),
                    value: current.clone(),
                });
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(Error::Conflict {
            message: format!(
                "owned keys were changed outside Superba: {}",
                conflicts.join(", ")
            ),
        });
    }

    // config.toml bytes.
    let current_config = inp.files.config_toml.as_ref().map(|b| b.expose());
    let mut new_config: Option<Vec<u8>> = if current_config.is_none() && key_changes.is_empty() {
        None
    } else {
        Some(doc.to_bytes())
    };
    // Byte-exact restore of a recorded file when only owned keys differ.
    let exact_source: Option<CodexFiles> = match inp.desired.mode {
        ResolvedMode::Original { .. } | ResolvedMode::Release { .. } => inp.original_files.cloned(),
        ResolvedMode::Generation { .. } => {
            inp.generation
                .and_then(|g| g.config_toml.clone())
                .map(|config_toml| CodexFiles {
                    config_toml,
                    auth_json: None,
                })
        }
        _ => None,
    };
    if let Some(orig) = &exact_source
        && let Some(exact) = byte_exact_original(&doc, orig, &path_set, inp.baseline)
    {
        new_config = exact;
    }
    if let Some(n) = &new_config {
        // Never write something we could not read back.
        ConfigDoc::parse(n)?;
    }
    let config_change = file_change(current_config, new_config);

    // auth.json.
    let live_auth = inp.files.auth_json.as_ref().map(|b| b.expose());
    let live_shape = authjson::shape(live_auth);
    let (auth_change_file, auth_change) = match auth_target(inp, &live_shape, &mut notes)? {
        AuthTarget::Keep => (FileChange::Unchanged, None),
        target => {
            let new = match target {
                AuthTarget::Write(b) => {
                    authjson::validate(b.expose())?;
                    Some(b.expose().to_vec())
                }
                _ => None,
            };
            let after = authjson::shape(new.as_deref());
            let change = file_change(live_auth, new);
            if matches!(change, FileChange::Unchanged) {
                (change, None)
            } else {
                let stash_login = matches!(live_shape, AuthShape::ChatGpt { .. });
                (
                    change,
                    Some(AuthChange {
                        before: live_shape.clone(),
                        after,
                        stash_login,
                    }),
                )
            }
        }
    };

    diagnostics(&doc, &mut notes);
    Ok(Rendered {
        files: RenderedFiles {
            config_toml: config_change,
            auth_json: auth_change_file,
            key_changes,
            auth_change,
            notes,
            // `ConfigEngine::render` callers (remote hosts) keep their own
            // ownership record and baseline from these.
            owned: owned.clone(),
            newly_owned: newly_owned.clone(),
        },
        owned,
        newly_owned,
    })
}

/// Plan 10.4 option 3: when nothing but owned keys changed since the takeover and
/// every owned key is back at its original value, return the original bytes
/// (`Some(None)` = the file did not exist).
fn byte_exact_original(
    doc: &ConfigDoc,
    orig: &CodexFiles,
    paths: &BTreeSet<String>,
    baseline: Option<&Baseline>,
) -> Option<Option<Vec<u8>>> {
    let orig_doc = match &orig.config_toml {
        Some(b) => ConfigDoc::parse(b.expose()).ok()?,
        None => ConfigDoc::empty(),
    };
    let mut in_play: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    if let Some(b) = baseline {
        in_play.extend(b.keys.iter().map(|s| s.path.as_str()));
    }
    let in_play: Vec<&str> = in_play.into_iter().collect();
    let values_match = in_play
        .iter()
        .all(|p| opt_eq(doc.get_repr(p).as_deref(), orig_doc.get_repr(p).as_deref()));
    if values_match && doc.stripped(&in_play) == orig_doc.stripped(&in_play) {
        Some(orig.config_toml.as_ref().map(|b| b.expose().to_vec()))
    } else {
        None
    }
}

fn diagnostics(doc: &ConfigDoc, notes: &mut Vec<Notice>) {
    if let Some(p) = doc.get_str(k::PROFILE) {
        notes.push(
            note(
                Notice::warning,
                "profile_override",
                "config.toml selects profile \"{profile}\"; its settings may override the keys Superba writes.",
            )
            .with_arg("profile", p),
        );
    }
    if let Some(store) = doc.get_str(k::CLI_AUTH_CREDENTIALS_STORE)
        && store != "file"
    {
        notes.push(
            note(
                Notice::warning,
                "credentials_store",
                "cli_auth_credentials_store = \"{store}\": Codex may read credentials from the OS keyring instead of auth.json.",
            )
            .with_arg("store", store),
        );
    }
    if doc.has_table(&format!("{}.{PROVIDER_OPENAI}", k::MODEL_PROVIDERS)) {
        notes.push(note(
            Notice::warning,
            "openai_provider_table",
            "config.toml defines [model_providers.openai]; Codex may ignore or reject it. Superba never writes it.",
        ));
    }
    // The table of a non-openai provider (or the legacy Krill one left behind by
    // a takeover) is kept untouched; say so in the preview.
    let table_owner = doc
        .get_str(k::MODEL_PROVIDER)
        .filter(|p| p != PROVIDER_OPENAI)
        .unwrap_or_else(|| superba_core::codex::LEGACY_KRILL_PROVIDER.to_owned());
    if doc.has_table(&format!("{}.{table_owner}", k::MODEL_PROVIDERS)) {
        notes.push(
            note(
                Notice::info,
                "legacy_provider_table",
                "The [model_providers.{provider}] table is kept unchanged.",
            )
            .with_arg("provider", table_owner),
        );
    }
}

/// Pure inspection (`ConfigEngine::inspect_files`). Unparseable config.toml is
/// reported in `parse_error` (writers refuse it).
pub(crate) fn inspect(codex_home: &Path, files: &CodexFiles) -> ConfigState {
    let config_sha256 = files.config_toml.as_ref().map(|b| sha256_hex(b.expose()));
    let auth_sha256 = files.auth_json.as_ref().map(|b| sha256_hex(b.expose()));
    let auth = authjson::shape(files.auth_json.as_ref().map(|b| b.expose()));
    let mut state = ConfigState {
        codex_home: codex_home.to_path_buf(),
        config_sha256,
        auth_sha256,
        parse_error: None,
        model_provider: None,
        provider_table_present: false,
        openai_base_url: None,
        chatgpt_base_url: None,
        model: None,
        model_owned: false,
        reasoning_effort: None,
        profile_override: None,
        cli_auth_credentials_store: None,
        auth,
        active: None,
        drift: Vec::new(),
        auth_drift: None,
        handed_off: None,
        since_release: Vec::new(),
        since_release_auth: None,
        journal_pending: false,
    };
    match parse_config(files) {
        Ok(doc) => {
            state.model_provider = doc.get_str(k::MODEL_PROVIDER);
            let provider = state.model_provider.as_deref().unwrap_or(PROVIDER_OPENAI);
            state.provider_table_present =
                doc.has_table(&format!("{}.{provider}", k::MODEL_PROVIDERS));
            state.openai_base_url = doc.get_str(k::OPENAI_BASE_URL);
            state.chatgpt_base_url = doc.get_str(k::CHATGPT_BASE_URL);
            state.model = doc.get_str(k::MODEL);
            state.reasoning_effort = doc.get_str(k::MODEL_REASONING_EFFORT);
            state.profile_override = doc.get_str(k::PROFILE);
            state.cli_auth_credentials_store = doc.get_str(k::CLI_AUTH_CREDENTIALS_STORE);
        }
        Err(e) => state.parse_error = Some(e.to_string()),
    }
    state
}

/// Drift of owned keys against what Superba last wrote.
pub(crate) fn drift(files: &CodexFiles, last_written: &[KeySnapshot]) -> Vec<KeyChange> {
    let Ok(doc) = parse_config(files) else {
        return Vec::new();
    };
    last_written
        .iter()
        .filter_map(|s| {
            let cur = doc.get_repr(&s.path);
            (!opt_eq(cur.as_deref(), s.value.as_deref())).then(|| KeyChange {
                path: s.path.clone(),
                before: s.value.clone(),
                after: cur,
            })
        })
        .collect()
}

/// Pure baseline capture (`ConfigEngine::capture_baseline`).
pub(crate) fn capture_baseline(
    codex_home: &Path,
    files: &CodexFiles,
    extras: BaselineExtras,
    now: Timestamp,
) -> Result<Baseline> {
    let doc = parse_config(files)?;
    let keys = baseline_paths()
        .into_iter()
        .map(|p| KeySnapshot {
            path: p.to_owned(),
            value: doc.get_repr(p),
        })
        .collect();
    let provider_key = doc.get_str(k::MODEL_PROVIDER);
    let provider_table_present = provider_key
        .as_deref()
        .is_some_and(|p| doc.has_table(&format!("{}.{p}", k::MODEL_PROVIDERS)));
    Ok(Baseline {
        captured_at: now,
        codex_home: codex_home.to_path_buf(),
        config_sha256: files.config_toml.as_ref().map(|b| sha256_hex(b.expose())),
        auth_sha256: files.auth_json.as_ref().map(|b| sha256_hex(b.expose())),
        keys,
        provider_key,
        provider_table_present,
        auth: authjson::shape(files.auth_json.as_ref().map(|b| b.expose())),
        login_stashed: false,
        session_distribution: extras.session_distribution,
        env: extras.env,
        legacy_env_lines: extras.legacy_env_lines,
        first_write_backups: Vec::new(),
        released: false,
    })
}
