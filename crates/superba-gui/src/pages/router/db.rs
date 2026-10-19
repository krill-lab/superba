//! Data access of the Usage page. The request log is read through the switch
//! service (`router_requests`, `router_request`, `router_stats`,
//! `router_inflight`, `router_clear`), which opens the router's SQLite database
//! read-only (so history stays visible while the router is stopped) and merges
//! the model requests in Codex's rollout files. Session
//! names come from `session_titles` (the local `state_5.sqlite`), are cached
//! here for display and never written anywhere.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_core::{
    Error, RequestGrouping, RequestPage, RequestQuery, Result, SessionTitle, SwitchService,
};

use super::requests::{is_resolved, merge_rows, totals_of};
use crate::runtime::ReadTask;

/// One background read with its last result.
#[derive(Debug)]
pub struct Loader<T> {
    task: Option<ReadTask<T>>,
    /// Last value.
    pub value: Option<T>,
    /// Last error.
    pub error: Option<Error>,
    /// When the last result arrived.
    pub at: Option<Instant>,
    /// The inputs changed while a read was running: load again.
    stale: bool,
}

impl<T> Default for Loader<T> {
    fn default() -> Self {
        Self {
            task: None,
            value: None,
            error: None,
            at: None,
            stale: false,
        }
    }
}

impl<T: Send + 'static> Loader<T> {
    /// A read is running.
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }

    /// Mark the value as outdated (inputs changed).
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    /// No value yet, inputs changed, or older than `every`.
    pub fn due(&self, every: Option<Duration>) -> bool {
        if self.task.is_some() {
            return false;
        }
        self.stale
            || self.at.is_none()
            || every.is_some_and(|e| self.at.is_some_and(|t| t.elapsed() >= e))
    }

    /// Start a read unless one is running.
    pub fn start(&mut self, future: impl std::future::Future<Output = Result<T>> + Send + 'static) {
        if self.task.is_none() {
            self.stale = false;
            self.task = Some(crate::runtime::task(future));
        }
    }

    /// Take a finished result; true when one arrived this frame.
    pub fn poll(&mut self) -> bool {
        let Some(task) = &self.task else {
            return false;
        };
        let Some(result) = task.poll() else {
            return false;
        };
        self.task = None;
        self.at = Some(Instant::now());
        match result {
            Ok(v) => {
                self.value = Some(v);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        true
    }

    /// Forget everything (tests, clear).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Load one page of the request log. A search also matches session names:
/// `title_keys` are the session keys whose (locally resolved) names match, and
/// their rows are merged in (then the totals are those of the merged rows).
pub async fn load_page(
    service: Arc<dyn SwitchService>,
    query: RequestQuery,
    title_keys: Vec<String>,
) -> Result<RequestPage> {
    let mut page = service.router_requests(query.clone()).await?;
    let searching = query
        .search
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if !searching || title_keys.is_empty() {
        return Ok(page);
    }
    let mut keys: Vec<String> = title_keys
        .into_iter()
        .map(|key| superba_core::harness_group_key(superba_core::RequestHarness::Codex, &key))
        .collect();
    if query.group_by == RequestGrouping::Session
        && let Some(g) = &query.group
    {
        keys.retain(|k| k == g);
    }
    let mut extra = Vec::new();
    for key in keys.into_iter().take(8) {
        let mut q = query.clone();
        q.search = None;
        q.after_id = None;
        // Keep the selected provider (or provider line) of the source list.
        let group = q.group.clone();
        let grouping = q.group_by;
        q.group_by = RequestGrouping::Session;
        q.group = Some(key);
        let rows = service.router_requests(q).await?.rows;
        extra.extend(rows.into_iter().filter(|r| {
            group.as_deref().is_none_or(|g| match grouping {
                RequestGrouping::Provider => r.in_provider_group(g),
                RequestGrouping::Harness => r.harness.id() == g,
                RequestGrouping::HarnessModel => r.harness_model_key() == g,
                _ => true,
            })
        }));
    }
    if extra
        .iter()
        .any(|r| !page.rows.iter().any(|x| x.id == r.id))
    {
        page.rows = merge_rows(page.rows, extra);
        page.totals = totals_of(&page.rows);
    }
    Ok(page)
}

/// How long an unresolved name (short key, first message) is kept before it is
/// asked again (the app names a thread a few seconds after its first turn).
const RETRY_UNRESOLVED: Duration = Duration::from_secs(30);
/// Keys per lookup.
const BATCH: usize = 200;

/// Session names of request log keys, resolved locally and cached for display.
#[derive(Debug, Default)]
pub struct Titles {
    map: BTreeMap<String, SessionTitle>,
    asked: HashMap<String, Instant>,
    task: Option<ReadTask<BTreeMap<String, SessionTitle>>>,
    /// The lookup is not available (shows short keys).
    pub error: Option<Error>,
}

impl Titles {
    /// Names resolved so far.
    pub fn map(&self) -> &BTreeMap<String, SessionTitle> {
        &self.map
    }

    /// Insert names (tests).
    pub fn insert(&mut self, key: String, title: SessionTitle) {
        self.map.insert(key, title);
    }

    /// A lookup is running.
    pub fn loading(&self) -> bool {
        self.task.is_some()
    }

    /// Take a finished lookup; true when names arrived.
    pub fn poll(&mut self) -> bool {
        let Some(task) = &self.task else {
            return false;
        };
        let Some(result) = task.poll() else {
            return false;
        };
        self.task = None;
        match result {
            Ok(names) => {
                self.map.extend(names);
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    /// Ask for the names of `keys` that are unknown, or unresolved and not asked
    /// recently (one lookup at a time).
    pub fn want<'a>(
        &mut self,
        service: &Arc<dyn SwitchService>,
        keys: impl IntoIterator<Item = &'a str>,
    ) {
        if self.task.is_some() {
            return;
        }
        let mut wanted: Vec<String> = Vec::new();
        for k in keys {
            if wanted.len() >= BATCH || wanted.iter().any(|w| w == k) {
                continue;
            }
            let known = self.map.get(k).is_some_and(is_resolved);
            let recent = self
                .asked
                .get(k)
                .is_some_and(|t| t.elapsed() < RETRY_UNRESOLVED);
            if !known && !recent {
                wanted.push(k.to_owned());
            }
        }
        if wanted.is_empty() {
            return;
        }
        let now = Instant::now();
        for k in &wanted {
            self.asked.insert(k.clone(), now);
        }
        let service = service.clone();
        self.task = Some(crate::runtime::task(async move {
            service.session_titles(wanted).await
        }));
    }
}
