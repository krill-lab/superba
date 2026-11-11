//! `superba://` links (feature 140): one-click import from the Krill website.
//!
//! The Krill website builds a link like
//!
//! ```text
//! superba://import/v1?key=sk-...&name=My%20laptop
//! ```
//!
//! and the browser hands it to Superba (a registered URL handler). The link
//! names no site or address: it always means "this API key, for the built-in
//! Krill provider", so it can only ever configure Krill. Superba picks the
//! Krill line itself, as the setup wizard does, and shows a confirmation
//! dialog with the masked key before anything is written.
//!
//! The link carries the API key itself. It is a secret from the moment it is
//! parsed: [`ImportLink`]'s `Debug` masks the key, [`redact_link`] gives the
//! only form a log may show, and the raw text is never stored. The format is
//! specified in `docs/deeplink.md`.

use std::fmt;

use url::Url;

use crate::i18n::LocalizedText;
use crate::secret::Secret;

/// The URL scheme Superba registers.
pub const SCHEME: &str = "superba";
/// The action of an import link (the URL's host part).
pub const IMPORT_ACTION: &str = "import";
/// The only format version this Superba understands (the URL's path).
pub const IMPORT_VERSION: &str = "v1";
/// Longest link accepted, in bytes. Real links are a few hundred bytes.
pub const MAX_LINK_LEN: usize = 2048;
/// Shortest API key accepted.
pub const MIN_KEY_LEN: usize = 8;
/// Longest API key accepted.
pub const MAX_KEY_LEN: usize = 512;
/// Longest display name accepted, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// A parsed, validated `superba://import/v1` link. `Debug` never shows the key.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportLink {
    /// The API key (`key=`).
    pub key: Secret,
    /// The provider's display name (`name=`), trimmed.
    pub name: Option<String>,
}

impl fmt::Debug for ImportLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportLink")
            .field("key", &self.key)
            .field("name", &self.name)
            .finish()
    }
}

/// Why a link was refused. [`LinkError::text`] is what the user sees. Parts
/// of the link a variant holds are short plain words (anything else is kept as
/// `…`), so no variant can hold the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// Longer than [`MAX_LINK_LEN`].
    TooLong,
    /// Not a `superba:` URL, or not a well-formed one.
    Malformed,
    /// A `superba://` link for something other than `import`.
    UnknownAction(String),
    /// `import/<version>` with a version this Superba does not know.
    UnsupportedVersion(String),
    /// A parameter that `v1` does not define.
    UnknownParameter(String),
    /// A parameter given twice.
    DuplicateParameter(String),
    /// A required parameter is missing.
    MissingParameter(&'static str),
    /// The key is empty, too short or too long, or has characters an API key
    /// cannot have.
    InvalidKey,
    /// The display name is empty, too long or has control characters.
    InvalidName,
}

impl LinkError {
    /// What the user is told.
    pub fn text(&self) -> LocalizedText {
        let t = LocalizedText::new;
        match self {
            LinkError::TooLong => t(
                "import.link.too_long",
                "The import link is too long; it was not opened.",
            ),
            LinkError::Malformed => t(
                "import.link.malformed",
                "This is not a valid Superba import link.",
            ),
            LinkError::UnknownAction(a) => t(
                "import.link.unknown_action",
                "Superba does not know the link action \"{action}\".",
            )
            .arg("action", a),
            LinkError::UnsupportedVersion(v) => t(
                "import.link.unsupported_version",
                "The import link has format \"{version}\", which this Superba does not support. Update Superba and try again.",
            )
            .arg("version", v),
            LinkError::UnknownParameter(p) => t(
                "import.link.unknown_parameter",
                "The import link has an unknown parameter \"{name}\"; it was not opened.",
            )
            .arg("name", p),
            LinkError::DuplicateParameter(p) => t(
                "import.link.duplicate_parameter",
                "The import link gives \"{name}\" more than once; it was not opened.",
            )
            .arg("name", p),
            LinkError::MissingParameter(p) => t(
                "import.link.missing_parameter",
                "The import link has no \"{name}\".",
            )
            .arg("name", *p),
            LinkError::InvalidKey => t(
                "import.link.invalid_key",
                "The API key in the import link is not valid.",
            ),
            LinkError::InvalidName => t(
                "import.link.invalid_name",
                "The provider name in the import link is not valid.",
            ),
        }
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text().render_fallback())
    }
}

