# Contributing to Superba

Read `ARCHITECTURE.md` first: it lists the crates, who owns which trait, and the
flow of a switch.

## Toolchain

- Rust stable 1.98 or newer, edition 2024 (`rust-version` is enforced).
- Linux needs the usual eframe build dependencies (X11/Wayland headers) only for
  `superba-gui`; `spb` builds without them.

## Build and check

```sh
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt -p <crate>          # format only the crates you changed
```

When several people or agents build in the same checkout, use a private target
directory to avoid lock contention: `export CARGO_TARGET_DIR=target-<label>`
(`target-*` is git-ignored).

## Cross-platform checks from Linux

CI runs clippy and the tests natively on Linux, macOS and Windows. To type-check
the macOS and Windows code from a Linux machine before pushing (C build scripts
for SQLite, zstd and the TLS library need a cross C compiler):

```sh
rustup target add x86_64-pc-windows-gnu aarch64-apple-darwin x86_64-apple-darwin

# Windows: llvm-mingw (https://github.com/mstorsjo/llvm-mingw/releases) on PATH.
# Linking additionally needs libgcc.a / libgcc_eh.a shims in a directory passed
# with -L (an empty archive and a link to llvm-mingw's libunwind.a).
CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-clang \
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-clang \
  cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings

# macOS: zig as the C compiler (https://ziglang.org/download/), through a small
# wrapper that drops cc-rs's --target/-arch flags and runs
# `zig cc -target aarch64-macos "$@"`. Type-check only: linking needs the SDK.
CC_aarch64_apple_darwin=/path/to/zig-cc-aarch64-macos \
  cargo clippy --workspace --all-targets --target aarch64-apple-darwin -- -D warnings
```

## End-to-end tests (Linux)

`e2e/run.sh` runs the real desktop app, the real Codex and the release binaries
through every core flow in an isolated home (about 12 minutes; Xvfb, xdotool,
scrot, Docker and Python with aiohttp/websockets needed). It reads a test relay,
a key and the extracted app from an env file outside the repository:

```sh
SUPERBA_E2E_ENV=/path/to/e2e.env e2e/run.sh
```

The report lands in `e2e/out/<run>/report.md`; see `e2e/README.md`.

## Rules

- **Stay in your crate.** Change the crate you own. Contract changes in
  `superba-core` (types, traits) are coordinated: propose them, then update every
  implementation in the same change.
- **Dependencies** are declared once in the root `[workspace.dependencies]`; crates
  use `foo.workspace = true`.
- **Tests never touch the real user environment.** Use temp dirs with
  `LocalContext::for_tests`, `MapEnv`, `ManualClock`; never read or write the real
  `~/.codex`, shell rc files, registry or keyring entries, and never signal processes
  you did not start (kill by PID).
- **No secrets in the repository.** API keys, passwords, certificates, internal
  hosts and test relay endpoints belong in the git-ignored `.secrets/` / `.private/`
  directories and reach code through environment variables.
- **User-facing text** goes through i18n (`LocalizedText` + catalogs) in English and
  Simplified Chinese; code comments and docs are in English.
- **Platform code** goes behind `cfg(target_os = "...")`, must compile on Linux, and
  stays small and explicitly commented when it cannot be run in CI.
- **Unwritten functionality** returns `Error::NotImplemented`; no `todo!()` /
  `unimplemented!()` (clippy denies them).

## Commits

- Conventional commits (`feat(config): …`, `fix(appctl): …`, `test(core): …`,
  `docs: …`).
- Stage explicit paths only (`git commit -m "…" -- <paths>`); never `git add -A`,
  never `git stash`, never rewrite published history.
