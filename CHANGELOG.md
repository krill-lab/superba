# Changelog

All notable changes to Superba are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/): later releases increment the patch
number; the minor number changes only for breaking changes.

## [0.1.1-alpha.9] - Unreleased

Changes since [v0.1.1-alpha.8](https://github.com/krill-lab/superba/releases/tag/v0.1.1-alpha.8).

> **alpha.7 users: the in-app update to this version fails on Windows (msiexec
> 1619, fixed in alpha.9's updater) — download and run the MSI manually once.**

### Added

- An Overview for every coding tool: today's use per tool, each tool's route
  to its provider, and Claude Code's own requests (read from its transcripts)
  next to Codex's in the request log.
- A session browser on the Sessions page: search, links to each session's
  folder and transcript, and exports. Session migration moved to Tools.
- Turn a provider's line off without deleting it, from the Providers page or
  the command line; neither the router nor a direct switch uses a disabled line.
- Mixed login: keep the official ChatGPT account for remote features while the
  models go through the router, and add the official ChatGPT plan as a router
  member. Providers has a switch for it on the official login.
- Failover rules on the Router page: the cooldowns, which HTTP statuses fail
  over, how long a conversation stays on its line and whether it returns to
  the preferred one. Overview and Providers describe the rules you set.
- A Routing tab on Usage shows where each request went, every hop it tried and
  any protocol conversion.
- Codex settings has a "New in latest" category with the keys the newest Codex
  releases add.
- Superba finds tools that are not on PATH, lets you pick a program's path, and
  uses Homebrew by default on macOS. Installed tools get their own sidebar
  entries, and tools show their icons on Usage, the Overview and the Router.
- Prices moved to the Models page, with long-context price bands, 5-minute and
  1-hour cache writes, time-window prices and other vendors' list prices.
- `superba --command` runs a page command at start-up.

### Changed

- The Router page is one flat page: the service sits under the status line and
  the Claude Code section is gone. Its price section is now a link to Models.
- The Cmd/Ctrl+K command palette and the Overview's ⋯ menu are gone. Superba
  settings has an inline filter instead, and the "跳到" label before its section
  list is gone.
- Every search box has a clear button and clears with Esc.
- Write confirmations show every change in full.

### Fixed

- A tested provider model reaches Codex, and switching the Models display mode
  takes effect at once.
- Windows: paths handed to other programs or shown to you are never in the
  `\\?\` form; processes Superba cannot open no longer block quitting the
  app, and the ones it cannot end are named; screen readers can open Router's
  sections and see which settings section is current.
- WSL: the distribution's model is named when the profile sets one.
- macOS: only the ChatGPT/Codex apps count as a desktop install.

### Limits of this preview

- Windows and macOS packages remain unsigned, as before. macOS packages are
  not notarized.
- The built-in catalog snapshot must be re-signed by a maintainer to carry the
  new price feed.

## [0.1.1-alpha.8] - 2026-10-20

Changes since [v0.1.1-alpha.7](https://github.com/krill-lab/superba/releases/tag/v0.1.1-alpha.7).

### Fixed

- Apply WSL routing configuration before restarting the desktop app, so its
  remote connection uses the selected router instead of returning to login.
- Quote notification hook commands correctly on Windows when executable,
  configuration or hook paths contain spaces.
- Dialogs never grow taller than the window: the title and the buttons stay in
  view and the content between them scrolls. Turning the router off no longer
  produces a confirmation whose buttons are off screen.
- Plan notices are grouped and sorted: repeated notices of one kind become one
  line with a count and an expander ("32 models are not in the built-in
  catalog of this Codex version"), blocking notices and warnings come first,
  and informational notes are collapsed.
- Turning the router off keeps the model picked through a router provider
  prefix (`provider/gpt-6.1-sol` becomes `gpt-6.1-sol`) instead of falling
  back to the first model the line lists, and says so as a warning.

### Limits of this preview

- Windows and macOS packages remain unsigned, as in alpha.7. macOS packages
  are not notarized.
- Fast mode availability depends on the model and upstream provider.

## [0.1.1-alpha.7] - 2026-10-20

Changes since [v0.1.1-alpha.6](https://github.com/krill-lab/superba/releases/tag/v0.1.1-alpha.6).

### Added

- **Claude Code** connection, model, settings and diagnostics pages, with direct
  connections or a local router. Configuration changes have a preview, backups
  and restore, shared with the other coding tools.
- **Tools** configuration for dsh, opencode, pi, ZCode and WorkBuddy / WorkBuddy AI,
  plus Gemini CLI configuration through the Gemini router ingress. Applied
  tool configurations keep their chosen provider until explicitly reapplied.
  `spb claude`, `spb harness` and `spb tools` expose the corresponding operations.
- **Tool installation and updates** in Superba → Updates: Claude Code, dsh,
  opencode, pi, ZCode and WorkBuddy / WorkBuddy AI, with versions, progress,
  cancellation and operation logs. New opencode installs use OpenCode 2;
  existing installs retain their major version. Available actions depend on
  the platform and the tool's installer and updater.
- **Notifications** for task completion, approval requests and failures through
  Codex and Claude Code hooks, plus Superba switch, router and update events.
  Configure them in Superba → Settings → Notifications or `spb notify`.
  Channels include Telegram, Feishu / Lark, WeCom, DingTalk, ServerChan,
  PushPlus, Bark, ntfy and signed webhooks, with quiet hours, rate limits,
  deduplication and opt-in reply summaries. ([#12](https://github.com/krill-lab/superba/issues/12))
- **Protocol conversion** between Anthropic Messages, OpenAI Responses,
  Chat Completions and Gemini, including a `/gemini` ingress. Native endpoints
  take precedence; conversions can be disabled by direction. The tools and
  router show whether a connection needs conversion. The `/openai/v1` ingress
  also supports native Responses and Chat Completions forwarding.
- **Merged router model catalogs**, combining built-in models, provider Codex
  catalogs, standard model lists and user overrides, with source errors and
  capability checks shown on the Models page. Optional `<provider>/<model>`
  names pin a provider while bare names retain failover order; legacy `krill/`,
  `krill-usage/` and `krill-balance/` prefixes are accepted in router mode.
  ([#16](https://github.com/krill-lab/superba/issues/16))
- **PAT per-model settings**: custom context windows and auto-compaction limits
  now use a model catalog in key-based PAT profiles too. The official ChatGPT
  profile keeps Codex's own catalog. ([#14](https://github.com/krill-lab/superba/issues/14))
- **Router-managed compaction** for eligible models synthesized from standard
  model lists, using authenticated encrypted summaries. Each selected target
  keeps its own native or synthetic compaction behavior during failover.
- **Data location and storage controls** in Superba → Data, including usage,
  request-record retention, cache cleanup and relocation of Superba's data with
  progress, verification and rollback. CLI: `spb data where|move|usage`.
  ([#15](https://github.com/krill-lab/superba/issues/15))
- **Launch at login** for the current user, with an option to start in the tray.
  If a usable tray is unavailable, the window opens so the app stays reachable.
- **Usage by tool**, request and session filters, router ingress cards and
  per-tool statistics-line settings.

### Changed

- Consolidated maintenance in the bottom **Superba** entry: Overview, Updates,
  Settings, Diagnostics and Data tabs. Settings uses one scrolling page with
  section links; all installs and updates live in Updates. The main Overview
  menu is named **Actions**.
- Refreshed the window frame and sidebar, removed duplicate headings, and let
  the unified update list use the available width.
- Routed PAT connections retain model speed-tier metadata for the desktop
  app's Fast-mode lightning control where the upstream supports it. Catalog
  caching preserves those fields and handles conditional requests.

### Fixed

- Restore Original defaults to restoring the original routing and moving
  session provider IDs back; keeping `openai_base_url` is an explicit choice.
- Preferred-line changes reload the router configuration. Routing status uses
  the recorded endpoint and mode, including local PAT, instead of guessing
  from the authentication method or URL text.
- WSL cold starts have bounded waits and clearer progress and failure stages;
  failed update results remain available.
- macOS launch-at-login, reopening a hidden window and dismissing setup;
  CJK font alignment and background conversion of cached system fonts.
- Session-lock diagnostics recover holder information from per-file-descriptor
  locks where the system-wide lock list does not expose it.
- Sessions refresh after migration and rollback without requiring a manual rescan.
- Uninstall restores the authentication endpoint owned by Superba when switching
  from routed PAT to a direct API-key profile.
- Narrow windows keep Claude settings readable and avoid overlapping network
  source controls and provider/router settings.

### Limits of this preview

- Moving the data location moves **Superba data only**. It does not relocate
  Codex's own home, raw conversations or session database, or change where
  future Codex conversations are stored. Codex disk usage is shown separately.
- Phone replies that continue a task are **design-only** in alpha.7. Personal
  WeChat notifications use ServerChan or PushPlus; no native personal-WeChat
  bot or two-way approval/reply control is included.
- Protocol conversion does not make every model in the merged Codex catalog
  usable. Newly synthesized standard-list models need a successful tool probe;
  entries marked **Needs protocol conversion** remain unavailable through that
  catalog path. Probe results are held in memory and may need to be repeated
  after a router restart.
- Merged-catalog and encrypted-summary routing use HTTP fallback. Native
  provider-encrypted history cannot be decoded for a synthetic model; return
  to the original provider or explicitly start a new conversation.
- PAT per-model settings do not provide concurrent official ChatGPT and API
  authentication. Speed tiers still depend on upstream capabilities; there
  is no blanket Fast or Ultrafast support for every provider or model.
- ZCode and WorkBuddy installers are managed on Windows; macOS and Linux use
  download links. WorkBuddy retains its own updater. Gemini CLI configuration
  is included, but Gemini CLI installation and updates are not managed here.
  Tool installation inside WSL and Windows ARM64 packages remain out of scope.
- macOS validation for this candidate relies on hosted CI tests and DMG/Finder
  checks. The full Mac device matrix was not rerun because the test VM is
  unavailable; earlier device checks do not establish full coverage of this build.

## [0.1.1-alpha.6] - 2026-10-19

[Release](https://github.com/krill-lab/superba/releases/tag/v0.1.1-alpha.6).
The cumulative overview below is retained from the alpha.6 source and covers
0.1.0 through alpha.6, rather than changes introduced by alpha.6 alone. Its
navigation, limitations and validation notes describe that historical snapshot;
the alpha.7 entry above supersedes them where applicable.

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
- **Coexistence with cc-switch, AI Toolbox and Codex itself** (GH#5): Superba
  owns exactly what its last switch wrote (its `config.toml` keys, the
  `auth.json` credential, its `CODEX_AUTHAPI_BASE_URL` values). A later change by
  another program is detected at start-up, before every write and while the
  window is open, attributed (cc-switch or AI Toolbox with their provider, from
  their own stores and the markers they write; Codex's own sign-in; the Codex
  app's model picker; a hand edit) and shown key by key with the credential's
  shape. Nothing is overwritten without an answer: take over again (GUI, `spb
  switch --take-over`, or the question `spb` asks at a terminal), let the other
  tool manage Codex (`spb hand-off`: Superba restores only the keys it still
  owns, keeps the other tool's changes and an encrypted copy of both files, and
  owns nothing until the next switch takes over from a fresh baseline), or leave
  it for now. Restore, uninstall and the router's fallback keep every external
  change and say so (`--take-over` restores those too); quitting writes nothing.
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
  The ChatGPT remote-control devices and WSL distributions the app also reports
  are listed read-only under the app's name for them (`spb remote list` shows each
  host's kind) and are never probed, synced, switched or scanned over SSH.

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
- **The model stays across a change of authentication**: for a provider without
  a default model, turning the router on (or switching between PAT and API key)
  writes the model Codex runs with now, because with an API key Codex and the
  desktop app choose a default of their own (often `gpt-5.5`). The plan shows it
  ("Keeps the model in use") and says how to choose another; turning the router
  off drops it again when the PAT's own default is the same model. WSL
  distributions that follow this computer get the same rule for their own
  Codex (their own `config.toml`, line and Codex version), named per
  distribution in the plan, and with the app's "Run Codex in WSL" the plan says
  the app's Codex inside that distribution keeps the model too.
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
- **Windows: the ChatGPT app installer package.** The Updates page (下载安装包)
  and `spb app download [--source official|store] [--open]` download the app's
  MSIX for this machine from OpenAI's server (`windows-release.json` on
  persistent.oaistatic.com) or, as the fallback or by choice (Preferences >
  Network), from the Microsoft Store CDN with links resolved by
  store.rg-adguard.net, a third-party resolver. Downloads show progress, can be
  cancelled and resume (`Range` + `If-Range`); each file is checked (size, the
  listed SHA-1 and the CDN's own, the manifest identity `OpenAI.Codex` of
  OpenAI's Store publisher, and the signature with `WinVerifyTrust`), framework
  packages it still needs are fetched too, and the package is opened with App
  Installer, where you click Install: never installed silently, never elevated.
  Afterwards the page offers to reopen ChatGPT through Superba so hot switching
  keeps working.

### Superba updates

- **Check**, on the update channel: **Stable** reads `releases/latest` of
  `krill-lab/superba` (final releases only), **Preview** one page of `releases`
  and takes the newest release including alpha, beta and release candidates by
  semantic-version precedence (`alpha < beta < rc < final`, drafts skipped). Until
  the user chooses (Preferences → Updates, the Updates page, `spb self-update
  check|install --channel stable|preview`), a pre-release build uses Preview and a
  final build Stable, so a preview user is offered the next alpha (user bug 73).
  One request per check, ETag cached per channel, 8 s deadline, at start-up, every
  24 hours and on demand (Updates page, Cmd/Ctrl+K "Check for Superba updates",
  `spb self-update check [--json]`). GitHub's limit of 60 requests an hour
  without an account is respected: a rate-limited answer stores the reset time,
  nothing is asked before it (also not on demand) and the last answer stays shown.
  Only a release newer than the running build is offered, never an older one: a
  preview build on Stable waits for the next final release; an older version is
  installed only when named (`spb self-update install --version 0.1.0`, refused on
  Windows, whose installer only upgrades). A release page that answers 404 / 401
  is shown as a calm "can't check right now" state, not an error. Preferences:
  "Check for Superba updates automatically" (on) and "Update channel".
- **UI.** A "Superba" section at the top of the Updates page (installed and latest
  version, the channel with "You're on preview 0.1.1-alpha.3 · latest preview … ·
  latest stable …", release notes as plain text, Update now / Later / Skip this
  version, last checked), an Overview to-do item and a dot on the sidebar's
  Updates entry.
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
- **Log.** Each attempt is logged step by step in `logs/selfupdate.log` (the
  last attempts are kept): download and checksum, the swap, and on Windows the
  wait for Superba to exit, the `msiexec` command line and exit code with its
  meaning (plus a verbose installer log), the router restart and the relaunch.
  A failed update is shown at the next start with the reason and the log path,
  stays on the Updates page and in `spb self-update check`.
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
  `spb settings list|get` without `--profile` show this computer's `config.toml`
  like the settings page, also on a fresh install or with the official login
  (user bug 72); edits without `--profile` go to the key-based provider applied
  on this computer, and with none applied the command says where a setting can
  be stored instead. `--profile` shows or edits one provider's own settings; the
  built-in `official` and `original` explain that they have none.
- **Edit Codex files** (GitHub issue 9): Settings → Edit Codex files, the
  command palette and `spb config edit|check|backups|restore` edit Codex's own
  `config.toml` and `auth.json` of this computer, a remote host or a WSL
  distribution. TOML / JSON highlighting with line numbers; a check as you type
  (syntax errors with line and column are never saved, values of the wrong type
  for the Codex versions reading the file need "Save anyway", unknown,
  deprecated and renamed keys are warnings); Format keeps every comment;
  `auth.json` keys are masked until shown (never in logs, diffs or `--json`);
  the diff and the managed keys an edit changes are shown before saving; every
  replaced version is backed up encrypted (newest 50 per file) and listed with
  the switch pre-images and the original, each restorable with a diff. A save
  is the user's own change (GH#5: never reverted by itself, the next switch
  asks, a restore keeps it, named as the editor's on the Overview), and Codex in
  effect (GH#4) names the running Codex still on the old file. `spb config edit`
  uses `$VISUAL` / `$EDITOR` (notepad on Windows) on a private copy, checks it
  when the editor closes and asks before saving.

### Router, usage and account

- **Remote compaction per provider** (GitHub issue 3): Codex compacts with a
  `compaction_trigger` request to the provider and only that provider can read
  the result, and for the configuration Superba writes it has no local-compaction
  setting. The Connection tab and `spb profile compaction <id> [--check]` show
  whether a provider compacts natively (Superba's check: one tiny compaction
  request, a few tokens, cached a week; Krill is native) with an override
  (`--compaction auto|native|unsupported`); the switch preview warns about a
  destination that cannot compact and counts recent conversations another site's
  provider compacted; the local router sends compaction requests only to members
  that can compact and keeps every compacted conversation with the member that
  made it (HTTP and WebSocket, no failover elsewhere, a clear error instead).
- **Conversations started on another provider** (GitHub issue 11): continuing
  one on a provider that cannot read the content another provider encrypted in
  it (hidden reasoning, a sub-agent's task, encrypted tool results) stopped
  every turn with `invalid_encrypted_content`. The local router now leaves out
  exactly what the provider refuses (a sub-agent task that is plain text stays
  as text) and sends the request once more, and leaves the same items out of
  the conversation's later turns before they go (HTTP and WebSocket; an event on
  the Router page and a line in the request detail). In direct mode the switch
  preview counts the recent conversations that would stop and the Overview /
  `spb status` names conversations that did, with what to do.
- **Usage history from cc-switch and AI Toolbox** (GitHub issue 1): the import
  dialog and `spb import usage ccs|ai-toolbox [--dry-run] [--remove]` preview
  (requests, daily totals, date range, recorded cost) and import their Codex
  usage records into the Usage page, read-only and once per request (the tool's
  ids), costs as recorded, rows from Codex's session files skipped where Superba
  reads them itself; imported rows show their source and the source filter
  (`spb usage --source`) selects them.

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
- **A stable router port**: the router listens on a per-user port in
  17000–19999, below every OS ephemeral range (Windows and macOS 49152–65535,
  Linux's configured `ip_local_port_range`), chosen once and kept across
  restarts, so another program's outgoing connections no longer take it after a
  reboot. When another program does hold it while the router is down, the
  watchdog (after its 20 s grace) and Install / Restart move the router to the
  next free port and point Codex — and WSL distributions behind NAT — at it
  through a normal router-on switch instead of leaving router mode; the WSL
  firewall rule names no port, so the move needs no new approval.
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
  navigation, works down to 960 x 640. Choosing another remote host, WSL
  distribution, file or provider shows the choice at once with a spinner and a
  loading state instead of the previous choice's data until the new data
  arrives; only the last choice counts, and a failure or a timeout is shown in
  place with Retry (user bug 83). Every glyph comes from the bundled fonts
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
- **Tray icon and "close to tray"** (GH#10): a notification-area icon on
  Windows, a menu-bar item (template image) on macOS and a StatusNotifierItem
  on Linux (no GTK: KDE Plasma, GNOME with the AppIndicator extension, Xfce,
  ...). Its menu opens Superba, shows the provider and line in use with a
  submenu to switch provider, line or back to the official login, opens or
  reopens ChatGPT through Superba, shows the router (on, off, not answering)
  and turns it off or opens the Router page, and quits Superba (the router
  keeps running as its own service). Preferences > Window: "When the window is
  closed: Quit / Keep running in the tray" (Windows and Linux ask on the first
  close; macOS keeps running in the menu bar and leaves the Dock while hidden)
  and "Start hidden in the tray" (off; `superba --start-in-tray`). One Superba
  per user: starting it again shows the running window. A Linux desktop without
  a tray host shows the setting disabled with the reason and quits on close; a
  Wayland window hides by reopening through XWayland.
- **App icon**: a coral krill on a pearl tile for the window, the taskbar and
  every package; on macOS it follows Apple's icon grid (an 824 px
  continuous-corner tile on the 1024 px canvas), so Superba is as big as the
  other apps in the Dock, Finder and Launchpad.
- **macOS disk image**: opening the `.dmg` shows a drag-to-install window:
  Superba on the left, an Applications link on the right, an arrow and the hint
  "拖到 Applications 即可安装 · Drag to Applications to install" on a light
  background in the icon's colours (sharp on Retina), no toolbar or sidebar, the
  app icon as the volume icon. `spb` and the release documents are still in the
  image, hidden in Finder; the ZIP is unchanged.
- **Windows without extras**: the programs link the C runtime statically (no
  VC++ redistributable; the release checks their imports), and the GUI draws with
  Direct3D 12, on WARP, the software rasterizer, where there is no 3D driver (VMs,
  remote desktops; Linux and macOS keep OpenGL). A window that cannot open or a
  crash is explained in a message box and in `logs/superba-gui.log` /
  `logs/superba-crash.log` in the data directory, never a silent exit. Paths are
  shown without the `\\?\` prefix, and the ChatGPT version as release version
  with the package version labeled (`26.1013.40934 (package 26.1013.4894.0)`).

### Security

- The debugging port is loopback-only with `--remote-allow-origins` pinned to
  its origin; its owner is checked before the first request and a mismatch
  disables hot switching for that session. Safe mode launches without it.
- Running as root never opens a debugging port; `sudo` is refused, and so is
  another Windows account than the signed-in user (SYSTEM, `runas`, a scheduled
  task). Running as administrator (an elevated token of the signed-in user, or
  any process with UAC off) is allowed, with a one-time note.
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
  systemd / Run-key backends and WebSocket passthrough to a relay that accepts
  WebSockets are not verified yet. Leaving out content another provider
  encrypted is verified end to end over HTTP with the real `codex` CLI and the
  Krill test relay; over WebSocket only with a fake upstream (the test relay
  does not accept WebSockets for API keys). Router mode uses API keys only (no
  PAT router mode).
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
