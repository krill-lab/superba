//! Account card data from the standard billing endpoints.
//!
//! Superba reads balances only through the legacy OpenAI billing API, which
//! one-api / new-api style relays (and Krill) serve with API-key auth on the
//! line origin:
//!
//! - `GET /v1/dashboard/billing/subscription` (fallback
//!   `/dashboard/billing/subscription`):
//!   `{"object":"billing_subscription","has_payment_method":bool,
//!   "soft_limit_usd":number,"hard_limit_usd":number,"system_hard_limit_usd":number,
//!   "access_until":unix-seconds (0 = none),"plan":{"title":string,"id":string}}`;
//! - `GET /v1/dashboard/billing/usage?start_date=YYYY-MM-DD&end_date=YYYY-MM-DD`
//!   (end exclusive, at most 100 days; same fallback): `{"object":"list",
//!   "total_usage": number in US cents}`.
//!
//! Parsing is lenient: unknown fields are ignored, numbers may be integers,
//! floats or numeric strings, and an answer wrapped in `{"data": {...}}` is
//! unwrapped. `limit = hard_limit_usd`, `used = total_usage / 100` for the
//! current UTC calendar month, `remaining = max(limit - used, 0)`; a hard limit of
//! 100,000,000 USD or more is one-api's "unlimited" sentinel.
//!
//! Identity (the email) still comes from Krill's whoami; whoami's
//! `chatgpt_plan_type` is a constant for Codex's benefit and is never shown.

use serde_json::{Map, Value};
use superba_core::{BillingSummary, Timestamp, Usd};

/// The parts of a billing subscription answer Superba shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub hard_limit: Option<Usd>,
    pub soft_limit: Option<Usd>,
    pub access_until: Option<Timestamp>,
    pub has_payment_method: Option<bool>,
    pub plan_title: Option<String>,
}

impl Subscription {
    /// Combine with the period's usage into the card's [`BillingSummary`].
    pub fn into_summary(self, used: Option<Usd>, period: (String, String)) -> BillingSummary {
        let mut summary = BillingSummary {
            limit: self.hard_limit,
            soft_limit: self.soft_limit,
            used,
            period_start: used.map(|_| period.0),
            period_end: used.map(|_| period.1),
            plan_title: self.plan_title,
            access_until: self.access_until,
            has_payment_method: self.has_payment_method,
            ..BillingSummary::default()
        };
        summary.derive_remaining();
        summary
    }
}

/// The JSON object of an answer, unwrapping `{"data": {...}}` envelopes whose
/// top level does not carry `wanted` itself.
fn object<'a>(v: &'a Value, wanted: &[&str]) -> Option<&'a Map<String, Value>> {
    let top = v.as_object()?;
    if wanted.iter().any(|k| top.contains_key(*k)) {
        return Some(top);
    }
    top.get("data").and_then(Value::as_object).or(Some(top))
}

/// A number, or a string holding a plain decimal number.
pub(crate) fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            let plain = !t.is_empty()
                && t.bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+'));
            if plain { t.parse::<f64>().ok() } else { None }
        }
        _ => None,
    }
    .filter(|f| f.is_finite())
}

/// A dollar amount: numbers go through `f64`, decimal strings are exact.
pub(crate) fn dollars(v: &Value) -> Option<Usd> {
    match v {
        Value::String(s) => Usd::parse_dollars(s),
        other => number(other).and_then(Usd::from_dollars),
    }
}

