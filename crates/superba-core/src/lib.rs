//! # superba-core
//!
//! Shared domain types, the built-in site catalog and the trait contracts of
//! Superba, a cross-platform launcher that configures OpenAI Codex (desktop app,
//! CLI and SSH remote hosts) for Krill or other sites, with hot switching and
//! restore.
//!
//! This crate has no IO-heavy dependencies. Every other crate depends on it and
//! implements one of the traits in [`traits`]; see `ARCHITECTURE.md` at the
//! repository root for the crate map and the data flow of a switch.
//!
//! Module guide:
//! - [`site`], [`catalog`], [`origin`]: sites, lines, the Krill catalog, URL rules.
//! - [`auth`], [`profile`], [`secret`], [`target`]: what to switch to and where.
//! - [`settings`]: Codex settings of a profile, auth choice, the catalog view and
//!   the settings engine's plans, diffs and `codex features list` cross-check.
//! - [`paths`]: `CODEX_HOME` / data-dir resolution with injectable env and home.
//! - [`codex`]: verified Codex / desktop-app facts (keys, env vars, IPC, versions).
//! - [`config`], [`env`], [`caps`], [`sessions`], [`app`], [`remote`]: request and
//!   response types of each component trait.
//! - [`plan`]: restart decisions, switch plans, execution events and the overview.
//! - [`traits`]: the component contracts.
//! - [`usage`]: account balance and usage data from standard endpoints.
//! - [`router`]: router mode settings and status; [`views`]: settings page rows,
//!   settings edits and the settings part of a plan.
//! - v0.3: [`models`] (model lists, overrides, speed tiers, the catalog file),
//!   [`requests`] (the router's request log and prices), [`versions`] (Codex and
//!   app versions, the Codex store, pinning, [`VersionStore`]), [`feeds`] (signed
//!   data feeds, [`FeedSource`]); remote host sync types live in [`remote`].
//! - [`i18n`], [`time`], [`error`]: infrastructure.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod auth;
pub mod caps;
pub mod catalog;
pub mod codex;
pub mod config;
pub mod env;
pub mod error;
pub mod feeds;
pub mod i18n;
pub mod ids;
pub mod models;
pub mod origin;
pub mod paths;
pub mod plan;
pub mod profile;
pub mod remote;
pub mod requests;
pub mod router;
pub mod secret;
pub mod sessions;
pub mod settings;
pub mod site;
pub mod target;
pub mod time;
pub mod traits;
pub mod transports;
pub mod usage;
pub mod versions;
pub mod views;

pub use app::*;
pub use auth::*;
pub use caps::*;
pub use codex::CodexVersion;
pub use config::*;
pub use env::{
    EnvApplied, EnvEdit, EnvEditAction, EnvLocation, EnvObservation, EnvOp, EnvPlan, EnvReport,
    EnvSnapshot, PersistedValue, RcLine, ShellKind,
};
pub use error::{Error, Result};
pub use feeds::*;
pub use i18n::{Catalog, Locale, LocalizedText, Notice, Severity};
pub use ids::*;
pub use models::*;
pub use origin::{CodexPath, Origin};
pub use paths::{CodexHome, DataDir, EnvSource, LocalContext, MapEnv, ProcessEnv};
pub use plan::*;
pub use profile::*;
pub use remote::*;
pub use requests::*;
pub use router::*;
pub use secret::*;
pub use sessions::*;
pub use settings::*;
pub use site::*;
pub use target::*;
pub use time::{Clock, Countdown, ManualClock, SystemClock, Timestamp};
pub use traits::*;
pub use transports::*;
pub use usage::*;
pub use versions::*;
pub use views::*;

/// Re-exported so implementors use the same macro version as the trait definitions.
pub use async_trait::async_trait;
