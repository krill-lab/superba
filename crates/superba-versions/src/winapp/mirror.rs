//! store.rg-adguard.net, a third-party resolver of Microsoft Store links: its
//! form posts `type=ProductId&url=<product id>&ring=<ring>&lang=` to
//! `/api/GetFiles` and gets an HTML table back, one row per file:
//!
//! ```text
//! <tr><td><a href="http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/<id>?P1=..&P2=..&P3=..&P4=.."
//!        rel="noreferrer">OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix</a></td>
//!     <td align="center">2026-10-17 21:50:51 GMT</td>
//!     <td align="center">29f6b5fe52f278cb0a9837338820443d153dedb6</td>
//!     <td align="center">905.03 MB</td></tr>
//! ```
//!
//! Package links are temporary (signed `P1`..`P4` query, `P1` the expiry) on
//! `tlu.dl.delivery.mp.microsoft.com`; block maps (`<name>.BlockMap|<n>`, size
//! `0 B`) are on `dl.delivery.mp.microsoft.com`. Sizes are MiB-based with two
//! decimals. Only links on those CDN hosts are ever followed
//! ([`is_store_cdn_url`]); the listed SHA-1 and size are checked after the
//! download, and the package signature decides in the end.

use superba_core::{AppPackageKind, Error, InstalledPackage, MsixDependency, Result};

use super::{OPENAI_PUBLISHER_ID, PACKAGE_NAME, publisher_id, version_cmp};

/// The resolver's site.
pub const STORE_MIRROR_BASE: &str = "https://store.rg-adguard.net";

/// Path of its API.
pub const API_PATH: &str = "/api/GetFiles";

/// Ring asked for: Release Preview, the site's default (the Store serves
/// third-party apps the same in every ring).
pub const RING: &str = "RP";

/// The base domain of the Microsoft Store CDN.
const CDN_DOMAIN: &str = "dl.delivery.mp.microsoft.com";

/// The form body of a lookup (what the site's own page sends).
pub fn request_body(product_id: &str, ring: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect()
    };
    format!(
        "type=ProductId&url={}&ring={}&lang=",
        clean(product_id),
        clean(ring)
    )
}

/// A link on the Microsoft Store CDN: http or https on
/// `dl.delivery.mp.microsoft.com` or one label below it (the resolver hands
/// out `tlu.dl.delivery.mp.microsoft.com`), default port, no credentials, a
/// `/filestreamingservice/files/` path. The CDN serves plain http (its TLS
/// certificate does not name these hosts); integrity comes from the checksum
/// and the package signature.
pub fn is_store_cdn_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let Some(url::Host::Domain(host)) = u.host() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let host_ok = host == CDN_DOMAIN
        || host
            .strip_suffix(CDN_DOMAIN)
            .and_then(|p| p.strip_suffix('.'))
            .is_some_and(|label| {
                (1..=63).contains(&label.len())
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            });
    matches!(u.scheme(), "http" | "https")
        && host_ok
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && u.path().starts_with("/filestreamingservice/files/")
}

/// A size as listed (`905.03 MB`) and the byte range it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedSize {
    /// The text.
    pub text: String,
    /// Smallest size that rounds to it.
    pub min: u64,
    /// Largest size that rounds to it.
    pub max: u64,
}

impl ListedSize {
    /// Parse `<number> <B|KB|MB|GB>` (binary units, rounded to two decimals).
    pub fn parse(text: &str) -> Option<Self> {
        let t = text.trim();
        let (num, unit) = t.split_once(' ')?;
        let mult: f64 = match unit.trim().to_ascii_uppercase().as_str() {
            "B" => 1.0,
            "KB" => 1024.0,
            "MB" => 1024.0 * 1024.0,
            "GB" => 1024.0 * 1024.0 * 1024.0,
            _ => return None,
        };
        let n: f64 = num.trim().parse().ok()?;
        if !n.is_finite() || n < 0.0 {
            return None;
        }
        let half = if mult == 1.0 { 0.0 } else { 0.005 };
        Some(Self {
            text: t.to_owned(),
            min: ((n - half).max(0.0) * mult).floor() as u64,
            max: ((n + half) * mult).ceil() as u64,
        })
    }

    /// `len` rounds to this size.
    pub fn admits(&self, len: u64) -> bool {
        (self.min..=self.max).contains(&len)
    }
}