/// A non-negative integer count (numbers, integral floats or digit strings).
pub(crate) fn count(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| *f >= 0.0 && f.fract() == 0.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Parse a billing subscription answer. At least one of the limit fields (or
/// `"object": "billing_subscription"`) must be present, so an unrelated JSON
/// answer (e.g. a web app's catch-all route) is not mistaken for one.
pub(crate) fn parse_subscription(body: &[u8]) -> Result<Subscription, String> {
    const LIMITS: [&str; 3] = ["hard_limit_usd", "soft_limit_usd", "system_hard_limit_usd"];
    let v: Value =
        serde_json::from_slice(body).map_err(|_| "billing subscription is not JSON".to_owned())?;
    let obj = object(&v, &LIMITS).ok_or("billing subscription is not a JSON object")?;
    let is_subscription = obj.get("object").and_then(Value::as_str) == Some("billing_subscription");
    if !is_subscription && !LIMITS.iter().any(|k| obj.contains_key(*k)) {
        return Err("the answer is not a billing subscription".to_owned());
    }
    let hard_limit = ["hard_limit_usd", "system_hard_limit_usd"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(dollars));
    let plan_title = match obj.get("plan") {
        Some(Value::Object(p)) => ["title", "name", "id"].iter().find_map(|k| {
            p.get(*k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        }),
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    };
    Ok(Subscription {
        hard_limit,
        soft_limit: obj.get("soft_limit_usd").and_then(dollars),
        access_until: obj
            .get("access_until")
            .and_then(parse_time)
            .filter(|t| t.as_millis() > 0),
        has_payment_method: obj.get("has_payment_method").and_then(Value::as_bool),
        plan_title,
    })
}

/// Parse a billing usage answer: `total_usage` in US cents.
pub(crate) fn parse_billing_usage(body: &[u8]) -> Result<Usd, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| "billing usage is not JSON".to_owned())?;
    let obj = object(&v, &["total_usage"]).ok_or("billing usage is not a JSON object")?;
    let cents = obj
        .get("total_usage")
        .and_then(number)
        .ok_or("the answer has no total_usage")?;
    Usd::from_cents(cents).ok_or_else(|| "total_usage is out of range".to_owned())
}

/// The current UTC calendar month as `(first day, tomorrow)`, both `YYYY-MM-DD`
/// (the end is exclusive, so today is included).
pub(crate) fn month_to_date(now: Timestamp) -> (String, String) {
    let (y, m, _) = now.utc_date();
    let first = Timestamp::from_utc_date(y, m, 1).unwrap_or(now.utc_day_start());
    let tomorrow = now
        .utc_day_start()
        .saturating_add(std::time::Duration::from_secs(86_400));
    (first.utc_date_string(), tomorrow.utc_date_string())
}

/// Epoch seconds or milliseconds (numbers or digit strings), or RFC 3339.
pub(crate) fn parse_time(v: &Value) -> Option<Timestamp> {
    match v {
        Value::Number(n) => n.as_i64().map(epoch_to_timestamp),
        Value::String(s) => {
            let t = s.trim();
            if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
                t.parse::<i64>().ok().map(epoch_to_timestamp)
            } else {
                parse_rfc3339(t)
            }
        }
        _ => None,
    }
}

/// Values above 10^11 are milliseconds (10^11 s is in the year 5138).
fn epoch_to_timestamp(n: i64) -> Timestamp {
    if n.unsigned_abs() >= 100_000_000_000 {
        Timestamp::from_millis(n)
    } else {
        Timestamp::from_millis(n.saturating_mul(1000))
    }
}

