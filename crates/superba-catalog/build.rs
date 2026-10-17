//! Packs the repository's `catalog/` tree (index, signature, version files,
//! overlays, curation) into a zstd-compressed bundle that `src/bundle.rs` embeds as
//! the built-in snapshot, and `catalog/feeds/` (signed model list, prices, Codex
//! and app versions, prompts) into a second bundle. Integrity is checked at run
//! time against each index, the same way as for downloaded data.

use std::io::Write as _;
use std::path::{Path, PathBuf};

#[path = "src/bundle_format.rs"]
#[allow(dead_code)]
mod bundle_format;

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../../catalog");
    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("catalog.bundle.zst");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/bundle_format.rs");
    for p in ["dist", "overlay", "curation.toml"] {
        println!("cargo:rerun-if-changed={}", root.join(p).display());
    }

    let mut entries = Vec::new();
    if root.join("dist/index.json").is_file() {
        add(&root, "dist/index.json", &mut entries);
        if root.join("dist/index.json.sig").is_file() {
            add(&root, "dist/index.json.sig", &mut entries);
        }
        if root.join("curation.toml").is_file() {
            add(&root, "curation.toml", &mut entries);
        }
        for (dir, prefix, ext) in [("dist", "codex-", ".json"), ("overlay", "", ".toml")] {
            let mut names: Vec<String> = std::fs::read_dir(root.join(dir))
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| n.starts_with(prefix) && n.ends_with(ext))
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            for n in names {
                add(&root, &format!("{dir}/{n}"), &mut entries);
            }
        }
    } else {
        println!(
            "cargo:warning=superba-catalog: {} not found; the built-in catalog is empty",
            root.join("dist/index.json").display()
        );
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    std::fs::write(&out, compress(&entries)).expect("write bundle");

    let feeds = root.join("feeds");
    println!("cargo:rerun-if-changed={}", feeds.display());
    let mut feed_entries = Vec::new();
    if feeds.join("index.json").is_file() {
        collect(&feeds, "", &mut feed_entries);
    } else {
        println!(
            "cargo:warning=superba-catalog: {} not found; the built-in feeds are empty",
            feeds.join("index.json").display()
        );
    }
    feed_entries.sort_by(|a, b| a.0.cmp(&b.0));
    let feeds_out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("feeds.bundle.zst");
    std::fs::write(&feeds_out, compress(&feed_entries)).expect("write feeds bundle");
}

/// Every file below `dir` as `(relative path with '/', bytes)`.
fn collect(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    let mut items: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let ty = e.file_type().expect("file type");
        if ty.is_dir() {
            println!("cargo:rerun-if-changed={}", e.path().display());
            collect(&e.path(), &rel, out);
        } else if ty.is_file() {
            let data = std::fs::read(e.path()).expect("read feed file");
            out.push((rel, data));
        }
    }
}

fn compress(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let raw = bundle_format::encode(entries);
    let mut enc = zstd::stream::Encoder::new(Vec::new(), 12).expect("zstd encoder");
    enc.long_distance_matching(true).expect("zstd ldm");
    enc.window_log(bundle_format::WINDOW_LOG)
        .expect("zstd window");
    enc.include_checksum(true).expect("zstd checksum");
    enc.write_all(&raw).expect("zstd write");
    enc.finish().expect("zstd finish")
}

fn add(root: &Path, rel: &str, entries: &mut Vec<(String, Vec<u8>)>) {
    let path = root.join(rel);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    entries.push((rel.to_owned(), data));
}
