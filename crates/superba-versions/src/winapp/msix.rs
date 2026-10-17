//! Read the manifest of an MSIX / APPX package or bundle without extracting
//! it: the zip central directory (ZIP64 included: makeappx always writes the
//! ZIP64 records) and one bounded entry, `AppxManifest.xml`, or for a bundle
//! `AppxMetadata/AppxBundleManifest.xml` and then the manifest of the inner
//! package for the machine's architecture (inner packages are stored, so they
//! are read in place).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use superba_core::{Error, MsixDependency, MsixIdentity, Result};

use super::publisher_id;
use crate::xml;

const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const CEN_SIG: u32 = 0x0201_4b50;
const LOC_SIG: u32 = 0x0403_4b50;

/// Largest central directory read (the 26.1013 msix has 3651 entries, ~300 KB).
const MAX_CENTRAL_DIRECTORY: u64 = 64 * 1024 * 1024;
/// Largest manifest read.
const MAX_MANIFEST: u64 = 4 * 1024 * 1024;

/// What the manifest says about a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageManifest {
    /// Identity (for a bundle: of its application package for the machine).
    pub identity: MsixIdentity,
    /// `Properties/DisplayName`.
    pub display_name: Option<String>,
    /// `Properties/PublisherDisplayName`.
    pub publisher_display_name: Option<String>,
    /// `Dependencies/PackageDependency` entries.
    pub dependencies: Vec<MsixDependency>,
    /// The file is a bundle: its own version (the inner package, whose
    /// manifest this is, has its own).
    pub bundle_version: Option<String>,
}

/// One central directory entry.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    flags: u16,
    method: u16,
    compressed: u64,
    size: u64,
    local_offset: u64,
}

fn bad(what: &str) -> Error {
    Error::Invalid {
        message: format!("not a readable MSIX/APPX package: {what}"),
    }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut x = [0_u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}

/// A window `[base, base + len)` of a file: the whole package, or an inner
/// package stored in a bundle.
struct Window<'a> {
    file: &'a mut File,
    base: u64,
    len: u64,
}

