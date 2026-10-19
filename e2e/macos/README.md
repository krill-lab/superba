# macOS end-to-end harness

Runs the real arm64 Superba app on a logged-in macOS VM, controlled from a Linux
host. Python uses SSH; `MacAgent` uses AccessKit's accessibility tree through
AXUIElement and captures individual windows with ScreenCaptureKit. No test
endpoint is exposed on the network. See [MATRIX.md](MATRIX.md) for M01–M09 and
their Windows counterparts.

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

Each case starts independently, copies the DMG into a marked run directory,
installs `/Applications/Superba.app`, and uses a fresh HOME, CODEX_HOME,
CLAUDE_CONFIG_DIR and application data directory. Existing Superba installations,
router services or running desktop apps block setup. The harness never downloads
ChatGPT/Codex. M01 stops before Apply if an installed desktop app could be launched
outside the temporary HOME. M02 is `BLOCKED` if no app exists, or a safe isolated hot switch
cannot be performed. It never approves a full restart that could lose the test
HOME. `page()`, `superba(section)`, `click()`, `expect()`, `find()`, `tree()`,
`set_value()` and `type_into()` mirror the Windows API. Locale strings and
`e2e/shared/desktop_flows.py` are shared with Windows.

M06 installs the **product-generated GUI autostart LaunchAgent** into the logged-in
user's LaunchAgents directory for the duration of the reboot test, with explicit
environment isolation. The product plist normally lives under the temporary
HOME, which loginwindow would not load. M06 requires a changed kernel boot time,
GUI auto-login, and a running job referencing the test directory. It invokes
`superba autostart on/off` through the app's embedded CLI so executable discovery
does not depend on where the standalone `spb` was copied. Builds predating the
autostart command are BLOCKED. M03 separately checks the router's service.

Results are `report.json`, `report.md`, `run.log`, and per-case CLI transcripts,
AX dumps, window PNGs, assertions and cleanup results. Text is scrubbed before
writing; input screens are not captured. Screenshots cannot be text-redacted, so
do not run against accounts displaying personal information. Exit codes: `0`
all passed, `1` failure, `2` blocked/unrun prerequisites. `BLOCKED` is never counted
as a pass. After a cleanup failure, later cases are `NOT RUN`.

## Cleanup and interruption

Every case has a finalizer: unload its router, stop only owned processes, detach
its DMG, remove the installed app, remove its temporary login plist, restore
the original launchd environment values and delete the run HOME (including keys).
No real Codex/Claude config files are read, modified or deleted. Cleanup failures
are reported, and the run marker is retained for recovery.

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

MacAgent is compiled on the VM with Apple's SDK; Linux syntax checks cannot
substitute for that build or for a permitted AX/ScreenCaptureKit session.

Workspace unit, integration and doc tests use the separate [VM test gate](WORKSPACE-TESTS.md).
