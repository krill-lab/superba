# Managed ChatGPT accounts

Open **Providers → Official ChatGPT login** to add an account, save the current
official login, view subscription limits, or switch accounts. Adding an account
opens the official browser login and saves it without changing the current
provider. The Overview provider tile (its body or chevron) opens the provider and
account picker.

## Implementation

- `superba-config::chatgpt::Accounts` encrypts the account collection with the
  existing Superba vault. GUI views contain an opaque local identity, email,
  subscription label, first-save time and live-account marker; no tokens.
  Older saved accounts recover their subscription label from their encrypted
  login token; the current quota response takes precedence for display. If its
  optional plan field is absent, `account/read` supplies the subscription type.
  Missing plan data is shown as a loading/error state, never as a plan tier.
  Both `prolite` and `pro_lite` display as Pro Lite. Other non-empty service tiers
  remain visible by their returned name instead of being mistaken for read failures.
- Login uses Codex app-server's `account/login/start` and
  `account/login/completed` protocol in a private, disposable `CODEX_HOME`, with
  file credentials selected explicitly. The browser URL can be reopened or
  copied; cancellation stops only Superba's helper child. The user's current
  Codex home is not used for the login ceremony.
- Each saved account refreshes automatically every 60 seconds on all pages and
  while hidden in the tray. Manual refresh resets that account's timer. Requests
  never overlap for one account, pause during switches or login, and retry at
  the next interval after a failure while retaining the last successful result.
- Limits use `account/rateLimits/read` with the selected account. Helpers preserve
  rotated credentials, including when the limits request fails. An unchanged
  active login can receive its own rotated tokens; a concurrent account change
  always wins. Switching away also saves the newest managed live credential.
- The Codex bucket is preferred from `rateLimitsByLimitId`, with legacy
  `rateLimits` as a fallback. Windows are identified by `windowDurationMins`
  (300 and 10080), independently of primary/secondary order. Only reported windows
  are shown: a weekly-only account leaves the five-hour column blank, with
  weekly usage aligned in the same column as other accounts. A response with
  neither window is labeled unavailable, never unlimited or 100% remaining.
  Subscription names do not determine which windows an account has. Old results
  keep their fetch time when a refresh fails. Quota bars are green above 20%,
  amber above 10% up to 20% and red at 10% or less, using each theme's status colors.
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
rollback, legacy subscription backfill, weekly-only limits, quota selection/missing fields, local-only planning and secret-free
plan serialization, provider-page quota rendering and the Overview account
picker. A credential-free protocol smoke check can verify the installed Codex's
initialize, limits, browser-login start and cancellation messages without opening
a browser or touching the user's login.

Successful browser authorization and real subscription quota responses require
the user to sign in; fixture tests do not establish those live results.
