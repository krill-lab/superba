//! The Windows app package: OpenAI's release JSON, the store.rg-adguard.net
//! answer (real captures in `tests/fixtures`), the CDN host allowlist, package
//! selection by architecture and dependencies, the MSIX manifest reader and
//! the identity / signer rules.

mod winapp_support;

use pretty_assertions::assert_eq;
use superba_core::{AppPackageKind, Error, InstalledPackage, MsixDependency, PackageSigner};
use superba_versions::winapp::mirror::{
    self, ListedSize, MirrorFile, is_store_cdn_url, parse_package_file_name,
};
use superba_versions::winapp::msix::{self, read_manifest};
use superba_versions::winapp::official::{self, is_official_url};
use superba_versions::winapp::trust::{self, SignerRole, same_dn};
use superba_versions::winapp::{
    MICROSOFT_PUBLISHER, MICROSOFT_PUBLISHER_ID, OPENAI_PUBLISHER_ID, OPENAI_STORE_PUBLISHER,
    msix_arch, publisher_id,
};
use winapp_support::{Entry, bundle, manifest, msix as build_msix, real_manifest, zip};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

const VCLIBS: &str = "Microsoft.VCLibs.140.00.UWPDesktop";

#[test]
fn official_release_json_lists_one_msix_per_architecture_on_oaistatic_only() {
    let r = official::parse_release(
        &fixture("windows-release-26.1013.4894.0.json"),
        is_official_url,
    )
    .unwrap();
    assert_eq!(r.package_version, "26.1013.4894.0");
    assert_eq!(r.release_date.as_deref(), Some("2026-10-17T05:55:44Z"));
    assert_eq!(
        r.package_for("x64").unwrap().url,
        "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-x64.msix"
    );
    assert_eq!(
        r.package_for("arm64").unwrap().url,
        "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-arm64.msix"
    );
    assert!(r.package_for("x86").is_none());

    for bad in [
        "http://persistent.oaistatic.com/codex-app-prod/releases/1/ChatGPT-x64.msix",
        "https://persistent.oaistatic.com.evil.example/ChatGPT-x64.msix",
        "https://evil.example/persistent.oaistatic.com/ChatGPT-x64.msix",
        "https://user:pw@persistent.oaistatic.com/ChatGPT-x64.msix",
        "https://persistent.oaistatic.com:8443/ChatGPT-x64.msix",
        "ftp://persistent.oaistatic.com/ChatGPT-x64.msix",
    ] {
        assert!(!is_official_url(bad), "{bad}");
    }
    let tampered = fixture("windows-release-26.1013.4894.0.json").replace(
        "https://persistent.oaistatic.com/codex-app-prod/releases/26.1013.4894.0/ChatGPT-arm64.msix",
        "https://evil.example/ChatGPT-arm64.msix",
    );
    let r = official::parse_release(&tampered, is_official_url).unwrap();
    assert!(r.package_for("arm64").is_none());
    assert!(r.package_for("x64").is_some());
    assert!(official::parse_release("{\"packageVersion\":\"../1\"}", is_official_url).is_err());
}

#[test]
fn the_store_update_json_maps_the_package_to_its_app_release() {
    let s = official::parse_store_update(&fixture("windows-store-update.json")).unwrap();
    assert_eq!(s.build_version.as_deref(), Some("26.1013.4894.0"));
    assert_eq!(s.product_id.as_deref(), Some("9PLM9XGG6VKS"));
    assert_eq!(s.package_identity.as_deref(), Some("OpenAI.Codex"));
    assert_eq!(
        official::release_version_of("26.1013.4894.0", &s).as_deref(),
        Some("26.1013.40934")
    );
    assert_eq!(official::release_version_of("26.1013.3656.0", &s), None);
}

#[test]
fn publisher_ids_match_the_package_family_names_windows_uses() {
    assert_eq!(publisher_id(OPENAI_STORE_PUBLISHER), OPENAI_PUBLISHER_ID);
    assert_eq!(publisher_id(MICROSOFT_PUBLISHER), MICROSOFT_PUBLISHER_ID);
    assert_eq!(msix_arch("x86_64"), Some("x64"));
    assert_eq!(msix_arch("aarch64"), Some("arm64"));
    assert_eq!(msix_arch("riscv64"), None);
}

