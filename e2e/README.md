# Superba end-to-end harness

Drives the real Linux desktop app (ChatGPT/Codex Electron app), the real Codex
binaries and the `spb` / `superba` / `superba-router` release binaries through
every core flow: takeover of a legacy home, launch with
CDP, hot switches of line and auth mode, busy-turn handling, the
unhealthy-auth-origin full restart, the CLI, Codex settings from the catalog, a
custom OpenAI-compatible site, the local router (failover, stickiness, watchdog),
account data from the standard endpoints, every GUI page in both themes,
languages and window sizes, restore of the original configuration and the
remote-host integration test, plus the setup wizard with the Krill
preset in a fresh home, model lists and the signed feeds through raced mirrors,
per-model context overrides through a model catalog file, Fast / Ultrafast in
API-key direct mode, the request log and the stats line under replies, pinning
the app's Codex (`CODEX_CLI_PATH`), a remote host in Docker (Codex install,
version-filtered settings sync, sessions per host) and the app update check.

Everything runs against an **isolated home** inside an **unprivileged user + PID
namespace**; the real `~/.codex`, shell rc files, keyring, service managers and
running apps are never touched, and desktop apps of other sessions are invisible.

## Requirements

- Linux with unprivileged user namespaces, `unshare` / `nsenter` (util-linux),
  `Xvfb`, `xdotool`, `scrot`, `openssl`, Rust (stable), Docker (for scenario 9)
  and Python 3 with `aiohttp`, `websockets` (plus `zstandard` if Codex compresses
  request bodies and Pillow to crop the GUI screenshots).
- An extracted Linux desktop app (a directory with `ChatGPT` and
  `resources/codex`).
- A Krill-compatible test relay reachable over https and a key for it (the
  stats-line and tier scenarios expect it to map the `-fast` / `-ultrafast`
  model-name suffixes to speed tiers).
- The feed signing key of the repository (`.secrets/catalog-signing.key`, or
  `SUPERBA_E2E_SIGNING_KEY`): s17 serves a newer feed signed with it.
- Docker with the image `superba-remote-it:v1` (built from
  `crates/superba-remote/tests/docker_it.rs` when missing) for s9 and s22.

## Private settings

Create an env file outside the repository (or under the gitignored `.secrets/`):

```sh
SUPERBA_E2E_UPSTREAM=https://relay.example.test   # test relay origin
SUPERBA_E2E_KEY=...                               # key for that relay
SUPERBA_E2E_APP_DIR=/path/to/extracted/app        # contains ChatGPT, resources/codex
```

Only these three keys are read; values are never printed or written to tracked
files. Optional variables from the environment: `SUPERBA_E2E_DISPLAY` (default
`:176`), `SUPERBA_E2E_PKGCACHE` (where the Codex release packages of s21/s22 are
cached, default `e2e/.work/pkgcache`; a missing package is downloaded from GitHub
once and checked against its `SHA256SUMS`) and `SUPERBA_E2E_SIGNING_KEY`.

## Running

```sh
SUPERBA_E2E_ENV=/path/to/e2e.env e2e/run.sh              # build release binaries, run all
SUPERBA_E2E_ENV=/path/to/e2e.env e2e/run.sh --no-build   # reuse the binaries next to $SPB_BIN
```