/// One row of the resolver's table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorFile {
    /// File name as listed.
    pub name: String,
    /// Link.
    pub url: String,
    /// Expiry as listed (`2026-10-17 21:50:51 GMT`).
    pub expires: Option<String>,
    /// SHA-1 (lowercase hex).
    pub sha1: Option<String>,
    /// Size.
    pub size: Option<ListedSize>,
}

fn decode_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

/// The inner HTML of each `<td>` of `row`.
fn cells(row: &str) -> Vec<&str> {
    let lower = row.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = lower[at..].find("<td") {
        let open = at + open;
        let Some(gt) = lower[open..].find('>') else {
            break;
        };
        let start = open + gt + 1;
        let end = lower[start..].find("</td").map_or(row.len(), |e| start + e);
        out.push(&row[start..end]);
        at = end;
    }
    out
}

/// The `href` and text of the first link in `cell`.
fn link(cell: &str) -> Option<(String, String)> {
    let lower = cell.to_ascii_lowercase();
    let a = lower.find("<a ")?;
    let href_at = a + lower[a..].find("href=")? + 5;
    let quote = cell[href_at..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = href_at + 1;
    let value_end = value_start + cell[value_start..].find(quote)?;
    let text_start = value_end + lower[value_end..].find('>')? + 1;
    let text_end = text_start + lower[text_start..].find("</a")?;
    Some((
        decode_entities(&cell[value_start..value_end]),
        strip_tags(&cell[text_start..text_end]),
    ))
}

/// Every file row of a resolver answer.
pub fn parse_listing(html: &str) -> Vec<MirrorFile> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = lower[at..].find("<tr") {
        let start = at + open;
        let end = lower[start..]
            .find("</tr")
            .map_or(html.len(), |e| start + e);
        let row = &html[start..end];
        at = end.max(start + 3);
        let cells = cells(row);
        let Some((url, name)) = cells.first().and_then(|c| link(c)) else {
            continue;
        };
        let text = |i: usize| {
            cells
                .get(i)
                .map(|c| strip_tags(c))
                .filter(|t| !t.is_empty())
        };
        let sha1 = text(2)
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()));
        out.push(MirrorFile {
            name,
            url,
            expires: text(1),
            sha1,
            size: text(3).and_then(|s| ListedSize::parse(&s)),
        });
    }
    out
}

/// What the resolver answered.
pub fn check_answer(status: u16, challenge_header: bool, body: &str) -> Result<Vec<MirrorFile>> {
    let lower = body.to_ascii_lowercase();
    if challenge_header
        || lower.contains("<title>just a moment")
        || lower.contains("challenges.cloudflare.com")
    {
        return Err(Error::Http {
            url: format!("{STORE_MIRROR_BASE}{API_PATH}"),
            status,
            message: "store.rg-adguard.net asked for a browser check (Cloudflare challenge) \
                      instead of answering; try again later, or use the official source or \
                      the Microsoft Store"
                .into(),
        });
    }
    if !(200..300).contains(&status) {
        return Err(Error::Http {
            url: format!("{STORE_MIRROR_BASE}{API_PATH}"),
            status,
            message: "store.rg-adguard.net did not answer the lookup".into(),
        });
    }
    let files = parse_listing(body);
    if files.is_empty() {
        let said = lower
            .find("<p")
            .map(|p| strip_tags(&body[p..body.len().min(p + 400)]))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "no files".into());
        return Err(Error::NotFound {
            what: format!("Store packages from store.rg-adguard.net ({said})"),
        });
    }
    Ok(files)
}

/// The parts of a package file name
/// (`Name_Version_Arch_ResourceId_PublisherId.ext`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageFileName {
    /// Package name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Architecture (`x64`, `arm64`, `x86`, `arm`, `neutral`).
    pub arch: String,
    /// Resource id (empty, `~` for bundles).
    pub resource_id: String,
    /// Publisher id.
    pub publisher_id: String,
    /// Kind by extension.
    pub kind: AppPackageKind,
}

