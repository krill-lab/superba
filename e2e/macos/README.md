# macOS end-to-end harness

Runs the installed arm64 Superba app locally on a disposable macOS runner.
`MacAgent` uses AccessKit's accessibility tree through AXUIElement and captures
windows with ScreenCaptureKit. The local transport launches the same guest
protocol without SSH. See [MATRIX.md](MATRIX.md) for M01–M09.

## GitHub Actions (the macOS gate)

[mac-e2e.yml](../../.github/workflows/mac-e2e.yml) is **workflow_dispatch only**;
there are no push, PR, workflow_run or scheduled triggers. It runs independent
`macos-15` and `macos-26` jobs, using the existing pinned checkout/upload actions.
Workers push their CI branch and do not dispatch it.

**Integration order:** GitHub requires a newly added dispatch workflow to exist
on the default branch before it can receive manual events. The captain must
register this workflow on `main` before the first mac-e2e gate, then choose the
integration branch with `--ref ci/alpha8-rc` (or the UI branch selector). Merely
pushing a new file to a CI branch does not register it. See GitHub's
[manual workflow documentation](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow).
Each run checks out `github.sha`, asserts HEAD matches it, and builds that exact
commit; no previous CI/release package is downloaded.

`ci-package.sh` builds GUI, CLI and router, generates actual notices with the
release's `cargo-about 0.9.2` configuration, and uses the release DMG layout.
`package.json` records source SHA, workspace version, DMG/binary/notices hashes.
The bundle's numeric Apple version is the workspace version without its
prerelease suffix; `SuperbaVersion` and `e2e-build.json` retain the **full** version
and `SuperbaSourceSHA` retains the commit. Every installation, including M08,
checks those fields, the CLI version and hashes against this run's manifest.
These are ad-hoc signed test packages, **not signed/notarized release packages**.
The older `ci.yml` DMG with a `0.1.0` bundle version and placeholder notices does
not satisfy this manifest contract.

Before scenarios, the driver probes AX trust without prompting, ScreenCaptureKit
permission and an actual `screencapture -x` full-screen capture. When all are
available it drives the existing GUI scenarios. Otherwise it still installs the
DMG and runs CLI checks, attempts full-screen PNGs, and lists the exact unavailable
wizard/page/desktop assertions per case. A failed capture has its own evidence;
missing permissions or screenshots never become a GUI PASS. It neither changes
TCC nor waits for someone to grant consent on the runner.

