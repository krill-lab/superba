#![allow(missing_docs)]
//! Who the GUI runs as (user bug 44): an elevated token of the signed-in
//! Windows user starts normally with a one-time notice; another account than
//! the signed-in user does not start, and says why.

use superba_core::Locale;
use superba_gui::design::widgets::{Status, Toasts};
use superba_gui::i18n::Tr;
use superba_gui::notices::{ELEVATED_SEEN_FILE, elevated_notice_due, show_elevated_once};
use superba_gui::startup::refusal;
use superba_switch::privilege::RunAs;

fn other(desktop: Option<&str>) -> RunAs {
    RunAs::OtherUser {
        account: "NT AUTHORITY\\SYSTEM".into(),
        desktop_user: desktop.map(str::to_owned),
    }
}

#[test]
fn only_another_account_keeps_the_gui_from_starting() {
    let tr = Tr::new(Locale::En);
    assert_eq!(refusal(&tr, &RunAs::User), None);
    assert_eq!(refusal(&tr, &RunAs::ElevatedUser), None);
    // sudo is refused per write (like spb), the window still opens.
    assert_eq!(refusal(&tr, &RunAs::Sudo), None);
    let text = refusal(&tr, &other(Some("PC\\alice"))).expect("refused");
    assert!(
        text.starts_with("Superba is running as NT AUTHORITY\\SYSTEM, not as PC\\alice"),
        "{text}"
    );
    assert!(text.contains("wrong user profile"), "{text}");
    let zh = refusal(&Tr::new(Locale::ZhCn), &other(None)).expect("refused");
    assert!(zh.contains("这不是本机已登录的用户"), "{zh}");
}

#[test]
fn the_elevated_notice_is_shown_once() {
    assert!(elevated_notice_due(&RunAs::ElevatedUser, false));
    assert!(!elevated_notice_due(&RunAs::ElevatedUser, true));
    assert!(!elevated_notice_due(&RunAs::User, false));
    assert!(!elevated_notice_due(&other(None), false));

    let data = tempfile::tempdir().unwrap();
    let tr = Tr::new(Locale::ZhCn);
    let mut toasts = Toasts::default();
    assert!(!show_elevated_once(
        &tr,
        &RunAs::User,
        Some(data.path()),
        &mut toasts
    ));
    assert!(toasts.is_empty());
    assert!(show_elevated_once(
        &tr,
        &RunAs::ElevatedUser,
        Some(data.path()),
        &mut toasts
    ));
    assert_eq!(toasts.titles(), ["Superba 正以管理员身份运行"]);
    assert!(data.path().join(ELEVATED_SEEN_FILE).is_file());
    // The next start: nothing.
    let mut again = Toasts::default();
    assert!(!show_elevated_once(
        &tr,
        &RunAs::ElevatedUser,
        Some(data.path()),
        &mut again
    ));
    assert!(again.is_empty());
}

#[test]
fn the_elevated_notice_explains_why_it_is_fine() {
    for (locale, title, body) in [
        (
            Locale::En,
            "Superba is running as administrator",
            [
                "That's fine",
                "your own account",
                "local router",
                "environment variables",
            ],
        ),
        (
            Locale::ZhCn,
            "Superba 正以管理员身份运行",
            ["这没有问题", "你自己的账户", "本地路由", "环境变量"],
        ),
    ] {
        let toast = superba_gui::notices::elevated_notice(&Tr::new(locale));
        assert_eq!(toast.title, title);
        assert_eq!(toast.status, Status::Idle);
        assert_eq!(toast.ttl, None, "dismissible: stays until closed");
        let text = toast.body.unwrap();
        for want in body {
            assert!(text.contains(want), "{locale:?} {want}: {text}");
        }
    }
}