impl std::error::Error for LinkError {}

/// An untrusted part of a link (an action, version or parameter name) as a
/// message or log may show it: short plain words only. Anything longer could
/// be (part of) a key someone put in the wrong place, so it becomes `…`.
fn short(s: &str) -> String {
    if s.len() <= 12
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
    {
        s.to_owned()
    } else {
        "…".to_owned()
    }
}

/// True when `arg` is meant as a `superba:` link (a command line argument or
/// a request). Only the scheme is looked at.
pub fn is_link(arg: &str) -> bool {
    arg.get(..SCHEME.len() + 1)
        .is_some_and(|p| p.eq_ignore_ascii_case("superba:"))
}

/// The form of a link a log or a message may show: scheme, action and version,
/// and the names of its parameters, never their values (the key is one).
pub fn redact_link(raw: &str) -> String {
    let Ok(url) = Url::parse(raw.get(..MAX_LINK_LEN).unwrap_or(raw)) else {
        return "superba:[unparsable]".into();
    };
    let names: Vec<String> = url
        .query_pairs()
        .map(|(k, _)| format!("{}=…", short(&k)))
        .collect();
    let mut out = format!(
        "{}://{}/{}",
        url.scheme(),
        short(url.host_str().unwrap_or("")),
        short(url.path().strip_prefix('/').unwrap_or(url.path()))
    );
    if !names.is_empty() {
        out.push('?');
        out.push_str(&names.join("&"));
    }
    out
}

/// True for a key a link may carry: [`MIN_KEY_LEN`] to [`MAX_KEY_LEN`]
/// visible ASCII characters without quotes or backslashes (what an HTTP
/// header, `auth.json` and every harness file take; the provider form refuses
/// the same characters, see [`crate::harness::check_key_text`]).
pub fn valid_key(key: &str) -> bool {
    (MIN_KEY_LEN..=MAX_KEY_LEN).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'\''))
}

impl ImportLink {
    /// Parse and validate a link. Strict: one format version (`v1`), only its
    /// parameters (`key`, `name`), each at most once; no user info,
    /// port or fragment.
    pub fn parse(raw: &str) -> Result<Self, LinkError> {
        if raw.len() > MAX_LINK_LEN {
            return Err(LinkError::TooLong);
        }
        let raw = raw.trim();
        if !is_link(raw) {
            return Err(LinkError::Malformed);
        }
        let url = Url::parse(raw).map_err(|_| LinkError::Malformed)?;
        if url.scheme() != SCHEME
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.fragment().is_some()
        {
            return Err(LinkError::Malformed);
        }
        let action = url.host_str().ok_or(LinkError::Malformed)?;
        if !action.eq_ignore_ascii_case(IMPORT_ACTION) {
            return Err(LinkError::UnknownAction(short(action)));
        }
        let version = url.path().strip_prefix('/').unwrap_or(url.path());
        if version != IMPORT_VERSION {
            return Err(LinkError::UnsupportedVersion(short(version)));
        }
        let mut key: Option<String> = None;
        let mut name: Option<String> = None;
        for (k, v) in url.query_pairs() {
            let slot = match k.as_ref() {
                "key" => &mut key,
                "name" => &mut name,
                other => return Err(LinkError::UnknownParameter(short(other))),
            };
            if slot.is_some() {
                return Err(LinkError::DuplicateParameter(short(&k)));
            }
            *slot = Some(v.into_owned());
        }
        let key = key.ok_or(LinkError::MissingParameter("key"))?;
        let key = key.trim();
        if !valid_key(key) {
            return Err(LinkError::InvalidKey);
        }
        let name = match name {
            None => None,
            Some(n) => {
                let n = n.trim();
                if n.is_empty()
                    || n.chars().count() > MAX_NAME_CHARS
                    || n.chars().any(char::is_control)
                {
                    return Err(LinkError::InvalidName);
                }
                Some(n.to_owned())
            }
        };
        Ok(Self {
            key: Secret::new(key),
            name,
        })
    }

