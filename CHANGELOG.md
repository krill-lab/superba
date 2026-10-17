# Changelog

All notable changes to Superba are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/): later releases increment the patch
number; the minor number changes only for breaking changes.

## [0.1.0] - Unreleased

First public version: a desktop launcher (`superba`, egui), a headless CLI (`spb`)
and an opt-in local router (`superba-router`) that point OpenAI Codex — the
ChatGPT / Codex desktop app, the `codex` CLI and the SSH hosts the app is
connected to — at the Krill relay, at any OpenAI-compatible Responses API site, or
back to the official ChatGPT login.

### Switching and connections

- **Hot switching** of the line and the auth mode while the desktop app keeps
  running: Superba rewrites the keys it owns in `config.toml` / `auth.json` and
  asks the app (over its loopback DevTools port) to restart its Codex core. The
  window, the open conversation and the recents list stay; the next turn goes to
  the new line with its full history (about 1 s on Linux).
- **Restart planning**: hot restart, full app restart (cross-auth-family
  changes, an unusable launch-time auth address, session migrations, an app not
  started by Superba) or no restart, computed before anything is written and
  shown with a cancellable countdown (Esc / Ctrl-C).
- **Providers**: a provider is a site, its lines in priority order (the next
  line is used when the first is unusable), the auth choice, the key, the model
  and the Codex settings Superba manages for it. Adding, renaming, duplicating
  or editing a provider that is not in use only saves it; switching is a separate
  action, and a restart is offered only when the applied provider changes. The
  Providers page has a list with a context menu, inline rename, Test and Delete
  (confirmed), and an add-provider dialog and first-run wizard that share the same
  option cards.
- **Import from cc-switch and AI Toolbox**: Providers > Add > Import from other
  tools, the first-run wizard, the command palette and `spb import ccs|ai-toolbox
  [--dry-run] [--json] [--select NAMES] [--on-conflict skip|rename|update]` read
  the Codex providers of cc-switch (`cc-switch.db` schemas 1 to 20, `config.json`
  v1 and v2) and AI Toolbox (`ai-toolbox.db` schemas 4 to 29) without writing to
  them (databases with pending changes are read from a private copy). A Krill line
  becomes the Krill preset on that line, any other address a custom site with its
  lines, model and model mapping; keys go into Superba's key storage. A preview
  per tool shows each target, whether the key comes along (a key in the keychain
  or an environment variable stays behind, with a note), and conflicts with
  existing providers (skip by default, rename or update). Non-Codex providers,
  ChatGPT logins and proxy-only providers are listed with the reason; unknown
  versions are skipped with one. Nothing is written to Codex and nothing restarts.
