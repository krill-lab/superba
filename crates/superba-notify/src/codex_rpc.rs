//! Ask Codex itself about its hooks: `codex app-server` over stdio,
//! `initialize` then `hooks/list` (Codex 0.140+). The answer carries each
//! hook's key (`<file>:<event>:<group>:<handler>`), its current hash and its
//! trust status, so Superba records exactly the hash Codex computes (it never
//! computes one itself).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// How long the app-server may take to answer.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(20);

/// One hook as `hooks/list` reports it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodexHook {
    /// `<file>:<event>:<group>:<handler>`.
    pub key: String,
    /// `permissionRequest`, `stop`, ...
    #[serde(default)]
    pub event_name: String,
    /// The command line.
    #[serde(default)]
    pub command: Option<String>,
    /// `sha256:<hex>`.
    #[serde(default)]
    pub current_hash: String,
    /// `trusted`, `untrusted`, `modified`, ...
    #[serde(default)]
    pub trust_status: String,
}

/// `hooks/list` for `cwd` with `codex` (CODEX_HOME = `codex_home`).
pub async fn hooks_list(
    codex: &Path,
    codex_home: &Path,
    cwd: &Path,
) -> Result<Vec<CodexHook>, String> {
    let mut std_cmd = superba_core::process::command(codex);
    std_cmd
        .arg("app-server")
        .env("CODEX_HOME", codex_home)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut cmd = tokio::process::Command::from(std_cmd);
    cmd.kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", codex.display()))?;
    let result = tokio::time::timeout(RPC_TIMEOUT, async {
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut lines = BufReader::new(stdout).lines();
        let send = |v: Value| format!("{v}\n");
        let init = json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "superba", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true}}});
        stdin
            .write_all(send(init).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        wait_for(&mut lines, 1).await?;
        stdin
            .write_all(send(json!({"method": "initialized"})).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let list = json!({"id": 2, "method": "hooks/list", "params": {"cwds": [cwd]}});
        stdin
            .write_all(send(list).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let answer = wait_for(&mut lines, 2).await?;
        if let Some(err) = answer.get("error") {
            return Err(format!("hooks/list: {err}"));
        }
        let mut hooks = Vec::new();
        for entry in answer["result"]["data"].as_array().into_iter().flatten() {
            for h in entry["hooks"].as_array().into_iter().flatten() {
                if let Ok(hook) = serde_json::from_value::<CodexHook>(h.clone()) {
                    hooks.push(hook);
                }
            }
        }
        Ok::<_, String>(hooks)
    })
    .await
    .map_err(|_| "Codex app-server did not answer".to_owned())?;
    let _ = child.kill().await;
    result
}

async fn wait_for(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: i64,
) -> Result<Value, String> {
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("id").and_then(Value::as_i64) == Some(id) && v.get("method").is_none() {
            return Ok(v);
        }
    }
    Err("Codex app-server closed its output".into())
}
