//! Account balance and usage data from standard, OpenAI-compatible endpoints.
//!
//! Superba reads account data only through standard endpoints, all on the selected
//! line origin with `Authorization: Bearer <key>`:
//!
//! | Data | Endpoint | Sites |
//! |---|---|---|
//! | Limit, used, remaining, plan, expiry ([`BillingSummary`]) | `GET /v1/dashboard/billing/subscription` and `GET /v1/dashboard/billing/usage` (each falls back to the path without `/v1`): the legacy OpenAI billing API that one-api / new-api style relays serve | every key-based site |
//! | Daily cost ([`UsageReport::daily`]) | `GET /v1/organization/costs` (OpenAI Costs API), else the summed `cost_usd` extension of the usage API | Krill |
//! | Per-model usage ([`UsageReport::models`]) | `GET /v1/organization/usage/completions?group_by=model` (OpenAI Usage API) | Krill |
//!
//! Every source is optional and feature-detected by `superba-net`: a missing
//! endpoint (404 / 405) or a network error hides that piece of data and never
//! blocks switching. Krill rate-limits the usage API (10 requests per hour per
//! user), so usage reports are fetched only on demand and cached on disk.

use std::fmt;
use std::iter::Sum;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;
use crate::i18n::LocalizedText;
use crate::ids::{LineId, ProfileId, SiteId};
use crate::time::Timestamp;

/// An amount of US dollars with micro-dollar precision (no float drift when
/// summing many small costs). Serialized as a JSON number of dollars; accepts a
/// number or a numeric string when deserialized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Usd(i64);

/// Largest accepted magnitude in dollars (keeps micros far from `i64::MAX`).
const MAX_DOLLARS: f64 = 1e12;

impl Usd {
    /// Zero dollars.
    pub const ZERO: Usd = Usd(0);

    /// From micro-dollars.
    pub const fn from_micros(micros: i64) -> Self {
        Usd(micros)
    }

    /// Micro-dollars.
    pub const fn micros(self) -> i64 {
        self.0
    }

    /// From a dollar amount; `None` for NaN, infinities and absurd magnitudes.
    pub fn from_dollars(dollars: f64) -> Option<Self> {
        (dollars.is_finite() && dollars.abs() < MAX_DOLLARS)
            .then(|| Usd((dollars * 1e6).round() as i64))
    }

    /// From an amount in US cents (the unit of `total_usage` in the legacy billing
    /// API).
    pub fn from_cents(cents: f64) -> Option<Self> {
        Self::from_dollars(cents / 100.0)
    }

    /// Parse a plain decimal dollar string (`12`, `-3.5`, `0.0042`, `+1.`); digits
    /// beyond the sixth decimal are rounded half away from zero. Exponents, commas
    /// and currency signs are rejected.
    pub fn parse_dollars(s: &str) -> Option<Self> {
        let t = s.trim();
        let (negative, unsigned) = match t.as_bytes().first() {
            Some(b'-') => (true, &t[1..]),
            Some(b'+') => (false, &t[1..]),
            _ => (false, t),
        };
        let (int, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
        if !digits(int) || !digits(frac) || (int.is_empty() && frac.is_empty()) || int.len() > 12 {
            return None;
        }
        let whole: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
        let mut micros = whole.checked_mul(1_000_000)?;
        let kept = &frac[..frac.len().min(6)];
        if !kept.is_empty() {
            let scale = 10i64.pow(6 - kept.len() as u32);
            micros = micros.checked_add(kept.parse::<i64>().ok()? * scale)?;
        }
        if frac.as_bytes().get(6).is_some_and(|b| *b >= b'5') {
            micros = micros.checked_add(1)?;
        }
        Some(Usd(if negative { -micros } else { micros }))
    }

    /// The amount in dollars as a float (display and serialization only).
    pub fn as_dollars(self) -> f64 {
        self.0 as f64 / 1e6
    }

    /// `self + other`, saturating.
    pub const fn saturating_add(self, other: Usd) -> Usd {
        Usd(self.0.saturating_add(other.0))
    }

    /// `self - other`, saturating.
    pub const fn saturating_sub(self, other: Usd) -> Usd {
        Usd(self.0.saturating_sub(other.0))
    }

    /// True for exactly zero.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The unsigned amount with `decimals` (0..=6) fraction digits, rounded half
    /// away from zero, e.g. `12.34` (no currency sign, no sign).
    pub fn amount(self, decimals: u8) -> String {
        let decimals = u32::from(decimals.min(6));
        let unit = 10u64.pow(6 - decimals);
        let abs = self.0.unsigned_abs();
        let rounded = (abs + unit / 2) / unit;
        if decimals == 0 {
            return rounded.to_string();
        }
        let scale = 10u64.pow(decimals);
        format!(
            "{}.{:0width$}",
            rounded / scale,
            rounded % scale,
            width = decimals as usize
        )
    }

    /// Fraction digits used by [`fmt::Display`]: two, or four for non-zero amounts
    /// below one cent (so a small cost does not read as `$0.00`).
    pub fn display_decimals(self) -> u8 {
        if self.0 != 0 && self.0.unsigned_abs() < 5_000 {
            4
        } else {
            2
        }
    }
}

impl fmt::Display for Usd {
    /// `$12.34`, `-$0.50`, `$0.0042`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        write!(f, "{sign}${}", self.amount(self.display_decimals()))
    }
}

