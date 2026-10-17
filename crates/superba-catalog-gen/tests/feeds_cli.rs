//! `superba-catalog-gen feeds ...` against the committed feeds and a temporary
//! catalog built from a tiny git repository and synthetic index files.

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

fn ok(o: Output) -> String {
    assert!(o.status.success(), "{}", text(&o));
    text(&o)
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "gc.auto=0",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
}

fn model(slug: &str, priority: i64, tiers: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "slug": slug, "display_name": slug.to_uppercase(), "description": "d",
        "visibility": "list", "supported_in_api": true, "priority": priority,
        "context_window": 272000, "max_context_window": 872000,
        "supported_reasoning_levels": [{"effort": "low", "description": "l"}],
        "service_tiers": tiers.iter().map(|t| serde_json::json!({"id": t, "name": "Fast", "description": ""})).collect::<Vec<_>>(),
        "base_instructions": "x".repeat(50)
    })
}

/// A git repository with three release tags.
fn codex_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("codex-rs/models-manager")).unwrap();
    git(dir, &["init", "-q"]);
    let commit = |models: serde_json::Value, prompt: &str, tag: &str| {
        std::fs::write(
            dir.join("codex-rs/models-manager/models.json"),
            serde_json::to_vec_pretty(&models).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("codex-rs/models-manager/prompt.md"), prompt).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", tag]);
        git(dir, &["tag", "-a", tag, "-m", tag]);
    };
    commit(
        serde_json::json!({"models": [model("old-model", 3, &[])]}),
        "Old prompt\n",
        "rust-v0.161.0",
    );
    commit(
        serde_json::json!({"models": [model("new-model", 1, &["priority"])]}),
        "New prompt\n",
        "rust-v0.163.0-alpha.2",
    );
    commit(
        serde_json::json!({"models": [model("new-model", 1, &["priority"]), model("next", 2, &[])]}),
        "New prompt\n",
        "rust-v0.163.0",
    );
}

const PACKAGES: &str = "Package: chatgpt\nVersion: 26.1013.40934\nArchitecture: amd64\n\
Depends: libgtk-3-0,\n libnss3\nFilename: pool/main/c/chatgpt/chatgpt_26.1013.40934_amd64.deb\n\
Size: 502087434\nSHA256: 3ca59e7f9a6641b9e2fb9ad680683784b5ab8026ff7418fb212a0ebe5df3a1d7\n\
Description: ChatGPT\n ChatGPT is an assistant.\n\nPackage: other\nVersion: 1\n";

const PRIMARY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata xmlns="http://linux.duke.edu/metadata/common" xmlns:rpm="http://linux.duke.edu/metadata/rpm" packages="1">
<package type="rpm">
  <name>chatgpt</name>
  <arch>x86_64</arch>
  <version epoch="0" ver="26.1013.40934" rel="1"/>
  <checksum type="sha256" pkgid="YES">4af029bbaab60a5220466d5b678b5a64ee3b1b63cada8f0002ee66062bc8395f</checksum>
  <time file="1792184616" build="1792183722"/>
  <size package="559195437" installed="1691428303" archive="1692335156"/>
  <location href="chatgpt-26.1013.40934-1.x86_64.rpm"/>
  <format><rpm:provides><rpm:entry name="chatgpt"/></rpm:provides></format>
</package>
</metadata>
"#;

const APPCAST: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
  <channel>
    <item>
      <title>26.1013.40934</title>
      <pubDate>Fri, 16 Oct 2026 22:31:52 +0000</pubDate>
      <sparkle:shortVersionString>26.1013.40934</sparkle:shortVersionString>
      <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
      <enclosure url="https://persistent.oaistatic.com/codex-app-prod/ChatGPT-darwin-arm64-26.1013.40934.zip" length="722770743" sparkle:edSignature="c2ln" />
      <sparkle:deltas>
        <enclosure url="https://persistent.oaistatic.com/codex-app-prod/x.delta" length="1" sparkle:edSignature="ZA==" />
      </sparkle:deltas>
    </item>
  </channel>
</rss>
"#;

const SUMS: &str = "582646c6bf0561eb0de540f7fc85b9121b0091a826bc6e96830b7f1722122182  codex-package-x86_64-unknown-linux-musl.tar.gz\n\
d500ef21bf9a497958c0d57252f30b7cb4155eb9b98a2b3252846b6838609ef6  codex-package-x86_64-unknown-linux-musl-bazel.tar.gz\n";

