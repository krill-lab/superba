# Managed ChatGPT accounts

Open **Providers → Official ChatGPT login** to add an account, save the current
official login, view subscription limits, or switch accounts. Adding an account
opens the official browser login and saves it without changing the current
provider. The Overview provider tile (its body or chevron) opens the provider and
account picker.

## Implementation

- `superba-config::chatgpt::Accounts` encrypts the account collection with the
  existing Superba vault. GUI views contain an opaque local identity, email,
  first-save time and live-account marker; no tokens.
- Login uses Codex app-server's `account/login/start` and
  `account/login/completed` protocol in a private, disposable `CODEX_HOME`, with
  file credentials selected explicitly. The browser URL can be reopened or
  copied; cancellation stops only Superba's helper child. The user's current
  Codex home is not used for the login ceremony.
- Limits use `account/rateLimits/read` with the selected account. Helpers preserve
  rotated credentials, including when the limits request fails. An unchanged
  active login can receive its own rotated tokens; a concurrent account change
  always wins. Switching away also saves the newest managed live credential.
- The Codex bucket is preferred from `rateLimitsByLimitId`, with legacy
  `rateLimits` as a fallback. Windows are identified by `windowDurationMins`
  (300 and 10080), independently of primary/secondary order. Missing values are
  shown as unavailable, never as zero usage or 100% remaining. Old results keep
  their fetch time when a refresh fails.
- A switch request carries only the opaque `chatgpt_account_id`. Planning resolves
  the credential into a non-deserializable executable config plan. The standard
  preview, running-turn checks, restart, journal, verification and rollback apply.
  Managed account switches are local only and do not inherit remote follow rules.

This requires an installed Codex executable supporting the account app-server
protocol. API-key providers and official-account logins keep their existing
connection behavior; official accounts connect directly.

## Validation

Tests cover encryption, identity deduplication, preservation of live token
refreshes, temporary-home cleanup, explicit A-to-B auth replacement and exact
rollback, quota selection/missing fields, local-only planning and secret-free
plan serialization, provider-page quota rendering and the Overview account
picker. A credential-free protocol smoke check can verify the installed Codex's
initialize, limits, browser-login start and cancellation messages without opening
a browser or touching the user's login.

Successful browser authorization and real subscription quota responses require
the user to sign in; fixture tests do not establish those live results.
