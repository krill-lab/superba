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

Two more crates are libraries / tools: `superba-catalog` (the Codex settings
catalog, section 10) and `superba-catalog-gen` (maintainer tool that generates it,
never linked into the binaries).

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
| `ConfigEngine` | Owned keys of `config.toml` + `auth.json`, journal, baseline, login stash | pure: `render`, `inspect_files`, `capture_baseline` (shared with remote); `read_state`, `baseline`, `snapshot_baseline`, `plan`, `apply`, `rollback`, `recover`, `generations`, `active_credentials`, `login_stash` / `stash_login` / `load_login_stash` (by `StashScope`: local home or remote target) |
| `EnvPersister` | Persist `CODEX_AUTHAPI_BASE_URL` for future processes | `inspect`, `plan(var, EnvOp)`, `apply`, `revert`, `set_authapi_base_url`, `clear_authapi_base_url` |
| `SecretStore` | Profile keys in the OS keyring (0600 file fallback) | `get`, `put`, `delete`, `backend` |
| `ProfileStore` | `profiles.json` (`ProfileSet`: profiles, custom sites, settings) | `load`, `save` |
| `LineProber` | Latency/health of lines, key validation, auth API health, models, Responses probe, WebSocket / zstd transport probe, account and usage data | `probe`, `probe_all`, `check_key`, `check_authapi(site, origin, key)`, `list_models`, `probe_responses`, `probe_transports`, `account_summary`, `usage_report` |
| `SessionMigrator` | Session provider scan, migration with backup + ledger, rollback, crash recovery | `scan`, `plan`, `migrate`, `rollback`, `history`, `recover` |
| `AppController` | Installs, CLI binaries, launch with CDP, bridge, hot restart, quit, processes, turn activity, renderer state, daemon, launcher entry | `detect_installs`, `codex_binaries`, `running_session`, `launch`, `probe_bridge`, `hot_restart`, `quit`, `full_restart`, `processes`, `turn_activity`, `renderer_state`, `current_thread`, `return_to_thread`, `restart_daemon`, `stop_daemon`, `remote_connections`, `launcher_entry`, `set_launcher_entry` |
| `RemoteHosts` | SSH hosts: discovery, probe, remote apply/rollback, turn activity, verification | `discover(home, from_app, live)`, `probe`, `plan_apply`, `apply`, `rollback`, `turn_activity`, `verify(RemoteVerifyRequest)` |
| `SwitchService` | Application service for GUI and CLI | `overview`, `sites`, `profile_set`, `save_profile`, `delete_profile`, `save_settings`, `save_site`, `delete_site`, `probe_lines`, `probe_site`, `line_transports`, `check_key`, `account`, `sessions`, `doctor`, `open_app`, `recover`, `plan`, `plan_settings`, `execute`, `settings_view`, `edit_settings`, `router_status`, `router_install`, `router_set_enabled`, `router_uninstall`, `router_watch`, `router_fall_back` |
| `SwitchDecider` | Mid-execution questions (wait for turns, force quit, IPC failed, verification failed) | `decide(DecisionRequest) -> Decision` |
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
     ├─ (migration) CloseConsumers                           stop daemon; user closes TUI/IDE (decider)
     ├─ Applying (state.lock + switch journal)               snapshot_baseline (first time) -> ConfigEngine::apply
     │                                                       (auth.json then config.toml) -> EnvPersister::apply
     │                                                       -> SessionMigrator::migrate; any failure -> rollback
     ├─ Restarting                                           re-check turns 200 ms before IPC;
     │                                                       AppController::hot_restart(local)
     │   or Launching                                        AppController::launch (CDP + CODEX_AUTHAPI_BASE_URL)
     ├─ remote hosts in parallel                             RemoteHosts::apply -> hot_restart(hostId, kill) -> verify
     ├─ Verifying                                            new app-server after T0, hashes, whoami/models, renderer
     ├─ NavRestore / Recovery / RollingBack                  return_to_thread; retry IPC once; rollback + restart
     └─ SwitchOutcome (+ stale CLI processes, remote results, notices)
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
| `profiles.json`, `state.json`, `journal/config/` | config |
| `state.lock`, `journal/switch/` | switch |
| `backups/first-write/`, `backups/switch/`, `backups/login-stash/` (encrypted), `backups/rc/` | config |
| `secrets/` (0600 key files when no keyring) | config |
| `backups/migrate-<id>/` (sqlite backup, rollout pre-images, ledger) | sessions |
| `app-sessions.json` (apps launched by Superba: pid, CDP port, start time) | appctl |
| `remote/` (per-host cache, journals) | remote |
| `logs/` | binaries |
| `cache/usage/` (usage reports per site, key fingerprint and range; no keys) | net |
| `cache/transports.json` (WebSocket / zstd checks per site, line, base URL and key fingerprint, with the time of the check; no keys) | switch |
| `router/` (`config.json`, 0600 `secrets.json` and `runtime.json`, log) | router |
| `catalog/` (a downloaded, signature-checked settings catalog; unused while remote updates are off) | catalog |
| `selfupdate.json` (ETags and answers of the release check, last check time, skipped version, macOS backup to remove; no secrets), `downloads/superba-update/` (verified Superba packages and staging) | versions |

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
In router mode `config.toml` keeps Codex's model name: the router maps names per
member. The GUI shell runs a tick every 5 s while router
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

