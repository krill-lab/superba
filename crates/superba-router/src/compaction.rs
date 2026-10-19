//! Remote compaction awareness (GitHub issue 3).
//!
//! Codex compacts a conversation remotely: it sends the history with a
//! `compaction_trigger` input item to the ordinary Responses endpoint (HTTP
//! or WebSocket) and keeps the single `compaction` output item — an opaque
//! `encrypted_content` blob only the provider that made it can read — as the
//! start of every later request of the conversation. Requests and answers
//! pass through untouched; the router only decides *where* they go:
//!
//! - **Reading** ([`inspect`]): a Responses request body (HTTP, decoded when
//!   Codex sent it zstd-compressed) or a WebSocket `response.create` frame is
//!   parsed only when it contains the bytes `compaction`; it yields whether the
//!   input holds a `compaction_trigger` (the request compacts) and the
//!   fingerprints of its `compaction` items (the conversation was compacted).
//! - **Learning** ([`Owners`]): the `compaction` item of an answer is
//!   fingerprinted (SHA-256 of its `encrypted_content`, 128 bits) and recorded
//!   with the member that made it; a request whose compaction item had no
//!   known maker and that a member answered successfully records that member.
//!   The table (fingerprints, member ids and times only — never content) lives
//!   in `router/compactions.json` and survives restarts.
//! - **Routing** ([`route`]): a request carrying a compaction item of a known
//!   maker goes only to that member's lines — never fails over to another
//!   member, which could not read it — and is answered 503
//!   `compaction_owner_unavailable` when none of them answers; an item of no
//!   known maker skips members marked [`CompactionSupport::Unsupported`]; a
//!   compaction request skips them too (so a conversation of a provider that
//!   cannot compact is compacted by one that can, and stays there). When no
//!   member is left the router answers 400 `compaction_unsupported` itself
//!   with an explanation Codex shows.
//! - **WebSocket** (`ws_relay`): a socket stays on its line, so a
//!   `response.create` frame that must go elsewhere is not forwarded: the
//!   conversation is pinned to the right member and the socket is closed with
//!   1012 ("service restart"); Codex reconnects (its handshake follows the pin)
//!   and sends the request again. A refusal is answered with an `error` event.
//!
//! Without a `stats` section (tests only) nothing is learned; routing by
//! [`CompactionSupport::Unsupported`] still applies.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use superba_core::CompactionSupport;
use superba_core::codex::{COMPACTION_ITEM, COMPACTION_TRIGGER_ITEM};

use crate::state::{lock, unix_ms};

/// Error code of the router's answer when a compacted conversation's member
/// does not answer.
pub const OWNER_UNAVAILABLE_CODE: &str = "compaction_owner_unavailable";
/// Error code of the router's answer when no member can serve a compaction
/// request or read a compacted conversation.
pub const UNSUPPORTED_CODE: &str = "compaction_unsupported";
/// File under the router directory.
pub const OWNERS_FILE: &str = "compactions.json";
/// Fingerprints kept at most (the oldest are dropped first).
pub const MAX_OWNERS: usize = 20_000;
/// A fingerprint unused for this long is forgotten.
pub const OWNER_TTL_MS: u64 = 90 * 24 * 60 * 60 * 1000;

/// SHA-256 of a compaction item's `encrypted_content`, first 16 bytes, hex.
pub type Fingerprint = String;