#[test]
fn the_real_rg_adguard_answer_lists_both_architectures_with_sha1_and_size() {
    let files =
        mirror::check_answer(200, false, &fixture("rg-adguard-9PLM9XGG6VKS-RP.html")).unwrap();
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "OpenAI.Codex_26.1013.4894.0_arm64__2p2nqsd0c76g0.BlockMap|1201515",
            "OpenAI.Codex_26.1013.4894.0_arm64__2p2nqsd0c76g0.msix",
            "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.BlockMap|1121963",
            "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
        ]
    );
    let x64 = &files[3];
    assert_eq!(
        x64.sha1.as_deref(),
        Some("29f6b5fe52f278cb0a9837338820443d153dedb6")
    );
    assert_eq!(x64.expires.as_deref(), Some("2026-10-17 21:50:51 GMT"));
    // The oaistatic x64 msix of the same build is exactly this file.
    assert!(x64.size.as_ref().unwrap().admits(948_993_815));
    assert!(!x64.size.as_ref().unwrap().admits(948_993_815 + 20_000));
    assert!(x64.url.starts_with(
        "http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/e1f72c97-edfa-465e-ac75-3b2be6f8196a?P1="
    ));
    assert!(
        x64.url.contains("&P2=404&P3=2&P4="),
        "entities decoded: {}",
        x64.url
    );
    assert!(files.iter().all(|f| is_store_cdn_url(&f.url)));
    // Block maps are not packages.
    assert!(parse_package_file_name(&files[0].name).is_none());

    let x = mirror::select_app(&files, "x64", &is_store_cdn_url).unwrap();
    assert_eq!(
        x.0.name,
        "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix"
    );
    assert_eq!(x.1.version, "26.1013.4894.0");
    assert_eq!(x.1.kind, AppPackageKind::Msix);
    let a = mirror::select_app(&files, "arm64", &is_store_cdn_url).unwrap();
    assert_eq!(
        a.0.name,
        "OpenAI.Codex_26.1013.4894.0_arm64__2p2nqsd0c76g0.msix"
    );
    assert!(mirror::select_app(&files, "x86", &is_store_cdn_url).is_none());
}

#[test]
fn a_cloudflare_challenge_or_an_empty_answer_is_explained() {
    let e = mirror::check_answer(403, true, &fixture("rg-adguard-challenge.html")).unwrap_err();
    let Error::Http {
        status, message, ..
    } = &e
    else {
        panic!("{e:?}");
    };
    assert_eq!(*status, 403);
    assert!(message.contains("browser check"), "{message}");
    // Recognised from the page alone too.
    assert!(mirror::check_answer(200, false, &fixture("rg-adguard-challenge.html")).is_err());
    let e = mirror::check_answer(
        200,
        false,
        "<p>The server returned an empty list.<br>Either you have not entered the link correctly.</p>",
    )
    .unwrap_err();
    assert!(
        matches!(&e, Error::NotFound { what } if what.contains("empty list")),
        "{e:?}"
    );
    assert!(matches!(
        mirror::check_answer(502, false, "bad gateway"),
        Err(Error::Http { status: 502, .. })
    ));
}

#[test]
fn the_lookup_sends_what_the_sites_own_form_sends() {
    assert_eq!(
        mirror::request_body("9PLM9XGG6VKS", "RP"),
        "type=ProductId&url=9PLM9XGG6VKS&ring=RP&lang="
    );
    // Nothing can be smuggled into the form.
    assert_eq!(
        mirror::request_body("9PLM&type=url", "RP"),
        "type=ProductId&url=9PLMtypeurl&ring=RP&lang="
    );
}

