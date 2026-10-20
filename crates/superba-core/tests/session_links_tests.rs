//! Thread links (the desktop app's `codex://threads/<id>?hostId=…`) and the
//! session browser's fuzzy match.

use superba_core::{form_urlencode, fuzzy_fields, fuzzy_score, thread_link};

const ID: &str = "01a158c7-a09e-7cd2-ba08-41459547a695";

#[test]
fn local_threads_link_without_a_host() {
    assert_eq!(thread_link(ID, "local"), format!("codex://threads/{ID}"));
    assert_eq!(thread_link(ID, ""), format!("codex://threads/{ID}"));
}

#[test]
fn remote_hosts_are_encoded_like_url_search_params() {
    assert_eq!(
        thread_link(ID, "remote-ssh-discovered:bj"),
        format!("codex://threads/{ID}?hostId=remote-ssh-discovered%3Abj")
    );
    assert_eq!(
        thread_link(ID, "remote-wsl:Ubuntu-24.04"),
        format!("codex://threads/{ID}?hostId=remote-wsl%3AUbuntu-24.04")
    );
    // An alias the app already escaped with encodeURIComponent is escaped again.
    assert_eq!(
        thread_link(ID, "remote-ssh-discovered:my%20host"),
        format!("codex://threads/{ID}?hostId=remote-ssh-discovered%3Amy%2520host")
    );
    assert_eq!(form_urlencode("a b~*'/é"), "a+b%7E*%27%2F%C3%A9");
}

#[test]
fn fuzzy_prefers_contiguous_and_word_starts() {
    assert!(fuzzy_score("dbx", "database migration").is_none());
    assert!(fuzzy_score("dtbs", "database migration").is_some());
    let exact = fuzzy_score("migr", "Plan the database migration").unwrap();
    let scattered = fuzzy_score("mgrt", "Plan the database migration").unwrap();
    assert!(exact > scattered);
    assert!(
        fuzzy_score("PLAN", "plan the database").unwrap()
            > fuzzy_score("plan", "explanation").unwrap()
    );
    assert_eq!(fuzzy_score("", "x"), Some(0));
    assert!(fuzzy_score("会话", "会话迁移").is_some());
}

#[test]
fn every_term_must_match_some_field() {
    let fields = [("Fix flaky test", 3), ("/srv/app", 1), (ID, 1)];
    assert!(fuzzy_fields("flaky srv", &fields).is_some());
    assert!(fuzzy_fields("flaky 01a158", &fields).is_some());
    assert!(fuzzy_fields("flaky zzz", &fields).is_none());
    // A title hit outranks the same hit in a path.
    let title = fuzzy_fields("app", &[("app", 3), ("/x", 1)]).unwrap();
    let path = fuzzy_fields("app", &[("x", 3), ("/srv/app", 1)]).unwrap();
    assert!(title > path);
}