fn temp_catalog(root: &Path) -> PathBuf {
    let cat = root.join("catalog");
    std::fs::create_dir_all(cat.join("feeds")).unwrap();
    std::fs::write(
        cat.join("sources.toml"),
        "format = 1\nrepo = \"openai/codex\"\nfloor = \"0.161.0\"\nextra_versions = [\"0.163.0-alpha.2\"]\n",
    )
    .unwrap();
    std::fs::write(
        cat.join("models-extra.json"),
        r#"{"format":1,"models":[{"slug":"acme-coder","display_name":"Acme Coder","context_window":128000},{"slug":"next"}]}"#,
    )
    .unwrap();
    std::fs::write(
        cat.join("feeds/prices.json"),
        r#"{"format":1,"rev":"2026-10-17","entries":{}}"#,
    )
    .unwrap();
    cat
}

#[test]
fn committed_feeds_are_up_to_date() {
    let out = ok(run(&repo_catalog(), &["feeds", "check"]));
    assert!(out.contains("feeds are up to date"), "{out}");
}

#[test]
fn feeds_are_generated_indexed_signed_and_checked() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("codex");
    std::fs::create_dir_all(&repo).unwrap();
    codex_repo(&repo);
    let cat = temp_catalog(tmp.path());
    let repo_s = repo.to_str().unwrap();

    let out = ok(run(
        &cat,
        &[
            "feeds",
            "models",
            "--git",
            repo_s,
            "--prompt-floor",
            "0.160.0",
        ],
    ));
    assert!(out.contains("4 models"), "{out}");
    assert!(out.contains("next is an OpenAI model; skipped"), "{out}");
    let models: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cat.join("feeds/models.json")).unwrap()).unwrap();
    assert_eq!(models["codex_version"], "0.163.0");
    let slugs: Vec<&str> = models["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["new-model", "next", "old-model", "acme-coder"]);
    assert!(models["models"][0].get("raw").is_none());
    assert!(
        !String::from_utf8_lossy(&std::fs::read(cat.join("feeds/models.json")).unwrap())
            .contains("xxxxxxxxxx")
    );
    let prompts: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cat.join("feeds/prompts.json")).unwrap()).unwrap();
    let p = prompts["prompts"].as_object().unwrap();
    assert_eq!(p.len(), 3, "{p:?}");
    assert_eq!(p["0.163.0"], p["0.163.0-alpha.2"], "deduplicated");
    assert_ne!(p["0.163.0"], p["0.161.0"]);

    let dir = tmp.path().join("in");
    std::fs::create_dir_all(dir.join("sums")).unwrap();
    std::fs::write(dir.join("Packages"), PACKAGES).unwrap();
    std::fs::write(dir.join("primary.xml"), PRIMARY).unwrap();
    std::fs::write(dir.join("appcast.xml"), APPCAST).unwrap();
    std::fs::write(
        dir.join("store.json"),
        r#"{"releaseVersion":"26.1013.40934","storeProductId":"9PLM9XGG6VKS"}"#,
    )
    .unwrap();
    std::fs::write(dir.join("sums/rust-v0.163.0.SHA256SUMS"), SUMS).unwrap();
    let p = |n: &str| dir.join(n).to_str().unwrap().to_owned();
    let rpm = format!("x86_64={}", p("primary.xml"));
    ok(run(
        &cat,
        &[
            "feeds",
            "app-versions",
            "--apt",
            &p("Packages"),
            "--rpm",
            &rpm,
            "--appcast",
            &p("appcast.xml"),
            "--windows-store",
            &p("store.json"),
            "--bundled",
            "26.1013.40934=0.163.0-alpha.2",
        ],
    ));
    let apps: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cat.join("feeds/app-versions.json")).unwrap())
            .unwrap();
    let r = &apps["releases"][0];
    assert_eq!(r["version"], "26.1013.40934");
    assert_eq!(r["bundled_codex"], "0.163.0-alpha.2");
    assert_eq!(r["store_product_id"], "9PLM9XGG6VKS");
    let kinds: Vec<String> = r["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| format!("{}/{}/{}", p["os"], p["kind"], p["arch"]).replace('"', ""))
        .collect();
    assert_eq!(
        kinds,
        ["linux/deb/amd64", "linux/rpm/x86_64", "macos/mac_zip/arm64"]
    );
    let deb = &r["packages"][0];
    assert_eq!(
        deb["url"],
        "https://persistent.oaistatic.com/codex-app-prod/linux/deb/pool/main/c/chatgpt/chatgpt_26.1013.40934_amd64.deb"
    );
    assert_eq!(deb["size"], 502_087_434u64);
    let rpm_pkg = &r["packages"][1];
    assert_eq!(
        rpm_pkg["url"],
        "https://persistent.oaistatic.com/codex-app-prod/linux/rpm/x86_64/chatgpt-26.1013.40934-1.x86_64.rpm"
    );
    assert_eq!(
        r["packages"][2]["signature"], "c2ln",
        "the full zip, not a delta"
    );
    assert_eq!(r["date"], 1_792_184_616_000i64);

    let sums = dir.join("sums");
    ok(run(
        &cat,
        &[
            "feeds",
            "codex-versions",
            "--git",
            repo_s,
            "--sums-dir",
            sums.to_str().unwrap(),
            "--floor",
            "0.161.0",
        ],
    ));
    let cv: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cat.join("feeds/codex-versions.json")).unwrap())
            .unwrap();
    let versions: Vec<&str> = cv["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["0.163.0", "0.163.0-alpha.2", "0.161.0"]);
    assert_eq!(cv["releases"][0]["sha256"].as_object().unwrap().len(), 1);
    assert_eq!(cv["releases"][1]["bundled_by"][0], "26.1013.40934");
    assert_eq!(cv["releases"][1]["prerelease"], true);
    assert!(cv["releases"][0]["date"].as_i64().unwrap() > 0);

    // Index, check, sign, verify.
    let check = run(&cat, &["feeds", "check"]);
    assert!(!check.status.success(), "no index yet");
    ok(run(&cat, &["feeds", "index"]));
    let out = ok(run(&cat, &["feeds", "check"]));
    assert!(out.contains("serial 1"), "{out}");
    assert!(out.contains("signature: none"), "{out}");
    ok(run(&cat, &["feeds", "index"]));
    let idx: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cat.join("feeds/index.json")).unwrap()).unwrap();
    assert_eq!(idx["serial"], 1, "unchanged files keep the serial");
    let key = tmp.path().join("k.key");
    ok(run(&cat, &["keygen", "--out", key.to_str().unwrap()]));
    let out = ok(run(
        &cat,
        &["feeds", "sign", "--key", key.to_str().unwrap()],
    ));
    assert!(out.contains("not in TRUSTED_KEYS"), "{out}");
    let out = ok(run(&cat, &["feeds", "verify"]));
    assert!(out.contains("signature INVALID"), "an untrusted key: {out}");
    assert!(
        !run(&cat, &["feeds", "verify", "--require-signature"])
            .status
            .success()
    );

    // A changed file: the index is stale, re-indexing bumps the serial and drops
    // the signature.
    std::fs::write(
        cat.join("feeds/prices.json"),
        r#"{"format":1,"rev":"2026-10-18","entries":{"m":{"input":1.0,"output":2.0}}}"#,
    )
    .unwrap();
    let out = text(&run(&cat, &["feeds", "check"]));
    assert!(out.contains("out of date"), "{out}");
    assert!(out.contains("no https source page"), "{out}");
    ok(run(&cat, &["feeds", "index"]));
    let idx: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cat.join("feeds/index.json")).unwrap()).unwrap();
    assert_eq!(idx["serial"], 2);
    assert!(!cat.join("feeds/index.json.sig").exists());
}

