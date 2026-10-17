//! Fake service, area "settings" (owner B6).
//! Codex settings page: settings view and edits.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here, so the area's
//! owner edits only this file. Bodies return `NotImplemented` until the owner
//! implements them.

use superba_core::*;

use super::FakeService;

/// Fake state of the area (kept for the life of the fake world).
#[derive(Debug, Default)]
pub(super) struct State {}

/// Body of `SwitchService::settings_view` (not implemented yet).
pub(super) async fn settings_view(
    fake: &FakeService,
    request: SettingsViewRequest,
) -> Result<SettingsView> {
    let _ = (fake, request);
    Err(Error::not_implemented("fake settings_view"))
}

/// Body of `SwitchService::edit_settings` (not implemented yet).
pub(super) async fn edit_settings(
    fake: &FakeService,
    edit: SettingsEdit,
) -> Result<SettingsEditReport> {
    let _ = (fake, edit);
    Err(Error::not_implemented("fake edit_settings"))
}
