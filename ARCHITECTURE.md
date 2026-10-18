# Superba architecture

Superba is a cross-platform (macOS, Linux, Windows) Rust launcher that configures
OpenAI Codex — the desktop app (ChatGPT/Codex Electron app), the Codex CLI and the
SSH remote hosts the app is connected to — for the Krill relay
(<https://www.krill-code.com>), for any OpenAI-compatible Responses API site, or back
to the official ChatGPT login. Its core features are hot switching of line and auth
mode through the desktop app's app-server restart, restore of the original
configuration, unified profiles (connection plus the Codex settings Superba manages,
checked against a per-version settings catalog; auth Auto / PAT / API key), and
session provider migration.

Three binaries ship from this workspace, always side by side (section 12):

| Binary | Crate | Notes |
|---|---|---|
| `superba` | `superba-gui` | egui/eframe GUI; `superba <subcommand>` behaves like `spb` |
| `spb` | `superba-cli` | headless CLI; never links GUI libraries (servers, remote hosts) |
| `superba-router` | `superba-router` | opt-in local router, run as a per-user service (section 8) |

Three more crates are libraries / tools: `superba-catalog` (the Codex settings
catalog, section 10), `superba-catalog-gen` (maintainer tool that generates it,
never linked into the binaries) and `superba-msi-ca` (`superba_msi_ca.dll`, the
Windows installer's folder check, embedded in the MSI and never installed;
`scripts/release/README.md`).

## 1. Crate map and ownership

```
                       superba-core  (types + traits only)
        ┌──────────┬────────┼─────────┬────────────┬─────────────┬───────────────┬──────────────┐
 superba-config superba-net superba-sessions superba-appctl superba-remote superba-catalog superba-router
        └──────────┴────────┴─────────┴────────────┴─────────────┴───────────────┴──────────────┘
                              superba-switch  (wires everything)
                          ┌─────────┴──────────┐
                     superba-cli ◄──────── superba-gui
```

| Crate | Implements (from `superba_core::traits`) | Placeholder type | Owner |
|---|---|---|---|
| `superba-core` | defines all contracts, catalog, i18n, facts | — | scaffold |
| `superba-config` | `ConfigEngine`, `EnvPersister`, `SecretStore`, `ProfileStore` | `TomlConfigEngine`, `ShellEnvPersister`, `KeyringSecretStore`, `JsonProfileStore` | config agent |
| `superba-net` | `LineProber` | `HttpLineProber` | net agent |
| `superba-sessions` | `SessionMigrator` | `CodexSessionMigrator` | sessions agent |
| `superba-appctl` | `AppController` | `DesktopAppController` | appctl agent |
| `superba-remote` | `RemoteHosts` | `SshRemoteHosts` | remote agent |
| `superba-catalog` | `CatalogSource` (`SnapshotSource`) | `CatalogStore` | catalog agent |
| `superba-router` | the `superba-router` binary; `RouterManager`, watchdog, admin client | `RouterServer` | router agent |
| `superba-import` | — (read-only scan of cc-switch / AI Toolbox stores, mapping, conflicts; used by `superba-switch`'s `import_scan` / `import_apply`) | `read_source`, `preview`, `plan` | import agent |
| `superba-switch` | `SwitchService` (+ `detect_context`, `SwitcherDeps`, `RouterControl`, `settings_catalog`) | `Switcher` | switch agent |
| `superba-cli` | `spb` binary + `superba_cli::main_with_args` | — | cli agent |
| `superba-gui` | `superba` binary | `SuperbaApp` | gui agent |

Dependency rules:

- The five component crates depend on `superba-core` only. They never depend on
  each other. When one needs another's behaviour it receives the trait object:
  `SshRemoteHosts::new(Arc<dyn ConfigEngine>, ..)` renders remote files with the
  same `ConfigEngine::render` the local engine uses.
- `superba-switch` is the only crate that names the concrete implementations
  (`SwitcherDeps::with_defaults`). Tests construct `SwitcherDeps` with fakes.
- `superba-cli` and `superba-gui` talk to `Arc<dyn SwitchService>` only; they take
  types from `superba-core` and the constructor from `superba-switch`.
- Each crate owner edits only their crate directory. Shared files
  (`Cargo.toml` workspace deps, `superba-core`) change through the scaffold owner /
  integration; every plausible dependency is pre-declared in
  `[workspace.dependencies]` and in each crate's `Cargo.toml` so `Cargo.lock` rarely
  changes.

## 2. Contracts

All traits are object safe (`#[async_trait]`, `Send + Sync`) and used as
`Arc<dyn Trait>`. Unwritten functionality returns `Error::NotImplemented`, never
panics.

| Trait | Purpose | Key methods |
|---|---|---|
| `ConfigEngine` | Owned keys of `config.toml` + `auth.json`, journal, baseline, login stash | pure: `render`, `inspect_files`, `capture_baseline` (shared with remote); `read_state`, `baseline`, `snapshot_baseline`, `plan`, `apply`, `rollback`, `recover`, `generations`, `active_credentials`, `login_stash` / `stash_login` / `load_login_stash` (by `StashScope`: local home or remote target), `owned_keys` |
| `EnvPersister` | Persist `CODEX_AUTHAPI_BASE_URL` for future processes | `inspect`, `plan(var, EnvOp)`, `apply`, `revert`, `set_authapi_base_url`, `clear_authapi_base_url` |
| `SecretStore` | Profile keys in the OS keyring (0600 file fallback) | `get`, `put`, `delete`, `backend` |
| `ProfileStore` | `profiles.json` (`ProfileSet`: profiles, custom sites, settings) | `load`, `save` |
| `LineProber` | Latency/health of lines, key validation, auth API health, models, Responses probe, WebSocket / zstd transport probe, remote compaction check, account and usage data | `probe`, `probe_all`, `check_key`, `check_authapi(site, origin, key)`, `list_models`, `probe_responses`, `probe_transports`, `probe_compaction`, `account_summary`, `usage_report` |
| `SessionMigrator` | Session provider scan, migration with backup + ledger, rollback, crash recovery | `scan`, `plan`, `migrate`, `rollback`, `history`, `recover` |
| `AppController` | Installs, CLI binaries, launch with CDP, bridge, hot restart, quit, processes, turn activity, renderer state, daemon, launcher entry | `detect_installs`, `codex_binaries`, `running_session`, `launch`, `probe_bridge`, `hot_restart`, `quit`, `full_restart`, `processes`, `session_and_processes`, `turn_activity`, `renderer_state`, `current_thread`, `return_to_thread`, `restart_daemon`, `stop_daemon`, `remote_connections`, `launcher_entry`, `set_launcher_entry`, `environment_stamp`, `forget_cached_probes`, `stop_process` |
| `RemoteHosts` | SSH hosts: discovery, probe, remote apply/rollback, turn activity, verification | `discover(home, from_app, live)`, `probe`, `plan_apply`, `apply`, `rollback`, `turn_activity`, `verify(RemoteVerifyRequest)`, `read_codex_file`, `write_codex_file` |
| `SwitchService` | Application service for GUI and CLI | `overview`, `sites`, `profile_set`, `save_profile`, `delete_profile`, `save_settings`, `save_site`, `delete_site`, `probe_lines`, `probe_site`, `line_transports`, `provider_compaction`, `import_scan`, `import_apply`, `import_usage`, `check_key`, `account`, `sessions`, `doctor`, `runtime`, `stop_processes`, `open_app`, `recover`, `plan`, `plan_settings`, `execute`, `settings_view`, `edit_settings`, `router_status`, `router_install`, `router_set_enabled`, `router_uninstall`, `router_watch`, `router_fall_back`, `environment_event`, `environment_generation`, `codex_file`, `save_codex_file`, `codex_file_backups`, `codex_file_backup` |
| `SwitchDecider` | Mid-execution questions (wait for turns, force quit, IPC failed, verification failed, close / stop Codex processes) | `decide(DecisionRequest) -> Decision` |
| `SwitchEvents` | Progress events to the UI | `emit(SwitchEvent)` |
| `Clock` | Injectable time (countdowns, ages) | `now`, `monotonic` |

Key types (all in `superba-core`): `Site`/`SiteKind`/`Line`/`Origin`/`CodexPath`,
`AuthMode`/`AuthFamily`/`AuthShape`, `Profile`/`KeyRef`/`Secret`, `Target`/
`RemoteHost`/`SshEndpoint`, `CodexHome`/`DataDir`/`LocalContext`, `LineCaps`/
`KeyCheck`/`KeyAccess`, `ResolvedProfile`/`ResolvedMode`/`ResolvedEndpoints`,
`ApplyPlan`/`ApplyReport`/`Baseline`/`ConfigState`, `EnvPlan`/`EnvApplied`,
`MigrationPlan`/`MigrationReceipt`, `AppSession`/`ProcessInfo`/`TurnActivity`,
`RemotePlan`/`RemoteState`, `RestartKind`/`FullRestartReason`/`BlockReason`,
`SwitchRequest`/`SwitchPlan`/`SwitchOutcome`/`SwitchPhase`/`SwitchEvent`,
`Overview`, `LocalizedText`/`Notice`, `Error`.

Conventions:

- **Plan, then apply.** Every planning method is free of side effects. Writes take
  the plan they execute and re-check file hashes before replacing anything.
  Executable plans (`ApplyPlan`, `RemotePlan`, `SwitchPlan`, `FileChange`) are
  serialize-only: their contents and keys are skipped, so they are never
  deserialized and executed; pass back the value `plan` returned.
- **Failure atomicity.** A writing method that fails restores what it changed
  before returning; if that fails too it says what is left and keeps its journal
  entry for start-up recovery.
- **Transaction and lock.** `superba-switch` holds `DataDir::state_lock` for a whole
  `execute` / `recover` and writes a switch journal (`journal/switch/`) with every
  component receipt (`ApplyReport`, `EnvApplied`, `MigrationReceipt`,
  `RemoteApplyReport`); on start-up it runs `ConfigEngine::recover`,
  `SessionMigrator::recover`, then completes or rolls back its own entries.
  Components never take `state.lock` themselves.
- **Secrets.** Keys travel as `Secret` / `SecretBytes` (zeroize on drop, redacted
  `Debug`, no `Serialize`); `CodexFiles` holds both files as `SecretBytes`
  (`config.toml` may contain MCP tokens). Persisted data holds a `KeyRef`. Plans that
  carry a key (`ApplyPlan::desired`, `RemotePlan::desired`) skip it when serialized.
  Foreign command lines are shown only through `app::redact_argv`.
- **Key scoping.** `LineProber` sends a site's key only to that site's origins
  (`check_authapi` refuses others), never over plain http except loopback, and
  never follows redirects on authenticated requests.
- **Observations vs. decisions.** Component results are observations;
  `superba-switch` derives decisions (e.g. it sets `ProcessInfo::stale` from start
  times and the generation history, applies the version table, groups shared
  remote homes).
- **Text.** Domain code returns `LocalizedText` (key + English fallback + args);
  binaries render it with `i18n::Catalog` (core catalog in
  `crates/superba-core/locales`, UI catalogs in each binary crate). Error variants
  map to `error.<code>`.
- **Extensibility.** `AuthMode`, `SiteKind`, `Target`, `BlockReason`, ... are
  `#[non_exhaustive]`; behaviour of auth modes is queried through methods
  (`family`, `uses_key`, `needs_authapi_env`, `writes_chatgpt_base_url`), not by
  matching. A future "Krill account display" mode
  (API key wrapped in an unsigned ChatGPT-style JWT, app shell pointed at a
  localhost forwarder) is added as a new variant plus its rendering in
  `superba-config`; `Origin` already accepts `http://127.0.0.1:<port>`.
- **Facts are constants.** Provider id, config keys, auth.json fields, env names,
  IPC message, renderer URL filter, edge-block header and the hot-restart JS live in
  `superba_core::codex`; the marker-block format in `superba_core::env`; Krill lines
  in `superba_core::catalog`.

### Unified profiles and the settings engine

A `Profile` is a connection (site, `line_id` plus ordered `backup_lines`, auth
choice, key) plus `codex_settings`: the `config.toml` keys Superba manages for it
(`CodexSettings`, dotted path -> value, or `null` = remove the key so Codex uses
its default). `profiles.json` schema 2; schema 1 files are migrated on load (v1 PAT
-> `AuthChoice::Auto`, API key stays manual, v1 recommended keys of Krill PAT
profiles become ordinary settings) and the first save keeps `profiles.v1.json`.

- **Auth.** `AuthChoice::decide(AuthContext)` (`superba-core`) picks PAT when the
  site and line allow it, else an API key, always with an `AuthReason` (line without
  PAT endpoints, http line, custom base URL, router mode, Codex too old). The
  planner feeds it the line probe, router mode and `pat_version_ok`
  (`rules::pat_version_ok` over `rules::local_versioned_consumers`: the app's
  bundled Codex and the CLI first on the PATH; one known version without PAT makes
  Automatic use an API key; unknown versions are left to the version gate;
  remote-only switches ignore local versions). Manual PAT / API key (Advanced)
  never fall back; a manual PAT on a too-old Codex is blocked by the version gate.
  The GUI shows the same decision (`pages::connection::decision_with` +
  `auto_facts`) but sends no mode with a switch. `Profile::auth_choice()` is what
  planners read; `auth_mode` remains as a mirror.
- **Settings engine** (`superba_config::settings`, pure): `plan_settings` filters a
  profile's settings for one Codex home against the catalog snapshot of every Codex
  reading it (app-bundled Codex + CLI for the local home, the host's Codex for a
  remote, with that home's `config.toml` and the keys Superba wrote there: the
  active profile's settings locally, `RemoteState::config_toml` / `owned_paths`
  for a host), validates values and returns `SettingsPlan { apply, skipped (with
  SkipReason), support per consumer, migrations on offer, notes }`. Reserved paths
  (routing, `model`, provider tables, profiles, credentials store) are never
  settings. Removals are never version-filtered; unknown versions filter nothing.
  `find_migrations` / `apply_migrations` handle legacy keys (aliases, curated
  replacements, ignored keys) as profile edits; `preview_changes` turns an apply
  plan's key changes into `(key, from, to, target, reason)` rows;
  `cross_check_features` compares `codex features list` (`FeaturesList::parse`)
  with the catalog, runtime data winning.
- **Ownership.** `ResolvedProfile::settings` (= `SettingsPlan::apply`) is rendered as
  owned keys: Superba owns exactly the keys a profile manages; a key the profile
  stops managing is restored to the value it had before Superba last took it over;
  drift needs an explicit `KeyResolution`; a setting colliding with a user value of
  another shape is skipped with a note.
- **Catalog.** `superba_core::CatalogSource` returns a `CatalogSnapshot` for an exact
  Codex version (else nearest older = approximate, newer = outdated, below floor);
  `superba-catalog`'s `SnapshotSource` implements it (section 10). There is one
  loader, `superba_switch::settings_catalog(data_dir)`: the planner, `spb
  settings` (through the service), remote hosts (`for_os`), the Chinese
  explanations and the GUI settings page all read the same store.
- **Krill's recommended keys** are ordinary settings: new Krill profiles (wizard,
  Connection page, `spb profile add` unless `--no-recommended-keys`) start with
  `CodexSettings::krill_recommended()`. They are written whatever auth mode the
  line resolves to (also when Automatic falls back to an API key).

## 3. Facts the design rests on (Codex 0.162, desktop app 26.1013)

- Codex never watches `config.toml`/`auth.json`; provider, base URLs, auth and feature
  gates are fixed per app-server process, env vars read at start. Every switch needs
  an app-server restart.
- The app main process handles IPC `codex-app-server-restart`; from the main
  renderer (`app://-/index.html`, excluding avatar-overlay and quick-chat-prewarm
  targets) call `window.electronBridge.sendMessageFromView({type, hostId:'local',
  intent:'restart'})`; SSH hosts pass their `hostId` plus `killCodexProcess:true`.
  The only way in is `--remote-debugging-port`, so hot switching requires that
  Superba launched the app (loopback only, `--remote-allow-origins`).
- The new app-server is up in ~1 s; window, open thread and recents survive; a turn
  in flight loses its output. It inherits the main process env, so
  `CODEX_AUTHAPI_BASE_URL` stays what it was at app launch.
- Config shapes:

| Mode | config.toml (owned keys) | auth.json | env |
|---|---|---|---|
| Krill PAT | `model_provider="openai"`, `openai_base_url="<line>/backend-api/codex"`, `chatgpt_base_url="<line>/backend-api/"`, the profile's Codex settings | `{"personal_access_token":K,"OPENAI_API_KEY":K}` | `CODEX_AUTHAPI_BASE_URL=<line origin>` |
| Krill API key | `model_provider="openai"`, `openai_base_url` (`/backend-api/codex` or `/codex/v1` per `LineCaps`), `chatgpt_base_url` restored, the profile's Codex settings | `{"auth_mode":"apikey","OPENAI_API_KEY":K}` | none needed |
| Compatible site | `model_provider="openai"`, `openai_base_url=<base>`, the profile's Codex settings | API key form | none |
| Official | owned keys removed | stashed ChatGPT login restored | may stay |

- Never write custom provider ids or `[model_providers.openai]`. Sessions are listed
  only when their provider equals the current one (`state_5.sqlite`
  `threads.model_provider`, rollout `session_meta.model_provider`); legacy Krill
  users have `model_provider="krill"` and are migrated to `openai`.
- `thread_history_*.sqlite` stores byte offsets into the uncompressed rollout
  files (`thread_turns.rollout_byte_offset` / `rollout_end_byte_offset`,
  `thread_history_projection_state.next_rollout_byte_offset`). Any rewrite of a
  rollout line must shift them; `superba-sessions` does.
- PAT needs an `https` line: Codex rejects a plain-http `chatgpt_base_url`
  ("workspace backend must use an HTTPS origin") at the first turn, also on
  loopback. `ResolvedEndpoints::for_line` refuses it and the prober reports
  `pat_ok = false` (`probe.pat_needs_https`).
- At start-up the app runs the user's interactive login shell once and merges its
  environment over its own. With zsh an `rc` export of `CODEX_AUTHAPI_BASE_URL`
  (`~/.zshenv`) wins over the value Superba injects at launch; with
  `SHELL=/bin/bash` the injected value won over `~/.bashrc` / `~/.profile` exports
  (e2e, app 26.1007). The env persister writes its marker block before launching,
  so both carry the same value unless the user's own rc line comes after the
  block. A hot restart does not re-read either.
- The desktop app writes its own keys to `config.toml` at any time (`[desktop]`
  settings, bundled `[mcp_servers.node_repl]` / `[mcp_servers.cua_repl]`), also
  right after a hot restart; verification therefore compares Superba's owned keys,
  not the file hash. The app lists threads of every source except `exec` (so a
  migrated `codex exec` session stays out of the recents list).
- Remote compaction (Codex 0.155+; GitHub issue 3): Codex sends the history with a
  `{"type":"compaction_trigger"}` input item to the ordinary `…/responses`
  endpoint (HTTP or WebSocket), expects exactly one `compaction` output item
  (`encrypted_content`, readable only by the provider that made it) and sends it
  as input in every later request of the conversation; `…/responses/compact` is
  no longer called. The built-in `openai` provider always compacts remotely
  (0.149–0.161: default-on `features.remote_compaction_v2`; 0.162+: the
  provider's capability). `capabilities.remote_compaction = "unsupported"`
  exists only on `[model_providers.<custom id>]`, and built-in ids cannot be
  redefined, so for the configuration Superba writes there is no local
  compaction switch: Superba warns and routes instead (section 8).
- Codex trusts extra roots from `CODEX_CA_CERTIFICATE` or `SSL_CERT_FILE`.
- On Linux, Chromium overwrites `/proc/<main pid>/environ` (the app's environment
  is read from its app-server child) and rewrites the command lines of its
  `--type=` children into one space-separated word.
- OS process start times are whole seconds and, on Linux, derived from the boot
  time, which drifts from the wall clock (~0.7 s measured); comparisons with
  Superba's own stamps use `rules::PROCESS_START_SLACK_MS` (5 s).

## 4. Data flow of one switch

```
UI selects a profile / line (auth: the profile's choice, Automatic by default)
 └─ SwitchService::plan(SwitchRequest)                       [superba-switch]
     ├─ ConfigEngine::recover / read_state / baseline        pending journal, provider, drift
     ├─ ProfileStore::load + SecretStore::get                resolve profile and key
     ├─ LineProber::probe / check_key                        LineCaps, key access per line
     ├─ AppController::detect_installs / codex_binaries      version gates (table in switch)
     ├─ LineProber::list_models                              model catalog check
     ├─ SessionMigrator::scan                                legacy provider? reverse migration?
     ├─ AppController::running_session / probe_bridge        CDP + bridge + app's authapi env
     ├─ LineProber::check_authapi                            whoami health on the app's origin (rule 7)
     ├─ AppController::turn_activity, RemoteHosts::turn_activity
     ├─ ConfigEngine::plan, EnvPersister::plan, SessionMigrator::plan, RemoteHosts::plan_apply
     └─ SwitchPlan { primary: RestartKind (+ reason), restarts, diffs, choices, alternatives, warnings }
 └─ UI: hot -> countdown bar (Armed, default 5 s, Esc = zero writes)
        full restart / migration -> explicit confirmation (ConfirmFull)
        blocked -> reasons; choices -> re-plan with answers
 └─ SwitchService::execute(plan, options, decider, events)
     ├─ TurnCheck / WaitingTurn                              turn_activity; SwitchDecider on busy threads
     ├─ (full) Quitting                                      AppController::quit; ForceQuitConfirm via decider
     ├─ (migration) CloseConsumers                           stop daemon; user closes TUI/IDE, or asks Superba
     │                                                       to stop them (StopProcesses: graceful where the OS
     │                                                       allows, then ForceStop only on a second answer)
     ├─ Applying (state.lock + switch journal)               snapshot_baseline (first time) -> ConfigEngine::apply
     │                                                       (auth.json then config.toml) -> EnvPersister::apply
     │                                                       -> SessionMigrator::migrate; any failure -> rollback
     ├─ Restarting                                           re-check turns 200 ms before IPC;
     │                                                       AppController::hot_restart(local)
     │   or Launching                                        AppController::launch (CDP + CODEX_AUTHAPI_BASE_URL)
     ├─ remote hosts in parallel                             RemoteHosts::apply -> hot_restart(hostId, kill) -> verify
     ├─ Verifying                                            new app-server after T0, hashes, whoami/models, renderer
     ├─ NavRestore / Recovery / RollingBack                  return_to_thread; retry IPC once; rollback + restart
     └─ SwitchOutcome (+ stale CLI processes, runtime view, remote results, notices)
```

The restart decision (first match wins) is documented on
`superba_core::RestartKind`: blocked → session migration → cross auth family → app
not running → no CDP / no bridge → PAT without authapi env → PAT with unhealthy
authapi → hot. CLI processes get `NextLaunch`, the daemon `DaemonRestart`, remotes
`AppServerHot { host_id, kill_remote: true }`.

The state machine is split: `Idle`, `Planning`, `Blocked`, `Armed`,
`ConfirmFull` live in the UI (no writes happen there); `execute` reports the rest
as `SwitchEvent::Phase`. Mid-execution questions go through `SwitchDecider`; the GUI
shows a dialog, `spb` answers from flags (`--wait-turn`, `--force`,
`--allow-full-restart`) or `DecisionRequest::safe_default`.

Remote discovery merges three sources: `RemoteHosts::discover(home, from_app,
live)` reads `.codex-global-state.json` itself and receives the hosts the renderer
knows (`AppController::remote_connections`, with `hostId`) and the live ssh links
(`AppController::processes`), so `superba-remote` never needs a CDP client.

## 5. Storage

Codex home (`CODEX_HOME` or `<home>/.codex`, `superba_core::CodexHome`):
`config.toml`, `auth.json`, `state_5.sqlite` (under `CODEX_SQLITE_HOME` if set),
`sessions/`, `archived_sessions/`, `.codex-global-state.json`,
`app-server-control/`, `tmp/superba-migrate.lock`, and Codex's
`thread-writer-locks/` (a session migration holds its `.coordination.lock` for the
whole write and only probes the per-thread locks, one at a time).

Superba data dir (`SUPERBA_DATA_DIR` or the platform data dir,
`superba_core::DataDir`):

| Path | Owner |
|---|---|
| `profiles.json`, `state.json` (generations, also the `handed_off` record), `journal/config/` | config |
| `state.lock`, `journal/switch/` | switch |
| `backups/first-write/`, `backups/switch/`, `backups/login-stash/` (encrypted), `backups/rc/` | config |
| `secrets/` (0600 key files when no keyring) | config |
| `backups/editor/` (the file editor: an encrypted copy of the file each save replaces, `index.json` with the backups and the last save per home and file; GitHub issue 9) | config (`codex_files::EditorStore`) |
| `backups/migrate-<id>/` (sqlite backup, rollout pre-images, ledger) | sessions |
| `app-sessions.json` (apps launched by Superba: pid, CDP port, start time) | appctl |
| `remote/` (per-host cache, journals) | remote |
| `logs/` | binaries |
| `cache/usage/` (usage reports per site, key fingerprint and range; no keys) | net |
| `cache/transports.json` (WebSocket / zstd checks per site, line, base URL and key fingerprint, with the time of the check; no keys) | switch |
| `cache/compaction.json` (remote compaction checks per site, base URL and key fingerprint, with the time and model of the check; no keys) | switch |
| `kept-model.json` (per Codex home: the `model` value a switch kept across a change of authentication and the generation that wrote it, user bug 58; per WSL distribution: the value and the distribution's `CODEX_HOME`, user bug 63) | switch |
| `imported-usage.sqlite3` (usage history imported from cc-switch / AI Toolbox: requests by the tool's own id, daily totals; numbers, model and provider names only) | switch |
| `cache/import/import-<uuid>/` (a private 0700 copy of another tool's database with pending `-wal` / `-journal` changes, removed right after reading) | import |
| `router/` (`config.json`, 0600 `secrets.json` and `runtime.json`, log, `compactions.json`: which member made each compaction item, by fingerprint) | router |
| `catalog/` (a downloaded, signature-checked settings catalog; unused while remote updates are off) | catalog |
| `selfupdate.json` (ETags and answers of the release check, last check time, skipped version, macOS backup to remove, the record of the last install attempt; no secrets), `downloads/superba-update/` (verified Superba packages and staging), `logs/selfupdate*.log` (the install attempts' steps) | versions |

Shell rc marker blocks (`# >>> superba >>>` … `# <<< superba <<<`) and legacy-line
comments (`# [superba] export …`) are rendered by `superba_core::env` for both local
and remote writers.

## 6. Testing strategy

- **Unit tests** in each crate's `tests/` (public API) or `#[cfg(test)]` modules for
  private helpers. Pure logic (render, decision table, parsers, version gates,
  marker blocks) is tested exhaustively, including golden files for
  `config.toml` with comments, CRLF, CJK paths, MCP servers and plugins: everything
  except owned keys must stay byte-identical.
- **Isolation.** Nothing reads the real home: implementations take a `LocalContext`
  (`LocalContext::for_tests(tempdir)`), `MapEnv` replaces the process env and
  `ManualClock` replaces time. Tests never touch the real `~/.codex`, shell rc
  files, registry, keyring entries of real profiles, or running Codex/ChatGPT
  processes; they kill only processes they started, by PID.
- **Fakes.** `superba-switch` tests implement the component traits in-memory and
  drive plan/execute through every 7.1 rule and 7.2 path (cancel during countdown
  = zero writes, waiting turns, IPC failure, verification failure and rollback).
- **e2e (Linux).** `e2e/run.sh` (see `e2e/README.md`) drives the real desktop app
  (extracted Linux build) and the real Codex under Xvfb (`:173`–`:179`) with an
  isolated `HOME` (`/tmp/sbe2e-<n>` → `e2e/.work/<run>/home`), a random CDP port and
  `spb` / `superba` release binaries: legacy takeover with session migration,
  launch with CDP, hot switches of line and auth mode with the thread preserved,
  busy-turn handling (cancel, `--wait-turn`, `--force`), the unhealthy launch-time
  auth origin (rule 7) full restart, `codex exec` from a login shell, restore of
  the original configuration with reverse migration, the remote Docker test and a
  GUI smoke with a GUI-driven hot switch. Lines A/B/C are logging forwarders on
  `https://127.0.0.1:<port>` with a throwaway CA (Codex and Superba trust it
  through `SSL_CERT_FILE` / `CODEX_CA_CERTIFICATE`), so PAT works end to end. The
  private relay origin, key and app path come from the file named by
  `SUPERBA_E2E_ENV` only. Hooks used: `SUPERBA_APP_PATH` / `SUPERBA_APP_DIR`,
  `SUPERBA_APP_ARGS` (`--no-sandbox --disable-gpu --password-store=basic`), `HOME`
  (plus a private `XDG_RUNTIME_DIR`; no inherited `CODEX_*` / `XDG_*` / DBus
  variables), `SUPERBA_SECRET_BACKEND=file`, `SUPERBA_TEST_SITES` (a Krill-kind
  site whose lines are the forwarders); also available: `SUPERBA_DATA_DIR`,
  `SUPERBA_TEST_LINES` (`a=https://127.0.0.1:P1,...`, loopback lines added to the
  Krill site) and `KeyRef::Env` keys (`spb ... --key-env`).
- **Gates.** `cargo clippy --all-targets -- -D warnings` and `cargo test` must stay
  green per crate; `cargo fmt -p <crate>` on owned crates.

## 7. Platform matrix

| Concern | Linux | macOS | Windows |
|---|---|---|---|
| App discovery | deb `/usr/lib/chatgpt`, `.desktop` Exec | bundle ids `com.openai.codex` / `com.openai.chat` / `com.openai.chatgpt` with a `*Framework.framework` (26.10 ships `Codex Framework.framework`) and `app.asar` | MSIX families `OpenAI.Codex_*`, `OpenAI.CodexBeta_*`, `OpenAI.ChatGPT-Desktop_*` |
| Launch with CDP + env | spawn + `setsid`, env in child | LaunchServices: `/usr/bin/open -n -a <bundle> --env K=V --args ...` (the app is its own TCC-responsible process and does not inherit Superba's environment); main pid found afterwards (spike S2) | AUMID activation (spike S3); env via HKCU only |
| CDP port | `--remote-debugging-port=0` + `DevToolsActivePort` | same as Linux (unverified) | reserved free port, verified after start |
| Hot switch | verified (26.1013) | pending S2; fallback full restart | pending S3; fallback full restart |
| Env persistence | rc marker blocks, `~/.config/environment.d` (re-login) | `launchctl setenv` + LaunchAgent + `~/.zshenv` block | `HKCU\Environment` + `WM_SETTINGCHANGE` |
| Process / port owner checks | `/proc` (fail closed: a listener whose socket inode is not among the app's readable fds is a mismatch) | `lsof` | `GetExtendedTcpTable` (IPv4 + IPv6), ToolHelp |
| Graceful quit | SIGTERM, confirm before kill | `NSRunningApplication.terminate` | `WM_CLOSE`, confirm before `taskkill /F` |
| ssh client | system OpenSSH | system OpenSSH | `System32\OpenSSH\ssh.exe` (ssh-agent often disabled) |
| Keyring | Secret Service (file fallback when headless) | Keychain | Credential Manager |
| App update | verified `.deb` / `.rpm`, install command shown | Sparkle (the app's own) | MSIX from `windows-release.json` (persistent.oaistatic.com) or the Store CDN via store.rg-adguard.net (`superba_versions::winapp`); size / SHA-1 / manifest identity checked, `WinVerifyTrust` signer must be OpenAI's Store publisher (`CN=50BDFD77-…`, Microsoft Marketplace CA); opened with App Installer (`ShellExecuteExW` "open"), never `Add-AppxPackage`, never elevated (`superba_appctl::package`) |

Platform code sits behind `cfg(target_os = ...)`, is kept small and commented, and
must compile on Linux. Linux is the reference implementation and the only platform
exercised end-to-end in CI at first.

## 8. Account data, router mode and account-display auth

### Account and usage data (standard endpoints only)

Superba reads account data only through standard, optional endpoints on the
selected line origin with `Authorization: Bearer <key>` (`crates/superba-net/src/
{account,org,fetch,cache}.rs`). A missing endpoint (404/405/501), a non-JSON or
foreign-format answer or a network error hides that piece of data and never blocks
switching; a 401 means the key is invalid; keys never reach logs, errors or cache
files.

| Data | Endpoint | Sites |
|---|---|---|
| Limit, used, remaining, plan, expiry (`LineProber::account_summary`, cached 60 s in memory) | `GET /v1/dashboard/billing/subscription` + `/v1/dashboard/billing/usage?start_date&end_date` (each falls back to the path without `/v1`); used = month-to-date `total_usage / 100`, remaining = `max(hard_limit_usd - used, 0)` | every key-based site |
| E-mail | whoami (its plan type is a constant and never shown) | Krill |
| Daily cost (`LineProber::usage_report`) | `GET /v1/organization/costs`, else the summed `cost_usd` of the usage API | Krill (`usage_api`) |
| Per-model usage | `GET /v1/organization/usage/completions?group_by=model` | Krill |

Krill allows 10 usage requests per hour per user, so usage reports are fetched only
on demand (`spb account --usage`, the usage view), cached on disk per (site, key
fingerprint, range) under `<data dir>/cache/usage/` for 10 minutes, and a 429 keeps
the saved report with a "rate limited" notice. `SwitchService::account` combines both
for a profile.

### Router mode (opt-in)

`superba-router` is a per-user loopback service; `superba-switch` owns its use
(`crates/superba-switch/src/router.rs`). Members are profiles
(`Settings.router.members`, priority order), each with its own site, lines (the
profile's priority, then the site's other lines) and key. While router mode is on,
the planner points the local Codex home at `http://127.0.0.1:<port>/backend-api/codex`
in API-key mode with the router token (`AuthReason::RouterMode`); remote hosts get the
direct resolution and the official login is never routed. `SwitchIntent::Router
{ enable }` turns it on or off through a normal switch (the flag is stored after
success); switching profiles moves the chosen profile to the front of the members
(a router reload, no Codex restart when Codex's own configuration is unchanged).
`SwitchService::router_watch` is one watchdog tick (20 s grace), active while the
local Codex home points at the router (`codex_routed`: its endpoint, or any earlier
port of it with the router token in `auth.json`): a router that stays down makes
Superba run a router-off switch to the applied profile (`router_fall_back`:
`SwitchIntent::Router { enable: false }`, write-only when it would need a full
restart, drifted keys kept except the routing keys). Router mode is stored as off
only when that switch succeeds, so a failed attempt is retried at a later tick.

**The router's port** (user bug 65; `superba_router::port`). A first
configuration gets a stable per-user port: the candidates are every port of
17000–19999 in a fixed order derived from the data directory's path (SHA-256;
stride 1009, so neighbours are far apart), those inside an OS ephemeral range
last (Linux reads `/proc/sys/net/ipv4/ip_local_port_range`, macOS the
`net.inet.ip.portrange.first/last` sysctls, Windows uses its documented
49152–65535), and the first one the router could bind now on `127.0.0.1` and
its WSL addresses is taken (Hyper-V's excluded port ranges refuse the bind).
It is stored in `config.json` and kept by every rewrite, reinstall and restart:
a port from `bind(:0)` is drawn from the ephemeral range every program's
outgoing connections use, and after a reboot another program could hold it
while the service manager kept restarting a router that could not listen. The
port moves only when another program (not a Superba router, `doctor::port_state`)
listens on it while the router is down (`RouterManager::port_to_leave`): the
watchdog, at the end of its grace period, asks `RouterControl::move_off_taken_port`
(store the next free candidate, restart the service, wait for it) and then
runs a router-on switch (`Switcher::router_repoint_impl`: `SwitchIntent::Router { enable: true }`
to the applied profile with the fallback's rules) that points Codex, and the
WSL distributions that follow this computer, at the new port; the tick reports
`RouterWatchState::Moved { from, to }`. Install / Restart (`RouterManager::enable`)
moves off a taken port the same way and `router_install` then points a routed
Codex at it at once. A tick that finds the router answering on another port
than Codex uses repeats that switch; until it succeeds the tick counts as an
outage, so the usual fallback follows after the grace period. Only when the
move fails does the watchdog fall back to direct mode. The WSL 2 NAT listener
binds the gateway on the router's current port, and the firewall rule names
no port, so it follows without another administrator approval.
In router mode `config.toml` keeps Codex's model name: the router maps names per
member. Codex chooses a model itself only when `config.toml` names none, and the
choice depends on the authentication: with a PAT it is the default of the
provider's own list (sorted by priority, the first entry shown in the picker),
with an API key (also through the router) Codex never reads that list and the
desktop app picks a default of its own (its fallback is `gpt-5.5`). So a switch
on the applied site whose destination sets no model and that changes the
authentication (PAT <-> API key, direct <-> router) writes the model Codex runs
with now and names it in the plan (`superba_switch::model_keep`, user bug 58;
preview reason "Keeps the model in use"); the value is Superba's
(`ConfigState::model_owned`, `kept-model.json`) and a later switch drops it
again when the PAT's own default is the same model. A WSL distribution that
follows the switch gets its own decision by the same rule (user bug 63): what
its Codex runs now (the model kept there, else the default its PAT gets from the
line its `config.toml` names, read with its own Codex version) against what its
destination gives it (routed = API key; a distribution that cannot reach the
router stays direct and keeps nothing); it is on the destination's site when its
line is one of the site's or it is routed while this computer runs that site.
This computer's kept model is never copied to a distribution, and a model of the
distribution's own is left alone. The plan names it per distribution
(`switch.warn.on_host`) and marks its preview row; `kept-model.json` records it
while the distribution's `config.toml` holds that value as a key Superba wrote
(`RemoteState::owned_paths`). With "Run Codex in WSL" the app's Codex in the
default distribution reads this computer's `config.toml`, so the local decision
is that Codex's, and the plan says so (`switch.models.keep_legacy_wsl`). A relay without capacity
for a model answers with a stream that ends in a retryable `server_busy`; that
stream has started, so the router passes it to Codex from its line and never
tries the member's other lines with it. The GUI shell runs a tick every 5 s while router
mode is on; `spb router watch` does the same from a terminal. Without either,
Codex waits on the dead loopback port ("Reconnecting...") until the service
manager restarts the router.

**Upstream transports per line** (user request 14). `LineProber::probe_transports`
checks a line without a billed turn: the Responses WebSocket by its handshake
alone (101 = supported, closed at once with a close frame; 426 or no upgrade =
not; 401/403/429/5xx = unknown) and zstd request bodies by one tiny invalid body
(empty model, no input, `max_output_tokens: 0`) sent plain, zstd-compressed and
as undecoded bytes (decoded when the compressed body is rejected exactly like
the plain one; 415 or a rejection like the undecoded bytes = not; identical
answers to all three = unknown). `SwitchService::line_transports` probes a
provider's lines at the base URL the router uses (missing or older than 24 h,
or all on Test), caches the results and, for a router member, rewrites the
router's `LineConfig::zstd` / `LineConfig::websocket` hints and the member's
WebSocket flag (Krill: unless every line refused; a compatible site: when some
line upgraded). The router then zstd-compresses Responses `POST` bodies of at
least 1 KiB that Codex sent uncompressed (API-key mode never compresses) for
lines that accept zstd, passes Codex's own zstd bodies through, sends a body
refused with 400/415/422 again uncompressed to the same line and remembers a
real refusal for 24 h; WebSocket handshakes skip lines that refused one (probe,
or a 426 met at run time, remembered for 1 h) and stay on the session's member,
otherwise the router answers 426 itself. HTTP/SSE turns are not upgraded to an
upstream WebSocket: Codex already opens a WebSocket per session whenever the
router accepts the handshake, a per-request socket would carry the same full
input plus a handshake, and the only gain (incremental `previous_response_id`
input) would need a per-session transcript that matches Codex's own
incremental-items logic exactly across failovers, or a conversation silently
loses context. The request log records how each body went upstream per
attempt (`RequestAttempt::compression`: uncompressed, zstd from Codex, zstd by
the router, zstd refused and resent; the answering attempt's value is shown in
the request detail); the transport (`RequestRow::transport`) is the same on both
legs.

**Remote compaction** (GitHub issue 3; `superba_router::compaction`,
`superba_switch::compaction`). Each member carries its
`MemberConfig::compaction` (native / unsupported / unknown): the profile's
choice (`Profile::compaction`), else a definite result of
`LineProber::probe_compaction` (Codex's own V2 request with a two-message
conversation; a few tokens; cached in `cache/compaction.json` 7 days, an
inconclusive answer 1 day; run by the provider page and `spb profile compaction
--check`, never by planning or routing), else Krill's built-in fact (native).
The router reads each Responses body (decoded zstd) and `response.create` frame
that contains the bytes `compaction`: a `compaction_trigger` (the request
compacts) and the SHA-256 fingerprints of `compaction` items. It learns the maker
of each compaction item from the answer's output item (and adopts an unknown
item for the member that answered a request carrying it successfully), kept in
`router/compactions.json` (20 000 fingerprints, 90 days). Routing: a known maker
→ only that member's lines, no failover to other members, 503
`compaction_owner_unavailable` when none answers; an unknown maker or a
compaction request → members not marked unsupported; none left → 400
`compaction_unsupported` with an explanation. A WebSocket socket stays on its
line, so a frame that must go elsewhere is not forwarded: the session is
re-pinned and the socket closed with 1012, Codex reconnects (its handshake
follows the pin) and resends; a refusal is an `error` event. Bodies are never
changed (no simulated compaction). Planning reads only caches and the rollout
index: it warns when the destination cannot compact (or was never checked),
names the router members compaction skips, and counts the conversations of the
last 7 days whose latest compaction was made by a provider on another site.

**Content another provider encrypted** (GitHub issue 11; `superba_router::cipher`,
`superba_switch::ciphers`). A Codex conversation carries opaque state only the
backend that made it reads: `reasoning.encrypted_content`, the payload of a
multi-agent `agent_message` (`amsg_…`), encrypted tool outputs (`notes` /
`history`), encrypted function-call arguments, compaction items, and with
output-token replay the `encrypted_content` of messages and tool calls. Codex
strips none of it for the built-in `openai` provider, and stores a sub-agent's
task as an `encrypted_content` part unless the provider marked the call's
arguments as plain (`encrypted_function_args: []`, which only OpenAI backends
send), so a sub-agent spawned on another provider leaves plain text where an
OpenAI backend expects its own ciphertext. Any move of a conversation between
providers — a direct switch, a failover, a reload that moves conversations to
the new first member — then gets `invalid_encrypted_content` on every turn. The
router reads the ciphertexts of each Responses body and `response.create` frame
(fingerprints, places), recognises a refusal (HTTP 400 / 422 / 502 JSON before
any byte reached Codex; a WebSocket `error` / `response.failed` before any
output) and what it names (item id, the ciphertext's first and last four
characters, the function-output wording), and neutralises byte-preservingly:
reasoning items go, an encrypted part holding plain text becomes `input_text`
with that text, a real ciphertext part or argument becomes a short note, an
item's own `encrypted_content` field goes, a compaction item is replaced by a
note only when named. On a refusal it neutralises what is named plus every
other non-compaction ciphertext the member is not known to read, remembers them
as refused by that member (`StatsHub::ciphers`, in memory, 100 000 fingerprints)
and sends the request once more (HTTP: at once, to the same line, never a
failover and no line is blamed; WebSocket: the socket is closed with 1012 and
Codex sends the turn again on a new one). Later requests to that member are
neutralised before they leave, so the conversation presents the same bytes
every turn. A member reads a ciphertext only when a request carrying it ended
with a completed response (a 200 whose stream fails reads nothing). A
sub-agent whose own task is a real ciphertext the member cannot read is answered
400 `encrypted_task_unreadable` instead. Each repair is a `RouteEventKind::
Ciphertext` event and a request attempt / log line. In direct mode Superba is not
in the path: the planner counts the conversations of the last 7 days last
continued on a provider of another "cipher family" (official and Krill share
the ChatGPT Codex backend; a compatible site is its own;
`switch.warn.ciphertext_elsewhere`, else `switch.warn.cross_site`), and the
overview names conversations whose latest turn stopped for it in the last 24 h
(the rollout index reads Codex's recorded refusal as `invalid_encrypted_content`;
`switch.overview.encrypted_refused`). No rollout is rewritten: a "repair this
thread" would have to rewrite every reasoning line and shift Codex's
`thread_history_*.sqlite` offsets, and the router repairs requests without it.

**Local budget** (user bug 35; `superba_router::limits`). Each proxied request
holds two sockets and a WebSocket session two for its life, and launchd starts
agents with a soft limit of 256 open files. The router sizes its budget from the
soft limit `L` it finds at start (after raising it towards 8192; the LaunchAgent
plist sets `SoftResourceLimits` / `HardResourceLimits` `NumberOfFiles` and the
systemd unit `LimitNOFILE` as well): `L/4` admissions (4 to 512) for proxied
requests (until the response body ends) and WebSocket sessions, at most `L/2`
inbound connections (at the cap the accepted connection waits for a place and
open connections stop keep-alive), and a capped idle pool per upstream host. An
idle keep-alive connection closes after `Tuning::inbound_idle_timeout_secs`
(150 s, longer than HTTP clients such as Codex's keep idle connections, so a
client never reuses one the router has just closed); only while 90 % of the
connection budget is in use do idle connections close after
`Tuning::pressure_idle_timeout_ms` (5 s), one at a time until the pressure is
gone, never one with a response in flight. A request that gets no admission within
`Tuning::busy_wait_ms` (2 s) is answered 503 `local_router_busy` with
`Connection: close` and `Retry-After`. `EMFILE` / `ENFILE` met while accepting,
connecting upstream, probing or opening the request log is local: the request
gets the same 503 naming the limit, no breaker counts it, nothing fails over,
and it is logged at most once a minute. `RouterStatus::local` (budget, in
flight, busy answers, file-limit errors, the last one) feeds
`RouterView::local`, `spb router status`, the doctor's capacity row and finding,
and the Router page.

**Claude Code ingress** (user request 79; `superba_router::claude`). The same
process and port serve Anthropic Messages under `/claude`
(`ANTHROPIC_BASE_URL = http://127.0.0.1:<port>/claude`,
`superba_router::claude::base_url`): `POST /claude/v1/messages` (streamed or
not), `POST /claude/v1/messages/count_tokens`, `GET /claude/v1/models[/<id>]`.
Codex's prefixes are unchanged. The token comes as a bearer
(`ANTHROPIC_AUTH_TOKEN`) or as `x-api-key`; errors are Anthropic-shaped
(`authentication_error`, `not_found_error`, 529 `overloaded_error` for a local
shortage, 502 `api_error` when no line answered).

- **Members and eligibility.** `RouterConfig::claude` names which router
  members serve Claude Code (same lines, keys, breakers, sticky table, budget
  and request log) and each one's Anthropic endpoint: a path below every line's
  origin (Krill's test relay `/codex`, DeepSeek `/anthropic`, GLM
  `/api/anthropic`; empty for Krill's `/v1/messages`) or full URLs per line;
  `/v1/messages` is appended. Requests are passed through; there is no protocol
  conversion in this release, so a member without an Anthropic endpoint is not
  eligible (`ClaudeMember::eligibility` → `superba_core::ClaudeIneligible::
  NoAnthropicEndpoint`, "protocol conversion comes in 0.1.2"). A member can be
  reserved for Claude Code (`claude_only`): Codex requests skip it.
- **Routing.** Aliases first (`claude-haiku-*` → the model that serves
  Claude Code's own background ids), then the first matching route (exact or
  `*` pattern → targets in failover order, each with its upstream model name),
  else the eligible members whose model list names the model (the list Superba
  wrote, else the member's own `GET <base>/v1/models`, Anthropic or OpenAI
  shape, pages followed, kept 10 minutes), then members whose list could not be
  read. Stickiness by `X-Claude-Code-Session-Id` × model; failover before the
  first byte on connect errors, 502/503/504, 529 and edge interceptions; 4xx
  (429 included) and 500 pass through.
- **Per-provider adjustments.** `x-api-key` or bearer auth; `anthropic-beta`
  values passed, dropped, allow- or deny-listed; top-level `drop_fields`;
  `role: "system"` messages folded into user text for endpoints without
  mid-conversation system messages. The client's credential, `Accept-Encoding`
  and the router's internal headers are never forwarded; an unchanged body
  keeps its bytes.
- **count_tokens** passes through; a member answering 404/405/501 is estimated
  locally for a day (`claude::estimate`, about four ASCII characters or one
  CJK character per token).
- **Request log.** Every row carries `harness` (by the router token: Superba
  writes each harness its own derived token `sbs_…` for source `h:<harness>`
  or `h:<harness>@wsl:<distro>`, `superba_router::token::harness_token`; the
  `User-Agent` only as the fallback), `harness_version` (`claude-cli/<v>`),
  `protocol`, `requested_model` next to `model` (alias applied) and
  `upstream_model`, `session_id`, `turn_id` (`x-claude-code-prompt-id`),
  `agent_kind` (main / subagent / background from
  `x-claude-code-request-class`, `x-claude-code-agent-id`, `x-app: cli-bg`,
  else main when the request offers tools) and the host (`codex_host`).
  Claude Code sends the prompt id and request class only with
  `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1` in its environment. Anthropic usage is
  normalised to the log's semantics (input = `input_tokens` + cache reads +
  cache writes). The log's schema is version 3; a log of an earlier build's
  format is not read and starts again (no migration before the first release).
- **Stats line.** Per-harness switch `StatsLineSettings::harnesses` (default
  Codex and Claude Code; the line itself stays off by default). Claude Code's
  main conversation only, on the final message of a turn (`stop_reason`
  `end_turn`, never `tool_use`): a `text_delta` appended to its last text block
  (the block's stop is held until `message_delta` shows the stop reason), a new
  text block when it ends without one, appended to the last text block of a
  whole answer. Later requests have it stripped from assistant text blocks (the
  `⚡` + U+2060 marker); thinking, redacted_thinking and tool_use are never
  touched. Turn totals: every request with the same prompt id.
- **Wiring** (`superba_switch::claude_router`). Claude Code's route (Claude Code
  → 连接与模型, `spb claude route direct|router`) can be the router once the
  router is set up; applying it writes `ANTHROPIC_BASE_URL =
  http://127.0.0.1:<port>/claude`, Claude Code's own harness token as
  `ANTHROPIC_AUTH_TOKEN` and `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1` into
  `settings.json` (`superba_switch::claude::claude_router_route`). Every router
  configuration Superba writes carries the section, built from Claude Code's
  provider (first) and the router's member profiles that have an Anthropic
  endpoint; a provider that is not a router member joins as a Claude-only
  member; every model the provider's configuration names gets a route (the
  provider first, the others as failover); `claude-*haiku*` / `*sonnet*` /
  `*opus*` / `*fable*` map to the slot models. An apply through the router writes
  the section (and starts a stopped router) before `settings.json`; going direct
  writes the provider's values again and empties the section.
  `RouterManager::write_config` keeps a stored section for the members still
  there when a writer passes none. The Router page and `spb claude status` /
  `spb router status` list Claude Code's providers with
  `ClaudeIneligible::NoAnthropicEndpoint` for those without an endpoint.
  Uninstalling the router, removing its last member, or the watchdog (it also
  runs while only Claude Code uses the router: `Overview::claude_routed`) moves
  Claude Code back to its provider (route direct; `settings.json` rewritten, or
  Superba's keys taken out when the provider has no Anthropic endpoint) and says
  so (`RouterView::notes`, `RouterWatch::notes`); a router that moved to another
  port gets `settings.json` pointed at it again.

Lifecycle, the same in `spb` and the GUI (both only through `SwitchService`):

| Step | `spb` | GUI | Service |
|---|---|---|---|
| members, install / repair | `router install [--member P]...` | Router page: add, reorder, remove (only saved while the router is not installed), Install / Restart | `router_install` (members = profiles; keys copied into the router's 0600 key file; nothing written when the program is missing; a reinstall keeps the stored port and token, unless another program holds the port: then the router moves and a routed Codex follows) |
| member profile deleted or re-keyed | `profile remove`, `key set --profile P` | Connection / Sites | `delete_profile` / `save_profile` rewrite the router's members and key file (`RouterControl::rewrite`, never starts the service); the last member cannot be deleted while Codex is routed, deleting it otherwise uninstalls the router |
| on | `router enable` (starts a stopped router first) | Turn on / palette "on" (starts a stopped router first) | `SwitchIntent::Router { enable: true }` through the normal plan / preview / execute |
| watch | `router watch` | shell, every 5 s | `router_watch` -> `router_fall_back` |
| off | `router disable [--uninstall]` | Turn off / palette "off" | `SwitchIntent::Router { enable: false }` (or `router_set_enabled(false)` when Codex is not routed) |
| remove | `router uninstall`, `spb uninstall` | Uninstall, removing the last member | `router_uninstall` (refused while Codex is routed); Superba's uninstall removes it after restoring |

The request log (`superba_router::store::Reader`, read-only, also while the router
is stopped) groups rows by session or by provider: `RequestGrouping::Provider` and
`StatsGroupBy::Provider` key a row by its router member (the profile that
answered: Krill, each custom site) and list the provider's lines under it
(`RequestGroup::lines`, `StatsBucket::lines`; a line key is
`provider_line_key(provider, line)`). The Usage page shows the current
profile and line names for them; `spb usage log --provider P` and
`spb usage stats --by provider` use the same grouping.

The service merges that log with the model requests in Codex's rollout files
(`superba_sessions::usage::UsageIndex`: `token_usage_record` per response, the
`token_count` fallback of older Codex, failed and stopped turns; incremental and
bounded, read-only, numbers and ids only), so the Usage page and `spb usage`
work without the router (`superba_switch::usage`). A rollout row gets the
provider Superba had applied to the local home at that time (generation
history) and a cost from the price table; a request both sources describe (same
thread, tokens and time window) is listed once, as the router's row
(`RequestRow::source`). Columns only the router measures (first token, TPS,
line, transport, HTTP status) are `None` on rollout rows. The router records
numbers only: no request or response headers or bodies are stored.

The program is found by `superba_router::manager::current_router_exe`: next to the
running `superba` / `spb`, next to its resolved path (symlinks), on macOS inside a
`Superba.app` beside it or in `/Applications` / `~/Applications`, then on the
`PATH`. `RouterManager::enable` refuses a missing program and, on macOS, a disk
image or App Translocation path (the service would lose its program).

### Codex inside WSL (user bug 45)

The Windows app can run the Codex of a WSL distribution in two ways: the
"Run Codex in WSL" desktop setting (the local host inside the default
distribution, below) and WSL connections, host ids
`remote-wsl:<encodeURIComponent(distro)>`, which start the distribution's own
`codex app-server` (`/usr/bin/bash -lc`, stdio, Codex 0.141+). Superba treats
every distribution with a Codex as a managed host of kind `HostKind::Wsl`
(`superba_core::wsl`, `superba_remote::wsl`):

- Discovery: `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss` first
  (name, state, default; WSL 2 = `Flags & 0x8`; the `Version` value is the
  registration format, 2 also for WSL 1), `wsl.exe --list --verbose` (UTF-16LE
  output, decoded) as the fallback; utility distributions (Docker Desktop,
  Rancher Desktop) are skipped; `.wslconfig` gives the networking mode (`[wsl2]` over
  `[experimental]`).
- Transport: `wsl.exe -d <d> --cd ~ --exec /bin/sh -s` runs the same nonce-framed
  POSIX scripts as SSH hosts (hash-checked writes, journal, baseline, ownership,
  stash); `WSL_UTF8=1`; wsl.exe failures become `Error::Wsl`. The probe reads the
  app's view (`/usr/bin/bash -lc` with the interactive PATH) and the CLI's view
  (the user's login shell); app-server pids are the stdio ones started with
  `features.code_mode_host`. Env blocks go into the files both read (bash login
  files and the user's shell rc).
- Switching: WSL hosts follow this computer by default (`HostSettings::follows`),
  scope both (connection + settings filtered by the distribution's Codex
  version, no model catalog file; the model its Codex runs with kept by its own
  decision, user bug 63, see the router section); a distribution without Codex, or never
  written and without a baseline, is skipped with a note. Restore (original
  configuration) and uninstall also reach a distribution Superba wrote that no
  longer follows (its baseline is kept locally: `RemoteHosts::written`). Hot
  restart is the app's own: closing the stdio app-server makes the app respawn
  it with the new files.
- Stopped distributions (user bug 54): a switch never starts one (`wsl.exe -d`
  boots a stopped WSL 2 VM). The planner asks `wsl.exe --list --running
  --quiet` (`RemoteHosts::wsl_running`, starts no distribution) and probes only
  running distributions; discovery stays registry-only (cached 30 s, read again
  on an explicit refresh) and carries the last answer, a probe marks the
  distribution running. A stopped distribution worth coming back to (Superba
  wrote it, its last check found a Codex, or the user turned following on) is
  noted and, after a successful switch, recorded as pending in the host cache
  (`HostView::pending`: when, and the switch's intent); others get a short note.
  The next switch that finds it running updates it and clears the record; "Update
  now" on its page (`spb wsl update <distro>`) plans the intent for that
  distribution alone (`HostPending::request`), which starts it. Uninstall starts
  the stopped distributions Superba wrote. An unknown running state (WSL could
  not tell) keeps probing.
- Router mode from WSL: each consumer gets its own source token
  `sbs_<hex(source)>_<HMAC-SHA256(router token, "superba-router-source-v1\0" + source)>`
  (source `wsl:<distro>`), so the request log names the distribution
  (`codex_host`, from the internal `x-superba-codex-host` header, never
  forwarded). WSL 1 and mirrored networking reach `127.0.0.1`. WSL 2 NAT cannot,
  so `RouterConfig::wsl_listen` adds a listener on the distribution's default
  gateway (the Windows vEthernet (WSL) address, read from `/proc/net/route`)
  that accepts private peers and `wsl:` source tokens only and serves no admin
  API but `health`; reachability is checked from inside the distribution (bash
  `/dev/tcp` GET of `/_superba/v1/health`, instance id must match). When the
  Windows firewall blocks it, `spb wsl router-access <distro>` / the host page
  add one rule with an elevated `netsh` (UAC, per machine):
  `localip=<gateway> remoteip=LocalSubnet program=<superba-router.exe>`, on any
  local port (the router's port can move, user bug 65).
  Unreachable distributions stay direct with a note. An in-distribution router
  was rejected: it duplicates the members' secrets and state into every
  distribution, keeps the WSL VM alive, and splits the request log.
- Sessions: scans run inside the distribution; migrations run Superba's Linux
  `spb` inside it (`RemoteHosts::run_helper`: streamed in once per checksum to
  `~/.local/share/superba/wsl-helper/<sha256>/spb`, verified, run with the
  host's `CODEX_HOME`, progress lines streamed back), because SQLite locking and
  Codex's `flock` writer locks do not work over `\\wsl.localhost`. Backups and
  the migration history stay inside the distribution. The helper is
  `SUPERBA_WSL_HELPER` or `spb-linux-<arch>` next to the program; the MSI
  ships `spb-linux-x86_64`, a static musl build
  (`scripts/release/build-musl.sh`).
- CLI: `spb wsl list` (state, pending updates), `spb wsl router-access <distro>`,
  `spb wsl update <distro>`, and `wsl:<distro>` as a host shorthand
  (`spb remote ...`, `spb sessions --host wsl:<distro> ...`).

"Run Codex in WSL" (`runCodexInWindowsSubsystemForLinux`, user bug 54 (b);
`superba_core::wsl::runs_codex_in_wsl`): the `[desktop]` table of the Windows
`config.toml` (else the not yet migrated top-level key of
`.codex-global-state.json`) turns it on; the app then runs its local app-server
as `wsl.exe -d <default distribution> --cd <dir> --exec /usr/bin/bash -lc 'set -m;
/bin/bash -c '… CODEX_SQLITE_HOME="$HOME/.codex/sqlite" exec "$(wslpath -u
'%CODEX_HOME%\bin\wsl\<16 hex>\codex')" … app-server …' <&0 & …; wait …'` (a
process-identity wrapper around its own Linux Codex copied out of the package; seen
in the test VM), with `CODEX_HOME` = the Windows Codex home forwarded by `WSLENV`
(`CODEX_HOME/p` → `/mnt/c/Users/<u>/.codex`). So that Codex reads **this
computer's** `config.toml` and `auth.json` (the local switch, restore and settings
already write them; only its SQLite index and plugin caches live in the
distribution), and:

- the planner says so on local switches; router mode is refused
  (`BlockReason::RouterFromWslNat`) when that distribution does not reach Windows
  on `127.0.0.1` (WSL 2 behind NAT: use mirrored networking or turn the setting
  off); WSL 1 and mirrored networking are routed like Windows;
- the `wsl.exe` child of the app main running that command is the app's
  `AppServer` (`superba_appctl::procs::is_wsl_app_server`), so hot restarts wait
  for it and verification checks it; it sees `CODEX_AUTHAPI_BASE_URL` only when
  `WSLENV` names it, which is what Superba reports for it;
- PAT mode: while Superba persists `CODEX_AUTHAPI_BASE_URL` in `HKCU\Environment`
  in this mode, `WSLENV` names it too (`EnvLocation::WslEnvForward`, added once,
  removed with the value or when the setting is off, the user's own entries
  kept; a full app restart passes it on, as for the variable itself);
- the distribution's own probe does not count that app-server or its wrapper
  (`…/bin/wsl/<hash>/codex` from the Windows home, `CODEX_SQLITE_HOME` in the
  wrapper's command), so the distribution is a host only for its own CLI and the
  app's WSL connection; `spb wsl list` names the distribution the app runs
  Codex in.

### Account-display auth (not implemented)

A planned auth mode shows the Krill plan, usage and name inside the app by
wrapping the API key in an unsigned ChatGPT-style JWT and pointing the app shell at
a localhost forwarder. The contracts leave room for it: a new `AuthMode` variant
(behaviour exposed through its methods, including its `AuthFamily`), a matching
`AuthShape`, rendering in `superba-config`, and `ResolvedEndpoints` with a loopback
`Origin` (already accepted over http). The forwarder's lifecycle (start, keep alive
while the app runs, verify, recover) is a side effect that pure rendering cannot own;
it would get its own `AuthRuntime`-style trait (`prepare` -> prepared material +
handle, `release`) in a new crate, called by `superba-switch` during execution, so
planning stays side-effect free.

### Provider import from other tools

`superba-import` reads the Codex providers of cc-switch (`~/.cc-switch/cc-switch.db`,
schemas 1 to 20, or `config.json` v1 / v2; the `app_paths.json` data-dir override)
and AI Toolbox (`<app data>/com.ai-toolbox/ai-toolbox.db`, JSONB documents, schemas
4 to 29; `AI_TOOLBOX_DATA_DIR`, `app_paths.json`) without writing to them:
rollback-journal databases are opened `mode=ro` in one read transaction, WAL
databases `immutable=1` when their WAL is empty and otherwise from a private copy of
the database and its WAL (header compared before and after, `quick_check`), a hot
journal likewise. An unknown layout or JSON version is skipped with a reason; a
newer schema whose provider table is unchanged is read with a note. `preview` maps
each provider (Krill line origin -> the Krill preset on that line, else a custom
site with its lines and model map), finds conflicts with existing profiles (same
name, same address) and pre-selects the importable ones; `plan` turns selections
(skip / rename / update) into sites and profiles. `superba-switch` re-reads the
sources for an import (keys never leave the service) and saves through
`save_site` / `save_profile` (secret store, router member resync); Codex is never
touched.

The same databases hold the tools' **usage history** (GitHub issue 1;
`superba_import::usage`, `superba_switch::usage_import`): `proxy_request_logs`
(one row per request, stable `request_id`, `app_type`, tokens, the tool's
`total_cost_usd`, `data_source` = its proxy or its reading of Codex's session
files) and `usage_daily_rollups` (totals of pruned days). Columns are detected
per database; only Codex rows are read; token semantics are normalised (cc-switch
`input_token_semantics`, AI Toolbox fresh input). `SwitchService::import_usage`
previews (counts, date range, recorded cost, rows covered by Superba's own
rollouts, other apps) or adds them to `<data dir>/imported-usage.sqlite3`,
keyed by (tool, request id) or the rollup's key, so a second import adds only
what is new; session-derived rows are imported only for threads the rollout
index does not know, session totals only for days before its oldest row. The
Usage page and `spb usage` merge imported requests like rollout rows
(`RequestSource::CcSwitch` / `AiToolbox`, ids below `-2^62`, filter
`RequestQuery::source`), and daily totals into the statistics only.

### Coexistence with other configuration managers (GH#5)

Superba owns exactly what its current generation wrote: the `last_written` keys,
the `auth.json` credential while that generation wrote a token credential, and
the `CODEX_AUTHAPI_BASE_URL` value the env persister set. Types in
`superba_core::ownership`.

- **Detection** (`superba-config`). `ConfigState::drift` (owned keys whose live
  value differs from what Superba wrote), `ConfigState::auth_drift` (the
  credential, compared with `authjson::credential_digest`: PAT, API key,
  `auth_mode` and the login identity, so AI Toolbox merging its key into the PAT
  form or cc-switch deleting the file count, a reformat does not; the shape shown
  is `authjson::drift_shape`). While Superba owns nothing (its current
  generation restored or handed off), `ConfigState::since_release` /
  `since_release_auth` list what changed since that write. `EnvReport::superba_value`
  is the value the persister set (`env-state.json` `written`).
- **Attribution** (`superba_import::detect`, read-only like the import). Each
  tool scores on its store (its current Codex provider uses the live address or
  key: strong; another provider: weaker; an applied ChatGPT account only when it
  is the signed-in e-mail) and its markers (AI Toolbox: `#:schema none`,
  `ai-toolbox-codex-model-catalog.json`, its gateway key and `/openai/v1`
  loopback address; cc-switch: the key as `experimental_bearer_token` with
  `auth.json` removed, `cc-switch-model-catalog.json`, `PROXY_MANAGED` / port
  15721, the first-write backup `.source` under `~/.cc-switch/backups/`).
  Otherwise: a ChatGPT login alone is `CodexLogin`, only the app's own keys
  (`model`, efforts, `service_tier`) `CodexApp`, else `Unknown`.
- **Overview and monitor** (`superba_switch::ownership`). `Overview::external`
  joins drift, credential and env drift (the persister is asked on the first
  overview and when the files' hashes change, never on a timer: macOS asks
  `launchctl`), attributed once per state of the files (`OwnershipCache`).
  The GUI's 5 s overview poll is the background monitor; `spb status` and `spb
  doctor` (`external_change`) show the same.
- **Before every write.** The renderer refuses unanswered drift of a key and,
  for token switches, of the credential (`Error::Conflict`); the planner turns
  it into `PlanChoice::OwnedKeyConflict { conflicts, auth, source }`. Answers go
  into `SwitchRequest::key_resolutions` per path (`auth.json` is
  `AUTH_JSON_PATH`); `ALL_CHANGED` (`*`) answers everything (take over),
  expanded by the planner against the state it read. A ChatGPT login is never a
  conflict for a switch (it is stashed first). Because `apply` re-renders from
  the live files, a change made between plan and write fails the apply safely.
- **Restores keep.** For Original / Official / Generation / Release
  destinations the planner answers `KeepCurrent` for every unanswered change
  (`ownership::keep_external`) and warns which were kept and by whom; the
  renderer keeps a changed credential by default for them. An apply that only
  releases ownership is committed even when no file changes
  (`ApplyPlan::releases_ownership`). The env persister's clear / restore leave
  locations whose value changed since it set it (`env.scope.kept_changed`).
  The router fallback writes nothing when the credential or `model_provider`
  was changed by someone else, and `codex_routed` is false for a non-`openai`
  provider.
- **Hand-off** (`SwitchIntent::HandOff { to }`, `ResolvedMode::Release`):
  rendered like an Original restore with provider `openai` kept, everything
  changed kept, no session migration, `RestartKind::None`; the generation records
  `handed_off` (`ConfigState::handed_off`) and keeps the post-apply copies of both
  files; the generation before it is what Superba had written (point-in-time
  restore). The next profile switch is a new takeover (fresh baseline = the other
  tool's configuration), so a later restore returns to it. Verification never
  sends a kept foreign credential anywhere (`PlanContext::kept_foreign_auth`).

### Editing Codex's files by hand (GitHub issue 9)

Settings → Edit Codex files (`superba_gui::pages::settings::files`) and `spb
config edit|check|backups|restore` edit `config.toml` / `auth.json` of the local
home, a remote host or a WSL distribution through four `SwitchService` methods
(`superba_switch::files`):

- **Open** (`codex_file`): the bytes and their hash, the keys Superba owns there
  (`ConfigEngine::owned_keys`; a host's `last_written`), whether it owns the
  credential, and a `FileSchema` for the check: the Codex versions reading the
  home (`local_consumers`: app-bundled Codex and CLI; the host's Codex from
  `RemoteHosts::read_codex_file`, which reads it with the probe) and the
  settings catalog of that OS. A pending config journal, an unreadable file or
  non-UTF-8 text make the document read-only.
- **Check** (`superba_config::codex_files`, pure, the same code in the GUI, `spb`
  and the service): TOML (`toml_edit` spans) / JSON (`serde_json`, plus a small
  span scanner) syntax with line and column; `config.toml` keys against the
  catalog with the settings engine's `key_support` / `validate_value` (all
  consumers missing = unknown key, deprecated / removed / legacy = warnings,
  a wrong type = an error that needs `force`; unknown tables are reported once,
  free-form tables are not descended); `auth.json` fields and types, a file
  without credential, partly changed placeholders. Messages never quote values.
  `format` tidies `config.toml` by normalising only `toml_edit` decor (key
  prefixes, ` = `, trailing comments, one blank line before headers) and refuses
  any result that parses to different data; `auth.json` is `serde_json` pretty.
  `Masker` replaces secret string values of `auth.json` (names with `token`,
  `key`, `secret`, `password`) by stable placeholders and puts them back.
- **Save** (`save_codex_file`): under `state.lock` (never during a switch),
  refused while a journal is pending, when the hash differs from the one opened
  (`Error::Conflict`), on a syntax error, and without `force` on a catalog
  error. The replaced file is sealed into `backups/editor/` first
  (`EditorStore::backup`, the vault's key); then the local file is staged and
  renamed after a second hash check (`codex_files::write_file`, auth.json 0600,
  config.toml keeps its mode, symlinks written through), or the host's file is
  replaced by the remote write script (per-home lock, compare and swap against
  the bytes opened, a backup in `<CODEX_HOME>/.superba/backups/edit-<id>/`, no
  first-write capture and no marker). The save is recorded
  (`EditRecord`: home, file, time, hashes).
- **Ownership (GH#5).** A save never touches `state.json`: Superba's owned keys
  and credential stay what its last switch wrote, so a managed key the user
  changed is ordinary drift. `Switcher::change_attribution` names the editor
  (`ChangeSource::SuperbaEditor`) when every drifted part is in a file whose hash
  is the editor's last save; the GUI lists it as a quiet to-do, never opens the
  GH#5 dialog by itself and offers "Keep my edits, stop managing" as the
  hand-off. The planner asks before a switch writes over it, restores keep it.
- **In effect (GH#4).** `RuntimeView::edited` carries the last local save when
  it is later than the applied generation; `runtime::effect_with_edit` counts a
  consumer started before it as running the previous file. The save's result
  carries the runtime view (local) or a reconnect note (remote).
- **Backups** (`codex_file_backups`, `codex_file_backup`): the editor's (all
  homes) plus, for the local home, the switch pre-images on disk
  (`switch-g<generation>-<file>`) and the first-write originals
  (`original-<file>`), newest first; a restore is a save with
  `SaveReason::Restore` (and a backup of its own).

## 9. Security

- CDP binds to loopback only, uses `--remote-allow-origins=http://127.0.0.1:<port>`
  with a reserved port (Windows; with port 0 no origin is allowed at all),
  verifies the listener PID is the app main process before the first evaluate (a
  mismatch disables CDP for that session), connects only while switching, and
  evaluates only the restart call and read-only checks. Safe mode launches without
  CDP (no hot switching); so does running as root.
- Keys are only sent to https origins (loopback allowed for tests), never logged,
  never placed on command lines (remote writes use stdin), stored in the keyring.
- `auth.json` is written 0600; stashes and backups of logins are encrypted
  (ChaCha20-Poly1305, key in the keyring).
- Superba refuses to run under `sudo` (euid 0 with `SUDO_USER`) and, on Windows, as
  another account than the user signed in at this computer (SYSTEM and the other
  service accounts, another user through `runas` or a scheduled task; the token's
  user SID compared with the SID of the user of the process's session, or of the
  active console session from session 0: `superba_switch::privilege`): switching,
  "Open ChatGPT", recovery and every `SwitchService` method that writes its files
  (`Switcher::refuse_elevated`); the GUI does not start at all as another account
  and says why in a message box (on the console user's desktop from session 0).
  An elevated token of the signed-in user is allowed (same account and profile;
  with UAC off every process is elevated); the GUI says so once. A
  real root user is
  allowed but never gets a debugging port (any local user could drive a root-owned
  Codex through it).
- Repository hygiene: no secrets, certificates, internal hosts or relay internals in
  tracked files; private material lives in the git-ignored `.secrets/` and
  `.private/` directories and is read at run time through environment variables.

## 10. Codex settings catalog

`superba-catalog` (library) and `superba-catalog-gen` (maintainer tool, never
linked into `superba` or `spb`) describe which `config.toml` keys and `[features]`
each Codex release understands. Sources per `openai/codex` release tag:
`codex-rs/core/config.schema.json`, the `FEATURES` registry in
`codex-rs/features/src/lib.rs` and the aliases in `codex-rs/features/src/legacy.rs`.

| Layer | Path | Owner |
|---|---|---|
| Per-version catalogs (upstream data only) | `catalog/dist/codex-<version>.json`, `dist/changes/*.md` | generator |
| SHA-256 manifest with a serial, ed25519 signature | `catalog/dist/index.json`, `index.json.sig` | generator / maintainer |
| Categories, platform hints | `catalog/curation.toml` | people |
| Official English (copied) + Chinese (people only) | `catalog/overlay/*.toml` | generator + people |
| Included releases | `catalog/sources.toml` | people |

- The tree is compiled in (`build.rs` packs it, zstd) and verified against the index
  at load. `CatalogStore::for_version` returns the exact version, else the nearest
  older one with a notice, or the latest with a "newer than the catalog" warning.
  Lookups never use `CodexVersion::meets`: the app bundles alphas that differ from
  their release (`0.162.0-alpha.17.2` is included explicitly).
- Explanations: `en` is upstream text only (schema descriptions, enum value docs,
  experimental menu text); missing text stays empty and is flagged. Chinese is
  written by people; `en_hash` / `zh_basis` mark stale translations. Rust doc
  comments are kept in `dist/` for reviewers and are not shown as explanations.
- Runtime cross-check: `parse_features_list` + `cross_check` compare
  `codex features list` of each target with its catalog version.
- Remote updates (`remote::refresh`) are off by default; when enabled they need a
  valid signature from `integrity::TRUSTED_KEYS` and a serial not below the one in
  use, and fall back to the built-in snapshot on any failure.
- `.github/workflows/catalog-update.yml` polls upstream tags every 6 hours and opens
  a pull request; the contributor guide is `catalog/CONTRIBUTING.md`.

## 11. GUI shell

`superba-gui` draws a sidebar (search button, then Overview; Connect: Providers /
Models / Router / Usage; Codex: Settings / Sessions / Remote hosts / Updates;
Superba: Diagnostics / Preferences at the bottom) and the page. There is no demo
mode; tests use an in-memory `SwitchService` under
`crates/superba-gui/tests/support`.

- **Page modules.** Pages with their own state (`pages::providers`, `models`,
  `router`, `settings`, `sessions`, `hosts`, `updates`) expose `State`,
  `show(ui, cx, state, out)`, `palette(cx, state)` and
  `command(state, cx, cmd, out)`; they return `Action`s and run background reads
  with `runtime::task` and `Cx::service`. The contract is documented in
  `pages/mod.rs`. Router (settings only: on/off, members and failover order,
  stickiness, prices, stats line, recording) and Usage (最近请求 / 活动请求 / 统计)
  share `pages::router::State`: `show` / `show_usage`, `command` /
  `usage_command`.
- **Command palette** (`palette.rs`, Cmd/Ctrl+K): entries from the shell (switch
  line or profile, restore, go to a page, test lines, open/restart the app) plus
  each page module's `palette()`; `Action::PageCommand` routes a choice back to
  its page.
- **Switching UI.** The plan states of 4. are a floating overlay at the bottom of
  the window (countdown hairline, Enter / Esc); it never moves the page. Line
  tiles, the Connection page and the palette show what Automatic authentication
  resolves to per line (`connection::decision_with`) but send no auth mode: the
  planner decides, and the overlay shows its fallback reason and the settings
  diff (`SwitchService::plan_settings`).
- **Account and usage.** The overview's account section calls
  `SwitchService::account` (identity from whoami, balance / plan / expiry from the
  standard billing endpoints) after each overview that changes the applied
  profile; the Usage page asks for a `UsageQuery` only when it opens without data
  for its range or on refresh (Krill rate-limits those endpoints; `superba-net`
  caches them on disk).
- **Tray and window life** (`tray/`, item 70). `tray::menu` builds the menu as
  data from `Data` (Open Superba, the provider and line in use with a switch
  submenu, ChatGPT through Superba, the router, Quit); the platform icon renders
  it: tray-icon + muda on Windows (`Shell_NotifyIcon`) and macOS (`NSStatusItem`,
  template image; the app leaves the Dock while hidden), ksni on Linux (a
  StatusNotifierItem over D-Bus, no GTK; no `org.kde.StatusNotifierWatcher` = no
  tray). Icon and menu events reach the app through a queue plus a repaint
  request, so a hidden window (eframe runs `App::logic` for it) handles them.
  `tray::close::WindowLife` decides what a close request does (hide, ask once,
  quit) from `Settings::close_action`; Quit from the tray or the palette always
  quits. A native Wayland window cannot hide: Superba opens through XWayland when
  it keeps running in the tray, or restarts itself with `--start-in-tray`. One
  window per user: a second start sends `show` over `instance.rs`
  (`superba-gui/1`, shared with `--launch-chatgpt`).
- **Fonts.** Geist Sans is bundled with tabular figures mapped as the default
  digits (`tools/freeze-tnum.py`), so numbers align without OpenType features.
- **Screenshots.** `tools/screenshots/shoot.py` runs the real `superba` / `spb`
  under Xvfb with an isolated `HOME`, a throwaway CA and `mockrelay.py` (three
  https lines serving whoami, accounts/check, models, billing, organization usage
  and costs, responses), optionally with the real desktop app for the countdown
  overlay, and writes `docs/screenshots/<page>-<theme>-<lang>.png`. Superba lists
  every desktop app of the user, so on Linux the script runs itself in an
  unprivileged user + PID namespace (same uid) and refuses to shoot while an app
  of another HOME is visible.

### Background probing (user request 34)

Nothing on a timer starts a process, and nothing runs while the window is hidden
except the router watchdog.

- **Environment monitor** (`superba_switch::monitor::EnvMonitor`, one thread
  `superba-env-monitor` per `Switcher`, started by the GUI's first
  `SwitchService::environment_event(Shown)`). It owns the slow facts: desktop app
  installs (paths, versions, MSIX packages) and Codex binaries (bundled and CLI,
  with versions). The overview, Updates, Models and Settings views read its
  snapshot (`Arc` clone); `plan`, `execute`, `recover` and installs still detect
  themselves. It detects again on: an explicit refresh (`EnvironmentEvent::Refresh`:
  Refresh, "Check now"; cached probes are forgotten, so each binary is asked once);
  an action (`invalidate` after `execute`, `install_codex`, `rollback_codex`,
  `versions(true)`); a change seen by the stamp check every 30 s while shown
  (`AppController::environment_stamp`: size and mtime of the install locations and
  the directories installs are discovered in, the PATH directories, the standalone
  Codex layout in the Codex home (`packages/standalone`, its `current` link), the
  binaries found and their launchers' targets, on Windows the MSIX repository
  registry key's last write time; metadata only, no directory listing; the Codex
  home's own files are read by the overview directly); the focus coming back
  (a check, a detection when the last one is over 10 min old); and every 30 min
  while shown. Hidden: no checks; a pending detection waits for the next read.
  `spb` and tests run no thread: the first read detects, events and `invalidate`
  make the next read detect again.
- **Probe caches** (`superba-appctl`): `codex --version` answers (also "none") are
  kept per binary file and launcher target stamp until an explicit refresh
  (`AppController::forget_cached_probes`); the Windows PowerShell package fallback
  (only without a readable registry) runs once until then; CDP listener checks
  (`lsof` on macOS) are kept for the life of the app process (pid + start time),
  negative answers re-checked three times with growing pauses.
- **Fast facts** keep cheap probes: the overview reads the process table once
  (`AppController::session_and_processes`), `config.toml` / `auth.json`, the
  bridge (CDP on loopback) and the account summary (cached 60 s by `superba-net`).
  The Router page's service status (`systemctl`, `launchctl print`, `tasklist`) is
  remembered by `RouterManager::overview` until a lifecycle call or a liveness
  change; liveness itself is HTTP on loopback. WSL distributions come from the
  registry (cached 30 s, read again on an explicit refresh); which of them run is
  asked (`wsl.exe --list --running`) only by a switch plan and `spb wsl list`,
  and nothing on a timer runs a script inside a distribution.
- **GUI cadence** (`superba_gui::cadence`): overview every 5 s while shown (an
  unchanged answer does not wake the window), router watchdog every 5 s also
  while hidden (eframe calls only `App::logic` then), remote host list every
  5 min and when the focus comes back after a minute, Superba's update check once
  a day while shown; window shown / hidden / focus events go to the monitor, and a
  new `environment_generation` reloads the views built from it.

## 12. Packaging and self-update

`superba`, `spb` and `superba-router` ship together in every release archive
(`.github/workflows/release.yml`, `scripts/release/`): next to each other in the
Linux tarballs and the Windows zip, `superba-router` inside
`Superba.app/Contents/MacOS` on macOS (with `spb` beside the bundle), and `spb` +
`superba-router` in the headless static musl archive. CI builds `spb` and
`superba-router` without GUI packages. Details: `scripts/release/README.md`.

### Self-update (`superba_versions::selfupdate`)

- **Check.** `SelfUpdater::check` follows the update channel (`UpdateChannel`:
  `Settings::update_channel`, else Preview on a pre-release build and Stable
  otherwise). Stable asks `<api>/releases/latest`; Preview asks
  `<api>/releases?per_page=30` and takes the newest release and the newest final
  release of that page by `semver` precedence (`release::parse_list`; drafts and
  tags the release workflow would not make are ignored), plus
  `releases/latest` only when the page holds no final release. Each answer is
  conditional on its stored ETag (both caches survive a channel switch), with an
  8 s deadline and no credentials, through the shared `http::client`. 404 / 401 /
  403 are `PageUnavailable` (the repository is private), network errors and 5xx
  are `CheckFailed`; neither is retried before 24 h / 1 h. A rate-limited answer
  (403 with `x-ratelimit-remaining: 0`, or 429) stores `x-ratelimit-reset` /
  `Retry-After` (at most an hour) as `limited_until`: no request goes out before
  it, also not for a manual check, and the cached answer stays. The cached answer
  is compared with `CARGO_PKG_VERSION` on every read (`semver` precedence: a
  pre-release is older than its release), so a finished update never shows itself
  as available and an older release is never offered. `SelfUpdater::install_version`
  installs a release the user named (`releases/tags/<tag>`), also an older one,
  except on Windows (the MSI only upgrades). The link to the release page is built
  from the tag, never taken from the API.
- **Assets** (`release::asset_name`): macOS `superba-<v>-macos-universal.zip`,
  Windows `superba-<v>-windows-x86_64.msi`, Linux `superba-<v>-linux-{x86_64,aarch64}.tar.gz`,
  the static build `spb-<v>-linux-x86_64-musl.tar.gz`. An asset counts only when the
  release lists it together with `SHA256SUMS.txt`.
- **Install** (`SelfUpdater::install`): download from
  `<downloads>/<tag>/<asset>` (override > GitHub; third-party proxies only when
  enabled and never for `SHA256SUMS.txt`), resumable, size from the release listing
  and SHA-256 from the same release's `SHA256SUMS.txt` verified **before**
  extraction (`http::download`), extraction with `extract::extract` (tar.gz) or
  `selfupdate::zipx` (macOS zip: no links, no traversal, `\` is a separator, CRC and
  size checked; the Windows MSI is not unpacked), a smoke test of the new `spb --version`, then the swap
  (`selfupdate::apply`): copies next to the targets, rename over them with the old
  file kept until all swaps worked (all or nothing); macOS renames the bundle
  aside as `.Superba.app.previous` (removed by the next start that runs the new
  version) and replaces the `spb` beside it; Windows downloads and verifies the
  MSI and hands it over (`SelfUpdater::windows_msi_handoff`): a copy of
  `superba.exe` outside the install folder runs `superba.exe --finish-update
  --wait-pid N --msi M ...` (`selfupdate::finish`), which waits for the old
  process, runs `msiexec /i M /passive` (per-user install, fixed UpgradeCode),
  runs `spb self-update restart-router` and starts the new Superba. If the
  location is not writable the verified package is kept and
  `SelfInstallOutcome::Manual` carries short steps.
- **Log and record.** Every attempt is logged step by step, with times, in
  `<data>/logs/selfupdate.log` (`selfupdate::runlog`; the four attempts before are
  kept as `selfupdate.1.log` ...): the download source and checksum, the swap or
  the hand-off, and the second stage (`finish::run_with`: waiting for the old
  process and its timeout, the `msiexec` command line and its exit code with the
  meaning, a verbose `selfupdate-msiexec.log`, the router restart and its output,
  the relaunch). `selfupdate.json` records the attempt (`selfupdate::record`:
  running, installed, handed off, done, manual, failed, with the reason); the
  second stage finishes it before it starts the new version. The next start turns
  a failed attempt, a problem after the install (router restart, a pending Windows
  restart), an attempt that never finished, or a "success" that left the old
  version running into `SelfUpdateView::problem`: a sticky toast once
  (`SwitchService::self_update_acknowledge`), a callout on the Updates page until
  the next attempt, and a line in `spb self-update check`.
- **Wiring.** `SwitchService::self_update_{view,check,skip,install}` and
  `self_update_restart_router` (defaults: not implemented) are implemented by
  `superba-switch` (`selfupdate.rs`), which adds Preferences (`Settings::
  auto_update_check`, `update_channel`), the network settings and the router
  (`RouterControl::restart`, only when the service is installed and running).
  `spb self-update check|install` and the GUI call only the service; `superba
  --finish-update` is handled in `main.rs` before any other argument parsing.
- **Overrides** (tests): `SUPERBA_SELF_UPDATE_API`, `SUPERBA_SELF_UPDATE_DOWNLOADS`,
  https or loopback only (`sources::override_base`).