/// Fingerprint of an `encrypted_content` value.
pub fn fingerprint(encrypted_content: &str) -> Fingerprint {
    let digest = Sha256::digest(encrypted_content.as_bytes());
    let mut s = String::with_capacity(32);
    for b in &digest[..16] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Item types holding a compacted history (`compaction_summary` is an alias
/// Codex accepts).
pub fn is_compaction_item(kind: &str) -> bool {
    kind == COMPACTION_ITEM || kind == "compaction_summary"
}

/// What a request's input says about compaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactionInput {
    /// The input ends a conversation with `compaction_trigger`: this request
    /// compacts.
    pub trigger: bool,
    /// Fingerprints of the `compaction` items, in input order.
    pub items: Vec<Fingerprint>,
}

impl CompactionInput {
    /// Neither a trigger nor a compaction item.
    pub fn is_empty(&self) -> bool {
        !self.trigger && self.items.is_empty()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BodyPeek<'a> {
    #[serde(borrow)]
    input: Option<InputPeek<'a>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum InputPeek<'a> {
    #[serde(borrow)]
    Items(Vec<ItemPeek<'a>>),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ItemPeek<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    encrypted_content: Option<Cow<'a, str>>,
}

/// Read the compaction facts of a Responses request body or a WebSocket
/// `response.create` frame (plain JSON). Bodies without the bytes
/// `compaction` are not parsed.
pub fn inspect(json: &[u8]) -> CompactionInput {
    if memchr::memmem::find(json, b"compaction").is_none() {
        return CompactionInput::default();
    }
    let Ok(peek) = serde_json::from_slice::<BodyPeek<'_>>(json) else {
        return CompactionInput::default();
    };
    let Some(InputPeek::Items(items)) = peek.input else {
        return CompactionInput::default();
    };
    let mut out = CompactionInput::default();
    for item in items {
        match item.kind.as_deref() {
            Some(COMPACTION_TRIGGER_ITEM) => out.trigger = true,
            Some(k) if is_compaction_item(k) => {
                if let Some(c) = item
                    .encrypted_content
                    .as_deref()
                    .filter(|c| !c.is_empty() && !c.starts_with(crate::compaction_proxy::PREFIX))
                {
                    // Router capsules are authenticated and expanded before
                    // each attempt. They are portable across this owner's
                    // configured accounts, unlike opaque native ciphertext.
                    out.items.push(fingerprint(c));
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ItemDonePeek<'a> {
    #[serde(borrow)]
    item: Option<ItemPeek<'a>>,
}

/// The fingerprint of the compaction item of a `response.output_item.done`
/// event, if it carries one.
pub fn output_item(json: &[u8]) -> Option<Fingerprint> {
    memchr::memmem::find(json, b"compaction")?;
    let peek: ItemDonePeek<'_> = serde_json::from_slice(json).ok()?;
    let item = peek.item?;
    if !item.kind.as_deref().is_some_and(is_compaction_item) {
        return None;
    }
    item.encrypted_content
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(fingerprint)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TerminalPeek<'a> {
    #[serde(borrow)]
    response: Option<OutputPeek<'a>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OutputPeek<'a> {
    #[serde(borrow)]
    output: Option<InputPeek<'a>>,
}

/// Fingerprints of the compaction items in the `output` of a terminal event
/// (`response.completed`): providers that send the item only there.
pub fn completed_items(json: &[u8]) -> Vec<Fingerprint> {
    if memchr::memmem::find(json, b"compaction").is_none() {
        return Vec::new();
    }
    let Ok(peek) = serde_json::from_slice::<TerminalPeek<'_>>(json) else {
        return Vec::new();
    };
    let Some(InputPeek::Items(items)) = peek.response.and_then(|r| r.output) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|i| i.kind.as_deref().is_some_and(is_compaction_item))
        .filter_map(|i| i.encrypted_content.as_deref().filter(|c| !c.is_empty()))
        .map(fingerprint)
        .collect()
}

// ---------------------------------------------------------------------------
// Owners
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OwnerEntry {
    fp: Fingerprint,
    member: String,
    at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OwnersFile {
    format: u32,
    #[serde(default)]
    entries: Vec<OwnerEntry>,
}

/// Which member made each compaction item seen (fingerprint → member id).
#[derive(Debug, Default)]
pub struct Owners {
    path: Option<PathBuf>,
    map: Mutex<HashMap<Fingerprint, (String, u64)>>,
}

impl Owners {
    /// The table kept in `path` (`None`: in memory only); an unreadable file
    /// starts empty.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut map = HashMap::new();
        if let Some(file) = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<OwnersFile>(&b).ok())
            .filter(|f| f.format == 1)
        {
            let now = unix_ms();
            for e in file.entries {
                if now.saturating_sub(e.at_ms) < OWNER_TTL_MS {
                    map.insert(e.fp, (e.member, e.at_ms));
                }
            }
        }
        Self {
            path,
            map: Mutex::new(map),
        }
    }

    /// The member that made the item with `fp`.
    pub fn owner(&self, fp: &str) -> Option<String> {
        lock(&self.map).get(fp).map(|(m, _)| m.clone())
    }

    /// Number of fingerprints known.
    pub fn len(&self) -> usize {
        lock(&self.map).len()
    }

    /// True when nothing is known.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Record that `member` made (or reads) the items `fps`; with `adopt`, only
    /// items without an owner are recorded. Writes the file when something
    /// changed (best effort).
    pub fn learn(&self, fps: &[Fingerprint], member: &str, adopt: bool) {
        if fps.is_empty() || member.is_empty() {
            return;
        }
        let now = unix_ms();
        let snapshot = {
            let mut map = lock(&self.map);
            let mut changed = false;
            for fp in fps {
                match map.get_mut(fp) {
                    Some(e) if adopt || e.0 == member => e.1 = now,
                    Some(e) => {
                        *e = (member.to_owned(), now);
                        changed = true;
                    }
                    None => {
                        map.insert(fp.clone(), (member.to_owned(), now));
                        changed = true;
                    }
                }
            }
            if map.len() > MAX_OWNERS {
                let mut by_age: Vec<(Fingerprint, u64)> =
                    map.iter().map(|(k, v)| (k.clone(), v.1)).collect();
                by_age.sort_by_key(|(_, at)| *at);
                for (k, _) in by_age.into_iter().take(map.len() - MAX_OWNERS) {
                    map.remove(&k);
                }
                changed = true;
            }
            if !changed || self.path.is_none() {
                return;
            }
            OwnersFile {
                format: 1,
                entries: map
                    .iter()
                    .map(|(fp, (m, at))| OwnerEntry {
                        fp: fp.clone(),
                        member: m.clone(),
                        at_ms: *at,
                    })
                    .collect(),
            }
        };
        if let Some(path) = &self.path
            && let Err(e) = write_atomic(path, &snapshot)
        {
            tracing::warn!(error = %e, "compaction owners not saved");
        }
    }
}

fn write_atomic(path: &std::path::Path, file: &OwnersFile) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(file).map_err(std::io::Error::other)?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{OWNERS_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// What [`route`] needs to know about a member.
#[derive(Clone, Copy, Debug)]
pub struct MemberFacts<'a> {
    /// Member id.
    pub id: &'a str,
    /// Display name.
    pub name: &'a str,
    /// Its compaction support.
    pub compaction: CompactionSupport,
}

/// Where a request may go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Anywhere (nothing about compaction).
    Any,
    /// Only the members marked `true` (by index). `kept_on` names the member a
    /// compacted conversation is kept on (for the event and the error text).
    Members {
        /// Allowed members.
        allowed: Vec<bool>,
        /// The member that made the conversation's compaction item.
        kept_on: Option<usize>,
    },
    /// No member can serve it: answer with this error.
    Refuse {
        /// HTTP status.
        status: u16,
        /// Error code.
        code: &'static str,
        /// Message for Codex (shown to the user).
        message: String,
    },
}

impl Route {
    /// True when member `m` may serve the request.
    pub fn allows(&self, m: usize) -> bool {
        match self {
            Route::Any => true,
            Route::Members { allowed, .. } => allowed.get(m).copied().unwrap_or(false),
            Route::Refuse { .. } => false,
        }
    }
}

fn names(members: &[MemberFacts<'_>], pick: impl Fn(&MemberFacts<'_>) -> bool) -> String {
    members
        .iter()
        .filter(|m| pick(m))
        .map(|m| format!("“{}”", m.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where a request with `input` may go among `members` (configuration
/// order), given the makers of compaction items in `owners`.
pub fn route(members: &[MemberFacts<'_>], input: &CompactionInput, owners: &Owners) -> Route {
    if input.is_empty() {
        return Route::Any;
    }
    // The newest compaction item stands for the whole earlier history.
    let owner = input
        .items
        .iter()
        .rev()
        .find_map(|fp| owners.owner(fp))
        .and_then(|id| members.iter().position(|m| m.id == id));
    if let Some(o) = owner {
        let mut allowed = vec![false; members.len()];
        allowed[o] = true;
        return Route::Members {
            allowed,
            kept_on: Some(o),
        };
    }
    let allowed: Vec<bool> = members
        .iter()
        .map(|m| m.compaction != CompactionSupport::Unsupported)
        .collect();
    if allowed.iter().any(|a| *a) {
        return if allowed.iter().all(|a| *a) {
            Route::Any
        } else {
            Route::Members {
                allowed,
                kept_on: None,
            }
        };
    }
    let all = names(members, |_| true);
    let message = if input.items.is_empty() {
        format!(
            "None of the router's providers ({all}) supports Codex remote compaction, so this conversation cannot be compacted here. Start a new conversation, or add a provider that compacts (Krill does) to the router."
        )
    } else {
        format!(
            "This conversation was compacted by a provider the router no longer has, and its providers ({all}) cannot read compacted history. Start a new conversation, or switch back to the provider it was compacted with."
        )
    };
    Route::Refuse {
        status: 400,
        code: UNSUPPORTED_CODE,
        message,
    }
}

/// The router's message when the member a compacted conversation is kept on
/// does not answer.
pub fn owner_unavailable_message(member: &str, failures: &str) -> String {
    format!(
        "This conversation was compacted by “{member}”, and no other provider can read its compacted history, so it does not fail over. “{member}” is not reachable ({failures}). Try again later, or start a new conversation."
    )
}

/// `names` of the members a [`Route::Members`] excludes (for logs and events).
pub fn excluded_names(members: &[MemberFacts<'_>], route: &Route) -> String {
    names_of(members, route, false)
}

fn names_of(members: &[MemberFacts<'_>], route: &Route, allowed: bool) -> String {
    members
        .iter()
        .enumerate()
        .filter(|(i, _)| route.allows(*i) == allowed)
        .map(|(_, m)| format!("“{}”", m.name))
        .collect::<Vec<_>>()
        .join(", ")
}