    /// The canonical text of this link (the single-instance hand-over sends it;
    /// it holds the key, so it is never logged).
    pub fn to_url(&self) -> String {
        let mut url = Url::parse("superba://import/v1").expect("static URL is valid");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("key", self.key.expose());
            if let Some(n) = &self.name {
                q.append_pair("name", n);
            }
        }
        url.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-krill-0123456789abcdef";

    fn link(q: &str) -> String {
        format!("superba://import/v1?{q}")
    }

    fn parse(q: &str) -> Result<ImportLink, LinkError> {
        ImportLink::parse(&link(q))
    }

    #[test]
    fn parses_key_and_optional_name() {
        let l = parse(&format!("key={KEY}&name=My%20laptop")).unwrap();
        assert_eq!(l.key.expose(), KEY);
        assert_eq!(l.name.as_deref(), Some("My laptop"));
        let l = parse(&format!("key={KEY}")).unwrap();
        assert_eq!(l.name, None);
        // `+` is a space in a query, names are trimmed, any order.
        let l = parse(&format!("name=+Work+box+&key={KEY}")).unwrap();
        assert_eq!(l.name.as_deref(), Some("Work box"));
        let l = parse("key=sk-%41bc_-.~%2B%2F%3D12").unwrap();
        assert_eq!(l.key.expose(), "sk-Abc_-.~+/=12");
        let l = parse(&format!("key={KEY}&name=%E6%88%91%E7%9A%84%20Krill")).unwrap();
        assert_eq!(l.name.as_deref(), Some("我的 Krill"));
    }

    #[test]
    fn scheme_and_action_are_case_insensitive_and_round_trip() {
        let l = ImportLink::parse(&format!("SUPERBA://IMPORT/v1?key={KEY}")).unwrap();
        assert_eq!(ImportLink::parse(&l.to_url()).unwrap(), l);
        let named = parse(&format!("key={KEY}&name=a%26b%3Dc+d")).unwrap();
        assert_eq!(ImportLink::parse(&named.to_url()).unwrap(), named);
        let odd = parse("key=sk-a%2Bb%2Fc%3D%3D%26x").unwrap();
        assert_eq!(ImportLink::parse(&odd.to_url()).unwrap(), odd);
    }