`run.sh` builds into the shared `target-v2-shared` directory (with the
repository's disk-policy settings) unless `CARGO_TARGET_DIR` is set; the driver
copies `spb`, `superba` and `superba-router` (and `superba-catalog-gen` for the
features cross-check and the feed signing) into the run's `sbin/` before it
starts, so a concurrent build cannot replace them mid-run. A full run takes about
90 minutes. When other work changes the working tree meanwhile, run from a
detached worktree of the commit under test (`git worktree add --detach`) with
`CARGO_TARGET_DIR` pointing at the shared target directory. The result is
`e2e/out/<run>/report.md` (and `report.json`) with PASS/FAIL per scenario and its
evidence; next to it are the forwarder logs (`line-a.jsonl` ...), the mock relay
log (`mockrelay.jsonl`), timestamped `spb` transcripts (`transcripts/`), file
diffs and saved outputs (`evidence/`), screenshots (`screens/`) and every command
with its output (`cmds.log`).

Step by step (state is kept in `e2e/.work/current.json`; `setup` starts a holder
namespace and every later step enters it, see below):

```sh
SUPERBA_E2E_ENV=... e2e/run.sh step setup        # namespace, CA, forwarders, mock relay, Xvfb, isolated home
SUPERBA_E2E_ENV=... e2e/run.sh step s1           # any scenario
SUPERBA_E2E_ENV=... e2e/run.sh step spb status   # spb inside the run's environment
SUPERBA_E2E_ENV=... e2e/run.sh step cdp probe    # renderer state (cdp.py)
SUPERBA_E2E_ENV=... e2e/run.sh step exec CMD...  # any command in the run's namespace and environment
SUPERBA_E2E_ENV=... e2e/run.sh step teardown     # also ends the namespace (and everything left in it)
```

## How it is wired

- **Namespace.** The driver runs itself, the desktop app, `superba`, `spb`, the
  router, the forwarders and the mock relay inside an unprivileged user + PID
  namespace (`unshare --user --map-current-user --pid --fork --mount-proc`,
  `lib/nsinit.py` as its init, which reaps orphans). Desktop apps and Codex
  processes of other sessions on the machine are invisible there, so Superba never
  lists, probes or signals them, and everything the run started dies with the
  namespace. The uid stays the same on purpose: Superba never gives a root-owned
  app a debugging port, so `--map-root-user` would turn hot switching off. `all`
  creates a namespace for the whole run; `step setup` starts a holder namespace
  (`e2e/.work/ns-holder.json`) and the other steps enter it with `nsenter`.
- **Isolated home.** `HOME=/tmp/sbe2e-<n>`, a symlink to `e2e/.work/<run>/home`
  (the app's IPC socket path must stay under 108 bytes). Every child process gets
  an environment built from scratch (no inherited `CODEX_*`, `XDG_*`, DBus or
  proxy variables), `SUPERBA_SECRET_BACKEND=file` and Chromium's
  `--password-store=basic`, so neither the keyring nor the real Codex home is
  reached. `XDG_RUNTIME_DIR` is a private directory of the run. `PATH` starts with
  the run's `bin/`: `codex` (the app's bundled CLI, so the CLI consumer has a known
  version), a `systemctl` that always fails, so the router service can only use
  its XDG autostart + supervisor fallback inside the isolated home and never
  reaches a real systemd user manager, and `sudo` / `pkexec` / `doas` stand-ins
  that only record a call (s23 asserts there is none). During s22 an `ssh` wrapper
  adds `-F ~/.ssh/config` of the isolated home: OpenSSH takes `~` from the password
  database, so it would otherwise read the real `~/.ssh`.
- **Lines.** Five logging forwarders (`lib/linefwd.py`) play lines A, B and C of
  the Krill-kind test site, D, the custom compatible site of scenario 12, and E,
  the Krill-kind site whose relay maps tier-suffixed model names (s19), on
  `https://127.0.0.1:<random port>` and forward to `SUPERBA_E2E_UPSTREAM`. They
  serve TLS with a throwaway CA (`lib/mkcert.sh`); Codex and Superba trust it via
  `SSL_CERT_FILE` / `CODEX_CA_CERTIFICATE` pointing at a bundle of the system roots
  plus that CA. With https lines the PAT mode works end to end, including the
  launch-time auth origin, so stopping a forwarder really takes an auth origin
  down (and a router line). Each request is logged as one JSON line (path, status,
  whether the PAT header `chatgpt-account-id` was present, originator, the
  conversation key `session-id`, model, number of input items, start of the last
  user message, the requested `service_tier` and whether it was sent, whether a
  `prompt_cache_key` was sent, and how many stats-line markers the input still
  carries); no other header values and no bodies are logged. WebSocket
  upgrades get 426 so turns stay visible as HTTP requests. For a turn whose prompt
  starts with `E2E-LONG` the forwarder holds back the end of the stream
  (`response.completed`) until 40 s after the request started
  (`LINEFWD_HOLD_SECS`), so busy-turn scenarios do not depend on how long the
  model's answer is. A per-line fault file (`.work/<run>/fault-<line>`, e.g.
  `401`) makes a forwarder answer with that status instead of forwarding.
- **Mock relay.** `crates/superba-gui/tools/screenshots/mockrelay.py` serves the
  account endpoints (billing subscription / usage, organization usage and costs,
  whoami) on two https lines of a second Krill-kind site `krill-mock`, with a fake
  key only it knows; `--fault-file` (`.work/<run>/mock-faults.json`, read per
  request) makes single endpoints answer 404 or 429.
- **Test sites.** `SUPERBA_TEST_SITES` points at a generated `sites.json` with
  `krill-e2e` (lines A, B, C), `krill-sfx` (line E, `tier_model_suffix`) and
  `krill-mock`.
