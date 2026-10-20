//! Test helpers: a small in-memory [`ConfigEngine`] fake (the real one lives in
//! `superba-config`, which this crate must not depend on) and fixtures.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use superba_core::codex::config_keys;
use superba_core::{
    ActiveCredentials, ApplyPlan, ApplyReport, AuthChange, AuthMode, AuthShape, Baseline,
    BaselineExtras, CodexFiles, CodexHome, CodexPath, ConfigEngine, ConfigState, Error, FileChange,
    GenerationRecord, KeyChange, KeySnapshot, LoginStashInfo, ModelDefaults, Origin,
    RecoveryReport, RemoteHost, RenderInput, RenderedFiles, ResolvedEndpoints, ResolvedMode,
    ResolvedProfile, Result, Secret, SecretBytes, SiteKind, SshEndpoint, StashScope, TargetId,
    Timestamp,
};

/// Fake engine: line-based edits of three top-level keys, auth.json per mode.
#[derive(Debug, Default)]
pub struct FakeEngine {
    pub stash: Mutex<BTreeMap<String, Vec<u8>>>,
}

fn sha(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Value of a top-level `key = "value"` line.
pub fn toml_get(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            break;
        }
        if let Some((k, v)) = t.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn toml_set(text: &str, key: &str, value: Option<&str>) -> String {
    let mut out = String::new();
    let mut done = false;
    let mut in_root = true;
    let mut lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.is_empty() {
        lines.push("");
    }
    for line in lines {
        let t = line.trim();
        if t.starts_with('[') && in_root {
            in_root = false;
            if !done && let Some(v) = value {
                out.push_str(&format!("{key} = \"{v}\"\n"));
                done = true;
            }
        }
        if in_root
            && let Some((k, _)) = t.split_once('=')
            && k.trim() == key
        {
            if let Some(v) = value {
                out.push_str(&format!("{key} = \"{v}\"\n"));
            }
            done = true;
            continue;
        }
        out.push_str(line);
    }
    if !done && let Some(v) = value {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("{key} = \"{v}\"\n"));
    }
    out
}