#[test]
fn price_entries_are_validated_tier_by_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let cat = temp_catalog(tmp.path());
    let page = "https://developers.openai.com/api/docs/pricing";
    let good = serde_json::json!({
        "format": 1, "rev": "2026-10-17",
        "entries": {"gpt-6-astra": {
            "input": 10.0, "cached_input": 1.0, "cache_write": 12.5, "output": 50.0,
            "long_context": [{"above_input_tokens": 272000, "input": 20.0, "cached_input": 2.0,
                              "cache_write": 25.0, "output": 75.0}],
            "tier_prices": {
                "priority": {"input": 20.0, "cached_input": 2.0, "output": 100.0},
                "ultrafast": {"input": 60.0, "output": 300.0, "source": "https://openai.com/api-fast-mode/"}
            },
            "source": page, "rev": "2026-10-17"
        }}
    });
    std::fs::write(cat.join("feeds/prices.json"), good.to_string()).unwrap();
    ok(run(&cat, &["feeds", "index"]));
    let out = text(&run(&cat, &["feeds", "check"]));
    assert!(!out.contains("prices.json"), "{out}");

    let bad = serde_json::json!({
        "format": 1, "rev": "2026-10-17",
        "entries": {"m": {
            "input": 1.0, "cached_input": 2.0, "output": 2.0,
            "tier_prices": {
                "fast": {"input": 2.0, "output": 4.0},
                "flex": {"input": 0.5, "cached_input": 0.6, "output": 1.0, "source": "http://x"},
                "priority": {"input": 2.0, "output": 4.0, "long_context": [
                    {"above_input_tokens": 300000, "input": 4.0, "output": 6.0},
                    {"above_input_tokens": 200000, "input": 4.0, "output": 6.0}]}
            },
            "source": page, "rev": "17.10.2026"
        }}
    });
    std::fs::write(cat.join("feeds/prices.json"), bad.to_string()).unwrap();
    let out = text(&run(&cat, &["feeds", "check"]));
    for want in [
        "m rev \"17.10.2026\" is not YYYY-MM-DD",
        "m tier_prices.fast is not one of priority, flex, ultrafast",
        "m tier_prices.flex source is not an https page",
        "m cached_input is above input",
        "m tier_prices.flex cached_input is above input",
        "m tier_prices.priority long_context is not ascending",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
}