#[test]
fn only_microsoft_store_cdn_links_are_followed() {
    for ok in [
        "http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/e1f72c97?P1=1&P2=404&P3=2&P4=x",
        "https://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/e1f72c97",
        "http://dl.delivery.mp.microsoft.com/filestreamingservice/files/cbd09858-190f-449e-affa-d1e5ff8f68b6",
        "http://TLU.DL.Delivery.MP.Microsoft.com/filestreamingservice/files/x",
        "http://2tlu-a.dl.delivery.mp.microsoft.com/filestreamingservice/files/x",
    ] {
        assert!(is_store_cdn_url(ok), "{ok}");
    }
    for bad in [
        "http://dl.delivery.mp.microsoft.com.evil.example/filestreamingservice/files/x",
        "http://evil.example/filestreamingservice/files/x",
        "http://a.b.dl.delivery.mp.microsoft.com/filestreamingservice/files/x",
        "http://xdl.delivery.mp.microsoft.com/filestreamingservice/files/x",
        "http://user@tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/x",
        "http://tlu.dl.delivery.mp.microsoft.com:8080/filestreamingservice/files/x",
        "http://tlu.dl.delivery.mp.microsoft.com/other/x",
        "ftp://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/x",
        "http://13.107.4.50/filestreamingservice/files/x",
        "not a url",
    ] {
        assert!(!is_store_cdn_url(bad), "{bad}");
    }
}

#[test]
fn listed_sizes_are_binary_units_with_two_decimals() {
    let s = ListedSize::parse("905.03 MB").unwrap();
    assert!(s.admits(948_993_815));
    assert!(!s.admits(949_100_000));
    assert!(ListedSize::parse("0 B").unwrap().admits(0));
    assert!(ListedSize::parse("12.50 KB").unwrap().admits(12_800));
    assert!(ListedSize::parse("1.00 GB").unwrap().admits(1_073_741_824));
    assert!(ListedSize::parse("lots").is_none());
    assert!(ListedSize::parse("-1 MB").is_none());
}

