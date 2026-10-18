# Superba Windows full-flow harness

Runs Superba's Windows user scenarios end to end in a local Windows 11 VM, driven from the
Linux host: the real MSI, the real ChatGPT desktop app (MSIX), the real Codex, the GUI through
UI Automation. The scenario matrix (ids W01–W88, what each one checks) lives with the VM notes
(`.private/wintest/MATRIX.md`, not tracked); `run.py list` prints the ids.

```sh
e2e/windows/run.py list
e2e/windows/run.py run W01 W04 W10 W11 W30 W31 W32 W33 W40 W42      # the core path
SUPERBA_E2E_ENV=/path/to/e2e.env e2e/windows/run.py run W12 W13 --net real
e2e/windows/run.py run --all
e2e/windows/run.py run W01 W04 W10 W11 W30 W40 W42 --base uacoff     # the UAC-off machine (snapshot `clean`)
e2e/windows/run.py run W80 W82 W83 W84 W85 W88 W86 W81               # Codex inside WSL 1 (snapshot `wintest-wsl1`)
e2e/windows/run.py run W81 W90 --base wintest-wsl1 --net real         # the same machine as their base
e2e/windows/run.py prep-snapshot                                      # rebuild the `wintest` base snapshot
```

Results: `e2e/out/windows-<timestamp>/` (gitignored) with `report.md` / `report.json` (per scenario
and step: checks, notes, manual items, evidence paths), `run.log`, `evidence/<id>/` (screenshots,
UI Automation dumps, saved files), `transcripts/` (every `spb` call) and `net/` (relay logs).

## Requirements

- The VM pool scripts of `.private/winvm/` (or `SUPERBA_WINVM_HELPERS`): `pool.sh` (acquire /
  release a VM), `snapshot.sh`, `winvm.sh`, `common.sh` (SSH port, key, user per VM). A run takes a
  free VM (`--vm ID` to choose one, `--held` when the caller already holds it), restores the
  scenario's snapshot, and gives the VM back restored to `clean`.
- Snapshot `wintest` = `clean` + `guest/prep-base.ps1` (UAC on with Windows' defaults, the old
  Superba MSI removed). Built automatically when missing. `--base uacoff` runs on `clean` instead (UAC
  off; its old MSI is removed after each restore). The WSL scenarios need `wintest-wsl1` (WSL 1 with a
  distribution `Ubuntu-WSL1` and the app's Linux Codex; made by hand, see the VM notes). A WSL 1 distribution
  stops ~30 s after it goes idle and a switch never starts one, so the install step starts a keep-alive
  (`wsl.exe -d Ubuntu-WSL1 --exec sleep 3600`); W88 stops it on purpose to test the pending update.
- Python 3.11 (stdlib only for the runner), ssh/scp, docker access for the snapshot scripts.
  `--net real` also needs `aiohttp` for `e2e/lib/linefwd.py` and the env file of `e2e/README.md`
  (`SUPERBA_E2E_UPSTREAM`, `SUPERBA_E2E_KEY`; values are never printed or written to tracked files).

## How it works

- **Desktop agent** (`guest/WtAgent.cs`, compiled in the guest by Windows PowerShell's `Add-Type`,
  nothing installed): runs in the logged-on user's desktop session, started by a one-off scheduled
  task with the *Limited* run level (so Superba and `spb` get the unelevated token a Start-menu click
  gives them). It listens on the guest's 127.0.0.1; the host reaches it through an SSH local
  forward, every request carries a random token, and it logs only command names. It does UI
  Automation (find by accessible name, Invoke / Toggle / Value / SelectionItem patterns, raw view for
  egui popovers), keyboard and mouse input, screenshots, window moves, processes (with a fresh user
  environment, like a new terminal), and CDP calls to the desktop app's renderer.
- **GUI**: egui → AccessKit → UI Automation; widgets are found by the texts of
  `crates/*/locales/*.toml` (`lib/strings.py`), in English or Chinese.
- **Desktop app**: through the debugging port Superba opened (onboarding, composer, sidebar state,
  app screenshots), `lib/app.py`.
- **Network** (`lib/net.py`): `mock` = `crates/superba-gui/tools/screenshots/mockrelay.py` with a
  fake key; `real` = `e2e/lib/linefwd.py` forwarders in front of the test relay. Both on host
  loopback with a throwaway CA, reverse-forwarded into the guest (`https://127.0.0.1:<port>`); the CA
  goes into the guest's `LocalMachine\Root`, the built-in Krill production origins are blocked in the
  guest's hosts file, and the test lines reach the Krill preset through `SUPERBA_TEST_LINES`.
- **Scenarios** (`lib/scenarios/`): functions that record steps and checks (`lib/runner.py`);
  prerequisites (`installed` → `superba` → `configured` → `app_ready` → `turn` → `router_on`,
  `lib/flows.py`) run when the VM state lacks them. A failed scenario makes the next one start from
  the base snapshot again.

## Debugging a session by hand

```sh
e2e/windows/run.py --vm 3 session up [--install]   # acquire VM 3, restore wintest, agent + network
e2e/windows/run.py --vm 3 ui dump                  # the Superba window's UI Automation tree
e2e/windows/run.py --vm 3 ui click "Continue"      # press a button by its name
e2e/windows/run.py --vm 3 ui shot NAME [--full]
e2e/windows/run.py --vm 3 spb status
e2e/windows/run.py --vm 3 guest 'Get-Process superba*'
e2e/windows/run.py --vm 3 session down             # restore clean, release the VM
```
