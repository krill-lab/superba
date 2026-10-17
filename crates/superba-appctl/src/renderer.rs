//! Read-only renderer probes and the "return to thread" navigation (plan 7.2
//! check 4, 7.4 signal C, 7.5).
//!
//! The DOM hooks are data ([`DomHooks`], serialized into the evaluated script) so a
//! new app build that renames something needs a data change, not new logic.
//! Observed on app 26.1007 (Linux):
//!
//! - open thread: `[data-above-composer-conversation-id]` holds its id;
//! - sidebar rows: `[data-app-action-sidebar-thread-row]` with
//!   `data-app-action-sidebar-thread-{id,host-id,title,selected}`; a running thread
//!   shows a spinner (`role="status"`, `aria-label="Working"`, `animate-spin`);
//! - running turn in the open thread: composer button `aria-label="Stop"`;
//! - login page: "Sign in to ChatGPT" / "Continue to sign in";
//! - the router is in-memory (`location.href` stays `app://-/index.html`); the app
//!   main process navigates the renderer with a `{type:"navigate-to-route", path}`
//!   message delivered as a window `message` event, and local threads live at
//!   `/local/<conversation id>`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use superba_core::codex::js_string;
use superba_core::{RendererState, TargetId, ThreadActivity, ThreadRef};

/// Selectors and labels used by the renderer scripts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomHooks {
    /// Element carrying the open conversation id.
    pub conversation: Vec<String>,
    /// Attribute holding it.
    pub conversation_attr: String,
    /// Sidebar thread rows.
    pub thread_row: String,
    /// Row attributes.
    pub row_id_attr: String,
    /// Row host id attribute.
    pub row_host_attr: String,
    /// Row title attribute.
    pub row_title_attr: String,
    /// Row "selected" attribute.
    pub row_selected_attr: String,
    /// Selectors that mark a row as running (any match).
    pub row_busy: Vec<String>,
    /// Stop-button selectors (any visible match = a turn runs in the open thread).
    pub stop_button: Vec<String>,
    /// Composer selectors (present when signed in).
    pub composer: Vec<String>,
    /// Texts that identify the login page (body text contains any).
    pub login_texts: Vec<String>,
    /// Route prefix of local threads.
    pub local_thread_route: String,
    /// Host message type that navigates the renderer.
    pub navigate_message: String,
}

impl Default for DomHooks {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        Self {
            conversation: s(&["[data-above-composer-conversation-id]"]),
            conversation_attr: "data-above-composer-conversation-id".into(),
            thread_row: "[data-app-action-sidebar-thread-row]".into(),
            row_id_attr: "data-app-action-sidebar-thread-id".into(),
            row_host_attr: "data-app-action-sidebar-thread-host-id".into(),
            row_title_attr: "data-app-action-sidebar-thread-title".into(),
            row_selected_attr: "data-app-action-sidebar-thread-selected".into(),
            row_busy: s(&[
                "[role=\"status\"][aria-label=\"Working\"]",
                "[class*=\"animate-spin\"]",
                "[aria-busy=\"true\"]",
            ]),
            stop_button: s(&[
                "button[aria-label=\"Stop\"]",
                "button[aria-label=\"Stop generating\"]",
                "button[aria-label=\"停止\"]",
                "button[aria-label=\"停止生成\"]",
                "[data-testid=\"stop-button\"]",
            ]),
            composer: s(&[
                "[contenteditable=\"true\"]",
                "textarea",
                "[data-above-composer-conversation-id]",
            ]),
            login_texts: s(&[
                "Sign in to ChatGPT",
                "Continue to sign in",
                "Log in to ChatGPT",
                "登录 ChatGPT",
            ]),
            local_thread_route: "/local/".into(),
            navigate_message: "navigate-to-route".into(),
        }
    }
}

/// Raw result of [`probe_js`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DomProbe {
    /// `location.href`.
    #[serde(default)]
    pub url: String,
    /// Open conversation id.
    #[serde(default)]
    pub conversation: Option<String>,
    /// A stop button is visible.
    #[serde(default)]
    pub stop: bool,
    /// A composer exists.
    #[serde(default)]
    pub composer: bool,
    /// A login marker text was found.
    #[serde(default)]
    pub login: bool,
    /// Sidebar rows.
    #[serde(default)]
    pub rows: Vec<DomRow>,
}

