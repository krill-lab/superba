//! Test-only helpers shared by the integration tests.

#![allow(dead_code)]

pub mod fake;
pub mod fit;
pub mod ui;
pub mod update_tools;

use std::sync::Arc;

use superba_core::SwitchService;

/// A fake service for contexts that never call it.
pub fn idle_service() -> Arc<dyn SwitchService> {
    Arc::new(fake::FakeService::new(fake::FakeOptions {
        fast: true,
        ..Default::default()
    }))
}
