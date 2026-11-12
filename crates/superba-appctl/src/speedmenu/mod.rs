//! The desktop app's own speed menu (Standard / Fast / Ultrafast) for API-key
//! sign-ins.
//!
//! The app shows the speed menu, reads the configured `service_tier` and sends
//! the chosen tier with a turn only when the window is signed in to ChatGPT
//! (or with a personal access token, or Bedrock). With an API key, which is
//! what Superba writes for Krill and custom sites, it drops the tier, so every
//! turn runs Standard. Codex itself sends `service_tier` for any sign-in.
//!
//! Superba changes nothing on disk. When it launches the app it attaches to
//! the window over the loopback debugging port before the first document runs
//! ([`attach`]) and registers a document-start script ([`document_script`]).
//! That script swaps four module factories of the renderer bundle for copies
//! whose speed-tier checks also accept the local host signed in with an API
//! key ([`plan`]). Which factories, and the copies, are computed once per app
//! build from its `app.asar` and cached ([`cached_plan`]).
//!
//! When anything does not match (a new app build changed the checks, a module
//! registered with other source), nothing is replaced and the app behaves as
//! it would without Superba. The tiers offered come from the model catalog
//! (`service_tiers` of each model), as for a ChatGPT sign-in.

pub mod asar;
pub mod attach;
pub mod jslex;
pub mod plan;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use superba_core::Result;

use crate::codexbin::FileStamp;
pub use plan::{GateKind, NoPlan, PatchedModule, Plan};

/// Directory under `DataDir::cache_dir` holding one plan file per app build.
pub const CACHE_DIR: &str = "speed-menu";

/// Plan files kept (the newest; one per recently seen app build).
const CACHE_KEEP: usize = 4;

const HOOK_TEMPLATE: &str = include_str!("hook.js");

#[derive(Serialize, Deserialize)]
struct CacheFile {
    stamp: FileStamp,
    outcome: std::result::Result<Plan, NoPlan>,
}

fn cache_file(dir: &Path, stamp: &FileStamp) -> PathBuf {
    let mut h = Sha256::new();
    h.update(stamp.path.to_string_lossy().as_bytes());
    h.update(stamp.size.to_le_bytes());
    h.update(stamp.mtime_ms.to_le_bytes());
    let digest = hex::encode(h.finalize());
    dir.join(format!("{}.json", &digest[..16]))
}

/// The plan for the app build in `asar`, from `cache_dir/speed-menu/` when it
/// was computed for this file (path, size, mtime) and this
/// [`plan::PLAN_VERSION`], else computed and stored there. Blocking.
pub fn cached_plan(asar: &Path, cache_dir: &Path) -> Result<std::result::Result<Plan, NoPlan>> {
    let stamp = FileStamp::of(asar).ok_or_else(|| superba_core::Error::NotFound {
        what: asar.display().to_string(),
    })?;
    let dir = cache_dir.join(CACHE_DIR);
    let file = cache_file(&dir, &stamp);
    if let Some(cached) = std::fs::read(&file)
        .ok()
        .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
        .filter(|c| c.stamp == stamp)
    {
        let version = match &cached.outcome {
            Ok(p) => p.version,
            Err(n) => n.version,
        };
        if version == plan::PLAN_VERSION {
            return Ok(cached.outcome);
        }
    }
    let outcome = plan::plan_for_asar(&stamp.path)?;
    store(
        &dir,
        &file,
        &CacheFile {
            stamp,
            outcome: outcome.clone(),
        },
    );
    Ok(outcome)
}

/// Write the cache file (best effort) and drop the oldest beyond
/// [`CACHE_KEEP`].
fn store(dir: &Path, file: &Path, content: &CacheFile) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(content) else {
        return;
    };
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_err() || std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, old) in files.into_iter().skip(CACHE_KEEP) {
        let _ = std::fs::remove_file(old);
    }
}

/// Start planning for `install` now (blocking work on the pool); the result
/// goes to the returned receiver as the attach task expects it.
pub fn start_planning(
    install: &superba_core::AppInstall,
    cache_dir: PathBuf,
) -> tokio::sync::oneshot::Receiver<attach::PlanResult> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let asar = crate::codexbin::asar_path(install);
    tokio::task::spawn_blocking(move || {
        let no = |reason: String| NoPlan {
            version: plan::PLAN_VERSION,
            reason,
        };
        let result = match asar {
            None => Err(no("the app has no app.asar".into())),
            Some(asar) => match cached_plan(&asar, &cache_dir) {
                Ok(Ok(p)) => {
                    let script = document_script(&p);
                    Ok((p, script))
                }
                Ok(Err(n)) => Err(n),
                Err(e) => Err(no(e.to_string())),
            },
        };
        let _ = tx.send(result);
    });
    rx
}

