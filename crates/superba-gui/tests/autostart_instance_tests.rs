use std::sync::{Arc, Mutex};
use superba_gui::instance::{self, InstanceLock, Request};

#[test]
fn process_lifetime_lock_prevents_simultaneous_starts_and_recovers_after_exit() {
    let dir = tempfile::tempdir().unwrap();
    let first = InstanceLock::acquire(dir.path()).unwrap().unwrap();
    assert!(InstanceLock::acquire(dir.path()).unwrap().is_none());
    drop(first);
    assert!(InstanceLock::acquire(dir.path()).unwrap().is_some());
}

#[test]
fn login_request_is_authenticated_and_does_not_become_show() {
    let dir = tempfile::tempdir().unwrap();
    let first = instance::claim(dir.path(), Request::Autostart)
        .unwrap()
        .unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    let listener = instance::listen(dir.path(), move |r| sink.lock().unwrap().push(r)).unwrap();
    assert!(
        instance::claim(dir.path(), Request::Autostart)
            .unwrap()
            .is_none()
    );
    // The IPC reply can arrive just before the callback.
    for _ in 0..100 {
        if !received.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(*received.lock().unwrap(), vec![Request::Autostart]);
    assert!(
        instance::parse_line(
            "superba-gui/1 wrong-token autostart",
            &listener.endpoint().token
        )
        .is_none()
    );
    assert_eq!(Request::parse("autostart"), Some(Request::Autostart));
    drop(listener);
    drop(first);
    assert!(
        instance::claim(dir.path(), Request::Autostart)
            .unwrap()
            .is_some()
    );
}
