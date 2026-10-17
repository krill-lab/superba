//! Parsers for the public desktop app indexes: APT `Packages`, RPM
//! `primary.xml(.gz)`, Sparkle appcasts and the Windows Store JSON. Each returns
//! the app builds it describes with their packages; nothing here touches the
//! network (the workflow downloads the files).

use std::io::Read as _;

use anyhow::{Context, Result, bail};
use quick_xml::events::Event;
use superba_core::{AppOs, AppPackage, AppPackageKind, Timestamp};

/// Base URL of the desktop app's package hosting.
pub const APP_BASE: &str = "https://persistent.oaistatic.com/codex-app-prod";
/// Linux package name.
pub const APP_PACKAGE: &str = "chatgpt";

/// One app build seen in an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub version: String,
    pub date: Option<Timestamp>,
    pub package: Option<AppPackage>,
    pub store_product_id: Option<String>,
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// APT `Packages` (`dists/stable/main/binary-<arch>/Packages`).
pub fn apt_packages(text: &str) -> Result<Vec<Seen>> {
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut fields = std::collections::BTreeMap::new();
        for line in stanza.lines() {
            if line.starts_with([' ', '\t']) {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                fields.insert(k.trim().to_owned(), v.trim().to_owned());
            }
        }
        if fields.get("Package").map(String::as_str) != Some(APP_PACKAGE) {
            continue;
        }
        let get = |k: &str| {
            fields
                .get(k)
                .cloned()
                .with_context(|| format!("Packages: no {k}"))
        };
        let (version, arch, filename) = (get("Version")?, get("Architecture")?, get("Filename")?);
        let sha = get("SHA256")?.to_ascii_lowercase();
        if !is_sha256(&sha) || filename.contains("..") || filename.starts_with('/') {
            bail!("Packages: bad entry for {version}");
        }
        let size = get("Size")?.parse().context("Packages: Size")?;
        out.push(Seen {
            version,
            date: None,
            package: Some(AppPackage {
                os: AppOs::Linux,
                kind: AppPackageKind::Deb,
                arch,
                url: format!("{APP_BASE}/linux/deb/{filename}"),
                sha256: Some(sha),
                size: Some(size),
                signature: None,
            }),
            store_product_id: None,
        });
    }
    Ok(out)
}

