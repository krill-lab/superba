//! Safe extraction of a verified `.zip` (the macOS and Windows packages).
//!
//! Same rules as the tar extraction in [`crate::extract`]: every entry path must
//! be relative and stay inside the destination (both `/` and `\` separate
//! components, as `Compress-Archive` on Windows PowerShell 5 writes `\`), only
//! regular files and directories are accepted (symbolic links, recorded in the
//! Unix mode of the entry, are refused), sizes and entry counts are bounded, a
//! file that does not decompress to its declared size and CRC is refused, and
//! encrypted entries, ZIP64 and unusual compression methods are not supported.
//! The central directory is the source of truth (data descriptors are fine).

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Component, Path, PathBuf};

use superba_core::{Error, Result};

use crate::extract::{MAX_ENTRIES, MAX_EXTRACTED_BYTES};

const EOCD_SIG: u32 = 0x0605_4b50;
const CEN_SIG: u32 = 0x0201_4b50;
const LOC_SIG: u32 = 0x0403_4b50;

fn reject(archive: &Path, what: impl std::fmt::Display) -> Error {
    Error::Invalid {
        message: format!("refused to extract {}: {what}", archive.display()),
    }
}

fn le16(b: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A relative path of normal components from a zip entry name.
fn clean_name(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            // A drive prefix or an NTFS stream in any component.
            p if p.contains(':') || p == ".." => return None,
            p => out.push(p),
        }
    }
    // `Component` check also refuses prefixes on Windows.
    if out.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// CRC-32 (IEEE) update.
fn crc_update(mut crc: u32, data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0_u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });
    crc = !crc;
    for &b in data {
        crc = table[((crc ^ u32::from(b)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

struct Entry {
    path: PathBuf,
    dir: bool,
    method: usize,
    crc: u32,
    csize: u64,
    size: u64,
    offset: u64,
    mode: u32,
}

fn read_directory(archive: &Path, f: &mut fs::File) -> Result<Vec<Entry>> {
    let io = |e: std::io::Error| Error::io(archive, &e);
    let len = f.metadata().map_err(io)?.len();
    let tail_len = len.min(22 + 65_535);
    f.seek(SeekFrom::Start(len - tail_len)).map_err(io)?;
    let mut tail = vec![0_u8; tail_len as usize];
    f.read_exact(&mut tail).map_err(io)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| le32(&tail, i) == EOCD_SIG)
        .ok_or_else(|| reject(archive, "not a zip file"))?;
    let count = le16(&tail, eocd + 10);
    let cd_size = u64::from(le32(&tail, eocd + 12));
    let cd_offset = u64::from(le32(&tail, eocd + 16));
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        return Err(reject(archive, "ZIP64 archives are not supported"));
    }
    if count > MAX_ENTRIES || cd_size > 64 * 1024 * 1024 || cd_offset + cd_size > len {
        return Err(reject(archive, "bad central directory"));
    }
    f.seek(SeekFrom::Start(cd_offset)).map_err(io)?;
    let mut cd = vec![0_u8; cd_size as usize];
    f.read_exact(&mut cd).map_err(io)?;
    let mut out = Vec::with_capacity(count);
    let mut at = 0_usize;
    for _ in 0..count {
        if at + 46 > cd.len() || le32(&cd, at) != CEN_SIG {
            return Err(reject(archive, "bad central directory entry"));
        }
        let made_by_host = cd[at + 5];
        let flags = le16(&cd, at + 8);
        let method = le16(&cd, at + 10);
        let crc = le32(&cd, at + 16);
        let csize = u64::from(le32(&cd, at + 20));
        let size = u64::from(le32(&cd, at + 24));
        let (n, x, c) = (le16(&cd, at + 28), le16(&cd, at + 30), le16(&cd, at + 32));
        let external = le32(&cd, at + 38);
        let offset = u64::from(le32(&cd, at + 42));
        let end = at + 46 + n + x + c;
        if end > cd.len() {
            return Err(reject(archive, "bad central directory entry"));
        }
        let raw = String::from_utf8_lossy(&cd[at + 46..at + 46 + n]).into_owned();
        at = end;
        if flags & 1 != 0 {
            return Err(reject(archive, format!("{raw} is encrypted")));
        }
        if size == 0xFFFF_FFFF || csize == 0xFFFF_FFFF || offset == 0xFFFF_FFFF {
            return Err(reject(archive, "ZIP64 archives are not supported"));
        }
        let mode = if made_by_host == 3 { external >> 16 } else { 0 };
        let dir = raw.ends_with('/') || raw.ends_with('\\') || external & 0x10 != 0;
        // File type bits of a Unix mode: only regular files and directories.
        match mode & 0o170_000 {
            0 | 0o100_000 | 0o040_000 => {}
            0o120_000 => return Err(reject(archive, format!("{raw} is a link"))),
            _ => return Err(reject(archive, format!("unsupported entry type for {raw}"))),
        }
        if !dir && !matches!(method, 0 | 8) {
            return Err(reject(archive, format!("{raw} uses an unsupported method")));
        }
        let Some(path) = clean_name(&raw) else {
            if raw.trim_matches(['/', '\\', '.']).is_empty() {
                continue;
            }
            return Err(reject(archive, format!("unsafe path {raw}")));
        };
        out.push(Entry {
            path,
            dir,
            method,
            crc,
            csize,
            size,
            offset,
            mode,
        });
    }
    Ok(out)
}

/// Extract `archive` into the (new or empty) directory `dest`.
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let mut f = fs::File::open(archive).map_err(|e| Error::io(archive, &e))?;
    let entries = read_directory(archive, &mut f)?;
    fs::create_dir_all(dest).map_err(|e| Error::io(dest, &e))?;
    let mut seen = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for e in &entries {
        if !seen.insert(e.path.clone()) {
            return Err(reject(
                archive,
                format!("{} appears twice", e.path.display()),
            ));
        }
        let out = dest.join(&e.path);
        if e.dir {
            fs::create_dir_all(&out).map_err(|x| Error::io(&out, &x))?;
            continue;
        }
        total = total.saturating_add(e.size);
        if total > MAX_EXTRACTED_BYTES {
            return Err(reject(archive, "package larger than the extraction limit"));
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|x| Error::io(parent, &x))?;
        }
        write_entry(archive, &mut f, e, &out)?;
    }
    Ok(())
}

