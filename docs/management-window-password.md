# Management window password

Superba can require a password before showing its management window. Configure
it under **Superba > Settings > General > Management window password**. Search
for "password" in Settings to find the entry. Protection is off until a password
is saved.

## Modes

| Mode | When a password is required |
| --- | --- |
| Ask whenever the window opens | After closing to the tray or quitting and restarting Superba. Minimizing does not lock the window. |
| Ask once per computer boot | On the first opening after a computer restart. Unlocking is remembered across window closes and Superba process restarts during that boot. |

The once-per-boot mode uses Linux's kernel boot ID. It is unavailable when that
ID cannot be read, including on macOS and Windows in this implementation. An
unreadable session record causes Superba to ask for the password again.

**Lock now** locks either mode and clears the remembered boot session. Changing
the mode, changing the password, or disabling protection requires the current
password. A new password needs at least eight characters; leave the new-password
fields empty to keep an existing password when changing modes.

Closing the window cancels pending password operations. The lock screen hides
the management pages, and the locked tray exposes only Unlock and Quit. Queued
tray management actions are checked again before execution. Background routing
and the router watchdog continue independently of unlocking the window.

## Storage and scope

The GUI stores its password settings under `gui-lock/settings.json` in Superba's
fixed data locator directory. The verifier uses a random 16-byte salt and
PBKDF2-HMAC-SHA256 with 600,000 iterations. The atomic storage writer creates
private files; password text is not written to disk. Verification runs on a
worker thread, failed attempts are delayed, and password fields discard their
undo history.

`gui-lock/session.json` remembers the boot ID and password-settings revision.
Changing the password settings invalidates older sessions. A corrupt password
settings file keeps the management window locked.

This is an application privacy lock. It does not restrict the `spb` CLI, encrypt
provider data, or defend against someone who can modify files or run programs
as the same operating-system user. Router API authentication remains separate.

## Validation

The implementation was built and checked on Ubuntu 20.04 ARM64 with Rust 1.98.0.
The following 84 tests passed:

| Test target | Passed |
| --- | ---: |
| `superba-gui --lib`, including ten password-lock tests | 41 |
| `app_lock_tests` | 1 |
| `i18n_tests` | 10 |
| `superba_page_tests` | 12 |
| `tray_tests` | 20 |

The complete UI test exercises password setup, mismatched and incorrect
passwords, both modes, closing to the tray, queued tray actions, GUI restarts,
manual locking, password undo history, stale boot-session records, disabling
protection, and watchdog activity while locked. It runs the real egui shell
against a fake backend in a temporary home. A changed boot ID is simulated; the
test does not reboot the host or send live model requests. macOS and Windows
were not validated on the local host.

Run the same targets with:

```sh
cargo test --release --locked -p superba-gui --lib \
  --test app_lock_tests --test i18n_tests \
  --test superba_page_tests --test tray_tests
```

The local ARM64 candidate used optimization level 1, 16 codegen units, and thin
LTO for the GUI and test targets, with the existing locked release dependencies.
These build overrides are not required to use the feature. The generated GUI's
architecture, dynamic libraries, `--version`, and `--gui-help` were checked on
the build host.