impl Sum for Usd {
    fn sum<I: Iterator<Item = Usd>>(iter: I) -> Usd {
        iter.fold(Usd::ZERO, Usd::saturating_add)
    }
}

impl<'a> Sum<&'a Usd> for Usd {
    fn sum<I: Iterator<Item = &'a Usd>>(iter: I) -> Usd {
        iter.copied().sum()
    }
}

impl Serialize for Usd {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.as_dollars())
    }
}

impl<'de> Deserialize<'de> for Usd {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UsdVisitor;
        impl Visitor<'_> for UsdVisitor {
            type Value = Usd;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a dollar amount as a number or a decimal string")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Usd, E> {
                Usd::from_dollars(v).ok_or_else(|| E::custom("dollar amount out of range"))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Usd, E> {
                self.visit_f64(v as f64)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Usd, E> {
                self.visit_f64(v as f64)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Usd, E> {
                Usd::parse_dollars(v).ok_or_else(|| E::custom("not a decimal dollar amount"))
            }
        }
        deserializer.deserialize_any(UsdVisitor)
    }
}

/// Balance view from the legacy OpenAI billing endpoints
/// (`dashboard/billing/subscription` + `dashboard/billing/usage`).
///
/// `limit` is `hard_limit_usd`; `used` is `total_usage / 100` for
/// [`BillingSummary::period_start`]..[`BillingSummary::period_end`] (the current
/// UTC calendar month); `remaining = max(limit - used, 0)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingSummary {
    /// `hard_limit_usd`.
    #[serde(default)]
    pub limit: Option<Usd>,
    /// `soft_limit_usd`, when sent.
    #[serde(default)]
    pub soft_limit: Option<Usd>,
    /// The limit is the "unlimited" sentinel of one-api style relays
    /// (`hard_limit_usd` of 100,000,000 or more); `limit` and `remaining` are then
    /// meaningless and not shown.
    #[serde(default)]
    pub unlimited: bool,
    /// Used in the period (`None` when the usage endpoint is unavailable).
    #[serde(default)]
    pub used: Option<Usd>,
    /// `max(limit - used, 0)`, when both are known and the limit is finite.
    #[serde(default)]
    pub remaining: Option<Usd>,
    /// First day of the usage period (`YYYY-MM-DD`, inclusive).
    #[serde(default)]
    pub period_start: Option<String>,
    /// Day after the usage period (`YYYY-MM-DD`, exclusive).
    #[serde(default)]
    pub period_end: Option<String>,
    /// `plan.title`, when sent.
    #[serde(default)]
    pub plan_title: Option<String>,
    /// `access_until` (`0` = none).
    #[serde(default)]
    pub access_until: Option<Timestamp>,
    /// `has_payment_method`, when sent.
    #[serde(default)]
    pub has_payment_method: Option<bool>,
}