- **Feeds and packages.** `lib/feedmirror.py` runs four https feed mirrors
  (`m1`–`m4`, each on its own port, so the race remembers the winner by name) with
  per-request fault modes (`404`, an HTML landing page, a stale lower serial, the
  valid newer serial, truncated files, dropped connections) and an app download
  fixture (an APT `Packages` index and a small package) that
  `SUPERBA_APP_DOWNLOADS` points at; the newer feed is the repository's
  `catalog/feeds` with that fixture build added, re-indexed and signed at setup.
  `SUPERBA_FEED_MIRRORS` is set from s17 on. `lib/pkgcache.py` serves Codex
  release packages from the package cache (`SUPERBA_CODEX_PACKAGES`), with a
  `corrupt` mode (one byte flipped per MiB, same size).
- **Legacy home.** `lib/legacy.py` writes a legacy Krill config
  (`model_provider = "krill"` custom provider, comments, an MCP server, a
  `[projects]` table, the legacy feature name `[features] collab`) and lets the
  real Codex create threads under it: two through `codex app-server`
  (`lib/codex_as.py`, listed by the app) and one through `codex exec`.
- **App automation.** `lib/cdp.py` talks to the main renderer over the debugging
  port Superba opened: it answers the onboarding (Engineering → Continue → Skip →
  Go to ChatGPT), types turns into the composer (any draft is cleared first, then
  `Input.insertText` + Enter), opens new chats, opens the model picker and reads
  the sidebar/composer state; `codex_as.py --model-list` reads the app-server's
  `model/list` from the same Codex and home.
