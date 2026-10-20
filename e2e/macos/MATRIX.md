# macOS scenario matrix

Run `python3 e2e/macos/run.py list`. Each ID gets a fresh HOME and mandatory
cleanup; cases do not inherit a previous case's pass or state. Live results live
in the selected output directory, never in this table.

| ID | Coverage | Observable assertions / evidence | Prerequisites / Windows counterpart |
|---|---|---|---|
| M01 | DMG installation, first launch and custom-provider wizard | Native arm64 app, first-run AX title and PNG, wizard applies endpoint to temporary Codex home, no raw key in profile metadata, restore removes endpoint | Permitted AX + capture; W01/W02/W04; shared wizard steps |
| M02 | Line switch with installed desktop app | Real mode: PAT, two HTTPS relay origins, editable composer; switch result, config endpoint, same isolated desktop PID, desktop and Superba PNGs | Installed ChatGPT/Codex, isolated hot-switch path; absent app or required full restart is BLOCKED; `--net real` and designated HTTPS relay for PAT; W10/W11 |
| M03 | Router enabled and disabled | Active endpoint uses actual router port, owned LaunchAgent, request reaches fixture, exact direct endpoint restored, job and listener removed; before/after PNGs | Router helper; W30/W33/W36. Mock records GUI's loopback-provider heuristic; real mode asserts cold-start off banner and omits inference |
| M04 | Claude Code config apply/restore | Endpoint/key/slot written, unrelated settings preserved, original JSON restored, Claude page PNGs | Superba Claude writer; no Claude installation or inference required |
| M05 | Notification hooks | Codex hook command installed/restored, async Claude Stop fragment, hook delivers to local sink, reply text excluded, notification-page PNG | No external chat credentials; GH#12 |
| M06 | GUI login startup (local: simulated login) | Product GUI autostart plist, isolated env, local bootstrap/kickstart with unchanged boot time (legacy SSH: real reboot), actual GUI with test HOME, before/after PNGs, off removes registration, finalizer unloads | Build with `autostart`, local mode never reboots; legacy SSH requires `--allow-reboot` |
| M07 | Data-location move (GH#15) | Persistent pointer used by fresh CLI, data at destination, providers survive cold start, data-page PNG | Build containing `spb data`; older build is BLOCKED; no env override masking the pointer |
| M08 | DMG overwrite / upgrade | App replaced, provider JSON and sentinel unchanged, new app launches; distinct hashes required for a two-build upgrade | Local workflow: exact-source `--reinstall`, cross-version coverage PARTIAL; `--upgrade-dmg` requires a second artifact |
| M09 | Restore/uninstall cleanup | Byte-identical original Codex config, restored GUI PNG, no service or temporary LaunchAgents, finalizer removes owned app/processes/HOME | W40/W41/W42 |

Local runs hold a local lease and need no SSH configuration. The permission probe
records AX, ScreenCaptureKit and actual full-screen capture independently.
Unavailable GUI operations fall back to CLI assertions and attempted full-screen
PNGs; each case lists specific missing coverage and is PARTIAL, never PASS for
unexercised GUI assertions. M02 installs only the newly downloaded, verified
OpenAI ChatGPT app and never logs in. The SSH interface retains its VM lease and
TCC blocking behavior. After each case, `cleanup.ok` is mandatory; a failed
cleanup makes the case FAIL and stops later cases.
