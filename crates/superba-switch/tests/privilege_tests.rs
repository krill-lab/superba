//! Who Superba runs as (user bug 44): an elevated token of the signed-in
//! Windows user is allowed (UAC off makes every process elevated); another
//! account than the signed-in user (SYSTEM, a service account, `runas`, a
//! scheduled task) is refused, and so is root through `sudo` on Unix.

use pretty_assertions::assert_eq;
use superba_core::{BlockReason, Catalog, Locale, MapEnv};
use superba_switch::privilege::{
    Account, RunAs, WindowsFacts, current, decide_unix, decide_windows, desktop_user,
    is_service_account, same_account,
};

const ALICE_SID: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";
const BOB_SID: &str = "S-1-5-21-1004336348-1177238915-682003330-1002";

fn alice() -> Account {
    Account::new(ALICE_SID, "PC\\alice")
}

fn bob() -> Account {
    Account::new(BOB_SID, "PC\\bob")
}

fn system() -> Account {
    Account::new("S-1-5-18", "NT AUTHORITY\\SYSTEM")
}

/// Alice is signed in at the console (session 1); this process runs as
/// `process` in `session`.
fn at_alices_desktop(process: Account, session: u32, elevated: bool) -> WindowsFacts {
    WindowsFacts {
        process: Some(process),
        elevated,
        session_id: Some(session),
        session_user: (session != 0).then(alice),
        console_user: Some(alice()),
    }
}

fn other(account: &str, desktop: Option<&str>) -> RunAs {
    RunAs::OtherUser {
        account: account.into(),
        desktop_user: desktop.map(str::to_owned),
    }
}

#[test]
fn the_signed_in_user_may_run_elevated_or_not() {
    assert_eq!(
        decide_windows(&at_alices_desktop(alice(), 1, false)),
        RunAs::User
    );
    // UAC off, or "Run as administrator": the same account and profile.
    let elevated = decide_windows(&at_alices_desktop(alice(), 1, true));
    assert_eq!(elevated, RunAs::ElevatedUser);
    assert_eq!(elevated.block_reason(), None);
    assert!(!elevated.refused());
}

#[test]
fn system_and_service_accounts_are_refused_with_or_without_a_desktop() {
    // A scheduled task as SYSTEM runs in session 0: compared with the console.
    assert_eq!(
        decide_windows(&at_alices_desktop(system(), 0, true)),
        other("NT AUTHORITY\\SYSTEM", Some("PC\\alice"))
    );
    // psexec -s -i: SYSTEM inside Alice's session.
    assert_eq!(
        decide_windows(&at_alices_desktop(system(), 1, true)),
        other("NT AUTHORITY\\SYSTEM", Some("PC\\alice"))
    );
    // Nobody signed in at all.
    let service = Account::new("S-1-5-19", "NT AUTHORITY\\LOCAL SERVICE");
    let alone = WindowsFacts {
        process: Some(service),
        session_id: Some(0),
        ..WindowsFacts::default()
    };
    assert_eq!(
        decide_windows(&alone),
        other("NT AUTHORITY\\LOCAL SERVICE", None)
    );
}

#[test]
fn another_user_through_runas_or_a_scheduled_task_is_refused() {
    // runas /user:bob in Alice's session.
    assert_eq!(
        decide_windows(&at_alices_desktop(bob(), 1, false)),
        other("PC\\bob", Some("PC\\alice"))
    );
    // A task as Bob "whether the user is logged on or not" (session 0).
    let refused = decide_windows(&at_alices_desktop(bob(), 0, true));
    assert_eq!(refused, other("PC\\bob", Some("PC\\alice")));
    assert_eq!(
        refused.block_reason(),
        Some(BlockReason::OtherUser {
            account: "PC\\bob".into(),
            desktop_user: Some("PC\\alice".into()),
        })
    );
}

#[test]
fn a_session_without_a_desktop_is_compared_with_the_console_user() {
    // Alice over SSH (session 0) while she is signed in at the console.
    assert_eq!(
        decide_windows(&at_alices_desktop(alice(), 0, true)),
        RunAs::ElevatedUser
    );
    // A user account in session 0 with nobody at the console: nothing to
    // compare with, and its files go to its own profile.
    let headless = WindowsFacts {
        process: Some(bob()),
        session_id: Some(0),
        ..WindowsFacts::default()
    };
    assert_eq!(decide_windows(&headless), RunAs::User);
}

#[test]
fn a_remote_desktop_session_is_compared_with_its_own_user_not_the_console() {
    // Bob in his Remote Desktop session (2) while Alice is at the console.
    let rdp = WindowsFacts {
        process: Some(bob()),
        elevated: false,
        session_id: Some(2),
        session_user: Some(bob()),
        console_user: Some(alice()),
    };
    assert_eq!(desktop_user(&rdp), Some(&bob()));
    assert_eq!(decide_windows(&rdp), RunAs::User);
    // An interactive session whose user is unknown falls back to the console.
    let unknown = WindowsFacts {
        session_user: None,
        ..rdp
    };
    assert_eq!(desktop_user(&unknown), Some(&alice()));
    assert_eq!(
        decide_windows(&unknown),
        other("PC\\bob", Some("PC\\alice"))
    );
}