impl BillingSummary {
    /// `hard_limit_usd` at or above this is the one-api "unlimited" sentinel.
    pub const UNLIMITED_DOLLARS: i64 = 100_000_000;

    /// Recompute `unlimited` and `remaining` from `limit` and `used`.
    pub fn derive_remaining(&mut self) {
        self.unlimited = self
            .limit
            .is_some_and(|l| l.micros() >= Self::UNLIMITED_DOLLARS.saturating_mul(1_000_000));
        self.remaining = match (self.limit, self.used) {
            (Some(limit), Some(used)) if !self.unlimited => {
                Some(limit.saturating_sub(used).max(Usd::ZERO))
            }
            _ => None,
        };
    }
}

/// Where the daily cost of a [`UsageReport`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CostSource {
    /// `GET /v1/organization/costs`.
    Costs,
    /// The summed `cost_usd` extension of the usage API (Krill).
    UsageApi,
}

impl CostSource {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            CostSource::Costs => LocalizedText::new("usage.cost_source.costs", "Costs API"),
            CostSource::UsageApi => {
                LocalizedText::new("usage.cost_source.usage_api", "Summed from usage records")
            }
        }
    }
}

/// What to fetch for a [`UsageReport`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageQuery {
    /// Number of UTC days ending today (1..=[`UsageQuery::MAX_DAYS`]).
    pub days: u32,
    /// Bypass the cache freshness window (the user pressed refresh). Requests are
    /// still debounced to one per [`UsageQuery::REFRESH_DEBOUNCE_SECS`].
    #[serde(default)]
    pub refresh: bool,
}

impl UsageQuery {
    /// Default range of the usage view.
    pub const DEFAULT_DAYS: u32 = 7;
    /// Longest range (one page of daily buckets on Krill).
    pub const MAX_DAYS: u32 = 31;
    /// A cached report younger than this is used without a request.
    pub const CACHE_TTL_SECS: u64 = 600;
    /// Explicit refreshes closer together than this reuse the cached report.
    pub const REFRESH_DEBOUNCE_SECS: u64 = 60;

    /// The last `days` days, cached.
    pub fn last_days(days: u32) -> Self {
        Self {
            days,
            refresh: false,
        }
    }

    /// Reject ranges outside 1..=[`UsageQuery::MAX_DAYS`].
    pub fn validate(&self) -> crate::Result<()> {
        if (1..=Self::MAX_DAYS).contains(&self.days) {
            Ok(())
        } else {
            Err(Error::invalid(format!(
                "usage range must be 1 to {} days, got {}",
                Self::MAX_DAYS,
                self.days
            )))
        }
    }

    /// `[start, end)` in UTC: midnight `days - 1` days before `now`'s day, and the
    /// next midnight after `now`.
    pub fn range(&self, now: Timestamp) -> (Timestamp, Timestamp) {
        let today = now.utc_day_start().as_millis();
        let day = 86_400_000i64;
        let start = today.saturating_sub(i64::from(self.days.saturating_sub(1)) * day);
        (
            Timestamp::from_millis(start),
            Timestamp::from_millis(today.saturating_add(day)),
        )
    }
}

impl Default for UsageQuery {
    fn default() -> Self {
        Self::last_days(Self::DEFAULT_DAYS)
    }
}

/// One UTC day of a [`UsageReport`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyUsage {
    /// Start of the day (UTC midnight).
    pub start: Timestamp,
    /// Cost of the day, when a cost source exists.
    #[serde(default)]
    pub cost: Option<Usd>,
    /// Requests (usage API only).
    #[serde(default)]
    pub requests: Option<u64>,
    /// Input tokens (usage API only).
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Output tokens (usage API only).
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

/// Usage of one model over the whole range.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Model slug (`"(unknown)"` when the site does not name it).
    pub model: String,
    /// Cost, when the site sends `cost_usd`.
    #[serde(default)]
    pub cost: Option<Usd>,
    /// Requests.
    #[serde(default)]
    pub requests: u64,
    /// Input tokens (including cached ones).
    #[serde(default)]
    pub input_tokens: u64,
    /// Cached input tokens.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
}