    #[test]
    fn rejects_malformed_unknown_and_oversized_links() {
        let cases: Vec<(String, LinkError)> = vec![
            (format!("https://import/v1?key={KEY}"), LinkError::Malformed),
            ("superba:".into(), LinkError::Malformed),
            ("superba://".into(), LinkError::Malformed),
            (
                format!("superbax://import/v1?key={KEY}"),
                LinkError::Malformed,
            ),
            (format!("superba:import/v1?key={KEY}"), LinkError::Malformed),
            (
                format!("superba://user@import/v1?key={KEY}"),
                LinkError::Malformed,
            ),
            (
                format!("superba://import:8080/v1?key={KEY}"),
                LinkError::Malformed,
            ),
            (
                format!("superba://import/v1?key={KEY}#x"),
                LinkError::Malformed,
            ),
            (
                format!("superba://export/v1?key={KEY}"),
                LinkError::UnknownAction("export".into()),
            ),
            (
                format!("superba://import/v2?key={KEY}"),
                LinkError::UnsupportedVersion("v2".into()),
            ),
            (
                format!("superba://import/v1/?key={KEY}"),
                LinkError::UnsupportedVersion("v1/".into()),
            ),
            (
                format!("superba://import?key={KEY}"),
                LinkError::UnsupportedVersion("".into()),
            ),
            (
                link(&format!("key={KEY}&line=overseas")),
                LinkError::UnknownParameter("line".into()),
            ),
            (
                link(&format!("key={KEY}&code=1")),
                LinkError::UnknownParameter("code".into()),
            ),
            (
                link(&format!("key={KEY}&Key={KEY}")),
                LinkError::UnknownParameter("Key".into()),
            ),
            (
                link(&format!("key={KEY}&key={KEY}")),
                LinkError::DuplicateParameter("key".into()),
            ),
            (
                link(&format!("key={KEY}&name=a&name=b")),
                LinkError::DuplicateParameter("name".into()),
            ),
            (link("name=x"), LinkError::MissingParameter("key")),
            (
                "superba://import/v1".into(),
                LinkError::MissingParameter("key"),
            ),
            (link("key="), LinkError::InvalidKey),
            (link("key=short"), LinkError::InvalidKey),
            (link("key=sk-has%20space-123"), LinkError::InvalidKey),
            (link("key=sk-quote%22-12345"), LinkError::InvalidKey),
            (link("key=sk-back%5C-12345"), LinkError::InvalidKey),
            (
                link("key=sk-%E4%BD%A0%E5%A5%BD-1234"),
                LinkError::InvalidKey,
            ),
            (link("key=sk-nl%0A-123456"), LinkError::InvalidKey),
            (
                link(&format!("key={}", "k".repeat(MAX_KEY_LEN + 1))),
                LinkError::InvalidKey,
            ),
            (link(&format!("key={KEY}&name=")), LinkError::InvalidName),
            (link(&format!("key={KEY}&name=+++")), LinkError::InvalidName),
            (
                link(&format!("key={KEY}&name=a%0Ab")),
                LinkError::InvalidName,
            ),
            (
                link(&format!(
                    "key={KEY}&name={}",
                    "n".repeat(MAX_NAME_CHARS + 1)
                )),
                LinkError::InvalidName,
            ),
            (
                link(&format!("key={}", "k".repeat(MAX_LINK_LEN))),
                LinkError::TooLong,
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(ImportLink::parse(&raw), Err(want), "{raw}");
        }
        // The longest key still fits a link.
        assert!(parse(&format!("key={}", "k".repeat(MAX_KEY_LEN))).is_ok());
    }

    #[test]
    fn errors_debug_and_redaction_never_show_the_key() {
        let l = parse(&format!("key={KEY}&name=n")).unwrap();
        let debug = format!("{l:?}");
        assert!(!debug.contains(KEY), "{debug}");
        assert!(debug.contains("cdef"));
        for raw in [
            link(&format!("key={KEY}&x={KEY}")),
            link(&format!("{KEY}={KEY}")),
            format!("superba://{KEY}/v1?key={KEY}"),
            format!("superba://import/{KEY}?key={KEY}"),
            link(&format!("key={KEY}&key={KEY}")),
        ] {
            let err = ImportLink::parse(&raw).unwrap_err();
            assert!(!err.to_string().contains(KEY), "{err}");
            assert!(!format!("{err:?}").contains(KEY), "{err:?}");
            assert!(!redact_link(&raw).contains(KEY), "{}", redact_link(&raw));
        }
        assert_eq!(
            redact_link(&link(&format!("key={KEY}&name=n"))),
            "superba://import/v1?key=…&name=…"
        );
        assert!(!redact_link("superba:%%%sk-12345678").contains("sk-"));
        assert_eq!(redact_link("not a url"), "superba:[unparsable]");
    }

    #[test]
    fn is_link_looks_at_the_scheme_only() {
        assert!(is_link("superba://import/v1"));
        assert!(is_link("Superba:x"));
        assert!(!is_link("superb:x"));
        assert!(!is_link("--page"));
        assert!(!is_link("超"));
    }
}
