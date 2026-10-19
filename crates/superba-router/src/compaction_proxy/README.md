# Synthetic model compaction

The catalog supplies explicit `SyntheticModel { slug, member, model }` records in
`RouterConfig.synthetic_models`. Multiple members can advertise a bare slug;
provider-qualified slugs should have one member. Configure that member's standard
Responses base through `OpenAiMember.responses` (for example `/v1`). No inference
from model names is used. Native models keep the native remote-compaction path.
The catalog remains responsible for eligibility and capability probes.

A trigger starts an ordinary streaming Responses request to the selected model,
using Codex's unmodified local compact prompt. The complete response is bounded
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
