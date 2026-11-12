//! Register the speed-menu script in the app's window before its first
//! document runs (verified on Linux app 26.1106).
//!
//! Superba connects to the browser endpoint of the loopback debugging port as
//! soon as the app writes it (about 0.5 s after start; the window's page
//! target appears about 3 s later) and asks for every new target to be
//! attached paused (`Target.setAutoAttach` with `waitForDebuggerOnStart`).
//! For the window's page it sends `Page.enable`, `Page.addScriptToEvaluateOnNew
//! Document` and `Runtime.runIfWaitingForDebugger` back to back: the paused
//! target answers none of them before it runs, and runs the queued script in
//! its first document. Other targets are resumed and detached at once.
//!
//! A page that is still waiting when the plan is not ready yet stays paused up
//! to [`HOLD_LIMIT`]; then it runs without the script. Once the page reports
//! every planned module patched (or after [`SETTLE_LIMIT`]) new targets are no
//! longer paused. The connection stays open as long as this process runs, so
//! the script also runs when the window reloads; it ends with the app.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use super::plan::{NoPlan, Plan};

/// Longest a paused window waits for the plan (below the 15 s a launch waits
/// for the window to be ready; a plan takes ~0.5 s, or nothing when cached).
pub const HOLD_LIMIT: Duration = Duration::from_secs(10);
/// Longest new targets are paused after the attach.
pub const SETTLE_LIMIT: Duration = Duration::from_secs(90);
/// How often the page's state is read while settling.
const PROBE_EVERY: Duration = Duration::from_millis(500);

/// What happened in the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Times each planned module was patched in the window.
    pub patched: BTreeMap<String, u64>,
    /// Modules registered with source other than planned.
    pub mismatched: BTreeSet<String>,
    /// The window was already running when Superba attached (its current
    /// document has no patch).
    pub late: bool,
    /// No plan for this build, or why the attach stopped.
    pub reason: Option<String>,
}

/// A page target of the app's window (before navigation its URL is empty).
fn is_app_page(info: &Value) -> bool {
    let kind = info.get("type").and_then(Value::as_str).unwrap_or_default();
    let url = info.get("url").and_then(Value::as_str).unwrap_or_default();
    kind == "page" && (url.is_empty() || url == "about:blank" || url.starts_with("app://-/"))
}

/// The plan, once known: the script, or why there is none.
pub type PlanResult = Result<(Plan, String), NoPlan>;

struct Session {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    next_id: u64,
}

