# Security

## Reporting a vulnerability

Please report security problems privately: use GitHub's *Report a
vulnerability* (private security advisory) on this repository, or email
admin@krill-code.com. Do not open a public issue for a vulnerability. We aim to
answer within a few working days.

## What Superba can reach, and how it protects it

Superba configures programs that hold credentials, so it handles a few sensitive
things. This page explains the exposure and the mitigations.

### The desktop app's debugging port (CDP)

Hot switching needs the Chromium DevTools protocol of the ChatGPT / Codex desktop
app: it is the only way to ask the app to restart its Codex core. Superba
therefore starts the app with a debugging port.

Exposure:

- Whoever can connect to that port controls the app's renderer. Through the
  app's bridge the renderer can drive Codex, and Codex can run commands, read
  your files and use your login.
- The port is on the loopback interface, but on a multi-user machine **other
  local users can connect to loopback ports**, and any program running as you
  can too.
- Same-user malware could already read `~/.codex/auth.json`, but CDP can also
  read the app's web session (for example ChatGPT cookies) without the macOS
  Keychain prompt that normally protects them.

Mitigations:

- The port is bound to loopback only (Superba never passes
  `--remote-debugging-address`). On Linux and macOS the app picks a free port
  itself (`--remote-debugging-port=0`, read back from `DevToolsActivePort`), so
  there is no window in which another process can claim a pre-chosen port. On
  Windows Superba reserves a free port and releases it just before the app binds
  it; the owner check below catches a process that grabs it in between.
- Web pages in a browser cannot open the DevTools WebSocket: with a reserved
  port (Windows) `--remote-allow-origins` is set to exactly
  `http://127.0.0.1:<port>`; with `--remote-debugging-port=0` (Linux, macOS) no
  origin is allowed at all, and Chromium refuses every WebSocket client that
  sends an `Origin` header (browsers always do; Superba sends none).
- Before its first request to the port, Superba checks that the listening
  socket belongs to the app process it launched (Linux `/proc`, macOS `lsof`,
  Windows `GetExtendedTcpTable`) and that the WebSocket URL is a loopback URL on
  that port; on a mismatch it does not use the port at all (switches become full
  restarts). It only talks to the main `app://` window. The macOS and Windows
  checks are implemented but not yet verified on real machines.
- Superba connects only while it plans or executes a switch, and only evaluates
  the restart call and read-only state checks.
- **Safe mode** (Settings) launches the app without a
  debugging port. Hot switching is then unavailable and every switch is a full
  restart. Use it on shared machines, or whenever you do not need hot
  switching.
- Superba never enables a debugging port in an app it did not launch.

### API keys and logins

- Keys you give Superba are stored in the OS keyring (macOS Keychain, Windows
  Credential Manager, Secret Service on Linux). Without a keyring (for example a
  headless Linux server) they are stored in files with mode 0600 inside the
  Superba data directory, and the doctor reports it. `profiles.json` holds only a
  reference to the key, or the name of an environment variable you chose.
- Keys are never accepted on the command line (`--key-stdin` / `--key-env`
  only), never logged, and masked in plans and output (last four characters).
  In memory they are zeroized on drop.
- A key is sent only to the origins of the site it belongs to, only over https
  (plain http only for loopback test lines), and authenticated requests never
  follow redirects. URLs containing the key are refused, and response bodies
  that echo it are redacted.
- `auth.json` is plain JSON by Codex's design; Superba writes it with mode 0600
  (Windows: inherited ACLs of your profile directory).
- Before replacing your ChatGPT login, Superba stashes it. Stashes and file
  backups are encrypted with ChaCha20-Poly1305; the key lives in the OS keyring
  (or a 0600 file when there is none). A login you signed out of is never
  restored from a stash.
- In PAT mode Codex sends the key to the line's auth endpoint, configured
  through `CODEX_AUTHAPI_BASE_URL`. Superba writes that
  variable for future processes and warns about Codex processes that still run
  without it, because such a process would send the key to OpenAI's default auth
  endpoint instead.

### The local router

Router mode is opt-in and off by default. While it is on, Codex on this computer
sends its requests to `superba-router`, a per-user process listening on
`127.0.0.1:<port>`, which forwards them to the member sites.

