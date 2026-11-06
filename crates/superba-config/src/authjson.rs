//! `auth.json` forms (plan 9.4): shape detection without exposing secrets, the PAT
//! and API-key forms Superba writes, ChatGPT login identity / age, and validation
//! (never write an empty or non-object file: Codex would fall back to the login page).

use serde_json::{Map, Value};
use superba_core::codex::auth_fields as f;
use superba_core::{AuthMode, AuthShape, Error, Result, Secret, SecretBytes, Timestamp};
use zeroize::Zeroize;

use crate::fsx::short_hash;

/// Parse auth.json bytes as a JSON object.
fn parse_object(bytes: &[u8]) -> std::result::Result<Map<String, Value>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("empty file".into());
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(other) => Err(format!("top-level JSON {} is not an object", kind(&other))),
        Err(e) => Err(format!("invalid JSON: {e}")),
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn non_empty_str<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn last4(s: &str) -> String {
    Secret::new(s).last4()
}

/// Describe auth.json (`None` = file absent) without revealing secrets.
///
/// Precedence: a `tokens` object means a ChatGPT login; then a non-empty
/// `personal_access_token` (PAT); then a non-empty `OPENAI_API_KEY` (API key);
/// anything else is `Unknown` and must never be overwritten silently.
pub fn shape(bytes: Option<&[u8]>) -> AuthShape {
    let Some(bytes) = bytes else {
        return AuthShape::Missing;
    };
    let m = match parse_object(bytes) {
        Ok(m) => m,
        Err(detail) => return AuthShape::Unknown { detail },
    };
    if m.get(f::TOKENS).is_some_and(Value::is_object) {
        let info = login_info_from(&m);
        return AuthShape::ChatGpt {
            email: info.email,
            account_id: info.account_id,
        };
    }
    if let Some(k) = non_empty_str(&m, f::PERSONAL_ACCESS_TOKEN) {
        return AuthShape::Pat {
            key_last4: last4(k),
        };
    }
    if let Some(k) = non_empty_str(&m, f::OPENAI_API_KEY) {
        return AuthShape::ApiKey {
            key_last4: last4(k),
        };
    }
    let mut fields: Vec<&str> = m.keys().map(String::as_str).collect();
    fields.truncate(8);
    AuthShape::Unknown {
        detail: if fields.is_empty() {
            "no credentials".into()
        } else {
            format!("no credentials (fields: {})", fields.join(", "))
        },
    }
}

/// The token credential in auth.json (PAT or API key), if any.
pub fn token_credentials(bytes: &[u8]) -> Option<(AuthMode, Secret)> {
    let m = parse_object(bytes).ok()?;
    if m.get(f::TOKENS).is_some_and(Value::is_object) {
        return None;
    }
    if let Some(k) = non_empty_str(&m, f::PERSONAL_ACCESS_TOKEN) {
        return Some((AuthMode::Pat, Secret::new(k)));
    }
    non_empty_str(&m, f::OPENAI_API_KEY).map(|k| (AuthMode::ApiKey, Secret::new(k)))
}

/// A digest of what Codex authenticates with in `auth.json` (`None` = file
/// absent): the PAT, the API key, `auth_mode` and the ChatGPT login identity.
/// Two files with the same digest hold the same credential even when another
/// writer reordered or reformatted them or added unrelated fields; any other
/// difference is a credential change (GH#5: AI Toolbox merges its key into the
/// file, cc-switch deletes it, `codex login` replaces it). No secret is kept.
pub fn credential_digest(bytes: Option<&[u8]>) -> Option<String> {
    let bytes = bytes?;
    let Ok(m) = parse_object(bytes) else {
        // Unreadable: its exact bytes are the credential.
        return Some(crate::fsx::sha256_hex(bytes));
    };
    let field = |k: &str| non_empty_str(&m, k).unwrap_or("");
    let login = if m.get(f::TOKENS).is_some_and(Value::is_object) {
        login_info_from(&m).identity
    } else {
        String::new()
    };
    let mut joined = format!(
        "pat={}\u{0}key={}\u{0}mode={}\u{0}login={}",
        field(f::PERSONAL_ACCESS_TOKEN),
        field(f::OPENAI_API_KEY),
        field(f::AUTH_MODE),
        login
    );
    let digest = crate::fsx::sha256_hex(joined.as_bytes());
    joined.zeroize();
    Some(digest)
}