/// Minimal RFC 3339 / ISO 8601 parser: `YYYY-MM-DD`, optionally followed by
/// `T` or a space, `HH:MM[:SS[.fraction]]` and `Z` or `±HH[:]MM`. A missing
/// offset means UTC.
pub(crate) fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter().all(u8::is_ascii_digit).then(|| {
            part.iter()
                .fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0'))
        })
    };
    let year = num(0, 4)?;
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let month = num(5, 2)?;
    let day = num(8, 2)?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86_400;
    let mut ms = 0i64;
    let mut i = 10;
    if i < b.len() {
        if !matches!(b[i], b'T' | b't' | b' ') {
            return None;
        }
        let hour = num(i + 1, 2)?;
        if b.get(i + 3) != Some(&b':') {
            return None;
        }
        let minute = num(i + 4, 2)?;
        i += 6;
        let mut second = 0;
        if b.get(i) == Some(&b':') {
            second = num(i + 1, 2)?;
            i += 3;
            if b.get(i) == Some(&b'.') {
                i += 1;
                let start = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let frac = &s[start..i.min(start + 3)];
                ms = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
            }
        }
        if hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        secs += hour * 3600 + minute * 60 + second;
        match b.get(i) {
            None => {}
            Some(b'Z' | b'z') if i + 1 == b.len() => {}
            Some(sign @ (b'+' | b'-')) => {
                let oh = num(i + 1, 2)?;
                let rest = &b[i + 3..];
                let om = match rest {
                    [b':', ..] if rest.len() == 3 => num(i + 4, 2)?,
                    _ if rest.len() == 2 => num(i + 3, 2)?,
                    [] => 0,
                    _ => return None,
                };
                if oh > 23 || om > 59 {
                    return None;
                }
                let offset = oh * 3600 + om * 60;
                secs -= if *sign == b'+' { offset } else { -offset };
            }
            _ => return None,
        }
    }
    Some(Timestamp::from_millis(secs * 1000 + ms))
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        _ => 28,
    }
}

