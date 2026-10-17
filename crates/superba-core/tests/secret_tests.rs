#![allow(missing_docs)]

use superba_core::{Secret, SecretBytes};

#[test]
fn secret_trims_surrounding_whitespace_only() {
    let secret = Secret::new(" \n\t sk-test middle-abcd \r\n");
    assert_eq!(secret.expose(), "sk-test middle-abcd");
    assert!(!secret.is_empty());
    assert!(Secret::new(" \n\t\r ").is_empty());
    assert!(Secret::new("").is_empty());
    assert_eq!(
        Secret::from("\u{2003}secret-value\u{2003}").expose(),
        "secret-value"
    );
    assert_eq!(
        Secret::from(String::from("  key-value  ")).expose(),
        "key-value"
    );
}

#[test]
fn secret_suffix_and_mask_obey_character_threshold_including_unicode() {
    for (input, last4, mask) in [
        ("", "", "••••"),
        ("a", "", "••••"),
        ("1234567", "", "••••"),
        ("12345678", "5678", "••••5678"),
        ("sk-long-secret-abcd", "abcd", "••••abcd"),
        ("一二三四五六七", "", "••••"),
        ("一二三四五六七八", "五六七八", "••••五六七八"),
    ] {
        let secret = Secret::new(input);
        assert_eq!(secret.last4(), last4, "input {input:?}");
        assert_eq!(secret.masked(), mask, "input {input:?}");
    }
}

#[test]
fn secret_debug_is_redacted_for_short_long_and_unicode_secrets() {
    for input in [
        "xy",
        "1234567",
        "12345678",
        "sk-long-secret-abcd",
        "一二三四五六七八",
    ] {
        let secret = Secret::new(input);
        for debug in [format!("{secret:?}"), format!("{secret:#?}")] {
            assert!(!debug.contains(input));
            assert_eq!(debug, format!("Secret(…{})", secret.last4()));
        }
    }
    assert_eq!(format!("{:?}", Secret::new("")), "Secret(…)");
}

#[test]
fn secret_bytes_exposes_bytes_but_debug_only_reports_length() {
    let raw = b"private-token-1234";
    let bytes = SecretBytes::new(raw.to_vec());
    assert_eq!(bytes.expose(), raw);
    assert_eq!(bytes.len(), raw.len());
    assert!(!bytes.is_empty());
    assert_eq!(format!("{bytes:?}"), "SecretBytes(18 bytes)");
    assert_eq!(format!("{bytes:#?}"), "SecretBytes(18 bytes)");
    assert!(!format!("{bytes:?}").contains("private-token-1234"));
    assert_eq!(SecretBytes::from(raw.to_vec()), bytes);
    let empty = SecretBytes::default();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert_eq!(format!("{empty:?}"), "SecretBytes(0 bytes)");
}
