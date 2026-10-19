//! Narrow repair of stale desktop recent-model IDs after changing providers.
//!
//! Never edit a running app's preferences: its in-memory copy would overwrite
//! the repair. The switch service stops the app before calling the writer.

use std::collections::BTreeMap;

use serde_json::Value;
use superba_core::{CodexHome, Error, LocalContext, Result, SecretBytes};

use crate::fsx::{self, Mode};
use crate::vault::Vault;

const STATE_FILE: &str = ".codex-global-state.json";
const ATOMS: &str = "electron-persisted-atom-state";
const RECENT: &str = "composer-recent-model-configurations-v1";
const MAX_BYTES: u64 = 16 * 1024 * 1024;

fn read(home: &CodexHome) -> Result<Option<(SecretBytes, Value)>> {
    let path = home.root().join(STATE_FILE);
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > MAX_BYTES => {
            return Err(Error::invalid(
                "desktop preferences exceed the repair size limit",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&path, &e)),
        _ => {}
    }
    let Some(bytes) = fsx::read_optional(&path)? else {
        return Ok(None);
    };
    let bytes = SecretBytes::new(bytes);
    let value: Value = serde_json::from_slice(bytes.expose())
        .map_err(|_| Error::parse(STATE_FILE, "desktop preferences are not valid JSON"))?;
    if !value.is_object() {
        return Err(Error::parse(
            STATE_FILE,
            "desktop preferences are not an object",
        ));
    }
    if let Some(recent) = value.get(ATOMS).and_then(|a| a.get(RECENT))
        && !recent.is_array()
    {
        return Err(Error::parse(STATE_FILE, "recent models are not an array"));
    }
    Ok(Some((bytes, value)))
}

pub(crate) fn recent_models(home: &CodexHome) -> Result<Vec<String>> {
    Ok(read(home)?
        .map(|(_, value)| {
            value
                .get(ATOMS)
                .and_then(|a| a.get(RECENT))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.get("model").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

pub(crate) fn repair(
    ctx: &LocalContext,
    home: &CodexHome,
    replacements: &BTreeMap<String, String>,
) -> Result<usize> {
    if replacements.is_empty() {
        return Ok(0);
    }
    for _ in 0..3 {
        let Some((before, mut value)) = read(home)? else {
            return Ok(0);
        };
        let Some(recent) = value
            .get_mut(ATOMS)
            .and_then(|a| a.get_mut(RECENT))
            .and_then(Value::as_array_mut)
        else {
            return Ok(0);
        };
        let mut changed = 0;
        for entry in recent.iter_mut() {
            if let Some(model) = entry.get("model").and_then(Value::as_str)
                && let Some(target) = replacements
                    .get(model)
                    .filter(|target| target.as_str() != model)
            {
                entry["model"] = Value::String(target.clone());
                changed += 1;
            }
        }
        if changed == 0 {
            return Ok(0);
        }
        // A mapped entry can now equal an earlier recent configuration. Keep its
        // original order, effort, tier and any future fields instead of merging.
        let mut unique = Vec::new();
        for entry in recent.drain(..) {
            let repaired_model = entry
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| replacements.values().any(|target| target == model));
            if !repaired_model || !unique.contains(&entry) {
                unique.push(entry)
            }
        }
        *recent = unique;
        let path = fsx::resolve_write_target(&home.root().join(STATE_FILE));
        let before_hash = fsx::sha256_hex(before.expose());
        let backup = ctx
            .data_dir
            .root()
            .join("backups/model-history")
            .join(fsx::short_hash(home.root().to_string_lossy().as_bytes()))
            .join(format!("{before_hash}.json.enc"));
        if !backup.exists() {
            Vault::new(ctx.clone()).write_sealed(&backup, before.expose())?;
        }
        let mut bytes = serde_json::to_vec_pretty(&value)
            .map_err(|_| Error::other("could not serialize desktop preferences"))?;
        bytes.push(b'\n');
        let bytes = SecretBytes::new(bytes);
        let temp = fsx::stage(&path, bytes.expose(), Mode::PreserveOrPrivate)?;
        if fsx::file_sha256(&path)?.as_deref() != Some(&before_hash) {
            fsx::remove_if_exists(&temp)?;
            continue;
        }
        if let Err(e) = fsx::publish(&temp, &path) {
            let _ = fsx::remove_if_exists(&temp);
            return Err(e);
        }
        return Ok(changed);
    }
    Err(Error::Conflict {
        message: "desktop preferences changed during recent-model repair".into(),
    })
}