#[test]
fn an_unreadable_token_is_refused() {
    let unknown = WindowsFacts {
        process: None,
        ..at_alices_desktop(alice(), 1, false)
    };
    assert_eq!(
        decide_windows(&unknown),
        other("(unknown account)", Some("PC\\alice"))
    );
}

#[test]
fn accounts_compare_by_sid_then_by_name() {
    // SIDs decide when both are known (names may be spelled differently).
    let renamed = Account::new(&ALICE_SID.to_lowercase(), "CONTOSO\\Alice.Smith");
    assert!(same_account(&alice(), &renamed));
    assert!(!same_account(&alice(), &Account::new(BOB_SID, "PC\\alice")));
    // A session user whose SID could not be resolved: names, ignoring case,
    // a missing domain matching on the user part.
    let by_name = |name: &str| Account {
        sid: None,
        name: name.into(),
    };
    assert!(same_account(&alice(), &by_name("pc\\ALICE")));
    assert!(same_account(&alice(), &by_name("alice")));
    assert!(!same_account(&alice(), &by_name("OTHERPC\\alice")));
    assert!(!same_account(&alice(), &by_name("bob")));
    assert!(!same_account(&by_name(""), &by_name("")));
    // Through the decision: the session user without SID is still Alice.
    let facts = WindowsFacts {
        session_user: Some(by_name("PC\\alice")),
        ..at_alices_desktop(alice(), 1, true)
    };
    assert_eq!(decide_windows(&facts), RunAs::ElevatedUser);
}

#[test]
fn service_account_sids() {
    for sid in [
        "S-1-5-18",
        "S-1-5-19",
        "S-1-5-20",
        "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
        "S-1-5-82-3006700770-424185619-1745488364-794895919-4004696415",
        "S-1-5-90-0-1",
        "S-1-5-96-0-1",
    ] {
        assert!(is_service_account(sid), "{sid}");
    }
    for sid in [
        ALICE_SID,
        "S-1-12-1-1234567890-1234567890-1234567890-1234567890", // Entra ID (Azure AD)
        "S-1-5-32-544",                                         // Administrators group
        "S-1-5-80",
        "S-1-5-180",
    ] {
        assert!(!is_service_account(sid), "{sid}");
    }
}

#[test]
fn unix_refuses_only_root_through_sudo() {
    assert_eq!(decide_unix(true, true), RunAs::Sudo);
    assert_eq!(
        decide_unix(true, true).block_reason(),
        Some(BlockReason::Elevated)
    );
    // A real root login, or SUDO_USER left over in a normal user's shell.
    assert_eq!(decide_unix(false, true), RunAs::User);
    assert_eq!(decide_unix(true, false), RunAs::User);
    assert_eq!(decide_unix(false, false), RunAs::User);
}

#[test]
fn the_refusals_say_who_runs_and_who_is_signed_in() {
    let en = Catalog::new(Locale::En);
    let zh = Catalog::new(Locale::ZhCn);
    let reason = other("NT AUTHORITY\\SYSTEM", Some("PC\\alice"))
        .block_reason()
        .unwrap()
        .text();
    assert_eq!(
        en.text(&reason),
        "Superba is running as NT AUTHORITY\\SYSTEM, not as PC\\alice, the user signed in at \
         this computer: its files would go to the wrong user profile. Start Superba from \
         PC\\alice's desktop."
    );
    assert!(zh.text(&reason).contains("NT AUTHORITY\\SYSTEM"));
    assert!(zh.text(&reason).contains("PC\\alice 的桌面"));
    let alone = other("NT AUTHORITY\\LOCAL SERVICE", None)
        .block_reason()
        .unwrap()
        .text();
    assert!(
        en.text(&alone)
            .contains("which is not a user signed in at this computer")
    );
    // The sudo refusal no longer mentions administrators (they are allowed).
    assert_eq!(
        en.text(&BlockReason::Elevated.text()),
        "Do not run Superba with sudo."
    );
    assert_eq!(
        zh.text(&BlockReason::Elevated.text()),
        "请不要用 sudo 运行 Superba。"
    );
}

#[test]
fn this_test_process_may_write() {
    // The test runner is the signed-in user (also on the Windows CI runners,
    // elevated administrators of their own desktop), never root through sudo.
    let env = MapEnv::new();
    let run_as = current(&env);
    assert!(!run_as.refused(), "{run_as:?}");
    #[cfg(windows)]
    {
        let facts = superba_switch::privilege::windows_facts().expect("facts");
        let process = facts.process.as_ref().expect("the token's user");
        assert!(
            process
                .sid
                .as_deref()
                .is_some_and(|s| s.starts_with("S-1-")),
            "{facts:?}"
        );
        assert!(process.name.contains('\\'), "{facts:?}");
    }
}
