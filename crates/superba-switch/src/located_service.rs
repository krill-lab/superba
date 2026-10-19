//! Production service boundary for data relocation. Calls keep a shared OS lock
//! until their future finishes, including blocking workers awaited by that call.
//! A move drains this instance's calls, takes the exclusive OS lock, then replaces
//! its dependencies. Other app/CLI instances reopen the pointer on their next call.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use superba_core::data::{
    DataCleanupReport, DataMovePhase, DataMoveProgress, DataMoveProgressSink, DataMoveReport,
};
use superba_core::{Error, LocalContext, Result};
use superba_storage::DataLock;
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::{Switcher, SwitcherDeps};

mod delegates;

pub(crate) struct LocatedService {
    inner: Arc<RwLock<Switcher>>,
    anchor: PathBuf,
    context: LocalContext,
}

fn service(ctx: LocalContext) -> Switcher {
    let dir = ctx.data_dir.clone();
    Switcher::new(SwitcherDeps::with_defaults(ctx)).with_data_guard(dir)
}

impl LocatedService {
    pub(crate) fn new(ctx: LocalContext) -> Self {
        Self {
            inner: Arc::new(RwLock::new(service(ctx.clone()))),
            anchor: ctx.data_dir.anchor().to_path_buf(),
            context: ctx,
        }
    }

    fn current_context(&self) -> Result<LocalContext> {
        let mut context = self.context.clone();
        context.data_dir = superba_storage::resolve(&self.anchor)?;
        Ok(context)
    }

    async fn read(&self) -> Result<(RwLockReadGuard<'_, Switcher>, DataLock)> {
        loop {
            let inner = self.inner.read().await;
            let lock = DataLock::shared(&self.anchor)?;
            let context = self.current_context()?;
            if context.data_dir == inner.deps().ctx.data_dir
                && !self.anchor.join(superba_storage::MOVE_FILE).exists()
            {
                return Ok((inner, lock));
            }
            drop(lock);
            drop(inner);
            let mut inner = self.inner.write().await;
            let _lock = DataLock::exclusive(&self.anchor)?;
            inner.monitor().stop_and_drain().await;
            crate::data::recover(&self.context).await?;
            *inner = service(self.current_context()?);
        }
    }

    async fn move_to(
        &self,
        directory: PathBuf,
        progress: DataMoveProgressSink,
    ) -> Result<DataMoveReport> {
        // A dedicated worker owns the transaction and locks even if its UI future
        // is dropped. Process death is covered by the durable journal.
        let inner = self.inner.clone();
        let anchor = self.anchor.clone();
        let context = self.context.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("superba-data-move".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| Error::other(e.to_string()))
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            Self::move_worker(inner, anchor, context, directory, progress).await
                        })
                    });
                let _ = tx.send(result);
            })
            .map_err(|e| Error::other(e.to_string()))?;
        rx.await
            .map_err(|e| Error::other(format!("data move worker: {e}")))?
    }

    async fn move_worker(
        inner: Arc<RwLock<Switcher>>,
        anchor: PathBuf,
        mut context: LocalContext,
        directory: PathBuf,
        progress: DataMoveProgressSink,
    ) -> Result<DataMoveReport> {
        if !progress(DataMoveProgress::default()) {
            return Err(Error::Cancelled);
        }
        let mut inner = tokio::time::timeout(Duration::from_secs(60), inner.write_owned())
            .await
            .map_err(|_| Error::Locked {
                what: "data.busy".into(),
            })?;
        let _lock = DataLock::exclusive(&anchor)?;
        crate::data::recover(&context).await?;
        context.data_dir = superba_storage::resolve(&anchor)?;
        if inner.deps().ctx.data_dir != context.data_dir {
            *inner = service(context.clone());
        }
        inner.monitor().stop_and_drain().await;
        let result = crate::data::move_data(&inner, directory, progress.clone()).await;
        // Rebuild even after failure: the monitor was drained, and every dependency
        // must agree with the recovered pointer before accepting another call.
        if let Ok(data) = superba_storage::resolve(&anchor) {
            context.data_dir = data;
            *inner = service(context);
        }
        if result.is_ok() {
            let _ = progress(DataMoveProgress {
                phase: DataMovePhase::Done,
                copied_bytes: result.as_ref().map_or(0, |r| r.copied_bytes),
                total_bytes: result.as_ref().map_or(0, |r| r.copied_bytes),
            });
        }
        result
    }

    async fn cleanup(&self) -> Result<DataCleanupReport> {
        let mut inner = self.inner.write().await;
        let _lock = DataLock::exclusive(&self.anchor)?;
        crate::data::recover(&self.context).await?;
        let context = self.current_context()?;
        if inner.deps().ctx.data_dir != context.data_dir {
            *inner = service(context.clone());
        }
        inner.monitor().stop_and_drain().await;
        let result = crate::data::cleanup(&inner).await;
        *inner = service(context);
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn production_wrapper_forwards_every_service_method() {
        let contract = include_str!("../../superba-core/src/traits.rs")
            .split("pub trait SwitchService: Send + Sync {")
            .nth(1)
            .unwrap();
        let forwards = include_str!("located_service/delegates.rs");
        for declaration in contract.split("fn ").skip(1) {
            let name = declaration.split('(').next().unwrap().trim();
            assert!(
                forwards.contains(&format!("fn {name}(")),
                "LocatedService must forward {name}; falling back to the trait default loses the operation"
            );
        }
    }
}