fn file(name: &str, sha1: Option<&str>) -> MirrorFile {
    MirrorFile {
        name: name.into(),
        url: format!("http://tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/{name}"),
        expires: None,
        sha1: sha1.map(str::to_owned),
        size: ListedSize::parse("1.00 MB"),
    }
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn the_newest_package_for_the_architecture_wins_and_strangers_are_ignored() {
    let files = vec![
        file(
            "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
            Some(SHA),
        ),
        file(
            "OpenAI.Codex_26.1014.10.0_x64__2p2nqsd0c76g0.msix",
            Some(SHA),
        ),
        // Newer, but another publisher, no checksum, encrypted, or off the CDN.
        file("OpenAI.Codex_27.0.0.0_x64__8wekyb3d8bbwe.msix", Some(SHA)),
        file("OpenAI.Codex_27.0.0.1_x64__2p2nqsd0c76g0.msix", None),
        file("OpenAI.Codex_27.0.0.2_x64__2p2nqsd0c76g0.emsix", Some(SHA)),
        MirrorFile {
            url: "http://evil.example/filestreamingservice/files/x".into(),
            ..file("OpenAI.Codex_27.0.0.3_x64__2p2nqsd0c76g0.msix", Some(SHA))
        },
        file(
            "OpenAI.Codex_26.1015.1.0_arm64__2p2nqsd0c76g0.msix",
            Some(SHA),
        ),
    ];
    let (f, p) = mirror::select_app(&files, "x64", &is_store_cdn_url).unwrap();
    assert_eq!(f.name, "OpenAI.Codex_26.1014.10.0_x64__2p2nqsd0c76g0.msix");
    assert_eq!(p.publisher_id, OPENAI_PUBLISHER_ID);
    assert_eq!(
        mirror::select_app(&files, "arm64", &is_store_cdn_url)
            .unwrap()
            .0
            .name,
        "OpenAI.Codex_26.1015.1.0_arm64__2p2nqsd0c76g0.msix"
    );

    // A neutral bundle of the same version yields to the exact architecture;
    // a newer bundle wins; arm64 falls back to x64 only without anything else.
    let files = vec![
        file(
            "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
            Some(SHA),
        ),
        file(
            "OpenAI.Codex_26.1013.4894.0_neutral_~_2p2nqsd0c76g0.msixbundle",
            Some(SHA),
        ),
    ];
    assert_eq!(
        mirror::select_app(&files, "x64", &is_store_cdn_url)
            .unwrap()
            .1
            .kind,
        AppPackageKind::Msix
    );
    assert_eq!(
        mirror::select_app(&files, "arm64", &is_store_cdn_url)
            .unwrap()
            .1
            .kind,
        AppPackageKind::MsixBundle
    );
    let files = vec![file(
        "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0.msix",
        Some(SHA),
    )];
    assert_eq!(
        mirror::select_app(&files, "arm64", &is_store_cdn_url)
            .unwrap()
            .1
            .arch,
        "x64"
    );
}

#[test]
fn only_missing_dependencies_are_chosen_for_the_apps_architecture() {
    let dep = MsixDependency {
        name: VCLIBS.into(),
        min_version: "14.0.30704.0".into(),
        publisher: MICROSOFT_PUBLISHER.into(),
    };
    let files = vec![
        file(
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.30704.0_x86__8wekyb3d8bbwe.appx",
            Some(SHA),
        ),
        file(
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.30704.0_x64__8wekyb3d8bbwe.appx",
            Some(SHA),
        ),
        file(
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx",
            Some(SHA),
        ),
        file(
            "Microsoft.VCLibs.140.00.UWPDesktop_14.0.20000.0_x64__8wekyb3d8bbwe.appx",
            Some(SHA),
        ),
        file(
            "Microsoft.VCLibs.140.00.UWPDesktop_15.0.0.0_x64__2p2nqsd0c76g0.appx",
            Some(SHA),
        ),
    ];
    let (chosen, unresolved) = mirror::select_dependencies(
        &files,
        std::slice::from_ref(&dep),
        "x64",
        &[],
        &is_store_cdn_url,
    );
    assert!(unresolved.is_empty());
    assert_eq!(chosen.len(), 1);
    assert_eq!(
        chosen[0].0.name,
        "Microsoft.VCLibs.140.00.UWPDesktop_14.0.33728.0_x64__8wekyb3d8bbwe.appx"
    );

    // Installed in a sufficient version: nothing to download.
    let installed = vec![InstalledPackage {
        name: VCLIBS.into(),
        version: "14.0.33519.0".into(),
        arch: "x64".into(),
        publisher_id: MICROSOFT_PUBLISHER_ID.into(),
    }];
    let (chosen, unresolved) = mirror::select_dependencies(
        &files,
        std::slice::from_ref(&dep),
        "x64",
        &installed,
        &is_store_cdn_url,
    );
    assert!(chosen.is_empty() && unresolved.is_empty());

    // Installed too old, or only for another architecture: still needed.
    for (version, arch) in [("14.0.20000.0", "x64"), ("14.0.33519.0", "x86")] {
        let installed = vec![InstalledPackage {
            version: version.into(),
            arch: arch.into(),
            ..installed[0].clone()
        }];
        let (chosen, _) = mirror::select_dependencies(
            &files,
            std::slice::from_ref(&dep),
            "x64",
            &installed,
            &is_store_cdn_url,
        );
        assert_eq!(chosen.len(), 1, "{version} {arch}");
    }

    // Nothing listed for arm64: unresolved.
    let (chosen, unresolved) = mirror::select_dependencies(
        &files,
        std::slice::from_ref(&dep),
        "arm64",
        &[],
        &is_store_cdn_url,
    );
    assert!(chosen.is_empty());
    assert_eq!(unresolved, vec![dep]);
}

fn write_tmp(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

#[test]
fn the_manifest_is_read_from_a_zip64_msix_like_the_real_package() {
    let tmp = tempfile::tempdir().unwrap();
    let real = write_tmp(&tmp, "real.msix", &build_msix(&real_manifest(), b"payload"));
    let m = read_manifest(&real, "x64").unwrap();
    assert_eq!(m.identity.name, "OpenAI.Codex");
    assert_eq!(m.identity.version, "26.1013.4894.0");
    assert_eq!(m.identity.arch, "x64");
    assert_eq!(m.identity.publisher, OPENAI_STORE_PUBLISHER);
    assert_eq!(m.identity.publisher_id, OPENAI_PUBLISHER_ID);
    assert_eq!(m.identity.family_name(), "OpenAI.Codex_2p2nqsd0c76g0");
    assert_eq!(m.publisher_display_name.as_deref(), Some("OpenAI"));
    assert_eq!(m.display_name.as_deref(), Some("ChatGPT"));
    // The real package declares no package dependencies.
    assert!(m.dependencies.is_empty());
    assert!(!m.identity.framework);
    trust::check_app_identity(&m, "x64", Some("26.1013.4894.0")).unwrap();

    // Classic (non-ZIP64) archives and stored entries read the same way.
    let plain = zip(
        &[Entry::stored(
            "AppxManifest.xml",
            real_manifest().as_bytes(),
        )],
        false,
    );
    let plain = write_tmp(&tmp, "plain.msix", &plain);
    assert_eq!(read_manifest(&plain, "x64").unwrap().identity, m.identity);
}

#[test]
fn dependencies_and_frameworks_are_read_from_the_manifest() {
    let text = manifest(
        "OpenAI.Codex",
        "27.0.0.0",
        "arm64",
        OPENAI_STORE_PUBLISHER,
        false,
        &[(VCLIBS, "14.0.30704.0", MICROSOFT_PUBLISHER)],
    );
    let m = msix::parse_package_manifest(&text).unwrap();
    assert_eq!(
        m.dependencies,
        vec![MsixDependency {
            name: VCLIBS.into(),
            min_version: "14.0.30704.0".into(),
            publisher: MICROSOFT_PUBLISHER.into(),
        }]
    );
    let fw = msix::parse_package_manifest(&manifest(
        VCLIBS,
        "14.0.33728.0",
        "x64",
        MICROSOFT_PUBLISHER,
        true,
        &[],
    ))
    .unwrap();
    assert!(fw.identity.framework);
    assert_eq!(fw.identity.publisher_id, MICROSOFT_PUBLISHER_ID);
}

#[test]
fn a_bundle_reads_the_inner_package_for_the_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let x64 = build_msix(
        &manifest(
            "OpenAI.Codex",
            "1.2.3.0",
            "x64",
            OPENAI_STORE_PUBLISHER,
            false,
            &[(VCLIBS, "14.0.0.0", MICROSOFT_PUBLISHER)],
        ),
        b"x64",
    );
    let arm = build_msix(
        &manifest(
            "OpenAI.Codex",
            "1.2.3.0",
            "arm64",
            OPENAI_STORE_PUBLISHER,
            false,
            &[],
        ),
        b"arm",
    );
    let b = bundle(
        "OpenAI.Codex",
        "2026.1017.1.0",
        OPENAI_STORE_PUBLISHER,
        &[
            ("ChatGPT_x64.msix", "x64", x64),
            ("ChatGPT_arm64.msix", "arm64", arm),
        ],
    );
    let path = write_tmp(&tmp, "b.msixbundle", &b);
    let m = read_manifest(&path, "x64").unwrap();
    assert_eq!(m.identity.arch, "x64");
    assert_eq!(m.bundle_version.as_deref(), Some("2026.1017.1.0"));
    assert_eq!(m.dependencies.len(), 1);
    trust::check_app_identity(&m, "x64", Some("2026.1017.1.0")).unwrap();
    let m = read_manifest(&path, "arm64").unwrap();
    assert_eq!(m.identity.arch, "arm64");
    assert!(m.dependencies.is_empty());
    assert!(read_manifest(&path, "x86").is_err());

    // An inner package of someone else is refused.
    let other = build_msix(
        &manifest(
            "Evil.App",
            "1.0.0.0",
            "x64",
            OPENAI_STORE_PUBLISHER,
            false,
            &[],
        ),
        b"e",
    );
    let b = bundle(
        "OpenAI.Codex",
        "1.0.0.0",
        OPENAI_STORE_PUBLISHER,
        &[("e.msix", "x64", other)],
    );
    let path = write_tmp(&tmp, "evil.msixbundle", &b);
    assert!(read_manifest(&path, "x64").is_err());
}

#[test]
fn broken_encrypted_or_foreign_archives_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let not_zip = write_tmp(&tmp, "a.msix", b"MZ this is not a zip at all");
    assert!(read_manifest(&not_zip, "x64").is_err());
    let no_manifest = write_tmp(
        &tmp,
        "b.msix",
        &zip(&[Entry::deflated("readme.txt", b"hi")], true),
    );
    assert!(read_manifest(&no_manifest, "x64").is_err());
    let mut enc = Entry::stored("AppxManifest.xml", b"<Package/>");
    enc.flags = 1;
    let encrypted = write_tmp(&tmp, "c.msix", &zip(&[enc], false));
    assert!(read_manifest(&encrypted, "x64").is_err());
    let mut truncated = build_msix(&real_manifest(), b"payload");
    truncated.truncate(truncated.len() / 2);
    let truncated = write_tmp(&tmp, "d.msix", &truncated);
    assert!(read_manifest(&truncated, "x64").is_err());
}

