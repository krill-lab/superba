# Synthetic model compaction

The merged catalog selects all eligible targets for a slug. Each attempt uses
that target's `standard` (L2) flag, upstream model and endpoint to prepare its
own request. A native and a synthetic provider can share a slug and fail over in
either direction without reusing a summary request or transaction. Another
provider's L1 catalog never makes a standard-only target native. No inference
from model names is used; the catalog owns eligibility and capability probes.

Without the merged catalog, callers can supply explicit
`SyntheticModel { slug, member, model }` records in `RouterConfig.synthetic_models`.
The merged catalog always takes precedence over that seam.

A trigger starts an ordinary streaming Responses request to the selected model,
using Codex's unmodified local compact prompt. Synthetic requests without an
output limit use a conservative 4096-token budget; explicit ordinary request
limits are preserved. Summary requests use at most 4096 output tokens and
respect a lower caller limit. This avoids provider failures when Codex omits
the output limit. An incomplete summary remains an error.
The complete response is bounded
at 2 MiB and five minutes. Empty, refused, malformed, incomplete and broken
responses fail visibly. A successful answer yields exactly one compaction item,
with actual upstream usage. HTTP failures remain errors. No partial summary or
history pointer is committed. Cancellation discards the pending result; a retry
can generate another valid capsule for the same prefix. There is no retry cache
that could confuse two forks or background snapshots.

Capsules use ChaCha20-Poly1305 with random nonces. The authenticated header has a
format version and key id. The encrypted payload contains the vault owner,
source provider and credential scope, selected model, summary, ordered covered
item hashes and their combined prefix hash. Summaries expand into assistant
history, never system/developer messages. Exact covered items before the capsule
are removed; current system/developer policy and all subsequent items remain.
Truncated retained items without an exact content match remain as additional
history. Tool call/output content and images stay intact during summarization.
Repeated capsules are idempotent; newer capsules can cover older ones.

Own capsules expand on switches to native or other synthetic models, including
another configured provider, only while the source provider credential still
belongs to this vault owner. Changing/removing that credential is a scope error.
Native ciphertext stays native on native models and is rejected on synthetic
models: the user must return to its provider or explicitly hand off to a new
conversation. The router never pretends to decrypt it or silently drops it.

Keys live in `secrets/compaction-keys.enc`, encrypted by Superba's existing vault
(OS keyring, with its usual private-file fallback). File locking and atomic
writes serialize creation and rotation. `superba-router rotate-compaction-key
--data-dir <dir>` rotates without losing old keys and takes effect on the next
request. `CompactionKeyStore::retire` explicitly revokes an old key; affected
history then reports `compaction_key_unavailable`. Preserve the vault and its
keyring when copying/backing up an installation. Corruption never regenerates
an existing keyring. Embedded in-memory routers have ephemeral test keys.

Mixed catalogs and installations with compaction keys return WebSocket 426 so
Codex falls back to HTTP with complete input. This prevents hidden connection
state from bypassing capsule validation. Native HTTP compaction stays native.
