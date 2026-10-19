# macOS VM workspace tests

Run the same workspace tests as `.github/workflows/ci.yml`'s `test (macos-latest)`
job over SSH, without registering a GitHub self-hosted runner:

```sh
scripts-ci/mac-vm-test.sh origin/ci/alpha7-rc
```

The local host needs Git, rsync, Python 3.11+ and PyYAML (`python3 -m pip install
PyYAML`). The VM needs macOS, SSH, rsync, curl and Xcode command-line tools.
Set `MACVM_SSH_HELPER` to an executable that accepts a remote command and calls
SSH with the VM's credentials. By default the helper is `macvm-ssh.sh` in the
main checkout's secrets directory. No address or key is stored in this repo.

The script archives the resolved commit locally, excludes private/secret
folders and transfers it with rsync to `~/ci/src-<shortsha>`. It reads the test
command (including features and `--locked`) and environment from that archive's
CI workflow. Rust comes from its `rust-toolchain.toml` or `rust-toolchain`;
older commits without either use their CI `rustup toolchain install` pin.
Unsupported workflow expressions fail instead of silently changing the test.

On the VM, tests run with `nice -n 10`, five Cargo build jobs, and an isolated
`HOME`/`TMPDIR` in `~/ci/home-<shortsha>`. The inherited SSH environment is cleared.
The source and test HOME are removed on completion, including test failures.
Rustup, crates and build artifacts persist in `~/ci/{rustup,cargo-home,target}`.
CI's `CARGO_INCREMENTAL=0` is preserved; compiled dependencies still get reused.
Below 20 GiB free, the script runs `cargo clean` before testing. A separate
`~/ci/unit-test.lock` serializes these cache users; it does not take the GUI
scenario lock or open windows. After an interrupted SSH connection, inspect the
remote process and lock before retrying; the controller will not delete a lock
that may still protect a running build.

Results go to `<main checkout's private directory>/vmci/<shortsha>/` by default.
`MACVM_LOG_ROOT` overrides that root. `RESULT.md` and `result.json` include pass,
failure and ignored counts, failing names, the exact commit and exit status.
`config.json`, `full.log`, `tests.log` and `controller.log` retain the complete
configuration, bootstrap/build/test output and transport diagnostics. Build or
transport failures also produce a failing report, even if zero tests failed.
Existing reports are never overwritten: choose a new `MACVM_LOG_ROOT` to rerun.
Remote logs remain in `~/ci/logs-<shortsha>` if retrieval fails.

This result is the macOS **test** gate for a merge batch. GitHub's macOS job
still runs, but the merger does not wait for it. Record and report disagreements
between the two. Other gates (including macOS clippy, packaging and GUI tests)
remain separate.