- **Wizard.** s16 runs Superba and a second desktop app in a fresh home inside a
  nested user + PID namespace (so neither sees the run's app); the Krill preset's
  production lines are made unreachable through a dead proxy and the run's lines
  A and B are added as its test lines (`SUPERBA_TEST_LINES`), so the key is never
  sent to a production line. The GUI is driven with `xdotool` (enabled primary
  buttons are found on the screenshot); the key is typed from a 0600 file, and
  only after the keyboard focus and the pointer are verified to be on the Superba
  window.
- **Router.** The scenarios read the router's state from its admin API
  (`/_superba/v1/status`, admin token from the 0600 `runtime.json`) and send
  direct requests with the router token from `auth.json`; neither is printed.
- **Display.** Xvfb on `:176` (`SUPERBA_E2E_DISPLAY` to use another free one of
  `:171`–`:179`). GUI screenshots are cropped to the Superba window.
- **Cleanup.** Only processes this run started are signalled, by pid (namespace
  pids): the forwarders, the mock relay, Xvfb, the GUI, the router, and the app's
  process tree; ending the namespace removes anything left.

## Scenarios

| # | What it proves |
|---|---|
| 1 | First takeover of a legacy home with a profile (Automatic auth, lines A > B): baseline ("Original") captured, legacy sessions migrated to `openai` (sqlite + rollouts), Automatic resolves to PAT on line A, Krill's recommended settings written; untouched keys, tables, comments and the legacy feature key preserved; rc marker blocks added next to the user's content. |
| 2 | `spb launch` starts the app with CDP and `CODEX_AUTHAPI_BASE_URL=<line A>`; PAT login (whoami, `wham/accounts/check`) passes; turn 1 reaches line A; migrated threads are listed in recents. |
| 3 | `spb switch --line b`: hot restart (main pid unchanged, new app-server), turn 2 in the same thread reaches line B with the full history; timing of the restart. |
| 4 | `spb switch --auth apikey` (hot; no `chatgpt-account-id` header; the profile's settings stay) and back to PAT. |
| 5 | A busy turn holds the switch: without flags it is cancelled with zero writes, `--wait-turn` waits for the turn to finish, `--force` proceeds and the turn is aborted; UI state afterwards. |
| 8 | The bundled `codex exec`, started from a login shell in the isolated home, picks up Superba's rc block and hits the current line. |
| 6 | The app's launch-time auth origin (line A) is stopped; the planner requires a full restart (with the API-key alternative), which relaunches the app with CDP and the new auth origin. |
| 11 | Codex settings: the catalog of the bundled Codex (`0.162.0-alpha.17.2`), `spb settings set` of two stable keys and a feature flag, a key only newer Codex knows refused (stored with `--force`, then skipped with its reason), the legacy `features.collab` migration, `spb settings diff`, a hot switch that writes them, `config.toml`, `codex features list` and its cross-check against the catalog. |
| 12 | Custom compatible site: `spb site add` with a model map, a profile with its own key, `spb site probe --responses`, the hot switch (API key, mapped model written), turns through forwarder D, back to Krill. |
| 13 | Local router: `spb router install` / `enable` (hot), uninstall refused while routed, turns through the router with stickiness, failover between turns and mid-turn without an app restart, no flapping back, a 401, 403 and 429 from a line passed back without failover (once that line's breaker is no longer open), `spb router disable` (hot, direct), the supervisor restarting a killed router, and `spb router watch` falling back to direct mode after the 20 s grace when the router stays down; no key or token in the router log. |
| 14 | Account data from the mock: `spb account` (whoami, billing) and `--usage` (costs, usage), the disk cache, each endpoint answering 404 hides only its part, a 429 shows the saved report with a rate-limit note, the site's retry time and when that report was fetched. |
| 17 | Model lists: the Krill-kind line's Codex-native `/backend-api/codex/models?client_version=…` and the custom site's OpenAI `/v1/models`; the signed feeds through four raced mirrors (404, HTML page, stale serial, valid newer serial): the newest valid serial wins and the winner is tried first next time; all mirrors down = the cache; no cache = the compiled-in snapshot; the Models page with its source strip. |
| 18 | Per-model context window and compaction: `spb models set` writes `<CODEX_HOME>/superba/models-<sha8>.json` (content-addressed) and `model_catalog_json`, the bundled Codex accepts it (`codex debug models`, app-server `model/list`), a turn's rollout shows the 95 % effective window; a Krill API-key profile gets its own file (with the fingerprint warning); `models reset --all` removes the key and the next catalog switch prunes the file; a terminal Codex that refuses the file: the start-up check rewrites without it and a switch that would write it again is blocked with zero writes. |
| 19 | Speed tiers in API-key direct mode: `service_tier = "fast"` reaches `codex exec` as `priority`; the app opened in API-key mode sends no tier (an app opened in PAT mode and hot-switched keeps sending the configured tier); Ultrafast through the catalog (astra only); tier-suffixed model names on `krill-sfx` give the app its tier through the model name (picker shows "… Ultrafast"); Standard = `service_tier = "default"`, no tier sent; the official login leaves it to the app. |
| 20 | Request log and stats line (router on, real app): the reply ends with `⚡ <n> tok/s · 首字 <s> · 本轮 <in> in / <out> out`, the rollout keeps it, the next request carries no marker; a tool loop (with an approval) gets one line on the final answer with the turn's totals; `codex exec -o` gets none; `spb usage log`/`stats` numbers (the router's rows of the log merged with Codex's session files); the timing marks of every completed routed request in order (sent, headers, first event, first output, last delta, end; 0 ms phases allowed) with TPS from the first event to the end, and each stats line showing the numbers of the request it was added to; the Usage page with session names and the detail pane (通用 and the 计时 tab); request content is never recorded; no key, router token or admin token in the request log or the router log. |
| 21 | `CODEX_CLI_PATH` pin: a corrupted package from the package source is rejected, the verified one installed into the store; `spb codex pin` = full restart, the app-server runs the store binary, a turn's `session_meta.cli_version` matches; the Updates page; `spb codex unpin`; an inherited `CODEX_CLI_PATH` without a pin is ignored. |
| 22 | Remote host in Docker (registered in the app's own `.codex-global-state.json` while the app is closed, plus an ssh-config-only host): only the app's host is listed; probe (OS/arch, no codex); `spb remote codex <host> install` over the SSH session's stdin; `remote sync --dry-run` skips the 0.163-only key for the host's 0.162.0, `--yes` writes the rest; follow off leaves the host byte-identical, follow on changes only connection keys; sessions per provider on the host; Remote hosts / Sessions pages; rollback brings the host's own command back. |
| 23 | App update check: installed build and bundled Codex, latest build from the local APT index with its bundled Codex from the signed feed, `spb app download` verified against the index, the command shown (never run). |
| 10 | GUI in the real runtime: the ten pages in dark/light × en/zh-CN at 1280×800 and 1440×900, the Router page with a live router, the switch overlay with the plan's settings rows (palette switch, 10 s countdown) and the GUI's hot switch. Screenshots are meant to be looked at. |
| 16 | Setup wizard in a fresh home (nested namespace): first start opens it, "Skip setup" writes nothing, Krill pre-selected, key typed, lines with latency and PAT availability (production lines unreachable by design), Apply = profile + switch, "Open ChatGPT with Superba" launches the app with CDP and a PAT turn on the chosen line; re-run from Preferences (another line) and from the palette (运行设置向导) edit the one Krill profile. |
| 7 | `spb restore original --provider reverse-migrate`: config lines and values, `auth.json` and rc files back to the pre-takeover bytes (keys the app itself added are kept), sessions back on `krill`. |
| 9 | `superba-remote`'s Docker sshd integration test. The app-level remote restart is out of scope (needs an app connected to an SSH host). |
| 15 | Tracked tree: no demo mode, demo data or demo wording, no user-facing "context management" wording of Superba's own (the official upstream description of the Codex feature `features.context_management` in the catalog is allowed), no account-summary endpoint, no internal hosts, no "for Krill" brand string; no key or test-relay host bytes in the tracked tree or the release binaries. |
