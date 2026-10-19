//! An accepted cancel and the live install commit cannot both win.
use superba_core::{Error, UpdateControl};
#[test]
fn accepted_cancel_prevents_commit() {
    let c = UpdateControl::default();
    assert!(c.can_cancel());
    assert!(c.cancel());
    assert!(matches!(c.begin_commit(), Err(Error::Cancelled)));
    assert!(c.cancelled());
}
#[test]
fn commit_must_finish_once_started() {
    let c = UpdateControl::default();
    c.begin_commit().unwrap();
    assert!(!c.cancel());
    assert!(c.committing());
    assert!(!c.can_cancel());
    c.check().unwrap();
}
#[test]
fn cancel_racing_commit_has_exactly_one_winner() {
    for _ in 0..100 {
        let c = std::sync::Arc::new(UpdateControl::default());
        let other = c.clone();
        let cancel = std::thread::spawn(move || other.cancel());
        let committed = c.begin_commit().is_ok();
        assert_ne!(cancel.join().unwrap(), committed);
    }
}
