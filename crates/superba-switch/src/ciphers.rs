//! Conversations that carry content another provider encrypted (GitHub
//! issue 11).
//!
//! Every Codex conversation keeps opaque state its provider encrypted for
//! itself: hidden reasoning, a sub-agent's task (`agent_message`), encrypted
//! tool results, compacted history. Only that provider's backend reads it.
//! Superba writes the built-in `openai` provider for every site, so Codex
//! lists every conversation under every profile, and continuing one on another
//! provider is refused with `invalid_encrypted_content` on every turn. The
//! router repairs such requests ([`superba_router::cipher`]); in direct mode
//! Superba is not in the path, so it says so:
//!
//! - **Before a switch** ([`Switcher::ciphertext_notes`]): the destination
//!   cannot read what another provider made — the conversations of the last
//!   [`RECENT_DAYS`] days that were last continued on a provider whose content
//!   the destination cannot read are counted, with what to do.
//! - **Afterwards** ([`Switcher::encrypted_refusals_notice`]): an Overview
//!   notice when conversations stopped for it in the last
//!   [`REFUSED_HOURS`] hours (Codex records the refusal in the rollout; the
//!   usage index reads it as [`ENCRYPTED_CONTENT_CODE`]).
//!
//! Who reads what: the official ChatGPT login and Krill (its relay serves
//! Codex from ChatGPT Codex backend accounts) share the ChatGPT Codex
//! backend; each compatible site is its own.

use std::collections::BTreeMap;
use std::time::Duration;

use superba_core::{Notice, ProfileId, ProfileSet, RequestStatus, Site, SiteId, SiteKind};
use superba_sessions::usage::{ENCRYPTED_CONTENT_CODE, RolloutRequest};

use crate::Switcher;
use crate::usage::Attribution;

/// Conversations used in the last this-many days are counted before a
/// switch.
pub const RECENT_DAYS: u64 = 7;
/// Conversations stopped in the last this-many hours are named on the
/// Overview.
pub const REFUSED_HOURS: u64 = 24;
/// Longest wait for the rollout index while planning.
const PLAN_BUDGET: Duration = Duration::from_secs(2);
/// Longest wait for it on the (polled) Overview.
const OVERVIEW_BUDGET: Duration = Duration::from_millis(300);

/// Who can read a site's ciphertexts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CipherFamily {
    /// The ChatGPT Codex backend: the official login and Krill.
    ChatGptBackend,
    /// A compatible site: only itself.
    Site(SiteId),
}

/// The family of `site`.
pub fn family(site: &Site) -> CipherFamily {
    match site.kind {
        SiteKind::Official | SiteKind::Krill => CipherFamily::ChatGptBackend,
        _ => CipherFamily::Site(site.id.clone()),
    }
}

/// The latest request of each thread among `rows`.
fn latest_by_thread(rows: &[RolloutRequest]) -> BTreeMap<&str, &RolloutRequest> {
    let mut latest: BTreeMap<&str, &RolloutRequest> = BTreeMap::new();
    for r in rows {
        let e = latest.entry(r.thread_id.as_str()).or_insert(r);
        if r.completed_at > e.completed_at {
            *e = r;
        }
    }
    latest
}

/// Threads whose latest request among `rows` was refused for content another
/// provider encrypted.
pub fn refused_threads(rows: &[RolloutRequest]) -> usize {
    latest_by_thread(rows)
        .values()
        .filter(|r| {
            matches!(&r.status, RequestStatus::Failed { code: Some(c) } if c == ENCRYPTED_CONTENT_CODE)
        })
        .count()
}

/// The threads among `rows` whose latest request ran on a provider of another
/// family than `dest` (`family_of` maps a profile to its family; providers
/// Superba does not know count as their own), and the most frequent such
/// provider's name.
pub fn continued_elsewhere(
    rows: &[RolloutRequest],
    a: &Attribution,
    dest: &CipherFamily,
    family_of: impl Fn(&ProfileId) -> Option<CipherFamily>,
) -> Option<(usize, String)> {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    let mut count = 0;
    for r in latest_by_thread(rows).values() {
        let (profile, name) = a.member(r.model_provider.as_deref(), r.started_at);
        let theirs = family_of(&profile)
            .unwrap_or_else(|| CipherFamily::Site(SiteId::new(profile.as_str())));
        if &theirs != dest {
            count += 1;
            *by.entry(name).or_default() += 1;
        }
    }
    // The most frequent; of equals, the first by name.
    let top = by
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))?
        .0;
    Some((count, top))
}

impl Switcher {
    /// The rollout requests since `secs` seconds ago, within `budget` (a cold
    /// index keeps filling in the background; `None` then).
    async fn recent_rollout_rows(
        &self,
        secs: u64,
        budget: Duration,
    ) -> Option<Vec<RolloutRequest>> {
        let now = self.deps.clock.now();
        let from = now.saturating_sub(Duration::from_secs(secs));
        let index = self.usage.clone();
        let scan = tokio::task::spawn_blocking(move || {
            index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .requests(Some(from), None)
        });
        let page = tokio::time::timeout(budget, scan).await.ok()?.ok()?;
        Some(page.rows)
    }

    /// Notes of a direct switch from `from_site` to `dest`: conversations
    /// carry content the destination cannot read (counted from the local
    /// rollouts; nothing is sent).
    pub(crate) async fn ciphertext_notes(
        &self,
        set: &ProfileSet,
        sites: &[Site],
        from_site: Option<&SiteId>,
        dest: &Site,
        dest_name: &str,
    ) -> Vec<Notice> {
        let text = crate::messages::text;
        let dest_family = family(dest);
        // A site Superba no longer knows is its own family.
        let Some(from_family) = from_site.map(|id| {
            sites
                .iter()
                .find(|s| &s.id == id)
                .map_or_else(|| CipherFamily::Site(id.clone()), family)
        }) else {
            return Vec::new();
        };
        if from_family == dest_family {
            return Vec::new();
        }
        let family_of = |id: &ProfileId| {
            set.profile(id)
                .and_then(|p| sites.iter().find(|s| s.id == p.site_id))
                .map(family)
        };
        let counted = match self
            .recent_rollout_rows(RECENT_DAYS * 86_400, PLAN_BUDGET)
            .await
        {
            Some(rows) => {
                let a = self.attribution().await;
                continued_elsewhere(&rows, &a, &dest_family, family_of)
            }
            None => None,
        };
        match counted {
            Some((count, provider)) if count > 0 => vec![Notice::warning(
                text("switch.warn.ciphertext_elsewhere")
                    .arg("count", count.to_string())
                    .arg("provider", provider)
                    .arg("site", dest_name)
                    .arg("days", RECENT_DAYS.to_string()),
            )],
            _ => vec![Notice::info(text("switch.warn.cross_site"))],
        }
    }

    /// The Overview's notice (direct mode): conversations stopped in the
    /// last [`REFUSED_HOURS`] hours because their provider could not read
    /// content another provider encrypted.
    pub(crate) async fn encrypted_refusals_notice(&self) -> Option<Notice> {
        let rows = self
            .recent_rollout_rows(REFUSED_HOURS * 3600, OVERVIEW_BUDGET)
            .await?;
        let count = refused_threads(&rows);
        (count > 0).then(|| {
            Notice::warning(
                crate::messages::text("switch.overview.encrypted_refused")
                    .arg("count", count.to_string()),
            )
        })
    }
}