/// Howard Hinnant's `days_from_civil`: (year, month, day) -> days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rfc3339_round_trips_through_core_rendering() {
        for ms in [
            0i64,
            1_793_491_200_123,
            951_782_400_000,   // 2000-02-29
            -86_400_000,       // 1969-12-31
            4_102_444_799_999, // 2099-12-31T23:59:59.999Z
        ] {
            let t = Timestamp::from_millis(ms);
            assert_eq!(parse_rfc3339(&t.to_rfc3339()), Some(t), "{t}");
        }
    }

    #[test]
    fn rfc3339_accepts_offsets_and_partial_forms() {
        let utc = parse_rfc3339("2026-11-01T00:00:00Z").unwrap();
        assert_eq!(parse_rfc3339("2026-11-01T08:00:00+08:00"), Some(utc));
        assert_eq!(parse_rfc3339("2026-10-31T19:00:00-0500"), Some(utc));
        assert_eq!(parse_rfc3339("2026-11-01"), Some(utc));
        assert_eq!(parse_rfc3339("2026-11-01 00:00"), Some(utc));
        assert_eq!(
            parse_rfc3339("2026-11-01T00:00:00.5Z").unwrap().as_millis() - utc.as_millis(),
            500
        );
        assert_eq!(
            parse_rfc3339("2026-11-01T00:00:00.123456Z")
                .unwrap()
                .as_millis()
                - utc.as_millis(),
            123
        );
        for bad in [
            "",
            "2026-13-01",
            "2026-02-30",
            "2025-02-29T00:00:00Z",
            "2026-11-01T24:00:00Z",
            "2026-11-01X00:00",
            "2026-11-01T00:00:00Zjunk",
            "2026-11-01T00:00:00.Z",
            "26-11-01",
            "tomorrow",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn epoch_values_are_seconds_or_milliseconds() {
        assert_eq!(
            parse_time(&json!(1_793_491_200)),
            Some(Timestamp::from_millis(1_793_491_200_000))
        );
        assert_eq!(
            parse_time(&json!(1_793_491_200_000i64)),
            Some(Timestamp::from_millis(1_793_491_200_000))
        );
        assert_eq!(
            parse_time(&json!("1793491200")),
            Some(Timestamp::from_millis(1_793_491_200_000))
        );
        assert_eq!(parse_time(&json!(true)), None);
    }

    #[test]
    fn subscriptions_parse_leniently() {
        let body = json!({
            "object": "billing_subscription",
            "has_payment_method": true,
            "soft_limit_usd": "80",
            "hard_limit_usd": 100.5,
            "system_hard_limit_usd": 100.5,
            "access_until": 1_793_491_200,
            "plan": {"title": "Pro monthly", "id": "pro"},
            "extra": {"ignored": true}
        });
        let s = parse_subscription(body.to_string().as_bytes()).unwrap();
        assert_eq!(s.hard_limit, Usd::from_dollars(100.5));
        assert_eq!(s.soft_limit, Usd::from_dollars(80.0));
        assert_eq!(
            s.access_until,
            Some(Timestamp::from_millis(1_793_491_200_000))
        );
        assert_eq!(s.has_payment_method, Some(true));
        assert_eq!(s.plan_title.as_deref(), Some("Pro monthly"));

        let wrapped =
            json!({"success": true, "data": {"hard_limit_usd": "12.5", "access_until": 0}});
        let s = parse_subscription(wrapped.to_string().as_bytes()).unwrap();
        assert_eq!(s.hard_limit, Usd::parse_dollars("12.5"));
        assert_eq!(s.access_until, None, "0 means no expiry");
        assert_eq!(s.plan_title, None);

        let only_system = json!({"system_hard_limit_usd": 7});
        let s = parse_subscription(only_system.to_string().as_bytes()).unwrap();
        assert_eq!(s.hard_limit, Usd::from_dollars(7.0));

        for bad in [
            &b"<html>"[..],
            b"[]",
            br#"{"ok":true}"#,
            br#"{"data":{"x":1}}"#,
        ] {
            assert!(
                parse_subscription(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn billing_usage_is_in_cents() {
        assert_eq!(
            parse_billing_usage(br#"{"object":"list","total_usage":1234.5}"#).unwrap(),
            Usd::from_micros(12_345_000)
        );
        assert_eq!(
            parse_billing_usage(br#"{"total_usage":"250"}"#).unwrap(),
            Usd::from_dollars(2.5).unwrap()
        );
        assert_eq!(
            parse_billing_usage(br#"{"data":{"total_usage":0}}"#).unwrap(),
            Usd::ZERO
        );
        assert!(parse_billing_usage(br#"{"object":"list"}"#).is_err());
        assert!(parse_billing_usage(br#"{"total_usage":"1e5"}"#).is_err());
    }

    #[test]
    fn summaries_derive_remaining_from_limit_and_month_usage() {
        let sub = Subscription {
            hard_limit: Usd::from_dollars(20.0),
            plan_title: Some("Team".into()),
            ..Subscription::default()
        };
        let period = ("2026-11-01".to_owned(), "2026-11-02".to_owned());
        let b = sub
            .clone()
            .into_summary(Usd::from_dollars(5.25), period.clone());
        assert_eq!(b.remaining, Usd::from_dollars(14.75));
        assert_eq!(b.period_start.as_deref(), Some("2026-11-01"));
        let b = sub.into_summary(None, period);
        assert_eq!(b.remaining, None);
        assert_eq!(b.period_start, None);
        assert_eq!(b.plan_title.as_deref(), Some("Team"));
    }

    #[test]
    fn month_to_date_includes_today() {
        let now = Timestamp::from_millis(1_793_491_200_000 + 13 * 86_400_000 + 1); // 2026-11-14
        assert_eq!(
            month_to_date(now),
            ("2026-11-01".to_owned(), "2026-11-15".to_owned())
        );
        let eom = Timestamp::from_utc_date(2026, 12, 31).unwrap();
        assert_eq!(
            month_to_date(eom),
            ("2026-12-01".to_owned(), "2027-01-01".to_owned())
        );
    }

    #[test]
    fn numbers_accept_strings_but_not_garbage() {
        assert_eq!(number(&json!("12.5")), Some(12.5));
        assert_eq!(number(&json!(3)), Some(3.0));
        assert_eq!(number(&json!("1e3")), None);
        assert_eq!(number(&json!(null)), None);
        assert_eq!(count(&json!(12)), Some(12));
        assert_eq!(count(&json!(12.0)), Some(12));
        assert_eq!(count(&json!(12.5)), None);
        assert_eq!(count(&json!("7")), Some(7));
        assert_eq!(count(&json!(-1)), None);
        assert_eq!(dollars(&json!("0.000001")), Some(Usd::from_micros(1)));
    }
}