/// One sidebar thread row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DomRow {
    /// Row id (`local:<id>` / `local:client-new-thread:<uuid>` / remote ids).
    #[serde(default)]
    pub id: Option<String>,
    /// Host id (`local` or a remote hostId).
    #[serde(default)]
    pub host: Option<String>,
    /// Title.
    #[serde(default)]
    pub title: Option<String>,
    /// Selected (open) row.
    #[serde(default)]
    pub selected: bool,
    /// Shows a running indicator.
    #[serde(default)]
    pub busy: bool,
}

/// Script returning a [`DomProbe`] (read-only).
pub fn probe_js(hooks: &DomHooks) -> String {
    let cfg = serde_json::to_string(hooks).unwrap_or_else(|_| "{}".into());
    format!(
        r#"(() => {{
  const cfg = {cfg};
  const all = (sel) => {{ try {{ return [...document.querySelectorAll(sel)]; }} catch (e) {{ return []; }} }};
  const any = (sels) => sels.some((s) => all(s).length > 0);
  const visible = (el) => !!(el && (el.offsetParent !== null || el.getClientRects().length > 0));
  let conversation = null;
  for (const s of cfg.conversation) {{
    const el = all(s)[0];
    const v = el && el.getAttribute(cfg.conversation_attr);
    if (v) {{ conversation = v; break; }}
  }}
  const stop = cfg.stop_button.some((s) => all(s).some(visible));
  const rows = all(cfg.thread_row).map((r) => ({{
    id: r.getAttribute(cfg.row_id_attr),
    host: r.getAttribute(cfg.row_host_attr),
    title: r.getAttribute(cfg.row_title_attr) || r.getAttribute('aria-label'),
    selected: r.getAttribute(cfg.row_selected_attr) === 'true',
    busy: cfg.row_busy.some((s) => {{ try {{ return !!r.querySelector(s); }} catch (e) {{ return false; }} }}),
  }}));
  const text = (document.body && document.body.innerText) || '';
  const composer = any(cfg.composer);
  const login = !composer && cfg.login_texts.some((t) => text.includes(t));
  return {{ url: String(location.href), conversation, stop, composer, login, rows }};
}})()"#
    )
}

/// Parse the value returned by [`probe_js`].
pub fn parse_probe(v: &Value) -> Option<DomProbe> {
    serde_json::from_value(v.clone()).ok()
}

fn row_thread_id(row_id: &str) -> String {
    // `local:<id>`, `local:client-new-thread:<uuid>`, `<host>:<id>`: keep the last part.
    row_id.rsplit(':').next().unwrap_or(row_id).to_owned()
}

/// Renderer state from a probe.
pub fn renderer_state(p: &DomProbe) -> RendererState {
    let title = p
        .rows
        .iter()
        .find(|r| r.selected)
        .and_then(|r| r.title.clone());
    RendererState {
        url: p.url.clone(),
        on_login_page: p.login,
        on_home: p.composer && p.conversation.is_none(),
        current_thread: p.conversation.as_ref().map(|id| ThreadRef {
            thread_id: id.clone(),
            title,
        }),
    }
}

/// Give busy threads found in rollout files (which carry no title) the sidebar
/// title of the same local thread, so "the conversation is running" messages name
/// it. The open thread's row may still have a client id
/// (`local:client-new-thread:<uuid>`); it is matched through the open conversation.
pub fn fill_titles(busy: &mut [ThreadActivity], p: &DomProbe) {
    for t in busy
        .iter_mut()
        .filter(|t| t.title.is_none() && t.target_id == TargetId::local())
    {
        let Some(id) = t.thread_id.as_deref() else {
            continue;
        };
        let row = p.rows.iter().find(|r| {
            r.host
                .as_deref()
                .is_none_or(|h| h == superba_core::codex::LOCAL_HOST_ID)
                && (r.id.as_deref().map(row_thread_id).as_deref() == Some(id)
                    || (r.selected && p.conversation.as_deref() == Some(id)))
        });
        if let Some(title) = row.and_then(|r| r.title.clone()).filter(|x| !x.is_empty()) {
            t.title = Some(title);
        }
    }
}

