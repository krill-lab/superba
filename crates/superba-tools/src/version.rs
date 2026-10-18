//! Version strings of the tools: `2.1.302`, `0.2.1-rc.2`, `v2.0.32`,
//! WorkBuddy's `5.8.3.41334564`, PyPI's `0.2.1rc2`; parsing from
//! `--version` output and ordering.

use std::cmp::Ordering;

/// A loosely parsed version: two or more numeric parts and an optional
/// pre-release (`-rc.2`, `-alpha.5`); build metadata (`+...`) is ignored.
/// Equality is precedence: `2.0` equals `2.0.0`.
#[derive(Clone, Debug)]
pub struct Version {
    /// Numeric parts (`[2, 1, 302]`).
    pub nums: Vec<u64>,
    /// Pre-release identifiers (`["rc", "2"]`); empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    /// Parse `text` (a leading `v` is accepted).
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
        let t = t.split('+').next().unwrap_or(t);
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (t, None),
        };
        let mut nums = Vec::new();
        for part in core.split('.') {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) || part.len() > 18 {
                return None;
            }
            nums.push(part.parse().ok()?);
        }
        if nums.len() < 2 {
            return None;
        }
        let pre = match pre {
            Some(p) if !p.is_empty() => {
                let ids: Vec<String> = p.split('.').map(str::to_owned).collect();
                if ids.iter().any(|i| {
                    i.is_empty() || !i.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }) {
                    return None;
                }
                ids
            }
            Some(_) => return None,
            None => Vec::new(),
        };
        Some(Self { nums, pre })
    }

    /// A pre-release.
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The first number.
    pub fn major(&self) -> u64 {
        self.nums[0]
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let nums: Vec<String> = self.nums.iter().map(u64::to_string).collect();
        f.write_str(&nums.join("."))?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

fn cmp_ident(a: &str, b: &str) -> Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.nums.len().max(other.nums.len());
        for i in 0..n {
            let a = self.nums.get(i).copied().unwrap_or(0);
            let b = other.nums.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            match cmp_ident(a, b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `a` is older than `b` (both must parse; otherwise "different" counts as
/// older when the texts differ, so an unreadable version can still be updated).
pub fn is_older(a: &str, b: &str) -> bool {
    match (Version::parse(a), Version::parse(b)) {
        (Some(x), Some(y)) => x < y,
        _ => normalize(a) != normalize(b),
    }
}

/// `a` and `b` name the same version (`v2.0.32` = `2.0.32`, `0.2.1rc2` =
/// `0.2.1-rc.2`).
pub fn same(a: &str, b: &str) -> bool {
    match (
        Version::parse(&pep440_to_semver(a)),
        Version::parse(&pep440_to_semver(b)),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => normalize(a) == normalize(b),
    }
}

fn normalize(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix(['v', 'V']).unwrap_or(v).to_owned()
}

/// The first version in a program's `--version` output: `2.1.302 (Claude
/// Code)` → `2.1.302`, `opencode v2.0.32` → `2.0.32`, `zcode 0.16.9` →
/// `0.16.9`.
pub fn from_output(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '"' | '\''))
        .filter(|w| !w.is_empty())
        .find_map(|w| {
            let w = w.trim_end_matches(['.', ':']);
            Version::parse(w).map(|v| v.to_string())
        })
}

/// A PyPI version (`0.2.1rc2`, `0.2.1a5`, `0.2.1b1`, `0.2.1.dev3`) as the npm
/// version it is published from (`0.2.1-rc.2`, `0.2.1-alpha.5`, ...); other
/// text is returned unchanged.
pub fn pep440_to_semver(v: &str) -> String {
    let v = v.trim();
    let core_end = v
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.'))
        .map_or(v.len(), |(i, _)| i);
    let (core, rest) = v.split_at(core_end);
    let core = core.trim_end_matches('.');
    if rest.is_empty() {
        return core.to_owned();
    }
    let rest = rest.trim_start_matches(['.', '-', '_']);
    for (tag, name) in [
        ("rc", "rc"),
        ("alpha", "alpha"),
        ("beta", "beta"),
        ("a", "alpha"),
        ("b", "beta"),
        ("dev", "dev"),
    ] {
        if let Some(n) = rest.strip_prefix(tag)
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            return format!("{core}-{name}.{n}");
        }
    }
    v.to_owned()
}

/// An npm version as its PyPI spelling (`0.2.1-rc.2` → `0.2.1rc2`,
/// `0.2.1-alpha.5` → `0.2.1a5`); `None` for pre-release tags PyPI has no
/// spelling for.
pub fn semver_to_pep440(v: &str) -> Option<String> {
    let parsed = Version::parse(v)?;
    let core: Vec<String> = parsed.nums.iter().map(u64::to_string).collect();
    let core = core.join(".");
    match parsed.pre.as_slice() {
        [] => Some(core),
        [tag, n] if n.bytes().all(|b| b.is_ascii_digit()) => {
            let short = match tag.as_str() {
                "rc" => "rc",
                "alpha" | "a" => "a",
                "beta" | "b" => "b",
                "dev" => return Some(format!("{core}.dev{n}")),
                _ => return None,
            };
            Some(format!("{core}{short}{n}"))
        }
        _ => None,
    }
}