/// Parse a package file name; `None` for block maps, encrypted packages
/// (`.eappx`, `.emsix`) and anything else.
pub fn parse_package_file_name(file: &str) -> Option<PackageFileName> {
    let kind = AppPackageKind::windows_from_file_name(file)?;
    let stem = file.rsplit_once('.')?.0;
    let parts: Vec<&str> = stem.split('_').collect();
    let [name, version, arch, resource_id, publisher] = parts.as_slice() else {
        return None;
    };
    let arch = arch.to_ascii_lowercase();
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        && super::official::valid_version(version)
        && matches!(arch.as_str(), "x86" | "x64" | "arm" | "arm64" | "neutral")
        && publisher.len() == 13
        && publisher
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase());
    ok.then(|| PackageFileName {
        name: (*name).to_owned(),
        version: (*version).to_owned(),
        arch,
        resource_id: (*resource_id).to_owned(),
        publisher_id: (*publisher).to_owned(),
        kind,
    })
}

/// A listed file Superba may download: a package on the CDN with a SHA-1.
fn usable<'a>(
    files: &'a [MirrorFile],
    url_ok: &'a dyn Fn(&str) -> bool,
) -> impl Iterator<Item = (&'a MirrorFile, PackageFileName)> + 'a {
    files.iter().filter_map(move |f| {
        let parsed = parse_package_file_name(&f.name)?;
        (f.sha1.is_some() && url_ok(&f.url)).then_some((f, parsed))
    })
}

/// The app package for an MSIX architecture: the newest `OpenAI.Codex` of
/// OpenAI's Store publisher, for exactly `arch`, else a neutral bundle, else
/// (on arm64, which runs x64 apps) an x64 package.
pub fn select_app(
    files: &[MirrorFile],
    arch: &str,
    url_ok: &dyn Fn(&str) -> bool,
) -> Option<(MirrorFile, PackageFileName)> {
    let rank = |p: &PackageFileName| -> Option<u8> {
        if p.arch == arch {
            Some(0)
        } else if p.arch == "neutral"
            && matches!(
                p.kind,
                AppPackageKind::MsixBundle | AppPackageKind::AppxBundle
            )
        {
            Some(1)
        } else if arch == "arm64" && p.arch == "x64" {
            Some(2)
        } else {
            None
        }
    };
    let mut best: Option<(&MirrorFile, PackageFileName, u8)> = None;
    for (f, p) in usable(files, url_ok) {
        if p.name != PACKAGE_NAME || p.publisher_id != OPENAI_PUBLISHER_ID {
            continue;
        }
        let Some(r) = rank(&p) else { continue };
        let better = match &best {
            None => true,
            Some((_, bp, br)) => match version_cmp(&p.version, &bp.version) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => r < *br,
                std::cmp::Ordering::Less => false,
            },
        };
        if better {
            best = Some((f, p, r));
        }
    }
    best.map(|(f, p, _)| (f.clone(), p))
}

/// `dep` is installed for an app of architecture `arch`.
pub fn dependency_installed(
    dep: &MsixDependency,
    arch: &str,
    installed: &[InstalledPackage],
) -> bool {
    let pid = publisher_id(&dep.publisher);
    installed.iter().any(|p| {
        p.name.eq_ignore_ascii_case(&dep.name)
            && p.publisher_id == pid
            && (p.arch.eq_ignore_ascii_case(arch) || p.arch.eq_ignore_ascii_case("neutral"))
            && version_cmp(&p.version, &dep.min_version).is_ge()
    })
}

/// Framework packages to download for `deps` of an app of architecture
/// `arch`: the newest listed package of each dependency that is not
/// installed, and the dependencies nothing was found for.
pub fn select_dependencies(
    files: &[MirrorFile],
    deps: &[MsixDependency],
    arch: &str,
    installed: &[InstalledPackage],
    url_ok: &dyn Fn(&str) -> bool,
) -> (Vec<(MirrorFile, PackageFileName)>, Vec<MsixDependency>) {
    let mut chosen = Vec::new();
    let mut unresolved = Vec::new();
    for dep in deps {
        if dependency_installed(dep, arch, installed) {
            continue;
        }
        let pid = publisher_id(&dep.publisher);
        let best = usable(files, url_ok)
            .filter(|(_, p)| {
                p.name.eq_ignore_ascii_case(&dep.name)
                    && p.publisher_id == pid
                    && (p.arch == arch || p.arch == "neutral")
                    && version_cmp(&p.version, &dep.min_version).is_ge()
            })
            .max_by(|(_, a), (_, b)| version_cmp(&a.version, &b.version));
        match best {
            Some((f, p)) => chosen.push((f.clone(), p)),
            None => unresolved.push(dep.clone()),
        }
    }
    (chosen, unresolved)
}
