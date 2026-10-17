//! Manifest and signature checks, trust policies and key handling.

use std::collections::BTreeMap;

use superba_catalog::format::{INDEX_PATH, SIGNATURE_PATH};
use superba_catalog::integrity::{
    TRUSTED_KEYS, key_id, sha256_hex, signed_message, validate_relative_path, verify_signature,
};

mod common;
use common::{sign_files, test_key};
use superba_catalog::{CatalogError, CatalogStore, SignatureStatus, StoreOrigin, TrustPolicy};

fn builtin_files_unsigned() -> BTreeMap<String, Vec<u8>> {
    let mut files = superba_catalog::builtin_files().unwrap();
    files.remove(SIGNATURE_PATH);
    files
}

#[test]
fn embedded_key_ids_match_their_public_keys() {
    assert!(!TRUSTED_KEYS.is_empty());
    for k in TRUSTED_KEYS {
        assert_eq!(k.id, key_id(&k.public_key), "{}", k.note);
        assert!(ed25519_dalek::VerifyingKey::from_bytes(&k.public_key).is_ok());
    }
}

#[test]
fn valid_signature_is_accepted_by_a_trusted_key_only() {
    let (sk, tk) = test_key(7);
    let (other, other_tk) = test_key(9);
    let mut files = builtin_files_unsigned();
    sign_files(&mut files, &[&sk]);

    let store = CatalogStore::from_files_with_keys(
        StoreOrigin::BuiltIn,
        files.clone(),
        TrustPolicy::RequireSignature,
        &[tk],
        &[],
    )
    .unwrap();
    assert_eq!(
        store.signature(),
        &SignatureStatus::Valid {
            key_id: tk.id.into()
        }
    );

    // Unknown key.
    let err = CatalogStore::from_files_with_keys(
        StoreOrigin::BuiltIn,
        files.clone(),
        TrustPolicy::RequireSignature,
        &[other_tk],
        &[],
    )
    .unwrap_err();
    assert!(
        matches!(err, CatalogError::Signature(ref m) if m.contains("unknown key")),
        "{err}"
    );

    // Revoked key.
    let err = CatalogStore::from_files_with_keys(
        StoreOrigin::BuiltIn,
        files.clone(),
        TrustPolicy::RequireSignature,
        &[tk],
        &[tk.id],
    )
    .unwrap_err();
    assert!(
        matches!(err, CatalogError::Signature(ref m) if m.contains("revoked")),
        "{err}"
    );

    // Rotation: two signatures, the verifier trusts only the new key.
    sign_files(&mut files, &[&other, &sk]);
    assert!(
        CatalogStore::from_files_with_keys(
            StoreOrigin::BuiltIn,
            files,
            TrustPolicy::RequireSignature,
            &[tk],
            &[],
        )
        .is_ok()
    );
}

#[test]
fn tampered_index_breaks_the_signature() {
    let (sk, tk) = test_key(3);
    let mut files = builtin_files_unsigned();
    sign_files(&mut files, &[&sk]);
    let mut index = files[INDEX_PATH].clone();
    // Same length, one byte changed inside the JSON whitespace-free content.
    let pos = index.iter().position(|b| *b == b'2').unwrap_or(10);
    index[pos] = if index[pos] == b'2' { b'3' } else { b'2' };
    let status = verify_signature(
        &index,
        files.get(SIGNATURE_PATH).map(Vec::as_slice),
        &[tk],
        &[],
    );
    assert!(
        matches!(status, SignatureStatus::Invalid { .. }),
        "{status:?}"
    );
    // The signed message is domain separated.
    assert!(signed_message(b"x").starts_with(b"superba-catalog-index-v1\n"));
}

#[test]
fn unsigned_catalog_needs_manifest_only_policy() {
    let files = builtin_files_unsigned();
    let err = CatalogStore::from_files(
        StoreOrigin::BuiltIn,
        files.clone(),
        TrustPolicy::RequireSignature,
    )
    .unwrap_err();
    assert!(matches!(err, CatalogError::Signature(_)));
    let store =
        CatalogStore::from_files(StoreOrigin::BuiltIn, files, TrustPolicy::ManifestOnly).unwrap();
    assert_eq!(store.signature(), &SignatureStatus::Unsigned);
}

#[test]
fn garbage_signature_file_is_invalid_not_a_crash() {
    let mut files = builtin_files_unsigned();
    files.insert(SIGNATURE_PATH.into(), b"not json".to_vec());
    let store =
        CatalogStore::from_files(StoreOrigin::BuiltIn, files, TrustPolicy::ManifestOnly).unwrap();
    assert!(matches!(store.signature(), SignatureStatus::Invalid { .. }));
}

#[test]
fn manifest_detects_changed_and_missing_files() {
    let files = builtin_files_unsigned();
    let path = files
        .keys()
        .find(|p| p.starts_with("dist/codex-"))
        .unwrap()
        .clone();

    let mut changed = files.clone();
    let bytes = changed.get_mut(&path).unwrap();
    let last = bytes.len() - 2;
    bytes[last] ^= 0x01;
    let err = CatalogStore::from_files(StoreOrigin::BuiltIn, changed, TrustPolicy::ManifestOnly)
        .unwrap_err();
    assert!(
        matches!(err, CatalogError::Integrity { path: ref p, .. } if *p == path),
        "{err}"
    );

    let mut missing = files.clone();
    missing.remove(&path);
    let err = CatalogStore::from_files(StoreOrigin::BuiltIn, missing, TrustPolicy::ManifestOnly)
        .unwrap_err();
    assert!(matches!(err, CatalogError::Integrity { .. }), "{err}");

    let mut no_index = files;
    no_index.remove(INDEX_PATH);
    assert!(
        CatalogStore::from_files(StoreOrigin::BuiltIn, no_index, TrustPolicy::ManifestOnly)
            .is_err()
    );
}

#[test]
fn index_paths_must_stay_inside_the_catalog() {
    for bad in [
        "",
        "/etc/passwd",
        "../x",
        "dist/../../x",
        "a//b",
        "a\\b",
        "c:x",
        "./a",
    ] {
        assert!(validate_relative_path(bad).is_err(), "{bad:?}");
    }
    for ok in ["dist/index.json", "overlay/features.toml", "curation.toml"] {
        assert!(validate_relative_path(ok).is_ok(), "{ok}");
    }
}

#[test]
fn sha256_is_lowercase_hex() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