impl Session {
    async fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Option<u64> {
        self.next_id += 1;
        let mut msg = json!({"id": self.next_id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::text(msg.to_string())).await.ok()?;
        Some(self.next_id)
    }
}

/// Attach to the browser endpoint `browser_ws` and register the script of
/// `plan` (resolved by the receiver) in the app's window. Returns when the
/// window settled (see [`SETTLE_LIMIT`]); the connection then stays open in a
/// background task until the app closes it.
pub async fn run(browser_ws: &str, plan: tokio::sync::oneshot::Receiver<PlanResult>) -> Outcome {
    let mut outcome = Outcome::default();
    let ws = match tokio_tungstenite::connect_async(browser_ws).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            outcome.reason = Some(format!("connect: {e}"));
            return outcome;
        }
    };
    let mut s = Session { ws, next_id: 0 };
    let auto_attach =
        |wait: bool| json!({"autoAttach": true, "waitForDebuggerOnStart": wait, "flatten": true});
    if s.send("Target.setAutoAttach", auto_attach(true), None)
        .await
        .is_none()
    {
        outcome.reason = Some("send failed".into());
        return outcome;
    }
    let started = Instant::now();
    let mut plan_rx = Some(plan);
    let mut plan: Option<PlanResult> = None;
    // Pages waiting for the plan: (session id, paused since; `None` when the
    // page was not paused or was resumed without the script).
    let mut held: Vec<(String, Option<Instant>)> = Vec::new();
    // Pages that got the script, with the id of the last state probe sent.
    let mut pages: Vec<String> = Vec::new();
    let mut late_probes: BTreeSet<u64> = BTreeSet::new();
    let mut state_probes: BTreeSet<u64> = BTreeSet::new();
    let mut tick = tokio::time::interval(PROBE_EVERY);

    loop {
        tokio::select! {
            msg = s.ws.next() => {
                let Some(Ok(msg)) = msg else {
                    outcome.reason.get_or_insert_with(|| "the app closed the connection".into());
                    return outcome;
                };
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                    Message::Close(_) => {
                        outcome.reason.get_or_insert_with(|| "the app closed the connection".into());
                        return outcome;
                    }
                    _ => continue,
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if v.get("method").and_then(Value::as_str) == Some("Target.attachedToTarget") {
                    let p = &v["params"];
                    let Some(sid) = p.get("sessionId").and_then(Value::as_str).map(str::to_owned) else { continue };
                    let waiting = p.get("waitingForDebugger").and_then(Value::as_bool) == Some(true);
                    if !is_app_page(&p["targetInfo"]) {
                        if waiting {
                            s.send("Runtime.runIfWaitingForDebugger", json!({}), Some(&sid)).await;
                        }
                        s.send("Target.detachFromTarget", json!({"sessionId": sid}), None).await;
                        continue;
                    }
                    match &plan {
                        Some(Ok((_, script))) => {
                            let script = script.clone();
                            if let Some(id) = register(&mut s, &sid, &script, waiting).await {
                                late_probes.extend(id);
                            }
                            pages.push(sid);
                        }
                        Some(Err(_)) => {
                            if waiting {
                                s.send("Runtime.runIfWaitingForDebugger", json!({}), Some(&sid)).await;
                            }
                            s.send("Target.detachFromTarget", json!({"sessionId": sid}), None).await;
                        }
                        // Not paused: nothing to hold; a reload gets the script once known.
                        None => held.push((sid, waiting.then(Instant::now))),
                    }
                    continue;
                }
                let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
                let value = v.pointer("/result/result/value");
                if late_probes.remove(&id) {
                    if value != Some(&Value::Bool(true)) {
                        outcome.late = true;
                    }
                } else if state_probes.remove(&id)
                    && let Some(state) = value
                        .and_then(Value::as_str)
                        .and_then(|t| serde_json::from_str::<Value>(t).ok())
                {
                    read_state(&state, &mut outcome);
                }
            }
            resolved = async { plan_rx.as_mut().expect("polled only while some").await }, if plan_rx.is_some() => {
                plan_rx = None;
                let resolved = resolved.unwrap_or_else(|_| Err(NoPlan { version: super::plan::PLAN_VERSION, reason: "planning stopped".into() }));
                match &resolved {
                    Ok((_, script)) => {
                        let script = script.clone();
                        for (sid, paused) in std::mem::take(&mut held) {
                            if let Some(id) = register(&mut s, &sid, &script, paused.is_some()).await {
                                late_probes.extend(id);
                            }
                            pages.push(sid);
                        }
                    }
                    Err(no) => {
                        outcome.reason = Some(no.reason.clone());
                        for (sid, _) in std::mem::take(&mut held) {
                            s.send("Runtime.runIfWaitingForDebugger", json!({}), Some(&sid)).await;
                        }
                        s.send("Target.setAutoAttach", json!({"autoAttach": false, "waitForDebuggerOnStart": false}), None).await;
                        let _ = s.ws.close(None).await;
                        return outcome;
                    }
                }
                plan = Some(resolved);
            }
            _ = tick.tick() => {
                // A page held too long runs without the script (it still gets
                // it for later documents once the plan is known).
                for (sid, paused) in &mut held {
                    if paused.is_some_and(|since| since.elapsed() >= HOLD_LIMIT) {
                        *paused = None;
                        s.send("Runtime.runIfWaitingForDebugger", json!({}), Some(sid)).await;
                        outcome.late = true;
                        outcome.reason.get_or_insert_with(|| "the plan took too long".into());
                    }
                }
                if plan.is_none() {
                    continue;
                }
                for sid in pages.clone() {
                    if let Some(id) = s.send(
                        "Runtime.evaluate",
                        json!({"expression": "JSON.stringify(globalThis.__superbaSpeedMenu?.state??null)", "returnByValue": true}),
                        Some(&sid),
                    ).await {
                        state_probes.insert(id);
                    }
                }
                let all_patched = match &plan {
                    Some(Ok((p, _))) => !p.modules.is_empty() && p.modules.iter().all(|m| outcome.patched.contains_key(&m.id)),
                    _ => false,
                };
                if all_patched || outcome.late || started.elapsed() >= SETTLE_LIMIT {
                    s.send("Target.setAutoAttach", auto_attach(false), None).await;
                    // Keep the connection (the script stays registered for
                    // reloads) without blocking the caller.
                    let result = outcome.clone();
                    tokio::spawn(idle(s));
                    return result;
                }
            }
        }
    }
}

