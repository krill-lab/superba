//! Round trip through real Codex binaries (ignored by default).
//!
//! `SUPERBA_TEST_CODEX_BINS` lists local Codex binaries, separated by the OS path
//! separator or commas (e.g. 0.140.0, 0.150.1, 0.162.0-alpha.17.2,
//! 0.163.0-alpha.10.2). For each one the test takes the binary's own
//! `codex debug models --bundled` output as the template, builds catalogs for the
//! situations Superba writes (bundled list with overrides, a Codex-native
//! upstream list, an OpenAI-style list with generic entries, tier-suffix
//! entries), and checks that `codex debug models -c model_catalog_json=<file>`
//! accepts each file and lists exactly its models. Every run uses a private empty
//! `CODEX_HOME` and `HOME` in a temp directory, never the real ones.
//!
//! ```sh
//! SUPERBA_TEST_CODEX_BINS=/path/codex-0.140.0:/path/codex-0.163.0-alpha.10.2 \
//!   cargo test -p superba-models --test preflight_tests -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use superba_core::{CodexVersion, ModelOverride, ModelSpec, SpeedTier};
use superba_models::*;

/// A private temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "superba-models-preflight-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn binaries() -> Vec<PathBuf> {
    let Some(list) = std::env::var_os("SUPERBA_TEST_CODEX_BINS") else {
        return Vec::new();
    };
    let list = list.to_string_lossy().into_owned();
    list.split([',', if cfg!(windows) { ';' } else { ':' }])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn run(binary: &Path, args: &[&str], home: &Path) -> (bool, String, String) {
    let codex_home = home.join(".codex");
    std::fs::create_dir_all(&codex_home).unwrap();
    let out = Command::new(binary)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("CODEX_HOME", &codex_home)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn version_of(binary: &Path, home: &Path) -> CodexVersion {
    let (ok, out, _) = run(binary, &["--version"], home);
    assert!(ok, "{}: --version failed", binary.display());
    let v = out.split_whitespace().last().unwrap();
    CodexVersion::parse(v).unwrap()
}

fn native_from(template: &BundledCatalog) -> Vec<ModelSpec> {
    template
        .entries
        .iter()
        .filter(|e| e["visibility"] == "list")
        .map(|e| {
            let mut e = e.clone();
            let o = e.as_object_mut().unwrap();
            o.insert("prefer_websockets".into(), Value::Bool(true));
            o.insert(
                "minimal_client_version".into(),
                serde_json::json!([0, 140, 0]),
            );
            if o["slug"] == template.entries[0]["slug"] {
                o.insert(
                    "service_tiers".into(),
                    serde_json::json!([
                        {"id": "priority", "name": "Fast", "description": "x"},
                        {"id": "ultrafast", "name": "Ultrafast", "description": "y"}
                    ]),
                );
            }
            ModelSpec::from_codex_entry(&e).unwrap()
        })
        .collect()
}

/// The Codex-native fixture (0.162 shape, relay-style extra fields).
fn fixture_native() -> Vec<ModelSpec> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native-upstream.json");
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    value["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| ModelSpec::from_codex_entry(e).unwrap())
        .collect()
}

#[test]
#[ignore = "needs local Codex binaries in SUPERBA_TEST_CODEX_BINS"]
fn generated_catalogs_load_in_the_consumer_binaries() {
    let bins = binaries();
    assert!(!bins.is_empty(), "set SUPERBA_TEST_CODEX_BINS");
    let tmp = TempDir::new();
    for (i, bin) in bins.iter().enumerate() {
        let home = tmp.0.join(format!("home-{i}"));
        std::fs::create_dir_all(&home).unwrap();
        let version = version_of(bin, &home);
        let (ok, dump, err) = run(bin, &["debug", "models", "--bundled"], &home);
        assert!(ok, "{version}: debug models failed: {err}");
        let mut template = parse_bundled(dump.as_bytes()).unwrap();
        template.codex_version = Some(version.clone());
        let first = template.entries[0]["slug"].as_str().unwrap().to_owned();

        let mut overrides = BTreeMap::new();
        overrides.insert(
            first.clone(),
            ModelOverride {
                context_window: Some(123_456),
                auto_compact: Some(100_000),
                display_name: Some("Override".into()),
                ..Default::default()
            },
        );
        let generic = ModelSpec {
            context_window: Some(64_000),
            ..ModelSpec::new("acme-coder")
        };
        let cases: Vec<(&str, CatalogSource, SpeedTier, bool, Option<String>)> = vec![
            (
                "bundled",
                CatalogSource::Bundled,
                SpeedTier::Standard,
                false,
                None,
            ),
            (
                "native",
                CatalogSource::Native(native_from(&template)),
                SpeedTier::Ultrafast,
                false,
                Some(first.clone()),
            ),
            (
                "openai-list",
                CatalogSource::OpenAiList(vec![ModelSpec::new(first.clone()), generic.clone()]),
                SpeedTier::Fast,
                false,
                Some("acme-coder".into()),
            ),
            (
                "native-0.162-shape",
                CatalogSource::Native(fixture_native()),
                SpeedTier::Standard,
                false,
                None,
            ),
            (
                "suffix",
                CatalogSource::Native(native_from(&template)),
                SpeedTier::Ultrafast,
                true,
                Some(format!("{first}-ultrafast")),
            ),
        ];
        for (label, source, speed, suffix, model) in cases {
            let file = build_catalog(&CatalogInput {
                consumer: version.clone(),
                template: &template,
                source,
                feed: &[],
                prompt: Some("Generic instructions (preflight test)."),
                overrides: &overrides,
                speed,
                tier_model_suffix: suffix,
                model: model.as_deref(),
            })
            .unwrap_or_else(|e| panic!("{version} {label}: {e}"));
            let path = home.join(file.file_name());
            std::fs::write(&path, &file.bytes).unwrap();
            let arg = format!("model_catalog_json={}", serde_json::json!(path));
            let (ok, out, err) = run(bin, &["debug", "models", "-c", &arg], &home);
            assert!(ok, "{version} {label}: codex refused the catalog: {err}");
            let loaded = parse_bundled(out.as_bytes()).unwrap();
            let slugs: Vec<String> = loaded
                .entries
                .iter()
                .map(|e| e["slug"].as_str().unwrap().to_owned())
                .collect();
            assert_eq!(slugs, file.slugs, "{version} {label}");
            let o = loaded
                .entries
                .iter()
                .find(|e| e["slug"] == first.as_str())
                .unwrap();
            if label != "openai-list" && label != "native-0.162-shape" {
                assert_eq!(o["context_window"], 123_456, "{version} {label}");
            }
            println!(
                "{version} {label}: ok ({} models, {})",
                slugs.len(),
                file.file_name()
            );
        }

        // Why the template check exists: the 0.162 dump written as is stops an
        // older Codex (missing `supports_reasoning_summaries`).
        if version.triple() < (0, 145, 0) {
            let path = home.join("foreign.json");
            let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/bundled-0.162.0-alpha.17.2.json");
            std::fs::copy(fixture, &path).unwrap();
            let arg = format!("model_catalog_json={}", serde_json::json!(path));
            let (ok, _, err) = run(bin, &["debug", "models", "-c", &arg], &home);
            assert!(!ok, "{version} accepted a 0.162 catalog");
            assert!(err.contains("supports_reasoning_summaries"), "{err}");
            println!("{version}: a 0.162-shaped file is refused by codex, as expected");
        }

        // A file for another version is refused by build_catalog itself.
        let other = if version.triple() < (0, 147, 0) {
            CodexVersion::new(0, 163, 0)
        } else {
            CodexVersion::new(0, 140, 0)
        };
        let refused = build_catalog(&CatalogInput {
            consumer: other,
            template: &template,
            source: CatalogSource::Bundled,
            feed: &[],
            prompt: None,
            overrides: &BTreeMap::new(),
            speed: SpeedTier::Standard,
            tier_model_suffix: false,
            model: None,
        });
        assert!(refused.is_err(), "{version}");
    }
}