fn auth_shape(bytes: Option<&SecretBytes>) -> AuthShape {
    let Some(b) = bytes else {
        return AuthShape::Missing;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(b.expose()) else {
        return AuthShape::Unknown {
            detail: "invalid json".into(),
        };
    };
    let last4 = |k: &str| {
        let s = v.get(k).and_then(|x| x.as_str()).unwrap_or_default();
        s.chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    };
    if v.get("personal_access_token").is_some() {
        AuthShape::Pat {
            key_last4: last4("personal_access_token"),
        }
    } else if v.get("tokens").is_some() {
        AuthShape::ChatGpt {
            email: None,
            account_id: None,
        }
    } else if v.get("OPENAI_API_KEY").is_some() {
        AuthShape::ApiKey {
            key_last4: last4("OPENAI_API_KEY"),
        }
    } else {
        AuthShape::Unknown {
            detail: "unknown".into(),
        }
    }
}

const KEYS: [&str; 3] = [
    config_keys::MODEL_PROVIDER,
    config_keys::OPENAI_BASE_URL,
    config_keys::CHATGPT_BASE_URL,
];

#[async_trait]
impl ConfigEngine for FakeEngine {
    fn render(&self, input: &RenderInput<'_>) -> Result<RenderedFiles> {
        let cur_cfg = input
            .files
            .config_toml
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.expose()).into_owned())
            .unwrap_or_default();
        let mut cfg = cur_cfg.clone();
        let (auth_after, wanted): (Option<Vec<u8>>, [Option<String>; 3]) = match &input.desired.mode
        {
            ResolvedMode::Token {
                auth,
                endpoints,
                key,
                ..
            } => {
                let json = if auth.needs_authapi_env() {
                    serde_json::json!({"personal_access_token": key.expose(), "OPENAI_API_KEY": key.expose()})
                } else {
                    serde_json::json!({"auth_mode": "apikey", "OPENAI_API_KEY": key.expose()})
                };
                (
                    Some(serde_json::to_vec_pretty(&json).unwrap_or_default()),
                    [
                        Some("openai".into()),
                        Some(endpoints.openai_base_url.to_string()),
                        endpoints.chatgpt_base_url.as_ref().map(|u| u.to_string()),
                    ],
                )
            }
            ResolvedMode::Official => (
                input.login_stash.map(|b| b.expose().to_vec()),
                [None, None, None],
            ),
            ResolvedMode::Original { .. } => {
                let orig = input
                    .original_files
                    .ok_or_else(|| Error::invalid("fake engine needs original files"))?;
                let rendered = RenderedFiles {
                    config_toml: change(
                        input.files.config_toml.as_ref(),
                        orig.config_toml.as_ref(),
                    ),
                    auth_json: change(input.files.auth_json.as_ref(), orig.auth_json.as_ref()),
                    ..RenderedFiles::default()
                };
                return Ok(rendered);
            }
            _ => return Err(Error::not_implemented("fake engine mode")),
        };
        // Desired value per path (`None` = absent). Routing keys are always
        // managed (as before); token modes add their settings and model keys.
        let mut targets: BTreeMap<String, Option<String>> = KEYS
            .iter()
            .zip(wanted.iter())
            .map(|(k, w)| ((*k).to_owned(), w.clone()))
            .collect();
        if matches!(input.desired.mode, ResolvedMode::Token { .. }) {
            for (path, entry) in input.desired.settings.iter() {
                let value = entry.value().map(|v| match v {
                    superba_core::SettingValue::String(s) => s.clone(),
                    other => other.to_toml_repr(),
                });
                targets.insert(path.to_owned(), value);
            }
        }
        if let Some(m) = &input.desired.model.model {
            targets.insert(config_keys::MODEL.to_owned(), Some(m.clone()));
        }
        if let Some(e) = &input.desired.model.reasoning_effort {
            targets.insert(
                config_keys::MODEL_REASONING_EFFORT.to_owned(),
                Some(e.clone()),
            );
        }
        // Keys the new state sets or removes become owned (an official switch
        // owns nothing, like the real engine).
        let managed: Vec<String> = if matches!(input.desired.mode, ResolvedMode::Token { .. }) {
            targets.keys().cloned().collect()
        } else {
            Vec::new()
        };
        // Owned keys the new state does not manage: back to the baseline value.
        let last_written: BTreeMap<&str, Option<&str>> = input
            .last_written
            .unwrap_or(&[])
            .iter()
            .map(|k| (k.path.as_str(), k.value.as_deref()))
            .collect();
        for path in last_written.keys() {
            if !targets.contains_key(*path) {
                let base = input
                    .baseline
                    .and_then(|b| b.keys.iter().find(|k| k.path == *path))
                    .and_then(|k| k.value.clone());
                targets.insert((*path).to_owned(), base);
            }
        }
        let mut key_changes = Vec::new();
        let mut notes = Vec::new();
        let mut owned = Vec::new();
        let mut newly_owned = Vec::new();
        for (key, want) in &targets {
            let before = toml_get(&cur_cfg, key);
            let mut own = || {
                if managed.contains(key) {
                    owned.push(KeySnapshot {
                        path: key.clone(),
                        value: want.as_ref().map(|v| format!("\"{v}\"")),
                    });
                    // Baseline values of this fake are unquoted.
                    if !last_written.contains_key(key.as_str()) {
                        newly_owned.push(KeySnapshot {
                            path: key.clone(),
                            value: before.clone(),
                        });
                    }
                }
            };
            if before.as_deref() == want.as_deref() {
                own();
                continue;
            }
            // Drift resolved "keep current" (the contract subset the scoped
            // renders rely on): an owned key whose live value differs from what
            // was written last is left alone.
            if let Some(written) = last_written.get(key.as_str())
                && before.as_ref().map(|v| format!("\"{v}\"")).as_deref() != *written
                && input.desired.key_resolutions.get(key)
                    == Some(&superba_core::KeyResolution::KeepCurrent)
            {
                notes.push(superba_core::Notice::info(
                    superba_core::LocalizedText::new("config.note.drift_kept", "Kept {key}.")
                        .arg("key", key),
                ));
                continue;
            }
            own();
            cfg = toml_set(&cfg, key, want.as_deref());
            key_changes.push(KeyChange {
                path: key.clone(),
                before: before.map(|v| format!("\"{v}\"")),
                after: want.as_ref().map(|v| format!("\"{v}\"")),
            });
        }
        let new_cfg = SecretBytes::new(cfg.into_bytes());
        let new_auth = auth_after.map(SecretBytes::new);
        let before_shape = auth_shape(input.files.auth_json.as_ref());
        let after_shape = auth_shape(new_auth.as_ref());
        let auth_json = change(input.files.auth_json.as_ref(), new_auth.as_ref());
        Ok(RenderedFiles {
            config_toml: change(input.files.config_toml.as_ref(), Some(&new_cfg)),
            auth_change: (!matches!(auth_json, FileChange::Unchanged)).then_some(AuthChange {
                stash_login: matches!(before_shape, AuthShape::ChatGpt { .. }),
                before: before_shape,
                after: after_shape,
            }),
            auth_json,
            key_changes,
            notes,
            owned,
            newly_owned,
        })
    }

    fn inspect_files(&self, codex_home: &Path, files: &CodexFiles) -> Result<ConfigState> {
        let cfg = files
            .config_toml
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.expose()).into_owned())
            .unwrap_or_default();
        Ok(ConfigState {
            codex_home: codex_home.to_path_buf(),
            config_sha256: files.config_toml.as_ref().map(|b| sha(b.expose())),
            auth_sha256: files.auth_json.as_ref().map(|b| sha(b.expose())),
            parse_error: cfg.contains("<<broken").then(|| "broken toml".to_owned()),
            model_provider: toml_get(&cfg, config_keys::MODEL_PROVIDER),
            provider_table_present: false,
            openai_base_url: toml_get(&cfg, config_keys::OPENAI_BASE_URL),
            chatgpt_base_url: toml_get(&cfg, config_keys::CHATGPT_BASE_URL),
            model: None,
            model_owned: false,
            reasoning_effort: None,
            profile_override: None,
            cli_auth_credentials_store: None,
            auth: auth_shape(files.auth_json.as_ref()),
            active: None,
            drift: Vec::new(),
            auth_drift: None,
            handed_off: None,
            since_release: Vec::new(),
            since_release_auth: None,
            journal_pending: false,
        })
    }

    fn capture_baseline(
        &self,
        codex_home: &Path,
        files: &CodexFiles,
        extras: BaselineExtras,
        now: Timestamp,
    ) -> Result<Baseline> {
        let cfg = files
            .config_toml
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.expose()).into_owned())
            .unwrap_or_default();
        Ok(Baseline {
            captured_at: now,
            codex_home: codex_home.to_path_buf(),
            config_sha256: files.config_toml.as_ref().map(|b| sha(b.expose())),
            auth_sha256: files.auth_json.as_ref().map(|b| sha(b.expose())),
            keys: KEYS
                .iter()
                .map(|k| KeySnapshot {
                    path: (*k).to_owned(),
                    value: toml_get(&cfg, k),
                })
                .collect(),
            provider_key: toml_get(&cfg, config_keys::MODEL_PROVIDER),
            provider_table_present: false,
            auth: auth_shape(files.auth_json.as_ref()),
            login_stashed: false,
            session_distribution: extras.session_distribution,
            env: extras.env,
            legacy_env_lines: extras.legacy_env_lines,
            first_write_backups: Vec::new(),
            released: false,
        })
    }

    async fn read_state(&self, _home: &CodexHome) -> Result<ConfigState> {
        Err(Error::not_implemented("fake"))
    }
    async fn baseline(&self, _home: &CodexHome) -> Result<Option<Baseline>> {
        Err(Error::not_implemented("fake"))
    }
    async fn snapshot_baseline(&self, _h: &CodexHome, _e: BaselineExtras) -> Result<Baseline> {
        Err(Error::not_implemented("fake"))
    }
    async fn plan(&self, _h: &CodexHome, _d: &ResolvedProfile) -> Result<ApplyPlan> {
        Err(Error::not_implemented("fake"))
    }
    async fn apply(&self, _p: &ApplyPlan) -> Result<ApplyReport> {
        Err(Error::not_implemented("fake"))
    }
    async fn rollback(&self, _r: &ApplyReport) -> Result<()> {
        Err(Error::not_implemented("fake"))
    }
    async fn recover(&self, _h: &CodexHome) -> Result<RecoveryReport> {
        Err(Error::not_implemented("fake"))
    }
    async fn generations(&self, _h: &CodexHome) -> Result<Vec<GenerationRecord>> {
        Err(Error::not_implemented("fake"))
    }
    async fn active_credentials(&self, _h: &CodexHome) -> Result<Option<ActiveCredentials>> {
        Err(Error::not_implemented("fake"))
    }
    async fn login_stash(&self, _s: &StashScope) -> Result<Option<LoginStashInfo>> {
        Ok(None)
    }
    async fn stash_login(&self, scope: &StashScope, auth: &SecretBytes) -> Result<LoginStashInfo> {
        self.stash
            .lock()
            .map_err(|_| Error::other("poisoned"))?
            .insert(format!("{scope:?}"), auth.expose().to_vec());
        Ok(LoginStashInfo {
            stashed_at: Timestamp::now(),
            last_refresh: None,
            id_token_exp: None,
            email: None,
            account_id: None,
        })
    }
    async fn load_login_stash(&self, scope: &StashScope) -> Result<Option<SecretBytes>> {
        Ok(self
            .stash
            .lock()
            .map_err(|_| Error::other("poisoned"))?
            .get(&format!("{scope:?}"))
            .cloned()
            .map(SecretBytes::new))
    }
}

