//! Downloads: verified against the published checksum before they are used,
//! a mismatch deletes the file, a cancel keeps the partial file and the next
//! attempt resumes it.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use superba_core::{
    Error, NoToolProgress, SharedToolProgress, ToolId, ToolProgress, ToolProgressSink, ToolStage,
};
use superba_tools::http::{self, Checksum, Download, part_path};
use support::{Mock, Route, sha256_hex, sha512_b64};

fn body() -> Vec<u8> {
    (0..300_000_u32).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn a_download_is_verified_by_sha256_and_sha512() {
    let mock = Mock::start().await;
    let data = body();
    mock.route("/f.bin", Route::ok(data.clone()));
    let dir = tempfile::tempdir().unwrap();
    let client = http::client().unwrap();
    for (name, sum) in [
        ("a.bin", Checksum::Sha256(sha256_hex(&data))),
        ("b.bin", Checksum::Sha512(sha512_b64(&data))),
    ] {
        let dest = dir.path().join(name);
        let url = mock.url("/f.bin");
        let d = Download {
            tool: ToolId::Pi,
            url: &url,
            dest: &dest,
            checksum: Some(&sum),
            size: Some(data.len() as u64),
        };
        let sha = http::download(&client, &d, &NoToolProgress).await.unwrap();
        assert_eq!(sha, sha256_hex(&data));
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
    }
}

#[tokio::test]
async fn a_checksum_mismatch_is_refused_and_nothing_is_kept() {
    let mock = Mock::start().await;
    mock.route("/f.bin", Route::ok(body()));
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("f.bin");
    let wrong = Checksum::Sha256("0".repeat(64));
    let url = mock.url("/f.bin");
    let d = Download {
        tool: ToolId::Opencode,
        url: &url,
        dest: &dest,
        checksum: Some(&wrong),
        size: None,
    };
    let err = http::download(&http::client().unwrap(), &d, &NoToolProgress)
        .await
        .unwrap_err();
    let Error::Invalid { message } = err else {
        panic!("Invalid expected, got {err:?}");
    };
    assert!(
        message.contains("does not match its published sha256"),
        "{message}"
    );
    assert!(!dest.exists(), "a file that fails verification is not used");
    assert!(!part_path(&dest).exists(), "and not kept");
}

#[tokio::test]
async fn a_size_mismatch_is_refused() {
    let mock = Mock::start().await;
    let data = body();
    mock.route("/f.bin", Route::ok(data.clone()));
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("f.bin");
    let sum = Checksum::Sha256(sha256_hex(&data));
    let url = mock.url("/f.bin");
    let d = Download {
        tool: ToolId::Pi,
        url: &url,
        dest: &dest,
        checksum: Some(&sum),
        size: Some(1000),
    };
    let err = http::download(&http::client().unwrap(), &d, &NoToolProgress)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert!(!dest.exists());
}

/// Cancels after the first progress report with bytes.
#[derive(Default)]
struct CancelEarly {
    reports: AtomicUsize,
}

impl ToolProgressSink for CancelEarly {
    fn report(&self, p: ToolProgress) {
        if p.stage == ToolStage::Downloading && p.done > 0 {
            self.reports.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn cancel_requested(&self) -> bool {
        self.reports.load(Ordering::SeqCst) > 0
    }
}

#[tokio::test]
async fn a_cancel_keeps_the_partial_file_and_the_next_attempt_resumes() {
    let mock = Mock::start().await;
    let data: Vec<u8> = (0..8_000_000_u32).map(|i| (i % 253) as u8).collect();
    mock.route("/big.bin", Route::ok(data.clone()));
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("big.bin");
    let sum = Checksum::Sha256(sha256_hex(&data));
    let url = mock.url("/big.bin");
    let d = Download {
        tool: ToolId::ClaudeCode,
        url: &url,
        dest: &dest,
        checksum: Some(&sum),
        size: Some(data.len() as u64),
    };
    let client = http::client().unwrap();
    let err = http::download(&client, &d, &CancelEarly::default())
        .await
        .unwrap_err();
    assert_eq!(err, Error::Cancelled);
    let part = part_path(&dest);
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    assert!(!dest.exists());
    let shared = Arc::new(SharedToolProgress::default());
    http::download(&client, &d, shared.as_ref()).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    if have > 0 {
        assert!(
            mock.log()
                .iter()
                .any(|s| s.range.as_deref() == Some(&format!("bytes={have}-"))),
            "resumed with a Range request: {:?}",
            mock.log()
        );
    }
    let last = shared.latest().unwrap();
    assert_eq!(last.tool, ToolId::ClaudeCode);
    assert_eq!(last.stage, ToolStage::Verifying);
}

#[tokio::test]
async fn metadata_404_is_none_and_an_error_names_the_url() {
    let mock = Mock::start().await;
    let client = http::client().unwrap();
    assert_eq!(
        http::get_optional(&client, &mock.url("/nope"))
            .await
            .unwrap(),
        None
    );
    mock.route("/boom", Route::status(503));
    let err = http::get_text(&client, &mock.url("/boom"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("/boom"), "{err}");
    assert!(matches!(err, Error::Http { status: 503, .. }));
}

#[test]
fn npm_integrity_is_a_sha512() {
    assert_eq!(
        Checksum::from_integrity("sha1-abc sha512-QUJD"),
        Some(Checksum::Sha512("QUJD".into()))
    );
    assert_eq!(Checksum::from_integrity("sha1-abc"), None);
}