/// Send the script and resume the page; when it was not paused, also ask
/// whether its current document already has the script.
async fn register(s: &mut Session, sid: &str, script: &str, waiting: bool) -> Option<Option<u64>> {
    s.send("Page.enable", json!({}), Some(sid)).await?;
    s.send(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": script}),
        Some(sid),
    )
    .await?;
    s.send("Runtime.enable", json!({}), Some(sid)).await?;
    if waiting {
        s.send("Runtime.runIfWaitingForDebugger", json!({}), Some(sid))
            .await?;
        return Some(None);
    }
    let probe = s
        .send(
            "Runtime.evaluate",
            json!({"expression": "Object.prototype.hasOwnProperty.call(globalThis,'__superbaSpeedMenu')", "returnByValue": true}),
            Some(sid),
        )
        .await?;
    Some(Some(probe))
}

fn read_state(state: &Value, outcome: &mut Outcome) {
    if let Some(p) = state.get("patched").and_then(Value::as_object) {
        for (id, n) in p {
            let n = n.as_u64().unwrap_or(0);
            let e = outcome.patched.entry(id.clone()).or_default();
            *e = (*e).max(n);
        }
    }
    if let Some(m) = state.get("mismatched").and_then(Value::as_object) {
        outcome.mismatched.extend(m.keys().cloned());
    }
}

/// Keep reading until the app closes the connection: new targets are attached
/// unpaused and detached again.
async fn idle(mut s: Session) {
    while let Some(Ok(msg)) = s.ws.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
            continue;
        };
        if v.get("method").and_then(Value::as_str) == Some("Target.attachedToTarget")
            && let Some(sid) = v.pointer("/params/sessionId").and_then(Value::as_str)
            && !is_app_page(&v["params"]["targetInfo"])
        {
            let sid = sid.to_owned();
            s.send("Target.detachFromTarget", json!({"sessionId": sid}), None)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_pages_only() {
        assert!(is_app_page(&json!({"type": "page", "url": ""})));
        assert!(is_app_page(
            &json!({"type": "page", "url": "app://-/index.html"})
        ));
        assert!(!is_app_page(
            &json!({"type": "page", "url": "https://example.com/"})
        ));
        assert!(!is_app_page(
            &json!({"type": "service_worker", "url": "app://-/sw.js"})
        ));
    }

    #[test]
    fn state_is_merged() {
        let mut o = Outcome::default();
        read_state(
            &json!({"patched": {"a": 2}, "mismatched": {"b": 10}}),
            &mut o,
        );
        read_state(&json!({"patched": {"a": 1, "c": 1}}), &mut o);
        assert_eq!(
            o.patched,
            BTreeMap::from([("a".into(), 2), ("c".into(), 1)])
        );
        assert_eq!(o.mismatched, BTreeSet::from(["b".to_owned()]));
    }
}