/// Why a [`UsageReport`] shows saved data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UsageNotice {
    /// The site answered 429; the report is the last saved one.
    RateLimited {
        /// `Retry-After`, when sent.
        #[serde(default)]
        retry_after_secs: Option<u64>,
    },
    /// The refresh failed (network, server error); the report is the last saved one.
    RefreshFailed {
        /// Short redacted reason.
        reason: String,
    },
}

impl UsageNotice {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            UsageNotice::RateLimited {
                retry_after_secs: Some(secs),
            } => LocalizedText::new(
                "usage.notice.rate_limited_retry",
                "The site limits usage queries; showing saved data. Try again in {minutes} min.",
            )
            .arg("minutes", secs.div_ceil(60).max(1)),
            UsageNotice::RateLimited { .. } => LocalizedText::new(
                "usage.notice.rate_limited",
                "The site limits usage queries; showing saved data.",
            ),
            UsageNotice::RefreshFailed { reason } => LocalizedText::new(
                "usage.notice.refresh_failed",
                "Could not refresh ({reason}); showing saved data.",
            )
            .arg("reason", reason),
        }
    }
}

/// Daily cost and per-model usage of one key on one site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    /// Site.
    pub site_id: SiteId,
    /// Range start (UTC midnight, inclusive).
    pub start: Timestamp,
    /// Range end (UTC midnight, exclusive).
    pub end: Timestamp,
    /// Days in the range.
    pub days: u32,
    /// One entry per day, oldest first.
    #[serde(default)]
    pub daily: Vec<DailyUsage>,
    /// Per-model totals, most expensive (then most requested) first. Empty when
    /// the usage API is unavailable.
    #[serde(default)]
    pub models: Vec<ModelUsage>,
    /// Sum of the daily costs.
    #[serde(default)]
    pub total_cost: Option<Usd>,
    /// Where the costs come from (`None` = no cost data).
    #[serde(default)]
    pub cost_source: Option<CostSource>,
    /// The usage API answered (per-model data is meaningful even when empty).
    #[serde(default)]
    pub usage_available: bool,
    /// When the data was fetched from the site.
    pub fetched_at: Timestamp,
    /// Served from the on-disk cache without a request.
    #[serde(default)]
    pub from_cache: bool,
    /// Why saved data is shown, if a refresh was attempted and failed.
    #[serde(default)]
    pub notice: Option<UsageNotice>,
}

/// Input of `SwitchService::account`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRequest {
    /// Profile whose key is used; `None` = the applied profile, else the last
    /// selected key-based profile.
    #[serde(default)]
    pub profile_id: Option<ProfileId>,
    /// Line to query; `None` = the applied line when this profile is applied, else
    /// the profile's line, else the site's default line.
    #[serde(default)]
    pub line_id: Option<LineId>,
    /// Also fetch a usage report (Krill only; rate-limited, so only on demand).
    #[serde(default)]
    pub usage: Option<UsageQuery>,
}

