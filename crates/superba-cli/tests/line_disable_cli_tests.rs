//! `spb profile edit --disable-line / --enable-line` (user bug 108): a disabled
//! line is saved with the profile, the last enabled line cannot be disabled,
//! and the edit never switches.

mod common;

use common::*;

#[tokio::test]
async fn disabling_and_enabling_lines_saves_the_profile() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["profile", "edit", "work", "--disable-line", "cdn"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let saves = service.calls().saves;
    let saved = &saves.last().unwrap().profile;
    assert!(saved.is_line_disabled(&"cdn".into()));
    assert_eq!(saved.disabled_lines.len(), 1);
    assert!(service.calls().plans.is_empty(), "never switches");

    // `cdn` is still disabled: the other two cannot both go.
    let (code, stdout, stderr) = invoke(
        service.clone(),
        &[
            "profile",
            "edit",
            "work",
            "--disable-line",
            "overseas",
            "--disable-line",
            "cn-fast",
        ],
    )
    .await;
    assert_ne!(code, 0, "the last enabled line stays: {stdout}");
    assert!(stderr.contains("last enabled line"), "{stderr}");

    let (code, stdout, stderr) = invoke(
        service.clone(),
        &["profile", "edit", "work", "--enable-line", "cdn"],
    )
    .await;
    assert_eq!(code, 0, "{stdout}{stderr}");
    let saves = service.calls().saves;
    assert!(saves.last().unwrap().profile.disabled_lines.is_empty());

    let (code, _, stderr) = invoke(
        service.clone(),
        &["profile", "edit", "work", "--disable-line", "nowhere"],
    )
    .await;
    assert_ne!(code, 0);
    assert!(stderr.contains("nowhere"), "{stderr}");
}
