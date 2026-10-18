//! Ciphertexts a member cannot read (GitHub issue 11).
//!
//! A Codex conversation carries opaque state its provider encrypted for
//! itself: `reasoning.encrypted_content`, the payload of a multi-agent
//! `agent_message` (`amsg_…`), encrypted tool outputs (`notes` / `history`),
//! encrypted function-call arguments, compaction items and, with Codex's
//! output-token replay, the `encrypted_content` of messages and tool calls.
//! Codex sends all of it back with every request, and only the backend that
//! made it can read it. Superba configures every provider as Codex's built-in
//! `openai` provider, so Codex never strips it (it does only for providers that
//! are not `openai`), and Codex stores a spawned sub-agent's task as an
//! `encrypted_content` part even when the provider sent it in plain text (it
//! trusts the text as plain only when the call says `encrypted_function_args:
//! []`, which only OpenAI backends send). A conversation that reaches another
//! provider — a failover, a reload that moves conversations to the new first
//! member, a profile switch, a conversation started before the router — is then
//! refused with `invalid_encrypted_content` ("The encrypted content for item
//! amsg_… could not be verified", "Encrypted function output content could not
//! be decrypted or decoded") on every turn.
//!
//! The router repairs such requests on their way out:
//!
//! - **Reading** ([`scan`]): the ciphertexts of a Responses body or
//!   `response.create` frame, where they sit and their SHA-256 fingerprints.
//! - **Recognising** ([`rejection`]): an answer that refuses one (HTTP 400 /
//!   422 / 502 JSON before any byte reached Codex, a WebSocket `error` /
//!   `response.failed` event), and what it names (an item id, a ciphertext's
//!   first and last four characters, or the function-output wording).
//! - **Neutralising** ([`rewrite`]): exactly the chosen items change, every
//!   other item keeps its bytes. A reasoning item goes (it holds nothing
//!   visible); an `encrypted_content` part that holds plain text becomes an
//!   `input_text` part with that text (the sub-agent keeps its task); a real
//!   ciphertext part becomes a short note; an item's own `encrypted_content`
//!   field and encrypted function arguments are dropped; a compaction item is
//!   replaced by a note only when the answer named it.
//! - **Remembering** ([`ledger`]): per member, the fingerprints it read (a
//!   request carrying them succeeded) and those it refused. On a refusal the
//!   router neutralises what it names plus every other ciphertext the member is
//!   not known to read, remembers them as refused and sends the request again
//!   once (HTTP: at once, to the same line; WebSocket: the socket is closed
//!   with 1012 and Codex sends the turn again on a new one). Later requests to
//!   that member get the same items neutralised before they leave, so the
//!   conversation keeps working, presents the same bytes every turn (its prompt
//!   cache stays warm) and never pays a refused round trip again. The memory is
//!   in memory only (fingerprints and member ids, never content).
//! - A sub-agent whose current task is a real ciphertext the member cannot read
//!   is not sent without its task: the router answers 400
//!   [`TASK_UNREADABLE_CODE`] with an explanation instead.

pub mod ledger;
pub mod rejection;
pub mod rewrite;
pub mod scan;

use std::collections::BTreeSet;

use crate::compaction::Fingerprint;

pub use ledger::{Ledger, Verdict};
pub use rejection::{Named, Rejection};
pub use rewrite::{Removed, neutralise};
pub use scan::{Cipher, Place, Scan, scan};

/// Error code upstreams use for a ciphertext they cannot read.
pub const REJECTION_CODE: &str = "invalid_encrypted_content";
/// Error code of the router's answer when a sub-agent's task cannot be read.
pub const TASK_UNREADABLE_CODE: &str = "encrypted_task_unreadable";

/// What to do with a request a member refused for a ciphertext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Neutralise these fingerprints and send the request again.
    Neutralise(BTreeSet<Fingerprint>),
    /// The member cannot read the sub-agent's current task: answer with an
    /// explanation (a task left out would leave the sub-agent without one).
    TaskUnreadable,
    /// Nothing left to neutralise: pass the refusal on.
    PassThrough,
}

/// The plan for a request whose ciphertexts are `scan` after `member`'s
/// refusal `rejection`; `reads` says whether the member is known to read a
/// fingerprint.
pub fn plan(scan: &Scan, rejection: &Rejection, reads: impl Fn(&Fingerprint) -> bool) -> Plan {
    let named: Vec<&Cipher> = scan.ciphers.iter().filter(|c| rejection.names(c)).collect();
    let is_task =
        |c: &Cipher| Some(c.item) == scan.task_item && c.place == Place::Part { plaintext: false };
    // Named precisely and it is the task itself. Upstream names a ciphertext
    // by an item id, or by its first and last four characters, which only
    // count when exactly one ciphertext of the request has them.
    let precise = match &rejection.named {
        Named::Item(_) => true,
        Named::Ends(..) => {
            let distinct: BTreeSet<&Fingerprint> = named.iter().map(|c| &c.fp).collect();
            distinct.len() == 1
        }
        Named::Parts | Named::Unnamed => false,
    };
    if precise && named.iter().any(|c| is_task(c)) {
        return Plan::TaskUnreadable;
    }
    let mut task_candidate = false;
    let mut remove = BTreeSet::new();
    for c in &scan.ciphers {
        let wanted = rejection.names(c) || (c.place != Place::Compaction && !reads(&c.fp));
        if !wanted {
            continue;
        }
        if is_task(c) {
            task_candidate = true;
            continue;
        }
        remove.insert(c.fp.clone());
    }
    // The same ciphertext may sit elsewhere too: never touch the task's.
    for c in scan.ciphers.iter().filter(|c| is_task(c)) {
        remove.remove(&c.fp);
    }
    if !remove.is_empty() {
        Plan::Neutralise(remove)
    } else if task_candidate {
        Plan::TaskUnreadable
    } else {
        Plan::PassThrough
    }
}

/// The plan after `member` refused `rejection` of a request whose
/// ciphertexts that reached it are `sent`; what a plan leaves out is
/// remembered as refused by `member`.
pub fn refused(ledger: &Ledger, member: &str, sent: &Scan, rejection: &Rejection) -> Plan {
    let p = plan(sent, rejection, |fp| ledger.reads(member, fp));
    if let Plan::Neutralise(set) = &p {
        ledger.learn(member, set.iter(), Verdict::Refuses);
    }
    p
}

/// The router's explanation when `member` cannot read a sub-agent's task.
pub fn task_unreadable_message(member: &str) -> String {
    format!(
        "This sub-agent's task was encrypted by another provider, and “{member}” cannot read it. Continue the conversation on the provider the main agent ran on, or ask the main agent to start the sub-agent again."
    )
}

/// The routing event's text for neutralised items.
pub fn event_detail(member: &str, removed: &Removed, websocket: bool) -> String {
    let then = if websocket {
        "the WebSocket reconnects and Codex sends the turn again"
    } else {
        "sent the request again"
    };
    format!(
        "“{member}” could not read content another provider encrypted in this conversation; left out {} — {then}",
        removed.describe()
    )
}