**Local budget** (user bug 35; `superba_router::limits`). Each proxied request
holds two sockets and a WebSocket session two for its life, and launchd starts
agents with a soft limit of 256 open files. The router sizes its budget from the
soft limit `L` it finds at start (after raising it towards 8192; the LaunchAgent
plist sets `SoftResourceLimits` / `HardResourceLimits` `NumberOfFiles` and the
systemd unit `LimitNOFILE` as well): `L/4` admissions (4 to 512) for proxied
requests (until the response body ends) and WebSocket sessions, at most `L/2`
inbound connections (at the cap the accepted connection waits for a place, open
connections stop keep-alive, idle ones close after 30 s anyway), and a capped
idle pool per upstream host. A request that gets no admission within
`Tuning::busy_wait_ms` (2 s) is answered 503 `local_router_busy` with
`Connection: close` and `Retry-After`. `EMFILE` / `ENFILE` met while accepting,
connecting upstream, probing or opening the request log is local: the request
gets the same 503 naming the limit, no breaker counts it, nothing fails over,
and it is logged at most once a minute. `RouterStatus::local` (budget, in
flight, busy answers, file-limit errors, the last one) feeds
`RouterView::local`, `spb router status`, the doctor's capacity row and finding,
and the Router page.

Lifecycle, the same in `spb` and the GUI (both only through `SwitchService`):

| Step | `spb` | GUI | Service |
|---|---|---|---|
| members, install / repair | `router install [--member P]...` | Router page: add, reorder, remove (only saved while the router is not installed), Install / Restart | `router_install` (members = profiles; keys copied into the router's 0600 key file; nothing written when the program is missing; a reinstall keeps the stored port and token) |
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
- Superba refuses to run under `sudo` (euid 0 with `SUDO_USER`) and with an elevated
  Windows token (`TokenElevation`): switching, "Open ChatGPT", recovery and every
  `SwitchService` method that writes its files (`Switcher::refuse_elevated`). A
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

## 12. Packaging and self-update

`superba`, `spb` and `superba-router` ship together in every release archive
(`.github/workflows/release.yml`, `scripts/release/`): next to each other in the
Linux tarballs and the Windows zip, `superba-router` inside
`Superba.app/Contents/MacOS` on macOS (with `spb` beside the bundle), and `spb` +
`superba-router` in the headless static musl archive. CI builds `spb` and
`superba-router` without GUI packages. Details: `scripts/release/README.md`.

### Self-update (`superba_versions::selfupdate`)

- **Check.** `SelfUpdater::check` asks `<api>/releases/latest` and, when the user
  opted into pre-releases, `<api>/releases?per_page=30` (newest pre-release; drafts
  and tags the release workflow would not make are ignored), conditional on the
  stored ETag, with an 8 s deadline and no credentials, through the shared
  `http::client`. 404 / 401 / 403 are `PageUnavailable` (the repository is
  private), network errors, 5xx and rate limits are `CheckFailed`; neither is
  retried before 24 h / 1 h. The cached answer is compared with
  `CARGO_PKG_VERSION` on every read (`semver` precedence: a pre-release is older
  than its release), so a finished update never shows itself as available. The
  link to the release page is built from the tag, never taken from the API.
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
- **Wiring.** `SwitchService::self_update_{view,check,skip,install}` and
  `self_update_restart_router` (defaults: not implemented) are implemented by
  `superba-switch` (`selfupdate.rs`), which adds Preferences (`Settings::
  auto_update_check`, `include_prereleases`), the network settings and the router
  (`RouterControl::restart`, only when the service is installed and running).
  `spb self-update check|install` and the GUI call only the service; `superba
  --finish-update` is handled in `main.rs` before any other argument parsing.
- **Overrides** (tests): `SUPERBA_SELF_UPDATE_API`, `SUPERBA_SELF_UPDATE_DOWNLOADS`,
  https or loopback only (`sources::override_base`).
