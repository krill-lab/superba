//! Cooperative cancellation with an atomic boundary before replacing binaries.
use crate::{Error, Result};
use std::sync::atomic::{AtomicU8, Ordering};

/// An install may be cancelled during preparation, but its commit must finish.
/// A cancel racing the commit wins or loses atomically; the UI never claims a
/// running commit stopped. Dropping the UI handle does not cancel an operation.
#[derive(Debug, Default)]
pub struct UpdateControl(AtomicU8);
impl UpdateControl {
    /// Request cancellation before the commit starts. False means the commit
    /// already owns the operation and must finish.
    pub fn cancel(&self) -> bool {
        self.0
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            || self.cancelled()
    }
    /// Cancellation was accepted.
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 2
    }
    /// The user may still request cancellation.
    pub fn can_cancel(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 0
    }
    /// The operation is replacing binaries; cancellation is no longer safe.
    pub fn committing(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 1
    }
    /// Check before starting another preparation step.
    pub fn check(&self) -> Result<()> {
        if self.cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    /// Enter the non-cancellable commit. Must be called before any live binary
    /// replacement, remote install command, or installer handoff.
    pub fn begin_commit(&self) -> Result<()> {
        match self
            .0
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) | Err(1) => Ok(()),
            _ => Err(Error::Cancelled),
        }
    }
}
