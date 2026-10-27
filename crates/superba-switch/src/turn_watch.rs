//! Read-only activity refresh for the GUI's pending hot restart.
use super::Switcher;
use std::time::Duration;
use superba_core::{Result, TargetId, TurnActivity};

impl Switcher {
    pub(crate) async fn switch_turn_activity_impl(
        &self,
        targets: Vec<TargetId>,
    ) -> Result<Vec<TurnActivity>> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let unknown = |target_id| TurnActivity {
            target_id,
            checked_at: d.clock.now(),
            busy: Vec::new(),
            complete: false,
        };
        let mut out = Vec::new();
        if targets.contains(&TargetId::local()) {
            let session = d.app.running_session().await.ok().flatten();
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                d.app.turn_activity(&home, session.as_ref()),
            )
            .await;
            out.push(match result {
                Ok(Ok(a)) => a,
                _ => unknown(TargetId::local()),
            });
        }
        let remote_ids: Vec<_> = targets
            .into_iter()
            .filter(|id| id != &TargetId::local())
            .collect();
        if !remote_ids.is_empty() {
            let hosts =
                tokio::time::timeout(Duration::from_secs(3), d.remote.discover(&home, &[], &[]))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
            for id in remote_ids {
                let Some(host) = hosts.iter().find(|h| h.target_id == id) else {
                    out.push(unknown(id));
                    continue;
                };
                let result =
                    tokio::time::timeout(Duration::from_secs(3), d.remote.turn_activity(host))
                        .await;
                out.push(match result {
                    Ok(Ok(a)) => a,
                    _ => unknown(id),
                });
            }
        }
        Ok(out)
    }
}