/// Output of `SwitchService::account`: identity + balance, optionally usage.
/// Each part fails independently; a failed part never fails the others.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountReport {
    /// Profile whose key was used.
    pub profile_id: ProfileId,
    /// Its name.
    pub profile_name: String,
    /// Site.
    pub site_id: SiteId,
    /// Line queried.
    pub line_id: LineId,
    /// Identity and balance (`None` = the site has no account data).
    #[serde(default)]
    pub summary: Option<crate::caps::AccountSummary>,
    /// Why `summary` is missing, if it failed.
    #[serde(default)]
    pub summary_error: Option<Error>,
    /// The usage report, when requested and available.
    #[serde(default)]
    pub usage: Option<UsageReport>,
    /// Why `usage` is missing, if it was requested and failed.
    #[serde(default)]
    pub usage_error: Option<Error>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_parses_rounds_and_displays() {
        assert_eq!(
            Usd::parse_dollars("12.34"),
            Some(Usd::from_micros(12_340_000))
        );
        assert_eq!(Usd::parse_dollars("-3"), Some(Usd::from_micros(-3_000_000)));
        assert_eq!(Usd::parse_dollars(".5"), Some(Usd::from_micros(500_000)));
        assert_eq!(Usd::parse_dollars("1."), Some(Usd::from_micros(1_000_000)));
        assert_eq!(
            Usd::parse_dollars("0.0000015"),
            Some(Usd::from_micros(2)),
            "seventh digit rounds"
        );
        for bad in ["", ".", "-", "1e3", "1,5", "$1", "1.2.3", "+-1"] {
            assert_eq!(Usd::parse_dollars(bad), None, "{bad}");
        }
        assert_eq!(Usd::from_cents(1234.0), Some(Usd::from_micros(12_340_000)));
        assert_eq!(Usd::from_dollars(f64::NAN), None);
        assert_eq!(Usd::from_micros(12_345_000).to_string(), "$12.35");
        assert_eq!(Usd::from_micros(-500_000).to_string(), "-$0.50");
        assert_eq!(Usd::from_micros(4_200).to_string(), "$0.0042");
        assert_eq!(Usd::ZERO.to_string(), "$0.00");
        assert_eq!(Usd::from_micros(1_999_999).amount(0), "2");
        let sum: Usd = [Usd::from_micros(1), Usd::from_micros(2)].iter().sum();
        assert_eq!(sum, Usd::from_micros(3));
    }

    #[test]
    fn usd_serializes_as_dollars_and_reads_strings() {
        let v = serde_json::to_value(Usd::from_micros(1_500_000)).unwrap();
        assert_eq!(v, serde_json::json!(1.5));
        let back: Usd = serde_json::from_value(v).unwrap();
        assert_eq!(back, Usd::from_micros(1_500_000));
        let s: Usd = serde_json::from_str("\"2.25\"").unwrap();
        assert_eq!(s, Usd::from_micros(2_250_000));
        let i: Usd = serde_json::from_str("3").unwrap();
        assert_eq!(i, Usd::from_micros(3_000_000));
        assert!(serde_json::from_str::<Usd>("\"abc\"").is_err());
    }

    #[test]
    fn remaining_is_floored_and_unlimited_is_detected() {
        let mut b = BillingSummary {
            limit: Usd::from_dollars(10.0),
            used: Usd::from_dollars(12.5),
            ..BillingSummary::default()
        };
        b.derive_remaining();
        assert_eq!(b.remaining, Some(Usd::ZERO));
        b.used = Usd::from_dollars(2.5);
        b.derive_remaining();
        assert_eq!(b.remaining, Usd::from_dollars(7.5));
        b.limit = Usd::from_dollars(100_000_000.0);
        b.derive_remaining();
        assert!(b.unlimited);
        assert_eq!(b.remaining, None);
        b.used = None;
        b.limit = Usd::from_dollars(5.0);
        b.derive_remaining();
        assert!(!b.unlimited);
        assert_eq!(b.remaining, None);
    }

    #[test]
    fn usage_ranges_are_whole_utc_days_ending_today() {
        let now = Timestamp::from_millis(1_793_491_200_000 + 3_600_000); // 2026-11-01T01:00Z
        let (start, end) = UsageQuery::last_days(7).range(now);
        assert_eq!(start.utc_date_string(), "2026-10-26");
        assert_eq!(end.utc_date_string(), "2026-11-02");
        assert_eq!(end.as_millis() - start.as_millis(), 7 * 86_400_000);
        assert!(UsageQuery::last_days(0).validate().is_err());
        assert!(UsageQuery::last_days(31).validate().is_ok());
        assert!(UsageQuery::last_days(32).validate().is_err());
    }

    #[test]
    fn notices_render_minutes() {
        let n = UsageNotice::RateLimited {
            retry_after_secs: Some(61),
        };
        assert_eq!(n.text().args, vec![("minutes".into(), "2".into())]);
    }
}