#[test]
fn the_app_identity_must_be_openais_store_package_for_this_machine() {
    let ok = |name: &str, arch: &str, publisher: &str| {
        let m = msix::parse_package_manifest(&manifest(
            name,
            "26.1013.4894.0",
            arch,
            publisher,
            false,
            &[],
        ))
        .unwrap();
        (
            trust::check_app_identity(&m, "x64", Some("26.1013.4894.0")).is_ok(),
            trust::check_app_identity(&m, "x64", Some("26.1013.3656.0")).is_ok(),
        )
    };
    assert_eq!(
        ok("OpenAI.Codex", "x64", OPENAI_STORE_PUBLISHER),
        (true, false)
    );
    assert_eq!(
        ok("OpenAI.Codex", "neutral", OPENAI_STORE_PUBLISHER),
        (true, false)
    );
    assert_eq!(
        ok("OpenAI.CodexBeta", "x64", OPENAI_STORE_PUBLISHER),
        (false, false)
    );
    assert_eq!(
        ok("OpenAI.Codex", "arm64", OPENAI_STORE_PUBLISHER),
        (false, false)
    );
    assert_eq!(
        ok(
            "OpenAI.Codex",
            "x64",
            "CN=OpenAI, O=OpenAI, L=San Francisco, S=California, C=US"
        ),
        (false, false)
    );
    // arm64 machines run x64 packages.
    let m = msix::parse_package_manifest(&real_manifest()).unwrap();
    trust::check_app_identity(&m, "arm64", None).unwrap();
}