- **Automatic authentication** for Krill keys: a personal access token (PAT,
  `CODEX_AUTHAPI_BASE_URL` persisted in shell rc blocks, `environment.d`,
  LaunchAgent or `HKCU\Environment`) where the line supports it, else a plain
  API key, always with the reason: a line whose edge blocks the PAT endpoints,
  a plain-http line, a custom base URL, router mode, or an installed Codex (the
  app's bundled one or the CLI first on the PATH) too old for PAT. PAT or API key
  can be pinned under Advanced; a pinned mode never falls back.
- **OpenAI-compatible sites** from the GUI or `spb site`: add by base URL,
  model mapping, probe (latency, key, model list, one Responses request), attach
  a key later.
- **Running turns**: switches wait for running turns (`--wait-turn`) or
  interrupt them on request (`--force`); after a restart Superba returns the app
  to the conversation that was open.
- **Verification and rollback**: new core process, owned config keys, auth
  (whoami against the auth address the app uses), authenticated line check and
  "not on the login page"; a failed verification rolls back (automatically in
  the GUI after a 10 s countdown). Every switch is journaled; interrupted
  switches are finished or rolled back on the next start.
- **Restore**: the original configuration (the state before Superba's latest
  takeover, byte for byte when nothing else changed), the official ChatGPT login
  (stashed encrypted, ChaCha20-Poly1305), or any earlier generation; `spb
  uninstall` also removes Superba's rc blocks, launcher entries and the router
  service.
- **Session provider migration** for users of the old Krill tutorial
  (`model_provider = "krill"`): sessions in `state_5.sqlite` and rollout files are
  migrated to `openai` (and back) with backups, a ledger and exact rollback.
  While it runs, Superba holds Codex's own writer coordination lock, so a Codex
  that starts meanwhile waits instead of writing a session being migrated; only
  a few files are open at once whatever the session count (hundreds of sessions
  used to fail under macOS's limit of 256 open files with a wrong "a Codex
  process holds …" message). Errors keep the real OS cause ("too many open
  files" with the limit, permission denied, file not found), and the binaries
  raise their soft open-file limit at startup (up to 8192, within the hard
  limit).
- **Remote hosts** connected to the app over SSH are switched together with the
  local machine (system `ssh`, `BatchMode`, secrets only through stdin), with
  their own settings plan; their sessions and Codex versions are listed per host.

### Models and speed

- **Models page and `spb models`**: the provider's model list (fetched and
  cached, with Superba's signed list as fallback), the default model and
  reasoning effort, a context window and auto-compact limit for all models or
  per model (shown as default or custom, validated, reset to default), display
  name, hidden models and the upstream name. The same numbers appear in the GUI,
  the CLI and the generated `model_catalog_json`, which is written only when an
  override, "show site models" or a tier through the model name needs it.
- **Speed tiers** Standard / Fast / Ultrafast (`spb speed`, Overview, Models):
  what each tier does for the terminal and for the desktop app is shown per
  provider (the desktop app in API-key mode runs Standard; a tier given through a
  `-fast` / `-ultrafast` model name only where the site supports it).
- **Signed feeds** (`spb feeds`): model, price and version lists signed with an
  Ed25519 key, fetched through configurable mirrors with the built-in lists as
  fallback; GitHub proxies are off unless enabled under Preferences → Network,
  and every download is checksum-verified.

### Codex versions and the desktop app

- **Updates page, `spb codex` and `spb app`**: the Codex versions the app, the
  CLI and each remote host run, with their fit to each other; install a Codex
  release into Superba's store (downloads verified), update or roll back the
  standalone CLI, **pin** the desktop app to a Codex version (full restart,
  `CODEX_CLI_PATH`), and check the newest ChatGPT desktop app build for this
  system. Downloads are verified and the install command is shown, never run with
  elevated rights.

### Superba updates

- **Check.** `releases/latest` of `krill-lab/superba` (and, with "Include
  pre-releases" in Preferences, the newest pre-release of `releases`), ETag cached,
  8 s deadline, at start-up, every 24 hours and on demand (Updates page, Cmd/Ctrl+K
  "Check for Superba updates", `spb self-update check [--json]`). Compared by
  semantic version with the running build. A release page that answers 404 / 401 is
  shown as a calm "can't check right now" state, not an error. Preferences:
  "Check for Superba updates automatically" (on) and "Include pre-releases" (off).
- **UI.** A "Superba" section at the top of the Updates page (installed and latest
  version, release notes as plain text, Update now / Later / Skip this version,
  last checked), an Overview to-do item and a dot on the sidebar's Updates entry.
- **Install** (after a click, or `spb self-update install` with confirmation /
  `--yes`): the package for this OS and architecture is downloaded from the
  release's download path (GitHub, a mirror of exactly that path, or an opted-in
  third-party proxy), verified for size and SHA-256 against the same release's
  `SHA256SUMS.txt`, extracted with the Codex-package path and link rules, smoke
  tested (`spb --version`) and swapped in place: macOS replaces `Superba.app`
  (the old bundle stays as a backup until the new one has started), Linux replaces
  the binaries; on Windows the verified MSI is run (`msiexec /i ... /passive`) by
  a copy of `superba.exe --finish-update` after Superba has exited. A folder that is not writable ends in the verified package plus short
  manual steps. A running local router service is restarted onto the new binary.
- Tests-only overrides `SUPERBA_SELF_UPDATE_API` and `SUPERBA_SELF_UPDATE_DOWNLOADS`
  accept only https or loopback http.

### Codex settings

- **Codex settings catalog** (`catalog/`): which `config.toml` keys and
  `[features]` each Codex release understands, generated from the release's own
  `config.schema.json`, feature registry and legacy aliases, with official
  English descriptions; Chinese descriptions are written by people (missing ones
  are shown as such). Built in and signed; fetching updates is implemented but
  off by default; a scheduled workflow proposes updates for new Codex releases.
- **Settings page and `spb settings`**: search, categories, stage and version
  badges, availability per target (app, CLI, each remote host), current value vs
  default, reset, curated migrations of legacy keys, and a cross-check with
  `codex features list`. Settings are filtered per Codex home by the versions
  that read it, and every switch previews each changed key (from, to, target,
  reason). Krill providers start with Krill's recommended settings.

### Router, usage and account

- **Local router (opt-in, off by default)**: `superba-router`, a per-user
  service on `127.0.0.1` (systemd user unit with an XDG autostart fallback,
  LaunchAgent, `HKCU` Run entry), forwards this computer's Codex requests to
  member providers (Krill lines and custom sites, each with its own key) with
  per-request failover (connection failures, resets before any answer,
  502/503/504, edge interceptions; never on 401/403/429), conversation
  stickiness, SSE / WebSocket / zstd passthrough and model mapping. Turning it on
  and off goes through the normal switch flow; a watchdog (GUI, `spb router
  watch`) switches Codex back to a direct line when the router stays down for
  20 s. The official ChatGPT login is never routed; remote hosts stay direct.
  `spb router status | install | enable | disable | uninstall | watch` and the
  Router page.
- **Router capacity and "too many open files"**: the router serves a bounded
  number of requests and WebSocket sessions at once (a quarter of its soft
  open-file limit, at most 512) and of inbound connections, keeps a capped
  upstream connection pool, and starts with a soft limit of 8192 open files
  (LaunchAgent `SoftResourceLimits`, systemd `LimitNOFILE`, and its own raise at
  startup). A request over the limit, or one that meets "too many open files"
  on this computer, gets a 503 `local_router_busy` that Codex retries; it is
  never a failover and never marks a line down (under macOS's 256 limit about
  110 concurrent requests used to make every line look down). `spb router
  status`, `spb router doctor` and the Router page show the capacity and recent
  shortages.
- **Request log and usage statistics** (Router and Usage pages, `spb usage`):
  recent requests with session names, provider, model, tokens, cache and cost,
  filters (provider, model, errors, failover, client), statistics by model,
  provider, day or session, and per-request timing (first token, TPS, the line
  that answered, the transport). It also works without the router: Superba reads
  the usage Codex writes into its session files (read-only, numbers only). The
  router's log is local SQLite with 90-day retention; request and response
  content is not stored unless capture is turned on, and keys are always redacted.
- **Stats line (opt-in, off by default)**: a one-line summary of the turn
  (first token, TPS, tokens in and out) added under replies in the desktop app
  (the interactive CLI on request, never `codex exec`).
- **Account and usage** from standard endpoints only, all optional: balance,
  plan and expiry from `/v1/dashboard/billing/*` on any key-based site; for
  Krill the account e-mail (whoami), daily cost (`/v1/organization/costs`, else
  the summed `cost_usd` of the usage API) and per-model usage
  (`/v1/organization/usage/completions`), fetched on demand and cached on disk
  for 10 minutes (Krill allows 10 usage requests an hour; a 429 keeps the saved
  report). `spb account [--usage]`, the Overview and the Usage page.

### Interface

- **GUI**: Vercel Geist look in light and dark (tabular figures), English and
  Simplified Chinese, a request-path Overview (this computer, router, provider,
  model and speed) with today's cost and an hourly heat strip, Providers,
  Models, Router, Usage, Settings, Sessions, Remote hosts, Updates, Diagnostics
  and Preferences pages, a floating switch overlay with the countdown and the
  settings diff, a Cmd/Ctrl+K command palette, a first-run wizard, keyboard
  navigation, works down to 960 x 640. Every glyph comes from the bundled fonts
  or a CJK font. No demo mode: screenshots come from the real binaries against an
  isolated home and a local mock relay.
- **CLI** (`spb`, also `superba <command>`): `status`, `lines --probe`,
  `profile`, `key`, `site`, `models`, `speed`, `settings`, `account`, `usage`,
  `router`, `switch`, `restore`, `launch`, `sessions`, `remote`, `codex`, `app`,
  `feeds`, `doctor`, `recover`, `uninstall`; `--json` output; `--dry-run` plans
  with the settings diff.
- **Doctor**: stale Codex processes, processes without the PAT auth address,
  foreign environment variables, unparseable configs, keyring problems.
- **Quiet in the background**: installs, Codex binaries and their versions are
  watched by one background monitor (a metadata check of a few paths, focus
  changes, refresh buttons, after switches and installs) instead of being
  detected on every 5-second overview read; no timer starts a process (`codex
  --version`, `lsof`, `systemctl`, `launchctl`, `tasklist`, PowerShell); while the
  window is hidden or minimised only the router watchdog keeps running (it also
  keeps running now, falling back to direct mode when the router dies).
- **Release pipeline**: Linux (glibc x86_64 / aarch64; static musl `spb` +
  `superba-router`), macOS universal (signed and notarized when the signing
  environment is configured; the router inside `Superba.app`), Windows x86_64;
  `superba-router` ships next to `superba` and `spb` everywhere; third-party
  license notices in every archive.

### Security

- The debugging port is loopback-only with `--remote-allow-origins` pinned to
  its origin; its owner is checked before the first request and a mismatch
  disables hot switching for that session. Safe mode launches without it.
- Running as root never opens a debugging port; `sudo` and elevated Windows
  tokens are refused.
- Keys live in the OS keyring (0600 file fallback on headless Linux), are sent
  only to their site's https origins, never on command lines, never logged,
  and are masked in process listings (`sk-…`, `nb_…`).
- The local router listens on loopback only, requires a random router token,
  swaps in each member's own key (a key never reaches another site), keeps its
  keys in a 0600 file, stores no bodies and logs no keys, tokens, bodies, query
  strings or session ids. See SECURITY.md.
- Release signing runs in a separate, protected GitHub environment; third-party
  actions are pinned to commit SHAs.

### Known limitations

- Hot switching, launch and quit are verified end to end on Linux only (desktop
  app 26.1007 / 26.1013, Codex 0.162). The macOS (LaunchServices launch,
  DevToolsActivePort, `lsof` owner check, `NSRunningApplication` quit) and
  Windows (MSIX activation, `GetExtendedTcpTable` owner check) paths compile and
  are unit-tested but have not run on real machines; Superba falls back to a
  full restart when it cannot confirm the restart bridge.
- The local router has been exercised with the real `codex` CLI and a test
  relay, and its switch-service integration with fakes; router mode with the
  real desktop app (enable, watchdog fallback, hot restarts), the real launchd /
  systemd / Run-key backends, WebSocket passthrough to a relay that accepts
  WebSockets, and failover between different providers (whose encrypted
  reasoning items are not stripped) are not verified yet. Router mode uses API
  keys only (no PAT router mode).
- Restarting the app's Codex core on an SSH remote host through the desktop app
  (`hostId` + `killCodexProcess`) is untested; remote file writes and
  verification are covered by a Docker sshd integration test.
- A turn that is running when the core restarts loses its answer (the user
  message is kept); the app then shows its home screen until the thread is
  reopened, which Superba does automatically when it can.
- The new Codex core inherits the app's launch environment, so PAT keeps using
  the auth address the app was started with until the next full restart.
- On the China Express line the CDN currently intercepts
  `/backend-api/wham/*`, so PAT is unavailable there and Superba uses API-key
  mode on that line.
- Most Codex feature flags have no official English description upstream yet;
  they are shown with an empty description and flagged.
- The desktop app may rewrite its own keys in `config.toml` at any time; if it
  ever changes a key Superba owns right after a restart, verification fails and
  the switch is rolled back (re-applying and resending the restart is not
  implemented yet).
- `uninstall --delete-backups` keeps stored profile keys (remove them with `spb
  profile remove` / `spb key`).
