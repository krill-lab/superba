//! Read-only access to the files packed in an Electron `app.asar`.
//!
//! Layout: a Chromium pickle holding the header size (`u32` 4, `u32` size),
//! then the header pickle (`u32` payload size, `u32` JSON length, JSON), then
//! the file data. The JSON is a tree of `{"files": {name: entry}}`; a file entry
//! has `offset` (a decimal string, relative to the data) and `size`, and
//! `unpacked: true` when it lives in `app.asar.unpacked` instead.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;
use superba_core::{Error, Result};

/// The header is a few megabytes in practice; refuse absurd values.
const HEADER_LIMIT: u64 = 256 * 1024 * 1024;

/// One packed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path inside the archive, `/`-separated (`webview/assets/x.js`).
    pub path: String,
    /// Absolute offset in the archive file.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
}

/// The packed files of an archive (unpacked files and links are left out).
#[derive(Debug)]
pub struct Archive {
    file: File,
    entries: Vec<Entry>,
}

fn invalid(path: &Path, what: impl std::fmt::Display) -> Error {
    Error::invalid(format!(
        "{}: not a readable app.asar ({what})",
        path.display()
    ))
}

impl Archive {
    /// Open `path` and read its header.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(|e| Error::io(path, &e))?;
        let mut head = [0u8; 16];
        file.read_exact(&mut head)
            .map_err(|e| Error::io(path, &e))?;
        let word = |i: usize| {
            u64::from(u32::from_le_bytes([
                head[i],
                head[i + 1],
                head[i + 2],
                head[i + 3],
            ]))
        };
        if word(0) != 4 {
            return Err(invalid(path, "header pickle"));
        }
        let header_size = word(4);
        let json_len = word(12);
        if header_size > HEADER_LIMIT || json_len + 8 > header_size {
            return Err(invalid(path, "header size"));
        }
        let mut json =
            vec![0u8; usize::try_from(json_len).map_err(|_| invalid(path, "header size"))?];
        file.read_exact(&mut json)
            .map_err(|e| Error::io(path, &e))?;
        let header: Value = serde_json::from_slice(&json)
            .map_err(|e| invalid(path, format!("header JSON: {e}")))?;
        let base = 8 + header_size;
        let mut entries = Vec::new();
        collect(&header, String::new(), base, &mut entries);
        entries.sort_by_key(|e| e.offset);
        Ok(Self { file, entries })
    }

    /// The packed files, by offset.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Read one file into `buf` (replacing its contents).
    pub fn read_into(&mut self, entry: &Entry, buf: &mut Vec<u8>) -> std::io::Result<()> {
        let size = usize::try_from(entry.size).map_err(std::io::Error::other)?;
        buf.clear();
        buf.resize(size, 0);
        self.file.seek(SeekFrom::Start(entry.offset))?;
        self.file.read_exact(buf)
    }
}

fn collect(node: &Value, prefix: String, base: u64, out: &mut Vec<Entry>) {
    let Some(files) = node.get("files").and_then(Value::as_object) else {
        return;
    };
    for (name, child) in files {
        if name.contains('/') || name == ".." || name == "." {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if child.get("files").is_some() {
            collect(child, path, base, out);
            continue;
        }
        if child.get("unpacked").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let offset = child
            .get("offset")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok());
        let size = child.get("size").and_then(Value::as_u64);
        if let (Some(offset), Some(size)) = (offset, size) {
            out.push(Entry {
                path,
                offset: base + offset,
                size,
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build an archive with `files` (path, contents) for tests.
    pub(crate) fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut root = serde_json::Map::new();
        let mut data = Vec::new();
        for (path, bytes) in files {
            let mut node = &mut root;
            let parts: Vec<&str> = path.split('/').collect();
            for dir in &parts[..parts.len() - 1] {
                let entry = node
                    .entry(dir.to_string())
                    .or_insert_with(|| serde_json::json!({"files": {}}));
                node = entry["files"].as_object_mut().unwrap();
            }
            node.insert(
                parts[parts.len() - 1].to_string(),
                serde_json::json!({"offset": data.len().to_string(), "size": bytes.len()}),
            );
            data.extend_from_slice(bytes);
        }
        let json = serde_json::to_vec(&serde_json::json!({"files": root})).unwrap();
        let padded = json.len().div_ceil(4) * 4;
        let header_payload = 4 + padded;
        let header_size = 4 + header_payload;
        let mut out = Vec::new();
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&(header_size as u32).to_le_bytes());
        out.extend_from_slice(&(header_payload as u32).to_le_bytes());
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        out.resize(8 + header_size, 0);
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn reads_packed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.asar");
        std::fs::write(
            &path,
            build(&[
                ("a.txt", b"hello"),
                ("webview/assets/x.js", b"export const y=1;"),
            ]),
        )
        .unwrap();
        let mut archive = Archive::open(&path).unwrap();
        let entries = archive.entries().to_vec();
        assert_eq!(
            entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            ["a.txt", "webview/assets/x.js"]
        );
        let mut buf = Vec::new();
        archive.read_into(&entries[1], &mut buf).unwrap();
        assert_eq!(buf, b"export const y=1;");
    }

    #[test]
    fn refuses_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.asar");
        std::fs::write(&path, b"PK\x03\x04 not an asar at all").unwrap();
        assert!(Archive::open(&path).is_err());
    }
}
