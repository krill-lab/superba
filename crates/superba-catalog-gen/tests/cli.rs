//! The generator binary against the repository's catalog and temporary copies.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_superba-catalog-gen")
}

fn repo_catalog() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog")
}

fn run(catalog: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("--catalog")
        .arg(catalog)
        .args(args)
        .output()
        .expect("run superba-catalog-gen")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn committed_catalog_is_up_to_date() {
    // Index, overlays (official English) and COVERAGE.md match the committed dist/.
    let o = run(&repo_catalog(), &["check"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("catalog is up to date"));
}

#[test]
fn reextraction_is_byte_identical_when_a_codex_clone_is_available() {
    // Set SUPERBA_CODEX_SRC to an openai/codex clone (a blobless clone is enough).
    let Some(src) = std::env::var_os("SUPERBA_CODEX_SRC") else {
        eprintln!("skipped: SUPERBA_CODEX_SRC not set");
        return;
    };
    let o = run(&repo_catalog(), &["check", "--git", src.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
}

#[test]
fn features_check_against_a_real_capture() {
    // Captured from the desktop app's bundled codex 0.162.0-alpha.17.2 on Linux with
    // an empty CODEX_HOME; compared through the settings engine's canonical
    // cross-check (superba-core FeaturesList + superba-config cross_check_features).
    let list = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../superba-catalog/tests/fixtures/features-list-0.162.0-alpha.17.2-linux.txt");
    let check = |version: &str| {
        run(
            &repo_catalog(),
            &[
                "features-check",
                "--version",
                version,
                "--list",
                list.to_str().unwrap(),
                "--os",
                "linux",
            ],
        )
    };
    let o = check("0.162.0-alpha.17.2");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("167 rows"), "{}", text(&o));
    assert!(text(&o).contains(" 0 differences"), "{}", text(&o));
    // Alphas are not their release: the same list differs from 0.162.0.
    let o = check("0.162.0");
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("instant_interrupt"), "{}", text(&o));
}

#[test]
fn signing_index_and_overlay_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let cat = tmp.path().join("catalog");
    std::fs::create_dir_all(&cat).unwrap();
    copy_tree(&repo_catalog(), &cat);
    let _ = std::fs::remove_file(cat.join("dist/index.json.sig"));

    let key = tmp.path().join("k.key");
    let o = run(&cat, &["keygen", "--out", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // Never overwrites a key.
    assert!(
        !run(&cat, &["keygen", "--out", key.to_str().unwrap()])
            .status
            .success()
    );

    let o = run(&cat, &["sign", "--key", key.to_str().unwrap()]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("not in TRUSTED_KEYS"), "{}", text(&o));
    assert!(cat.join("dist/index.json.sig").is_file());

    // A key that is not embedded is rejected by clients.
    let o = run(&cat, &["verify"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("unknown key"), "{}", text(&o));
    assert!(
        !run(&cat, &["verify", "--require-signature"])
            .status
            .success()
    );

    // A human edit: Chinese text in an overlay. The index goes stale until `index`
    // runs, which also drops the now-invalid signature.
    let overlay = cat.join("overlay/features.toml");
    let src = std::fs::read_to_string(&overlay).unwrap();
    // The coverage row of features before the edit (replacing one Chinese text
    // with another keeps every count).
    let feature_row = |cat: &std::path::Path| {
        std::fs::read_to_string(cat.join("COVERAGE.md"))
            .unwrap()
            .lines()
            .find(|l| l.starts_with("| feature |"))
            .map(str::to_owned)
            .expect("feature row")
    };
    let row_before = feature_row(&cat);
    // Replace the entry's Chinese text, whatever people wrote there so far.
    let start = src
        .find("[\"feature:fast_mode\"]\n")
        .expect("fast_mode entry");
    let zh = start + src[start..].find("\nzh = ").expect("zh line") + 1;
    let end = zh + src[zh..].find('\n').expect("end of the zh line");
    let edited = format!("{}zh = \"测试\"{}", &src[..zh], &src[end..]);
    assert_ne!(src, edited, "anchor not found");
    std::fs::write(&overlay, edited).unwrap();
    let digest_before = text(&run(&cat, &["human-digest"]));
    assert!(
        !run(&cat, &["verify"]).status.success(),
        "manifest must catch the edit"
    );
    let o = run(&cat, &["check"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("index"), "{}", text(&o));

    // overlay-sync keeps the human text and records its basis.
    assert!(run(&cat, &["overlay-sync"]).status.success());
    assert_eq!(text(&run(&cat, &["human-digest"])), digest_before);
    assert!(
        std::fs::read_to_string(&overlay)
            .unwrap()
            .contains("zh = \"测试\"\nzh_basis = \"\"")
    );
    assert!(run(&cat, &["index"]).status.success());
    assert!(!cat.join("dist/index.json.sig").exists());
    assert!(run(&cat, &["coverage", "--write"]).status.success());
    let o = run(&cat, &["check"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(feature_row(&cat), row_before);
}