impl Window<'_> {
    fn read_at(&mut self, at: u64, n: u64) -> Result<Vec<u8>> {
        if at.checked_add(n).is_none_or(|end| end > self.len) {
            return Err(bad("an entry points outside the archive"));
        }
        self.file
            .seek(SeekFrom::Start(self.base + at))
            .map_err(|e| Error::other(format!("seek: {e}")))?;
        let mut buf = vec![0_u8; usize::try_from(n).map_err(|_| bad("entry too large"))?];
        self.file
            .read_exact(&mut buf)
            .map_err(|e| Error::other(format!("read: {e}")))?;
        Ok(buf)
    }

    fn entries(&mut self) -> Result<Vec<Entry>> {
        let tail_len = self.len.min(65_557 + 20);
        let tail_at = self.len - tail_len;
        let tail = self.read_at(tail_at, tail_len)?;
        let eocd = (0..=tail.len().saturating_sub(22))
            .rev()
            .find(|&i| le32(&tail, i) == EOCD_SIG)
            .ok_or_else(|| bad("no end of central directory"))?;
        let mut count = u64::from(le16(&tail, eocd + 10));
        let mut cd_size = u64::from(le32(&tail, eocd + 12));
        let mut cd_offset = u64::from(le32(&tail, eocd + 16));
        if eocd >= 20 && le32(&tail, eocd - 20) == ZIP64_LOCATOR_SIG {
            let at = le64(&tail, eocd - 20 + 8);
            let rec = self.read_at(at, 56)?;
            if le32(&rec, 0) != ZIP64_EOCD_SIG {
                return Err(bad("broken ZIP64 end of central directory"));
            }
            count = le64(&rec, 32);
            cd_size = le64(&rec, 40);
            cd_offset = le64(&rec, 48);
        }
        if cd_size > MAX_CENTRAL_DIRECTORY || count > 1_000_000 {
            return Err(bad("central directory too large"));
        }
        let cd = self.read_at(cd_offset, cd_size)?;
        let mut out = Vec::new();
        let mut p = 0_usize;
        while out.len() < count as usize {
            if p + 46 > cd.len() || le32(&cd, p) != CEN_SIG {
                return Err(bad("broken central directory"));
            }
            let flags = le16(&cd, p + 8);
            let method = le16(&cd, p + 10);
            let mut compressed = u64::from(le32(&cd, p + 20));
            let mut size = u64::from(le32(&cd, p + 24));
            let name_len = usize::from(le16(&cd, p + 28));
            let extra_len = usize::from(le16(&cd, p + 30));
            let comment_len = usize::from(le16(&cd, p + 32));
            let mut local_offset = u64::from(le32(&cd, p + 42));
            let name_end = p + 46 + name_len;
            let extra_end = name_end + extra_len;
            if extra_end + comment_len > cd.len() {
                return Err(bad("broken central directory"));
            }
            let name = String::from_utf8_lossy(&cd[p + 46..name_end]).into_owned();
            // ZIP64 extended information: the fields that overflowed, in order.
            let extra = &cd[name_end..extra_end];
            let mut q = 0;
            while q + 4 <= extra.len() {
                let id = le16(extra, q);
                let n = usize::from(le16(extra, q + 2));
                let body = &extra[q + 4..extra.len().min(q + 4 + n)];
                if id == 1 {
                    let mut k = 0;
                    for field in [&mut size, &mut compressed, &mut local_offset] {
                        if *field == u64::from(u32::MAX) && k + 8 <= body.len() {
                            *field = le64(body, k);
                            k += 8;
                        }
                    }
                }
                q += 4 + n;
            }
            out.push(Entry {
                name,
                flags,
                method,
                compressed,
                size,
                local_offset,
            });
            p = extra_end + comment_len;
        }
        Ok(out)
    }

    /// Offset of an entry's data within the window.
    fn data_offset(&mut self, e: &Entry) -> Result<u64> {
        let head = self.read_at(e.local_offset, 30)?;
        if le32(&head, 0) != LOC_SIG {
            return Err(bad("broken local header"));
        }
        Ok(e.local_offset + 30 + u64::from(le16(&head, 26)) + u64::from(le16(&head, 28)))
    }

    /// The contents of a small entry.
    fn read_entry(&mut self, e: &Entry, max: u64) -> Result<Vec<u8>> {
        if e.flags & 1 != 0 {
            return Err(bad("encrypted entry"));
        }
        if e.size > max || e.compressed > max.saturating_mul(4).max(1024) {
            return Err(bad(&format!("{} is too large", e.name)));
        }
        let at = self.data_offset(e)?;
        let raw = self.read_at(at, e.compressed)?;
        let data = match e.method {
            0 => raw,
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(raw.as_slice())
                    .take(max + 1)
                    .read_to_end(&mut out)
                    .map_err(|err| bad(&format!("{}: {err}", e.name)))?;
                out
            }
            m => return Err(bad(&format!("{}: compression method {m}", e.name))),
        };
        if data.len() as u64 != e.size {
            return Err(bad(&format!("{} does not have its declared size", e.name)));
        }
        Ok(data)
    }
}

fn find<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
}

fn text(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim_start_matches('\u{feff}').to_owned()
}

/// Parse an `AppxManifest.xml`.
pub fn parse_package_manifest(xml_text: &str) -> Result<PackageManifest> {
    let els = xml::elements(xml_text, "AppxManifest.xml")?;
    let identity = els
        .iter()
        .find(|e| e.path == "Package/Identity")
        .ok_or_else(|| Error::parse("AppxManifest.xml", "no Identity"))?;
    let attr = |k: &str| identity.attr(k).unwrap_or_default().to_owned();
    let prop = |name: &str| {
        els.iter()
            .find(|e| e.path == format!("Package/Properties/{name}"))
            .map(|e| e.text.trim().to_owned())
            .filter(|t| !t.is_empty())
    };
    let publisher = attr("Publisher");
    let name = attr("Name");
    let version = attr("Version");
    if name.is_empty() || version.is_empty() || publisher.is_empty() {
        return Err(Error::parse("AppxManifest.xml", "incomplete Identity"));
    }
    let dependencies = els
        .iter()
        .filter(|e| e.path == "Package/Dependencies/PackageDependency")
        .filter_map(|e| {
            Some(MsixDependency {
                name: e.attr("Name")?.to_owned(),
                min_version: e.attr("MinVersion").unwrap_or("0.0.0.0").to_owned(),
                publisher: e.attr("Publisher")?.to_owned(),
            })
        })
        .collect();
    Ok(PackageManifest {
        identity: MsixIdentity {
            publisher_id: publisher_id(&publisher),
            arch: identity
                .attr("ProcessorArchitecture")
                .unwrap_or("neutral")
                .to_ascii_lowercase(),
            framework: prop("Framework").is_some_and(|f| f.eq_ignore_ascii_case("true")),
            name,
            version,
            publisher,
        },
        display_name: prop("DisplayName"),
        publisher_display_name: prop("PublisherDisplayName"),
        dependencies,
        bundle_version: None,
    })
}