fn attr(e: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// RPM `repodata/<..>-primary.xml.gz` (or uncompressed) of `<arch>`.
pub fn rpm_primary(bytes: &[u8], arch_dir: &str) -> Result<Vec<Seen>> {
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut s = String::new();
        flate2::read::GzDecoder::new(bytes)
            .take(64 * 1024 * 1024)
            .read_to_string(&mut s)
            .context("primary.xml.gz")?;
        s
    } else {
        String::from_utf8(bytes.to_vec()).context("primary.xml is not UTF-8")?
    };
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut out = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    #[derive(Default)]
    struct Pkg {
        name: String,
        arch: String,
        ver: String,
        sha: String,
        sha_type: String,
        size: Option<u64>,
        href: String,
        time: Option<i64>,
    }
    let mut cur: Option<Pkg> = None;
    loop {
        match reader.read_event().context("primary.xml")? {
            Event::Start(e) => {
                let n = e.name().as_ref().to_vec();
                if n == b"package" {
                    cur = Some(Pkg::default());
                }
                if let Some(p) = cur.as_mut()
                    && n == b"checksum"
                {
                    p.sha_type = attr(&e, b"type").unwrap_or_default();
                }
                path.push(n);
            }
            Event::Empty(e) => {
                if let Some(p) = cur.as_mut() {
                    match e.name().as_ref() {
                        b"version" => p.ver = attr(&e, b"ver").unwrap_or_default(),
                        b"size" => p.size = attr(&e, b"package").and_then(|s| s.parse().ok()),
                        b"location" => p.href = attr(&e, b"href").unwrap_or_default(),
                        b"time" => p.time = attr(&e, b"file").and_then(|s| s.parse().ok()),
                        _ => {}
                    }
                }
            }
            Event::Text(t) => {
                if let (Some(p), Some(last)) = (cur.as_mut(), path.last()) {
                    let s = t.decode().unwrap_or_default().trim().to_owned();
                    match last.as_slice() {
                        b"name" if path.len() == 3 => p.name = s,
                        b"arch" if path.len() == 3 => p.arch = s,
                        b"checksum" => p.sha = s,
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                path.pop();
                if e.name().as_ref() == b"package"
                    && let Some(p) = cur.take()
                    && p.name == APP_PACKAGE
                {
                    let sha = p.sha.to_ascii_lowercase();
                    if p.sha_type != "sha256"
                        || !is_sha256(&sha)
                        || p.href.contains("..")
                        || p.href.starts_with('/')
                    {
                        bail!("primary.xml: bad entry for {}", p.ver);
                    }
                    out.push(Seen {
                        version: p.ver,
                        date: p.time.map(Timestamp::from_unix_secs),
                        package: Some(AppPackage {
                            os: AppOs::Linux,
                            kind: AppPackageKind::Rpm,
                            arch: p.arch,
                            url: format!("{APP_BASE}/linux/rpm/{arch_dir}/{}", p.href),
                            sha256: Some(sha),
                            size: p.size,
                            signature: None,
                        }),
                        store_product_id: None,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// RFC 2822 date of an appcast (`Fri, 16 Oct 2026 22:31:52 +0000`).
pub fn rfc2822(s: &str) -> Option<Timestamp> {
    let s = s.trim();
    let s = s.split_once(", ").map_or(s, |(_, r)| r);
    let mut it = s.split_whitespace();
    let day: u32 = it.next()?.parse().ok()?;
    let month = match it.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = it.next()?.parse().ok()?;
    let mut hms = it.next()?.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, sec) = (
        hms.next()??,
        hms.next()??,
        hms.next().flatten().unwrap_or(0),
    );
    let zone = it.next().unwrap_or("+0000");
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let digits = zone.trim_start_matches(['+', '-']);
    let off = if digits.len() == 4 {
        let zh: i64 = digits[..2].parse().ok()?;
        let zm: i64 = digits[2..].parse().ok()?;
        sign * (zh * 3600 + zm * 60)
    } else {
        0
    };
    let start = Timestamp::from_utc_date(year, month, day)?;
    Some(Timestamp::from_unix_secs(
        start.as_unix_secs() + h * 3600 + m * 60 + sec - off,
    ))
}

/// A Sparkle appcast (macOS): every item's full package (deltas are ignored).
pub fn appcast(text: &str) -> Result<Vec<Seen>> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut out = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    #[derive(Default)]
    struct Item {
        version: String,
        date: String,
        hw: String,
        url: String,
        length: Option<u64>,
        sig: Option<String>,
    }
    let mut cur: Option<Item> = None;
    let mut in_deltas = false;
    let on_enclosure =
        |e: &quick_xml::events::BytesStart<'_>, cur: &mut Option<Item>, deltas: bool| {
            if let Some(i) = cur.as_mut()
                && !deltas
                && e.name().as_ref() == b"enclosure"
            {
                i.url = attr(e, b"url").unwrap_or_default();
                i.length = attr(e, b"length").and_then(|s| s.parse().ok());
                i.sig = attr(e, b"sparkle:edSignature");
            }
        };
    loop {
        match reader.read_event().context("appcast")? {
            Event::Start(e) => {
                let n = e.name().as_ref().to_vec();
                match n.as_slice() {
                    b"item" => cur = Some(Item::default()),
                    b"sparkle:deltas" => in_deltas = true,
                    _ => on_enclosure(&e, &mut cur, in_deltas),
                }
                path.push(n);
            }
            Event::Empty(e) => on_enclosure(&e, &mut cur, in_deltas),
            Event::Text(t) => {
                if let (Some(i), Some(last)) = (cur.as_mut(), path.last()) {
                    let s = t.decode().unwrap_or_default().trim().to_owned();
                    match last.as_slice() {
                        b"sparkle:shortVersionString" => i.version = s,
                        b"pubDate" => i.date = s,
                        b"sparkle:hardwareRequirements" => i.hw = s,
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                path.pop();
                match e.name().as_ref() {
                    b"sparkle:deltas" => in_deltas = false,
                    b"item" => {
                        if let Some(i) = cur.take()
                            && !i.version.is_empty()
                            && i.url.starts_with("https://")
                        {
                            let arch = if i.hw.contains("arm64") || i.url.contains("darwin-arm64") {
                                "arm64"
                            } else {
                                "x64"
                            };
                            out.push(Seen {
                                version: i.version,
                                date: rfc2822(&i.date),
                                package: Some(AppPackage {
                                    os: AppOs::Macos,
                                    kind: AppPackageKind::MacZip,
                                    arch: arch.into(),
                                    url: i.url,
                                    sha256: None,
                                    size: i.length,
                                    signature: i.sig,
                                }),
                                store_product_id: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// `windows-store-update.json`: the Store's release version and product id.
pub fn windows_store(text: &str) -> Result<Vec<Seen>> {
    let v: serde_json::Value = serde_json::from_str(text).context("windows-store-update.json")?;
    let version = v["releaseVersion"]
        .as_str()
        .context("windows-store-update.json: no releaseVersion")?;
    let product = v["storeProductId"].as_str().map(str::to_owned);
    if product
        .as_deref()
        .is_some_and(|p| !p.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        bail!("windows-store-update.json: unexpected storeProductId");
    }
    Ok(vec![Seen {
        version: version.to_owned(),
        date: None,
        package: None,
        store_product_id: product,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2822_dates() {
        let t = rfc2822("Fri, 16 Oct 2026 22:31:52 +0000").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-16T22:31:52.000Z");
        let t = rfc2822("16 Oct 2026 22:31:52 +0200").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-16T20:31:52.000Z");
        assert!(rfc2822("garbage").is_none());
    }
}