/// Attach to the app started as `pid` as soon as its debugging port is up and
/// register the speed-menu script (see [`attach`]); logs the outcome. Gives up
/// when no verified port shows up within `wait`.
pub async fn attach_after_launch(
    pid: u32,
    cdp: crate::launch::CdpPlan,
    launched_at: superba_core::Timestamp,
    plan: tokio::sync::oneshot::Receiver<attach::PlanResult>,
    wait: std::time::Duration,
) -> Option<attach::Outcome> {
    use crate::launch::CdpPlan;
    use crate::owner::OwnerCheck;
    let deadline = tokio::time::Instant::now() + wait;
    let ws = loop {
        let port = match &cdp {
            CdpPlan::Disabled => return None,
            CdpPlan::Fixed(p) => Some(*p),
            CdpPlan::ActivePortFile(file) => crate::read_fresh_active_port(file, launched_at),
        };
        if let Some(port) = port {
            let check = tokio::task::spawn_blocking(move || crate::owner::check_owner(port, pid))
                .await
                .unwrap_or(OwnerCheck::Unknown);
            if matches!(check, OwnerCheck::NotLoopback | OwnerCheck::Mismatch { .. }) {
                tracing::warn!(
                    port,
                    ?check,
                    "speed menu: the debugging port is not the app's"
                );
                return None;
            }
            if check != OwnerCheck::NotListening
                && let Ok(v) =
                    crate::cdp::browser_version(port, std::time::Duration::from_secs(2)).await
                && let Some(ws) = v
                    .get("webSocketDebuggerUrl")
                    .and_then(serde_json::Value::as_str)
                && crate::cdp::validate_ws_url(ws, port)
            {
                break ws.to_owned();
            }
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(pid, "speed menu: no debugging port to attach to");
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let outcome = attach::run(&ws, plan).await;
    if outcome.patched.is_empty() {
        tracing::info!(
            reason = outcome.reason.as_deref().unwrap_or("-"),
            late = outcome.late,
            mismatched = ?outcome.mismatched,
            "speed menu: not available in this window"
        );
    } else {
        tracing::info!(
            patched = ?outcome.patched,
            late = outcome.late,
            mismatched = ?outcome.mismatched,
            "speed menu: available"
        );
    }
    Some(outcome)
}

/// The document-start script for `plan` (see `hook.js`).
pub fn document_script(plan: &Plan) -> String {
    let modules = serde_json::Value::Array(
        plan.modules
            .iter()
            .map(|m| serde_json::json!({"id": m.id, "len": m.original_len, "fnv": m.original_fnv}))
            .collect(),
    );
    // One object per variant: a module may have several (see `plan`).
    let factories = format!(
        "[{}]",
        plan.modules
            .iter()
            .map(|m| format!("{{{}}}", m.source))
            .collect::<Vec<_>>()
            .join(",\n")
    );
    HOOK_TEMPLATE
        .replacen("__SUPERBA_MODULES__", &modules.to_string(), 1)
        .replacen("__SUPERBA_FACTORIES__", &factories, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_plan() -> Plan {
        Plan {
            version: plan::PLAN_VERSION,
            modules: vec![PatchedModule {
                key: "aB12".into(),
                id: "aB12".into(),
                original_len: 11,
                original_fnv: 7,
                source: "aB12(e,t){t.x=1}".into(),
                gates: vec![GateKind::UiAccess],
            }],
        }
    }

    #[test]
    fn script_embeds_metadata_and_factories() {
        let s = document_script(&sample_plan());
        assert!(
            s.contains(r#"const modules = [{"id":"aB12","len":11,"fnv":7}];"#),
            "{s}"
        );
        assert!(s.contains("const factories = [{aB12(e,t){t.x=1}}];"), "{s}");
        assert!(!s.contains("__SUPERBA_"));
    }

    #[test]
    fn plans_are_cached_per_asar_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let asar = dir.path().join("app.asar");
        std::fs::write(
            &asar,
            asar::tests::build(&[("webview/assets/a.js", b"export const x=1;")]),
        )
        .unwrap();
        let cache = dir.path().join("cache");
        let first = cached_plan(&asar, &cache).unwrap();
        assert_eq!(
            first.as_ref().unwrap_err().reason,
            "ui_access: 0 matches, requirements: 0 matches, queue_default: 0 matches, queue_explicit: 0 matches"
        );
        let files: Vec<_> = std::fs::read_dir(cache.join(CACHE_DIR))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(files.len(), 1);
        // A forged cache entry for the same stamp is what a second call returns.
        let stamp = FileStamp::of(&asar).unwrap();
        let forged = CacheFile {
            stamp: stamp.clone(),
            outcome: Ok(sample_plan()),
        };
        std::fs::write(files[0].path(), serde_json::to_vec(&forged).unwrap()).unwrap();
        assert_eq!(cached_plan(&asar, &cache).unwrap(), Ok(sample_plan()));
    }
}
