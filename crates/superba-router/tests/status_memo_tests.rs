//! The Router page polls `RouterManager::overview` every 2 s (user request 34):
//! the service manager (`systemctl`, `launchctl`, `tasklist`: a process each) is
//! asked once and again only after a lifecycle call or when the router's
//! liveness changes.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use superba_core::{DataDir, Result};
use superba_router::manager::RouterManager;
use superba_router::service::{ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

#[derive(Debug, Default)]
struct CountingService {
    status_calls: Arc<AtomicUsize>,
}

impl ServiceManager for CountingService {
    fn kind(&self) -> ServiceKind {
        ServiceKind::SystemdUser
    }
    fn install(&self, _spec: &ServiceSpec) -> Result<ServiceStatus> {
        Err(superba_core::Error::not_implemented("install"))
    }
    fn uninstall(&self, _spec: &ServiceSpec) -> Result<()> {
        Ok(())
    }
    fn status(&self, _spec: &ServiceSpec) -> Result<ServiceStatus> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ServiceStatus {
            kind: ServiceKind::SystemdUser,
            installed: true,
            running: Some(false),
            pid: None,
            detail: Some("inactive".into()),
        })
    }
    fn restart(&self, _spec: &ServiceSpec) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn the_polled_overview_asks_the_service_manager_once_per_state() {
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let manager = RouterManager::new(
        &DataDir::new(temp.path().join("data")),
        PathBuf::from("/nonexistent/superba-router"),
        Box::new(CountingService {
            status_calls: calls.clone(),
        }),
    );
    // A minute of the Router page (the router is down: no runtime file).
    for _ in 0..30 {
        let overview = manager.overview().await;
        assert!(overview.service.unwrap().installed);
        assert!(!overview.liveness.is_up());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A lifecycle call: asked again, once.
    manager.uninstall_service().unwrap();
    for _ in 0..30 {
        manager.overview().await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Another data directory has its own answer.
    let other = RouterManager::new(
        &DataDir::new(temp.path().join("other")),
        PathBuf::from("/nonexistent/superba-router"),
        Box::new(CountingService {
            status_calls: calls.clone(),
        }),
    );
    other.overview().await;
    manager.overview().await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
