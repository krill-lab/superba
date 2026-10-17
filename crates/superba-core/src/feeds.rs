//! Superba's signed data feeds: built-in model list, price table, Codex releases
//! and desktop app builds ([`crate::FeedSource`], implemented by
//! `superba-catalog`'s `FeedStore` / `HttpFeeds`).
//!
//! Feeds are compiled in as a snapshot and optionally refreshed (at most daily,
//! [`crate::NetworkSettings::online_checks`]) from a raced list of mirrors. An
//! index signed with its own context (`superba-feeds-index-v1\n`) lists every
//! file with its sha256, so any mirror is safe to use; a serial below the one in
//! use is never accepted. Any failure keeps the cache, then the snapshot.
//!
//! Additive owner in v0.3 Phase 1: the catalog/models builder.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::models::ModelSpec;
use crate::requests::PriceTable;
use crate::time::Timestamp;
use crate::versions::{AppRelease, CodexRelease};

/// Where the feed data in use came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeedOrigin {
    /// Compiled into this Superba.
    #[default]
    Builtin,
    /// The verified cache under `<data dir>/feeds/`.
    Cache,
    /// Fetched just now from a mirror.
    Mirror {
        /// Mirror name (`jsdelivr`, `raw`, a user mirror's host).
        name: String,
    },
}

/// The feed data in use.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedSnapshot {
    /// Built-in model list (newest Codex release's models plus hand-written
    /// third-party entries).
    #[serde(default)]
    pub models: Vec<ModelSpec>,
    /// Price table.
    #[serde(default)]
    pub prices: PriceTable,
    /// Codex releases, newest first.
    #[serde(default)]
    pub codex_versions: Vec<CodexRelease>,
    /// Desktop app builds, newest first.
    #[serde(default)]
    pub app_versions: Vec<AppRelease>,
    /// Index serial.
    #[serde(default)]
    pub serial: u64,
    /// Origin.
    #[serde(default)]
    pub source: FeedOrigin,
    /// When the data was fetched (`None` for the built-in snapshot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<Timestamp>,
}

/// Feed state for Diagnostics, Preferences and `spb feeds status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedsStatus {
    /// Serial in use.
    #[serde(default)]
    pub serial: u64,
    /// Serial of the compiled-in snapshot.
    #[serde(default)]
    pub builtin_serial: u64,
    /// Origin of the data in use.
    #[serde(default)]
    pub source: FeedOrigin,
    /// When the data in use was fetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<Timestamp>,
    /// Last online check (successful or not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Timestamp>,
    /// Mirror that won the last race.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    /// Online checks are allowed.
    #[serde(default)]
    pub online_checks: bool,
    /// Why the last check failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<Error>,
    /// Revision of the price table in use (`None` = the table is empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices_rev: Option<String>,
}
