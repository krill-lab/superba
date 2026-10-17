//! Helpers shared by the integration tests: deterministic test keys and signing.

#![allow(dead_code)]

use std::collections::BTreeMap;

use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use superba_catalog::format::{
    INDEX_PATH, SIGNATURE_FORMAT, SIGNATURE_PATH, SignatureEntry, SignatureFile, to_pretty_json,
};
use superba_catalog::integrity::{TrustedKey, key_id, signed_message};

pub fn test_key(seed: u8) -> (SigningKey, TrustedKey) {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let pk = sk.verifying_key().to_bytes();
    let id: &'static str = Box::leak(key_id(&pk).into_boxed_str());
    (
        sk,
        TrustedKey {
            id,
            public_key: pk,
            note: "test",
        },
    )
}

pub fn sign_files(files: &mut BTreeMap<String, Vec<u8>>, keys: &[&SigningKey]) {
    let msg = signed_message(&files[INDEX_PATH]);
    let sig = SignatureFile {
        format: SIGNATURE_FORMAT,
        signatures: keys
            .iter()
            .map(|k| SignatureEntry {
                key_id: key_id(&k.verifying_key().to_bytes()),
                algorithm: "ed25519".into(),
                signature: base64::engine::general_purpose::STANDARD
                    .encode(k.sign(&msg).to_bytes()),
            })
            .collect(),
    };
    files.insert(
        SIGNATURE_PATH.into(),
        to_pretty_json(&sig).unwrap().into_bytes(),
    );
}