/// Busy threads seen in the renderer (signal C). Rows of remote hosts map to
/// `TargetId::remote(host id)`.
pub fn busy_threads(p: &DomProbe) -> Vec<ThreadActivity> {
    let mut out: Vec<ThreadActivity> = Vec::new();
    for r in p.rows.iter().filter(|r| r.busy) {
        let host = r.host.clone().unwrap_or_else(|| "local".into());
        let target_id = if host == superba_core::codex::LOCAL_HOST_ID {
            TargetId::local()
        } else {
            TargetId::remote(&host)
        };
        let thread_id = if r.selected {
            p.conversation.clone()
        } else {
            r.id.as_deref().map(row_thread_id)
        };
        out.push(ThreadActivity {
            target_id,
            thread_id,
            title: r.title.clone(),
            rollout_path: None,
            last_event_at: None,
            signal: superba_core::ActivitySignal::RendererDom,
        });
    }
    if p.stop
        && !out
            .iter()
            .any(|t| t.thread_id.is_some() && t.thread_id == p.conversation)
    {
        let selected = p.rows.iter().find(|r| r.selected);
        let host = selected
            .and_then(|r| r.host.clone())
            .unwrap_or_else(|| "local".into());
        out.push(ThreadActivity {
            target_id: if host == superba_core::codex::LOCAL_HOST_ID {
                TargetId::local()
            } else {
                TargetId::remote(&host)
            },
            thread_id: p.conversation.clone(),
            title: selected.and_then(|r| r.title.clone()),
            rollout_path: None,
            last_event_at: None,
            signal: superba_core::ActivitySignal::RendererDom,
        });
    }
    out
}

/// Remote host ids visible in the renderer (sidebar rows of non-local hosts).
pub fn remote_host_ids(p: &DomProbe) -> Vec<String> {
    let mut ids: Vec<String> = p
        .rows
        .iter()
        .filter_map(|r| r.host.clone())
        .filter(|h| h != superba_core::codex::LOCAL_HOST_ID && !h.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// True if `id` is safe to place in a route (ids are UUID-like).
pub fn valid_thread_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Script that navigates to a local thread and resolves to `"already"`, `"route"`,
/// `"click"` or `"failed"`. Uses the app's own `navigate-to-route` host message,
/// falling back to clicking the sidebar row with that id or title.
pub fn navigate_js(hooks: &DomHooks, thread: &ThreadRef) -> String {
    let cfg = serde_json::to_string(hooks).unwrap_or_else(|_| "{}".into());
    let id = js_string(&thread.thread_id);
    let title = thread
        .title
        .as_deref()
        .map(js_string)
        .unwrap_or_else(|| "null".into());
    format!(
        r#"(async () => {{
  const cfg = {cfg};
  const id = {id};
  const title = {title};
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const cur = () => {{
    for (const s of cfg.conversation) {{
      try {{
        const el = document.querySelector(s);
        const v = el && el.getAttribute(cfg.conversation_attr);
        if (v) return v;
      }} catch (e) {{}}
    }}
    return null;
  }};
  const waitFor = async () => {{ for (let i = 0; i < 30; i++) {{ if (cur() === id) return true; await sleep(100); }} return cur() === id; }};
  if (cur() === id) return 'already';
  window.dispatchEvent(new MessageEvent('message', {{ data: {{ type: cfg.navigate_message, path: cfg.local_thread_route + id }} }}));
  if (await waitFor()) return 'route';
  const rows = [...document.querySelectorAll(cfg.thread_row)];
  const row = rows.find((r) => (r.getAttribute(cfg.row_id_attr) || '').endsWith(id))
    || (title && rows.find((r) => r.getAttribute(cfg.row_title_attr) === title));
  if (row) {{ row.click(); if (await waitFor()) return 'click'; }}
  return 'failed';
}})()"#
    )
}
