//! What a hot restart that saw no new Codex core says (user bug 62(a)).

#![allow(missing_docs)]

use std::time::Duration;

use superba_appctl::{Tunables, restart_timeout_detail};

#[test]
fn a_restart_timeout_names_the_app_the_wait_and_the_previous_app_server() {
    assert_eq!(
        restart_timeout_detail(4208, &[3136], &[3136], Duration::from_millis(30_200)),
        "a new Codex core (codex app-server child of ChatGPT, pid 4208) within 30 s of the \
         restart request; the previous one (pid 3136) is still running, so the app did not \
         act on the request"
    );
    assert_eq!(
        restart_timeout_detail(4208, &[3136, 3137], &[], Duration::from_secs(30)),
        "a new Codex core (codex app-server child of ChatGPT, pid 4208) within 30 s of the \
         restart request; the previous one (pid 3136, 3137) exited and none replaced it"
    );
}

#[test]
fn the_restart_wait_covers_a_slow_machine() {
    // 10 s were not enough on the Windows VMs (the app ends the old
    // app-server before it starts the new one).
    assert!(Tunables::default().restart_timeout >= Duration::from_secs(30));
}
