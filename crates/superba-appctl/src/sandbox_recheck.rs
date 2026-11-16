//! Windows: run the desktop app's Agent sandbox check again when an app-server
//! restart left it failed.
//!
//! ChatGPT 26.1111 (Windows, checked against the official 26.1111.2228.0
//! package) asks its app-server `windowsSandbox/readiness` once and keeps the
//! answer (`staleTime` infinite, `retry: false`). After every
//! `codex-app-server-restart` IPC (Superba's hot switch, but also the app's own
//! first-start Windows setup and its MCP settings restart button) its restart
//! recovery asks again at once, while the app's main process is still
//! re-reading the account of the new connection: the call often fails with
//! "Hosted AppServerManager identity is not ready" (4 of 6 restarts through
//! the IPC on a Windows 11 VM, 5 of 5 with `intent: 'reconnect'`, every Superba
//! hot switch tried; never after the app-server was killed by hand). The failure
//! sticks: the composer shows "Couldn't check Windows setup" and refuses to
//! send ("Couldn't check Agent sandbox status") until the user clicks
//! "Try again", which only fetches the check again.
//!
//! Superba does what "Try again" does: a small script in the app's window
//! watches the app's query cache for a while and fetches a failed readiness
//! check again (at most [`MAX_RETRIES`] times, never a check that worked). It
//! runs in the page on its own, so the caller does not wait for it; a second
//! call extends the running watch. Nothing happens when the app has no such
//! check (other platforms, other builds) or its query cache cannot be found.

use std::time::Duration;

/// How long the watch runs after a hot restart.
pub const AFTER_RESTART: Duration = Duration::from_secs(60);
/// How long the watch runs after Superba opened the app: the app's first-start
/// Windows setup restarts its app-server itself once the setup was confirmed.
pub const AFTER_LAUNCH: Duration = Duration::from_secs(600);
/// Fetches of a failed check per watch.
pub const MAX_RETRIES: u32 = 5;
/// The app's query key of the readiness check (followed by the host id).
pub const READINESS_QUERY_KEY: [&str; 2] = ["windows-sandbox", "readiness"];

/// True where the app has the check (its renderer asks it only on Windows).
pub const fn wanted() -> bool {
    cfg!(windows)
}

/// The script that starts (or extends) the watch for `window`; it evaluates to
/// `"started"` or `"extended"` at once.
pub fn watch_js(window: Duration) -> String {
    let ms = window.as_millis();
    let key = serde_json::to_string(&READINESS_QUERY_KEY).unwrap_or_default();
    format!(
        r#"(() => {{
  const w = window, key = {key}, until = Date.now() + {ms};
  const running = w.__superbaSandboxRecheck;
  if (running) {{
    running.until = Math.max(running.until, until);
    running.left = {MAX_RETRIES};
    return "extended";
  }}
  const s = w.__superbaSandboxRecheck = {{ until, left: {MAX_RETRIES}, client: null }};
  const findClient = () => {{
    const root = document.getElementById("root");
    const k = root && Object.keys(root).find((k) => k.startsWith("__reactContainer$"));
    if (!k) return null;
    const stack = [root[k]];
    for (let n = 0; stack.length > 0 && n < 100000; n += 1) {{
      const f = stack.pop();
      if (!f) continue;
      const c = f.memoizedProps && f.memoizedProps.client;
      if (c && typeof c.getQueryCache === "function" && typeof c.refetchQueries === "function") return c;
      if (f.sibling) stack.push(f.sibling);
      if (f.child) stack.push(f.child);
    }}
    return null;
  }};
  const failed = (q) => q.state.status === "error" && q.state.fetchStatus === "idle";
  const tick = () => {{
    if (Date.now() > s.until) {{
      delete w.__superbaSandboxRecheck;
      return;
    }}
    try {{
      const c = s.client || (s.client = findClient());
      if (c && s.left > 0 && c.getQueryCache().findAll({{ queryKey: key }}).some(failed)) {{
        s.left -= 1;
        c.refetchQueries({{ queryKey: key, type: "all", predicate: failed }}).catch(() => {{}});
      }}
    }} catch (e) {{}}
    setTimeout(tick, 1000);
  }};
  setTimeout(tick, 1000);
  return "started";
}})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_targets_only_the_readiness_query() {
        let js = watch_js(Duration::from_secs(60));
        assert!(
            js.contains(r#"key = ["windows-sandbox","readiness"]"#),
            "{js}"
        );
        assert!(js.contains("Date.now() + 60000"));
        assert!(js.contains(r#"q.state.status === "error""#));
        assert!(js.contains(&format!("left: {MAX_RETRIES}")));
        assert!(js.contains("predicate: failed"));
        // Returns at once; the watch runs in the page.
        assert!(js.contains(r#"return "started";"#));
        assert!(js.contains(r#"return "extended";"#));
        assert!(!js.contains("await "));
    }

    #[test]
    fn braces_are_balanced() {
        let js = watch_js(AFTER_LAUNCH);
        let open = js.matches('{').count();
        let close = js.matches('}').count();
        assert_eq!(open, close, "{js}");
        assert!(js.contains("Date.now() + 600000"));
    }

    #[test]
    fn only_on_windows() {
        assert_eq!(wanted(), cfg!(windows));
    }
}
