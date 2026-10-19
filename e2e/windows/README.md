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
  `Ui.superba(section)` opens the pinned sidebar entry, then finds a top tab
  inside the UIA Tab named Superba. General, Appearance, Network and Notifications
  use the Settings tab and its fixed jump buttons. The driver waits for the tab's
  selected state and for the section heading to settle at the top of the scrolling
  panel. Structural Tab/Pane containers have no bounds and are searched without
  the visibility filter. If UIA omits a jump button's selected state, the driver
  records a WARN in the scenario report; an explicit false state still fails.
- **Desktop app**: through the debugging port Superba opened (onboarding, composer, sidebar state,
  app screenshots), `lib/app.py`.
- **Network** (`lib/net.py`): `mock` = `e2e/lib/windows_mockrelay.py` with a
  fake key; `real` = `e2e/lib/linefwd.py` forwarders in front of the test relay. Both on host
  loopback with a throwaway CA, reverse-forwarded into the guest (`https://127.0.0.1:<port>`); the CA
  goes into the guest's `LocalMachine\Root`, the built-in Krill production origins are blocked in the
  guest's hosts file, and the test lines reach the Krill preset through `SUPERBA_TEST_LINES`.
  The Windows wrapper reuses the screenshot relay and emits the complete final-answer Responses
  event sequence, including item/part creation and completion, for the router's stats line.
  Run `python3 e2e/lib/windows_mockrelay.py --self-test` to check both relay contracts.
- **Scenarios** (`lib/scenarios/`): functions that record steps and checks (`lib/runner.py`);
  prerequisites (`installed` → `superba` → `configured` → `app_ready` → `turn` → `router_on`,
  `lib/flows.py`) run when the VM state lacks them. A failed scenario makes the next one start from
  the base snapshot again.
- **ChatGPT's first start**: the app the wizard's switch opens runs its one-time Windows sandbox
  setup (`codex-windows-sandbox-setup.exe`, "Finishing Windows setup"), and a switch meanwhile
  pauses (`spb switch` without `--wait-turn` exits 5). `flows.first_start_setup` waits for it the
  way Superba detects it (that process; up to 15 min, with a stable Codex core afterwards)
  after the wizard's switch and once the app is ready, so a scenario's next switch never runs into
  it. Pauses for running turns (W13, the WSL busy scenarios) are not affected.
- **WSL router verification (W85)**: records shell startup and Codex command durations, requires
  Codex's real exit status and the mock answer, and checks a new successful request row for the
  distribution. A failed WSL config write also saves the GUI log and a separate remote probe;
  the scenario fails without reapplying the switch, so a product timeout remains visible.
- **WSL line switching (W82)**: checks the distribution is still running after first-start setup
  and onboarding, renews its test keep-alive before the switch, and saves the state and resulting
  config. Superba deliberately leaves stopped distributions unchanged; W88 covers that case.

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

## Alpha.7 regression coverage

The 19 navigation destinations and new W91–W101 scenarios are listed in
[MATRIX.md](MATRIX.md), alongside the adapted update, appearance and notification checks.
Run the new scenarios after the tool editor, unified update list and installer have
been integrated into the MSI. Missing dependencies produce SKIP, not PASS.

From a worktree, set `SUPERBA_WINVM_HELPERS` to the shared VM helper directory
containing `pool.sh`, `snapshot.sh`, `ssh.sh` and `common.sh`, and pass `--msi PATH`.
The variable changes helper discovery only; scenarios and locale keys still come
from the current checkout.

Driver-only regression checks (no VM required):

```sh
python3 -m unittest discover -s e2e/windows/tests -v
```

## Documentation screenshots

`screenshots.py` leases a disposable `wintest` VM and installs an explicitly supplied
MSI. It captures all eight Superba destinations at 1280×800 in both themes and
languages, then checks the five tabs and four fixed settings anchors at 960×640.
It releases the VM on completion or failure; it does not run the full matrix.

```sh
SUPERBA_WINVM_HELPERS=/path/to/vm-helpers python3 e2e/windows/screenshots.py \
  --msi /path/to/superba.msi --source-commit <full-source-commit> \
  --out /path/to/review/windows --evidence /path/to/evidence
```

The fixture uses `C:\Users\Public\SuperbaDemo`, a nonworking example API key,
`api.example.invalid`, and generated usage records. Superba and Codex data paths
are passed explicitly. No real account is needed. The evidence directory holds
the MSI install log, UIA trees and a manifest with the MSI/source identity and PNG
hashes. Inspect every image before copying it into `docs/screenshots/windows`.