fn write_entry(archive: &Path, f: &mut fs::File, e: &Entry, out: &Path) -> Result<()> {
    let io = |x: std::io::Error| Error::io(archive, &x);
    f.seek(SeekFrom::Start(e.offset)).map_err(io)?;
    let mut head = [0_u8; 30];
    f.read_exact(&mut head)
        .map_err(|_| reject(archive, "truncated archive"))?;
    if le32(&head, 0) != LOC_SIG {
        return Err(reject(archive, "bad local header"));
    }
    let skip = (le16(&head, 26) + le16(&head, 28)) as i64;
    f.seek(SeekFrom::Current(skip)).map_err(io)?;
    let data = (&mut *f).take(e.csize);
    let mut src: Box<dyn std::io::Read + '_> = match e.method {
        0 => Box::new(data),
        _ => Box::new(flate2::read::DeflateDecoder::new(data)),
    };
    let mut file = fs::File::create(out).map_err(|x| Error::io(out, &x))?;
    let mut buf = vec![0_u8; 1 << 16];
    let (mut written, mut crc) = (0_u64, 0_u32);
    loop {
        let n = src
            .read(&mut buf)
            .map_err(|_| reject(archive, format!("{} is corrupt", e.path.display())))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > e.size {
            return Err(reject(
                archive,
                format!("{} is larger than declared", e.path.display()),
            ));
        }
        crc = crc_update(crc, &buf[..n]);
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|x| Error::io(out, &x))?;
    }
    drop(file);
    if written != e.size || crc != e.crc {
        return Err(reject(
            archive,
            format!("{} does not match its size or checksum", e.path.display()),
        ));
    }
    set_mode(out, e.mode).map_err(|x| Error::io(out, &x))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if mode & 0o777 == 0 { 0o644 } else { mode };
    fs::set_permissions(path, fs::Permissions::from_mode((mode & 0o755) | 0o600))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}