/// The shape a reader should be shown for a changed `auth.json`: like
/// [`shape`], except that `auth_mode = "apikey"` with an `OPENAI_API_KEY` is
/// an API key even when a PAT another writer left behind is still in the file
/// (AI Toolbox merges its key into Superba's PAT form).
pub fn drift_shape(bytes: Option<&[u8]>) -> AuthShape {
    let shape = shape(bytes);
    if let (AuthShape::Pat { .. }, Some(m)) = (&shape, bytes.and_then(|b| parse_object(b).ok()))
        && non_empty_str(&m, f::AUTH_MODE) == Some(f::AUTH_MODE_APIKEY)
        && let Some(k) = non_empty_str(&m, f::OPENAI_API_KEY)
    {
        return AuthShape::ApiKey {
            key_last4: last4(k),
        };
    }
    shape
}

/// Refuse to write anything that is not a non-empty JSON object.
pub fn validate(bytes: &[u8]) -> Result<()> {
    parse_object(bytes)
        .map(|_| ())
        .map_err(|e| Error::parse("auth.json", e))
}

fn check_key(key: &Secret) -> Result<()> {
    if key.is_empty() {
        return Err(Error::invalid("the key is empty"));
    }
    if key.expose().chars().any(char::is_control) {
        return Err(Error::invalid("the key contains control characters"));
    }
    Ok(())
}

/// Build `{ "<a>": K, "<b>": K, ... }` pretty-printed like Codex, without leaving
/// copies of the key in non-zeroized buffers.
fn render_fields(fields: &[(&str, &str)]) -> SecretBytes {
    let mut out = String::from("{\n");
    for (i, (name, value)) in fields.iter().enumerate() {
        let mut quoted = superba_core::codex::js_string(value);
        out.push_str("  ");
        out.push_str(&superba_core::codex::js_string(name));
        out.push_str(": ");
        out.push_str(&quoted);
        quoted.zeroize();
        if i + 1 < fields.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push('}');
    let bytes = SecretBytes::new(out.as_bytes().to_vec());
    out.zeroize();
    bytes
}

/// PAT form: `{"personal_access_token":K,"OPENAI_API_KEY":K}`.
pub fn render_pat(key: &Secret) -> Result<SecretBytes> {
    check_key(key)?;
    Ok(render_fields(&[
        (f::PERSONAL_ACCESS_TOKEN, key.expose()),
        (f::OPENAI_API_KEY, key.expose()),
    ]))
}

/// API-key form: `{"auth_mode":"apikey","OPENAI_API_KEY":K}`.
pub fn render_apikey(key: &Secret) -> Result<SecretBytes> {
    check_key(key)?;
    Ok(render_fields(&[
        (f::AUTH_MODE, f::AUTH_MODE_APIKEY),
        (f::OPENAI_API_KEY, key.expose()),
    ]))
}

/// The auth.json Superba writes for a token mode.
pub fn render_token(mode: AuthMode, key: &Secret) -> Result<SecretBytes> {
    if mode.writes_chatgpt_base_url() && mode.needs_authapi_env() {
        render_pat(key)
    } else if mode.uses_key() {
        render_apikey(key)
    } else {
        Err(Error::Unsupported {
            what: format!("auth.json form for {mode:?}"),
        })
    }
}

/// What Superba knows about a ChatGPT login (for stash keying and age warnings).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginInfo {
    /// Stable identity hash (account id + user / email), used as stash key.
    pub identity: String,
    /// Email from the id_token.
    pub email: Option<String>,
    /// ChatGPT account id.
    pub account_id: Option<String>,
    /// Subscription claim, used only as a display label, never to infer limits.
    pub plan_type: Option<String>,
    /// `last_refresh`.
    pub last_refresh: Option<Timestamp>,
    /// id_token `exp`.
    pub id_token_exp: Option<Timestamp>,
}