fn change(cur: Option<&SecretBytes>, new: Option<&SecretBytes>) -> FileChange {
    match (cur, new) {
        (Some(a), Some(b)) if a.expose() == b.expose() => FileChange::Unchanged,
        (None, None) => FileChange::Unchanged,
        (_, Some(b)) => FileChange::Write {
            sha256: sha(b.expose()),
            contents: b.clone(),
        },
        (Some(_), None) => FileChange::Remove,
    }
}

/// A Krill-style token profile on `origin`.
pub fn token_profile(origin: &str, mode: AuthMode, key: &str) -> ResolvedProfile {
    let origin = Origin::parse(origin).expect("origin");
    ResolvedProfile {
        model_catalog: None,
        profile_id: Some("p1".into()),
        site_id: Some("krill".into()),
        line_id: Some("overseas".into()),
        mode: ResolvedMode::Token {
            site_kind: SiteKind::Krill,
            auth: mode,
            endpoints: ResolvedEndpoints::for_line(&origin, CodexPath::BackendApiCodex, mode)
                .expect("endpoints"),
            key: Secret::new(key),
        },
        model: ModelDefaults::default(),
        key_resolutions: BTreeMap::new(),
        settings: Default::default(),
    }
}

/// The official ChatGPT login.
pub fn official_profile() -> ResolvedProfile {
    ResolvedProfile {
        mode: ResolvedMode::Official,
        ..original_profile()
    }
}

/// Restore-to-original profile.
pub fn original_profile() -> ResolvedProfile {
    ResolvedProfile {
        model_catalog: None,
        profile_id: None,
        site_id: None,
        line_id: None,
        mode: ResolvedMode::Original {
            provider: superba_core::ProviderRestore::KeepOpenai,
        },
        model: ModelDefaults::default(),
        key_resolutions: BTreeMap::new(),
        settings: Default::default(),
    }
}

/// A remote host reached through an ssh alias.
pub fn host(alias: &str, host_id: Option<&str>) -> RemoteHost {
    RemoteHost {
        target_id: TargetId::remote(host_id.unwrap_or(alias)),
        label: alias.to_owned(),
        host_id: host_id.map(str::to_owned),
        kind: superba_core::HostKind::Ssh,
        ssh: SshEndpoint {
            alias: Some(alias.to_owned()),
            ..SshEndpoint::default()
        },
        sources: Vec::new(),
        connected: None,
        wsl: None,
    }
}

/// Shared engine handle.
pub fn engine() -> Arc<FakeEngine> {
    Arc::new(FakeEngine::default())
}