Exposure:

- The router sees your Codex traffic in clear text (prompts, files Codex sends,
  answers) because it terminates Codex's plain-http loopback connection before
  opening its own https connection to the site. It holds the API key of every
  member in `<data dir>/router/secrets.json`.
- Any program running as you, and on a multi-user machine any local user, can
  connect to a loopback port.

Mitigations:

- The router binds to `127.0.0.1` only. Codex authenticates to it with a random
  router token (a local secret in Codex's `auth.json` and the router's key file);
  requests without it are refused. Codex's own credential is never forwarded:
  the router replaces it with the chosen member's key, and a member's key is
  sent only to that member's own https origins (a Krill key never reaches a
  custom site, a custom site's key never reaches Krill). The official ChatGPT
  login is never routed, and remote hosts always connect directly.
- `secrets.json` and `runtime.json` (process id, port and the per-instance
  token of the router's admin API on `/_superba/v1/`) are written with mode 0600;
  `config.json` holds no secrets. The admin API needs its own token.
- The router stores no request or response headers or bodies (a request body is
  held in memory only until it has been sent, so it can be retried on another
  line). Its request log (`<data dir>/router/usage.sqlite3`, mode 0600) holds
  numbers only: times, member, line, model, status, token counts, cost.
  Its log never contains keys, tokens, bodies, query strings or session ids (a
  test enforces it), only the member, line, status and timings.
- Member lines must be https (plain http only to a loopback address, for tests).
- It never fails over on 401, 403 or 429 (so one site's refusal is not retried
  with another site's key), only on connection failures, resets before any
  answer, 502/503/504 and edge interceptions, and only before anything was sent
  back to Codex.
- Uninstalling the router (or Superba) stops the service and deletes the key
  file and the runtime file. Deleting a member profile, or giving it a new key,
  rewrites the key file without the old key (and reloads a running router).
- If the router stops answering while Codex uses it, Superba switches Codex
  back to a direct line after 20 seconds, **but only while the Superba window
  or `spb router watch` is running**; otherwise Codex waits on the loopback port
  until the service manager restarts the router.
- Router mode is not recommended on machines shared with other users: while the
  router is down, another local user could listen on its loopback port and
  receive Codex's requests together with the router token (which is reused
  until the router is uninstalled).

### Usage page and `spb usage`

Superba reads Codex's session files (`CODEX_HOME/sessions`, `archived_sessions`)
read-only to list requests without the router. It keeps only thread and turn
ids, model names, token counts, times and error codes in memory; prompt and
answer text is skipped while parsing and never stored or logged, and nothing is
written back.

### Remote hosts

- Superba uses your system `ssh` with `BatchMode=yes`. It never asks for or
  stores passwords, never changes host-key checking, and only refers to identity
  files by path (it never reads private keys).
- Keys and file contents travel over the ssh session's stdin, never on a remote
  command line.

### Privileges and telemetry

- Superba refuses to switch, open the app or change its own files (profiles,
  sites, settings, the router) through `sudo` (files would end up owned by root)
  and, on Windows, as another account than the user signed in at the computer
  (SYSTEM, a service account, another user through `runas` or a scheduled task:
  the files would land in that account's profile; the window does not open at
  all then). An elevated (Administrator) token of the signed-in user is the
  same account and profile and is allowed. Read-only commands such as
  `spb status` still work. A real root login (for
  example on a VPS) is allowed, but then Superba never opens a debugging port:
  any local user could otherwise drive a root-owned Codex through it. Switches
  are full restarts in that case.
- On macOS the app is started through LaunchServices (`open`), so it asks for
  its own privacy permissions (microphone, screen, files) instead of inheriting
  Superba's or Terminal's.
- Superba sends no telemetry. Its only network traffic is the line probes, key
  checks, model lists and account data (balance, usage, costs, identity; only
  the standard endpoints, all optional) of the sites you configure, sent with
  that site's key to that site only. Usage reports are cached on disk without
  keys.
- The Codex settings catalog is built in. Fetching a newer catalog is
  implemented but off by default; when enabled, a download is used only with a
  valid ed25519 signature from a key built into Superba and a serial not older
  than the catalog in use, else the built-in catalog stays.