fn signer(subject: &str, issuer: &str) -> PackageSigner {
    PackageSigner {
        subject: subject.into(),
        issuer: Some(issuer.into()),
    }
}

/// The issuer of the real package's signing certificate, in the order
/// `CertNameToStr` prints it.
const MARKETPLACE_CA: &str = "C=US, S=Washington, L=Redmond, O=Microsoft Corporation, OU=AOC, CN=Microsoft Marketplace CA G 024";

#[test]
fn the_signer_must_be_the_expected_publisher_from_the_expected_authority() {
    let app = msix::parse_package_manifest(&real_manifest())
        .unwrap()
        .identity;
    // The real package: Store publisher GUID, Microsoft Marketplace CA.
    trust::check_signer(
        &app,
        &signer("CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B", MARKETPLACE_CA),
        SignerRole::App,
    )
    .unwrap();
    for (subject, issuer) in [
        ("CN=Someone Else", MARKETPLACE_CA),
        (
            "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B, O=Evil",
            MARKETPLACE_CA,
        ),
        (
            "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
            "CN=Evil CA, O=Evil",
        ),
        (
            "CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B",
            "CN=Microsoft Code Signing PCA 2011, O=Microsoft Corporation",
        ),
    ] {
        assert!(
            trust::check_signer(&app, &signer(subject, issuer), SignerRole::App).is_err(),
            "{subject} / {issuer}"
        );
    }
    let no_issuer = PackageSigner {
        subject: OPENAI_STORE_PUBLISHER.into(),
        issuer: None,
    };
    assert!(trust::check_signer(&app, &no_issuer, SignerRole::App).is_err());

    let fw = msix::parse_package_manifest(&manifest(
        VCLIBS,
        "14.0.33728.0",
        "x64",
        MICROSOFT_PUBLISHER,
        true,
        &[],
    ))
    .unwrap()
    .identity;
    trust::check_signer(
        &fw,
        &signer(
            "C=US, S=Washington, L=Redmond, O=Microsoft Corporation, CN=Microsoft Corporation",
            "C=US, S=Washington, L=Redmond, O=Microsoft Corporation, CN=Microsoft Code Signing PCA 2011",
        ),
        SignerRole::Dependency,
    )
    .unwrap();
    assert!(
        trust::check_signer(
            &fw,
            &signer(OPENAI_STORE_PUBLISHER, MARKETPLACE_CA),
            SignerRole::Dependency
        )
        .is_err()
    );
    // The app's identity never passes as a dependency and vice versa.
    assert!(
        trust::check_signer(
            &app,
            &signer(MICROSOFT_PUBLISHER, MARKETPLACE_CA),
            SignerRole::Dependency
        )
        .is_err()
    );
}