/// An inner package listed by a bundle manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundlePackage {
    /// `application` or `resource`.
    pub kind: String,
    /// Architecture.
    pub arch: String,
    /// File name inside the bundle.
    pub file_name: String,
}

/// Parse an `AppxBundleManifest.xml`: the bundle identity (`arch` neutral) and
/// its packages.
pub fn parse_bundle_manifest(xml_text: &str) -> Result<(MsixIdentity, Vec<BundlePackage>)> {
    let els = xml::elements(xml_text, "AppxBundleManifest.xml")?;
    let id = els
        .iter()
        .find(|e| e.path == "Bundle/Identity")
        .ok_or_else(|| Error::parse("AppxBundleManifest.xml", "no Identity"))?;
    let publisher = id.attr("Publisher").unwrap_or_default().to_owned();
    let identity = MsixIdentity {
        name: id.attr("Name").unwrap_or_default().to_owned(),
        version: id.attr("Version").unwrap_or_default().to_owned(),
        arch: "neutral".into(),
        publisher_id: publisher_id(&publisher),
        publisher,
        framework: false,
    };
    let packages = els
        .iter()
        .filter(|e| e.path == "Bundle/Packages/Package")
        .filter_map(|e| {
            Some(BundlePackage {
                kind: e.attr("Type").unwrap_or("application").to_ascii_lowercase(),
                arch: e
                    .attr("Architecture")
                    .unwrap_or("neutral")
                    .to_ascii_lowercase(),
                file_name: e.attr("FileName")?.to_owned(),
            })
        })
        .collect();
    Ok((identity, packages))
}

/// The manifest of the package at `path`; for a bundle, of its application
/// package for `arch` (else neutral, else x64 on arm64).
pub fn read_manifest(path: &Path, arch: &str) -> Result<PackageManifest> {
    let mut file = File::open(path).map_err(|e| Error::io(path, &e))?;
    let len = file.metadata().map_err(|e| Error::io(path, &e))?.len();
    let mut win = Window {
        file: &mut file,
        base: 0,
        len,
    };
    let entries = win.entries()?;
    if let Some(e) = find(&entries, "AppxManifest.xml") {
        let bytes = win.read_entry(e, MAX_MANIFEST)?;
        return parse_package_manifest(&text(&bytes));
    }
    let Some(e) = find(&entries, "AppxMetadata/AppxBundleManifest.xml") else {
        return Err(bad("no AppxManifest.xml or AppxBundleManifest.xml"));
    };
    let bytes = win.read_entry(e, MAX_MANIFEST)?;
    let (bundle_id, packages) = parse_bundle_manifest(&text(&bytes))?;
    let apps: Vec<&BundlePackage> = packages
        .iter()
        .filter(|p| p.kind == "application")
        .collect();
    let inner = [arch, "neutral", if arch == "arm64" { "x64" } else { "" }]
        .iter()
        .filter(|a| !a.is_empty())
        .find_map(|a| apps.iter().find(|p| p.arch == *a))
        .ok_or_else(|| bad(&format!("the bundle has no package for {arch}")))?;
    let entry = find(&entries, &inner.file_name)
        .ok_or_else(|| bad(&format!("{} is listed but missing", inner.file_name)))?
        .clone();
    if entry.method != 0 {
        return Err(bad(&format!("{} is compressed in the bundle", entry.name)));
    }
    let at = win.data_offset(&entry)?;
    let mut inner_win = Window {
        file: win.file,
        base: at,
        len: entry.compressed,
    };
    if at.checked_add(entry.compressed).is_none_or(|end| end > len) {
        return Err(bad("an inner package points outside the bundle"));
    }
    let inner_entries = inner_win.entries()?;
    let e = find(&inner_entries, "AppxManifest.xml")
        .ok_or_else(|| bad(&format!("{} has no AppxManifest.xml", inner.file_name)))?;
    let bytes = inner_win.read_entry(e, MAX_MANIFEST)?;
    let mut manifest = parse_package_manifest(&text(&bytes))?;
    if manifest.identity.name != bundle_id.name
        || manifest.identity.publisher != bundle_id.publisher
    {
        return Err(bad("the inner package is not the bundle's package"));
    }
    manifest.bundle_version = Some(bundle_id.version);
    Ok(manifest)
}