/// Login details if `bytes` is a ChatGPT login (`tokens` object present).
pub fn login_info(bytes: &[u8]) -> Option<LoginInfo> {
    let m = parse_object(bytes).ok()?;
    m.get(f::TOKENS)
        .is_some_and(Value::is_object)
        .then(|| login_info_from(&m))
}

fn login_info_from(m: &Map<String, Value>) -> LoginInfo {
    let tokens = m.get(f::TOKENS).and_then(Value::as_object);
    let claims = tokens
        .and_then(|t| t.get("id_token"))
        .and_then(|v| match v {
            Value::String(jwt) => jwt_claims(jwt),
            // Some Codex versions store the decoded token info as an object.
            Value::Object(o) => Some(o.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let auth_claims = claims
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    let account_id = tokens
        .and_then(|t| t.get("account_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            auth_claims
                .and_then(|a| a.get("chatgpt_account_id"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    let user = auth_claims
        .and_then(|a| a.get("chatgpt_user_id").or_else(|| a.get("user_id")))
        .and_then(Value::as_str)
        .or_else(|| claims.get("sub").and_then(Value::as_str))
        .map(str::to_owned);
    let id_token_exp = claims
        .get("exp")
        .and_then(Value::as_i64)
        .map(|s| Timestamp::from_millis(s.saturating_mul(1000)));
    let last_refresh = m
        .get(f::LAST_REFRESH)
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    let identity_src = format!(
        "{}|{}|{}",
        account_id.as_deref().unwrap_or(""),
        user.as_deref().unwrap_or(""),
        email.as_deref().unwrap_or("")
    );
    LoginInfo {
        identity: if identity_src == "||" {
            "unknown".into()
        } else {
            short_hash(identity_src.as_bytes())
        },
        email,
        account_id,
        plan_type: auth_claims
            .and_then(|a| a.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        last_refresh,
        id_token_exp,
    }
}

/// Decode the (unverified) claims of a JWT. Only used to label and age logins.
fn jwt_claims(jwt: &str) -> Option<Map<String, Value>> {
    use base64::Engine as _;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Parse RFC 3339 timestamps such as `2026-10-16T08:30:00Z`,
/// `2026-10-16T08:30:00.123456789Z` or `2026-10-16T16:30:00+08:00`.
pub fn parse_rfc3339(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut millis = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        let ms: String = digits.chars().chain("000".chars()).take(3).collect();
        millis = ms.parse().ok()?;
        rest = &frac[digits.len()..];
    }
    let offset_secs = match rest {
        "Z" | "z" => 0,
        o if o.len() == 6 && matches!(&o[..1], "+" | "-") && &o[3..4] == ":" => {
            let h: i64 = o[1..3].parse().ok()?;
            let m: i64 = o[4..6].parse().ok()?;
            let v = h * 3600 + m * 60;
            if o.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs;
    Some(Timestamp::from_millis(secs * 1000 + millis))
}

/// Howard Hinnant's `days_from_civil`.
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
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(
            parse_rfc3339("1970-01-01T00:00:00Z"),
            Some(Timestamp::from_millis(0))
        );
        let t = parse_rfc3339("2026-10-16T08:30:00.123456Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-16T08:30:00.123Z");
        let t2 = parse_rfc3339("2026-10-16T16:30:00.123+08:00").unwrap();
        assert_eq!(t, t2);
        assert_eq!(parse_rfc3339("2026-10-16"), None);
        assert_eq!(parse_rfc3339("2026-13-16T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-10-16T00:00:00"), None);
    }

    #[test]
    fn days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }
}