M02 downloads the full current ChatGPT DMG from OpenAI's distribution endpoint
linked by the [official download page](https://chatgpt.com/download/). It records
the URL and digest, checks DMG integrity, OpenAI's Developer ID / TeamIdentifier,
`codesign --verify --deep --strict`, Gatekeeper's **Notarized Developer ID**
assessment and the stapled ticket **before installation**. A failed check keeps
its diagnostics and prevents installation. Only the session's new app/data are
removed afterward. It never signs in; a desktop that cannot hot-switch while
isolated is explicitly BLOCKED. In CLI fallback it verifies installation and
write-only line configuration, not a live desktop switch.

M06 uses the product's plist and `launchctl bootstrap` / `kickstart`, checks the
actual process's isolated HOME/CODEX_HOME and an unchanged kernel boot time,
then removes the registration. Local mode rejects `--allow-reboot`.

M08 uses `--reinstall`: it reinstalls this run's DMG and checks retained provider
configuration and data. **Cross-version upgrade is PARTIAL**, because this run
has only one source version. It does not pretend two copies of one image are
different builds. The standalone harness still supports two explicit DMGs for
an independently supplied upgrade test.

Network defaults to mock. Selecting real mode uses only `KRILL_E2E_KEY` and
`KRILL_E2E_UPSTREAM` repository secrets in the harness step; absence of either
selects mock and the report records the effective mode. No shell env dump,
credentials, raw session HOME, or login environment is uploaded. Local helper
processes inherit a small environment allowlist; application processes receive
only the isolated fixture environment. Text evidence is scrubbed before writing;
credential-entry screens are not captured. In real mode, screenshots after
credential setup are omitted and listed as unavailable, since configured relay
URLs may also be secrets and text redaction cannot protect image pixels. The
initial permission-probe screenshot remains available when capture works.

Artifacts include the exact DMG/manifest/notices, package log, permission probe,
per-case scrubbed CLI/process/verification logs, AX dumps where supported, PNGs,
coverage reports and recovery results. `PARTIAL`/`BLOCKED` yields harness exit 2
and an explicit workflow warning; assertion/verification/cleanup failures yield
exit 1 and fail the job. Review `report.md`, not just the job color. Each case
cleans in a finalizer; an always-run recovery step cleans any marked interrupted
sessions and deletes the job's build directory. Cleanup failure fails the job.
The local lease uses a kernel file lock, released automatically when its process
exits, so an interrupted runner does not leave a stale lock blocking recovery.

To reproduce on a **disposable local Mac**, using the manifest and DMG generated
by this same workflow (Python 3.11+ and Apple's Command Line Tools required):

```sh
python3 e2e/macos/run.py run --local --all --reinstall \
  --local-root "$RUNNER_TEMP/mac-e2e-local" \
  --dmg /path/to/current-source.dmg --package-manifest /path/to/package.json \
  --out e2e/out/local-mac
```

The following sections describe the legacy SSH interface for reference. The
alpha.8 gate uses GitHub Actions; it does not access a Mac VM.

## Requirements and first use

- Host: Python 3.11+, OpenSSH, rsync, git; `gh` for the CI artifact fallback.
- Guest: macOS 14+ arm64, Command Line Tools (`swiftc`, Python), a logged-in GUI
  session, and SSH access. M06 requires passwordless `sudo reboot` and auto-login.
- An SSH helper that takes one remote command argument and preserves stdin,
  stdout and the exit code, for example `exec ssh "$MAC_TEST_HOST" "$@"` in a
  private executable file. Keep the destination and authentication outside this
  repository. An rsync-capable SSH connection is needed for development builds.
- All users of the VM must use the same host-side lock file. The harness creates
  it exclusively, writes the owner and UTC time, and removes only its own lock.
  It never breaks another user's lock. `--wait-lock SECONDS` waits for release.

```sh
export SUPERBA_MACVM_SSH=/absolute/path/to/private-ssh-helper
export SUPERBA_MACVM_LOCK=/absolute/path/to/shared-mac-vm.lock
python3 e2e/macos/run.py list
python3 e2e/macos/run.py prepare
```

The stable guest executable is
`/Users/Shared/superba-e2e/agent/MacAgent` (override the root with
`SUPERBA_MACVM_ROOT`). The prepare command uploads the reviewed source, builds
with `xcrun swiftc`, ad-hoc signs it and checks permission. An unchanged source
does not replace the executable, preserving its TCC identity.

If permission is missing, **stop and enable that executable manually** in
System Settings → Privacy & Security → Accessibility. For screenshots, also
allow it under Screen & System Audio Recording. Use the path printed by the
command, then rerun `permissions` or `prepare`. A rebuilt executable may need
consent again. The harness does not edit TCC databases, use private APIs, disable
SIP, or reset anyone's permissions. A missing permission produces `BLOCKED` and
no scenario begins. The public API contracts are documented by Apple:
[AXIsProcessTrustedWithOptions](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions)
and [SCScreenshotManager](https://developer.apple.com/documentation/screencapturekit/scscreenshotmanager).
The agent initializes [NSApplication.shared](https://developer.apple.com/documentation/appkit/nsapplication/shared)
on the main thread to establish its WindowServer connection before capturing.

## Build or select a DMG

```sh
export SUPERBA_GH_TOKEN_HELPER=/absolute/path/to/github-app-token-helper
e2e/macos/build-on-vm.sh --ref origin/main --out e2e/out/mac-build
# Or select an existing exact-ref CI artifact without building:
e2e/macos/build-on-vm.sh --ref origin/main --ci-only --ci-run RUN_ID --out e2e/out/mac-ci
```

The builder checks host space (30 GiB minimum) and guest space (**25 GiB
minimum before compiling**). Below the guest threshold it downloads the existing
`dmg-test` artifact with `gh run download`; the run's commit must match `--ref`.
It never triggers a workflow or silently substitutes another revision. Some CI
artifact filenames say `universal` despite containing a native binary; the
installer checks `lipo -archs`, requiring arm64.

For a VM build, `git archive` selects committed files at the ref, archive paths
and links are checked, and rsync transfers that tree. Private directories and
credentials are excluded. An isolated rustup/cargo cache under the guest build
directory installs stable once, without modifying shell startup files. Cargo
builds `superba-gui`, `superba-cli` and the required `superba-router` helper for
`aarch64-apple-darwin`. There is no Intel build. The existing bundle script creates
the `.app`; a development DMG contains it, `spb`, and the Applications link.
This DMG is ad-hoc signed and is not a notarized release or the release's Finder
layout. Build provenance and logs are saved beside it. The target directory is
capped and removed on normal build exit, including failures; source/toolchain
caches remain for the next build. A forcibly killed driver may need cache cleanup
by its owner.

## Run and inspect

```sh
python3 e2e/macos/run.py run M01 M02 M03 M04 --dmg /path/to/superba.dmg --out e2e/out/mac-core
python3 e2e/macos/run.py run --all --dmg /path/to/old.dmg \
  --upgrade-dmg /path/to/new.dmg --allow-reboot --out e2e/out/mac-all
```

Default `--net mock` uses a small server on the guest's loopback interface for
wizard probes and an actual request through the real router. It checks the
fixture response id, not a successful CLI exit alone. M05 sends notifications
only to a local Telegram-shaped sink. It does not send messages to people.

For credential/configuration checks against a test relay, use `--net real` and
`SUPERBA_E2E_ENV=/absolute/path/to/test.env`. The env file has
`SUPERBA_E2E_UPSTREAM` and `SUPERBA_E2E_KEY`; `--responses-path` defaults to
`/codex/v1`. Values are parsed as data, never sourced as shell code. The key goes
over SSH stdin and into the guest's temporary HOME using the file secret backend.
It never goes on an argv, into the real Keychain, or into tracked files. M03's
mock model request is omitted in real mode; this mode does not claim upstream
inference coverage. M04 tests the real Superba Claude config writer/restorer;
it does not require installing Claude Code or claim a real Claude conversation.
M02's real mode uses PAT and two HTTPS origins in front of the designated relay:

```sh
python3 e2e/macos/run.py run M02 --net real --dmg /path/to/superba.dmg --out e2e/out/mac-desktop
```

An isolated loopback CONNECT proxy maps two virtual ports to that relay while
preserving its hostname and end-to-end TLS. Only the test processes use the
proxy; system DNS and certificate trust are unchanged. The proxy permits only
the designated relay and records line connection counts, never payloads or
headers. M02 requires PAT files, a usable desktop composer, a verified hot
restart and the unchanged desktop PID. Its screenshots include the desktop and
Superba. This is a switching test, not an inference benchmark. A first run may
need the app's ordinary onboarding; the harness never chooses account sign-in.
M03 and M04 reopen the GUI after external CLI changes to capture the persisted
state in a fresh process. M03 also preserves a screenshot before restarting.
The current GUI classifies every HTTP `127.0.0.1` provider as router mode, including
the mock upstream. Mock M03 therefore records that UI limitation and verifies the
exact active endpoint, unloaded service and closed router port. Real M03 also
requires the fresh GUI to show the router-off banner; it sends no inference request.

Each case starts independently, copies the DMG into a marked run directory,
installs `/Applications/Superba.app`, and uses a fresh HOME, CODEX_HOME,
CLAUDE_CONFIG_DIR and application data directory. Existing Superba installations,
router services or running desktop apps block setup. The legacy SSH mode never downloads
ChatGPT/Codex; local M02 performs the verified download described above. M01 stops before Apply if an installed desktop app could be launched
outside the temporary HOME. M02 is `BLOCKED` if no app exists, or a safe isolated hot switch
cannot be performed. It never approves a full restart that could lose the test
HOME. `page()`, `superba(section)`, `click()`, `expect()`, `find()`, `tree()`,
`set_value()` and `type_into()` mirror the Windows API. Locale strings and
`e2e/shared/desktop_flows.py` are shared with Windows.
Desktop startup opens a run-specific debugging port so Superba can verify the
hot-switch path. Existing OpenAI desktop user data blocks that case. Cleanup
stops the owned app and its helpers, removes only newly created desktop data,
and keeps the desktop bundle installed.

DMG transfers stream to disk with a separate 15-minute timeout. A guest artifact
cache keyed and verified by SHA-256 avoids uploading the same image for every
case; each case still installs independently. Native builds seed this cache from
their existing guest image. The cache contains only application images, no test
HOME or credentials, and may be removed while the VM is idle.

M06 installs the **product-generated GUI autostart LaunchAgent** into the logged-in
user's LaunchAgents directory for the duration of the reboot test, with explicit
environment isolation. The product plist normally lives under the temporary
HOME, which loginwindow would not load. M06 requires a changed kernel boot time,
GUI auto-login, a loaded job, and a real GUI process with the temporary HOME and
CODEX_HOME. When the product uses `/usr/bin/open`, the fixture adds explicit
`--env` arguments because LaunchServices does not inherit open's environment;
the one-shot launcher is allowed to exit after starting the GUI. It invokes
`superba autostart on/off` through the app's embedded CLI so executable discovery
does not depend on where the standalone `spb` was copied. Builds predating the
autostart command are BLOCKED. M03 separately checks the router's service.
M06 checks for running Rust builds/tests before setup and immediately before
rebooting. Background CI may share the VM without holding the GUI lease; those
jobs must finish first, or M06 is BLOCKED without rebooting.

## Installing the desktop prerequisite

Desktop installation is a separate operator action. Obtain approval and use the
macOS download button on the [official ChatGPT download page](https://chatgpt.com/download/).
Choose the current ChatGPT app containing Codex; ChatGPT Classic is a separate
download. Record the source page, actual download URL, download time and SHA-256.
If the page supplies a small installer, verify that installer first and preserve
the official download chain for the full application payload as well.

Mount the downloaded DMG read-only and inspect the app **before copying it into
Applications or launching it**:

```sh
shasum -a 256 /path/to/ChatGPT.dmg
codesign -dv --verbose=4 /path/to/mounted/ChatGPT.app
codesign --verify --deep --strict --verbose=2 /path/to/mounted/ChatGPT.app
spctl -a -vv /path/to/mounted/ChatGPT.app
xcrun stapler validate /path/to/mounted/ChatGPT.app
```

Require an OpenAI Developer ID Application authority, record its TeamIdentifier,
and require `accepted` with `source=Notarized Developer ID`. Validate the stapled
ticket when the tool is available. Stop if a required check fails. Do not remove
quarantine attributes, disable Gatekeeper, or substitute an unsigned payload.
Once verified, copy the bundle to an empty `/Applications/ChatGPT.app` slot.
Do not replace an existing installation without its owner's approval.

Do not sign into a personal ChatGPT account. Scenario credentials belong only
in the marked temporary HOME. Inventory the login user's OpenAI application data
before testing so that later cleanup can distinguish test-created files from
existing data. Keep the verified desktop app installed for later runs, stop its
test processes, and remove only the test's data. Superba's test installation and
the temporary HOME are still removed after each case.

Results are `report.json`, `report.md`, `run.log`, and per-case CLI transcripts,
AX dumps, window PNGs, assertions and cleanup results. Text is scrubbed before
writing; input screens are not captured. Screenshots cannot be text-redacted, so
do not run against accounts displaying personal information. Exit codes: `0`
all passed, `1` failure, `2` partial/blocked/unrun prerequisites. `BLOCKED` is never counted
as a pass. After a cleanup failure, later cases are `NOT RUN`.

## Cleanup and interruption

Every case has a finalizer: unload its router, stop only owned processes, detach
its DMG, remove the installed app, remove its temporary login plist, restore
the original launchd environment values and delete the run HOME (including keys).
No real Codex/Claude config files are read, modified or deleted. Cleanup failures
are reported, and the run marker is retained for recovery.

An SSH disconnect does not tell the caller whether the guest finished the last
operation. The transport reports this uncertainty and never automatically
retries the mutation. Restore connectivity, confirm the original runner has
stopped, and clean up its recorded session before starting another case. Until
cleanup and a fresh idle audit succeed, do not report that the VM is clean or
count an interrupted scenario as passed.

After a host interruption, check that the old process has exited before the lock's
owner removes the abandoned lock. Reacquire normally and run:

```sh
python3 e2e/macos/run.py cleanup --session ID_FROM_SESSION_JSON
```

The compiled, idle agent and toolchain caches can remain for reuse; there is no
agent daemon. Removing the agent invalidates its permission setup. Only remove
your own build cache when no build is running. Never delete another worker's
lease, process, source tree or target directory.

## Local checks

```sh
python3 -m compileall -q e2e/macos e2e/shared
python3 -m unittest discover -s e2e/macos/tests -v
bash -n e2e/macos/build-on-vm.sh e2e/macos/guest/build.sh
python3 e2e/macos/run.py list
```

MacAgent is compiled on the selected macOS host with Apple's SDK; Linux syntax checks cannot
substitute for that build or for a permitted AX/ScreenCaptureKit session.

The historical [VM test gate](WORKSPACE-TESTS.md) is not used for alpha.8; the captain runs the GitHub macOS workspace test job.