#[test]
fn distinguished_names_compare_by_attributes() {
    assert!(same_dn(
        "CN=Microsoft Corporation, O=Microsoft Corporation, L=Redmond, S=Washington, C=US",
        "C=US, ST=Washington, L=Redmond, O=Microsoft Corporation, CN=Microsoft Corporation"
    ));
    assert!(same_dn("CN=\"A, B\", O=X", "O=X, CN=\"A, B\""));
    assert!(!same_dn("CN=A, O=X", "CN=A"));
    assert!(!same_dn("", ""));
}

#[test]
fn a_dependency_file_must_be_the_declared_microsoft_framework() {
    let dep = MsixDependency {
        name: VCLIBS.into(),
        min_version: "14.0.30704.0".into(),
        publisher: MICROSOFT_PUBLISHER.into(),
    };
    let m = |version: &str, arch: &str, publisher: &str| {
        msix::parse_package_manifest(&manifest(VCLIBS, version, arch, publisher, true, &[]))
            .unwrap()
    };
    assert!(
        trust::check_dependency_identity(
            &m("14.0.33728.0", "x64", MICROSOFT_PUBLISHER),
            &dep,
            "x64"
        )
        .is_ok()
    );
    assert!(
        trust::check_dependency_identity(&m("14.0.1.0", "x64", MICROSOFT_PUBLISHER), &dep, "x64")
            .is_err()
    );
    assert!(
        trust::check_dependency_identity(
            &m("14.0.33728.0", "x86", MICROSOFT_PUBLISHER),
            &dep,
            "x64"
        )
        .is_err()
    );
    assert!(
        trust::check_dependency_identity(
            &m("14.0.33728.0", "x64", OPENAI_STORE_PUBLISHER),
            &dep,
            "x64"
        )
        .is_err()
    );
}

/// `SUPERBA_REAL_MSIX=<path to ChatGPT-x64.msix>`: read the real package (not
/// run otherwise; the file is ~950 MB).
#[test]
fn the_real_package_when_one_is_given() {
    let Some(path) = std::env::var_os("SUPERBA_REAL_MSIX") else {
        return;
    };
    let m = read_manifest(std::path::Path::new(&path), "x64").unwrap();
    assert_eq!(m.identity.family_name(), "OpenAI.Codex_2p2nqsd0c76g0");
    trust::check_app_identity(&m, "x64", None).unwrap();
    let (sha1, _) =
        superba_versions::winapp::download::hash_file(std::path::Path::new(&path)).unwrap();
    eprintln!("{} {} sha1 {sha1}", m.identity.name, m.identity.version);
}
