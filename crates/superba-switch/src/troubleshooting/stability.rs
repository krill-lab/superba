//! Three fresh observations per enabled supplier line, two seconds apart.

use super::{check, record, text};
use crate::Switcher;
use std::time::Duration;
use superba_core::*;

#[derive(Debug)]
struct Measurement {
    id: LineId,
    name: LocalizedText,
    samples: Vec<u32>,
}

impl Measurement {
    fn average(&self) -> Option<u32> {
        (!self.samples.is_empty()).then(|| {
            (self.samples.iter().map(|v| u64::from(*v)).sum::<u64>() / self.samples.len() as u64)
                as u32
        })
    }

    fn better_than(&self, other: &Self) -> bool {
        self.average().is_some()
            && self.samples.len() >= other.samples.len()
            && self.average().unwrap() < other.average().unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_comparison_does_not_trade_reliability_for_speed() {
        let measured = |samples: Vec<u32>| Measurement {
            id: LineId::new("test"),
            name: LocalizedText::default(),
            samples,
        };
        let current = measured(vec![300, 250, 350]);
        assert_eq!(current.average(), Some(300));
        assert!(!measured(vec![20, 30]).better_than(&current));
        assert!(measured(vec![80, 90, 100]).better_than(&current));
        assert!(!measured(vec![]).better_than(&current));
        assert!(current.better_than(&measured(vec![])));
        assert_eq!(measured(vec![u32::MAX; 3]).average(), Some(u32::MAX));
    }
}

impl Switcher {
    pub(super) async fn troubleshooting_stability(
        &self,
        profile: &Profile,
        state: &ConfigState,
        report: &mut TroubleshootingReport,
        progress: &TroubleshootingProgressSink,
    ) -> Result<()> {
        let site = self.site(&profile.site_id).await?;
        let mut override_site = if site.kind == SiteKind::Compatible {
            profile
                .base_url_override
                .as_ref()
                .map(|base| {
                    crate::new_compatible_site(
                        SiteId::new("diagnostic-endpoint"),
                        &profile.name,
                        base,
                    )
                })
                .transpose()?
        } else {
            None
        };
        if let Some(endpoint) = &mut override_site {
            endpoint.lines[0].id = LineId::new("configured-endpoint");
            endpoint.lines[0].name = text("configured_endpoint", "Configured endpoint");
        }
        let current = if report.routed {
            None
        } else {
            override_site
                .as_ref()
                .map(|s| s.lines[0].id.clone())
                .or_else(|| report.line_id.clone())
                .or_else(|| profile.line_id.clone())
                .or_else(|| profile.enabled_line_ids(&site).first().cloned())
        };
        let mut measured = Vec::new();
        let mut targets: Vec<_> = site
            .lines
            .iter()
            .filter(|line| !profile.disabled_lines.contains(&line.id))
            .filter(|line| {
                !override_site.as_ref().is_some_and(|s| {
                    s.lines[0].origin == line.origin
                        && s.endpoints.models_unauth == site.endpoints.models_unauth
                })
            })
            .map(|line| (&site, line))
            .collect();
        if let Some(endpoint) = &override_site {
            targets.push((endpoint, &endpoint.lines[0]));
        }
        for (probe_site, line) in targets {
            let mut result = Measurement {
                id: line.id.clone(),
                name: line.name.clone(),
                samples: Vec::new(),
            };
            let mut samples = Vec::new();
            for attempt in 0..3 {
                if !progress(TroubleshootingProgress::Backend) {
                    return Err(Error::Cancelled);
                }
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                if !progress(TroubleshootingProgress::Backend) {
                    return Err(Error::Cancelled);
                }
                let sample = tokio::time::timeout(
                    Duration::from_secs(30),
                    self.deps.prober.latency_sample(probe_site, line),
                )
                .await
                .ok()
                .and_then(Result::ok)
                .flatten();
                if let Some(ms) = sample {
                    result.samples.push(ms);
                }
                samples.push(sample.map_or_else(|| "—".to_owned(), |ms| format!("{ms} ms")));
            }
            let failed = 3 - result.samples.len();
            record(
                report,
                progress,
                check(
                    format!("stability:line:{}", line.id),
                    text(
                        if current.as_ref() == Some(&line.id) {
                            "check.line_current"
                        } else {
                            "check.line"
                        },
                        "Line · {line}",
                    )
                    .arg_text("line", line.name.clone()),
                    if failed == 3 {
                        TroubleshootingStatus::Failed
                    } else if failed > 0 || result.average().is_some_and(|ms| ms > 200) {
                        TroubleshootingStatus::Warning
                    } else {
                        TroubleshootingStatus::Passed
                    },
                    text(
                        "line_measurement",
                        "Samples: {samples}; average {average} ms; failed {failed}/3 ({loss}%).",
                    )
                    .arg("samples", samples.join(" / "))
                    .arg(
                        "average",
                        result
                            .average()
                            .map_or_else(|| "—".into(), |ms| ms.to_string()),
                    )
                    .arg("failed", failed)
                    .arg("loss", failed * 100 / 3),
                ),
            )?;
            measured.push(result);
        }
        if measured.is_empty() {
            record(
                report,
                progress,
                check(
                    "stability:empty",
                    text("issue.stability", "Unstable or slow sessions"),
                    TroubleshootingStatus::Unknown,
                    text(
                        "websocket_not_checked",
                        "No enabled supplier lines are available to check.",
                    ),
                ),
            )?;
        }
        if let Some(active) = current
            .as_ref()
            .and_then(|id| measured.iter().find(|m| &m.id == id))
        {
            if let Some(best) = measured
                .iter()
                .filter(|m| m.better_than(active))
                .min_by_key(|m| (3 - m.samples.len(), m.average()))
            {
                record(report, progress, check("stability:switch", text("line_recommendation", "Suggested line"), TroubleshootingStatus::Warning,
                    text("line_switch", "Try {line}: it has lower latency and no higher probe failure rate than the current line.").arg_text("line", best.name.clone())))?;
            }
        }
        let doc = self
            .codex_file_impl(TargetId::local(), CodexFileKind::ConfigToml)
            .await
            .ok();
        let ws = doc
            .and_then(|d| d.text())
            .and_then(|contents| {
                superba_config::settings::read_config_values(Some(contents.as_bytes())).ok()
            })
            .is_some_and(|values| {
                let provider = state.model_provider.as_deref().unwrap_or("openai");
                let supports = values
                    .get(&format!("model_providers.{provider}.supports_websockets"))
                    .and_then(SettingValue::as_bool);
                supports != Some(false)
                    && (supports == Some(true)
                        || values
                            .get("features.responses_websockets")
                            .and_then(SettingValue::as_bool)
                            == Some(true)
                        || values
                            .get("features.responses_websockets_v2")
                            .and_then(SettingValue::as_bool)
                            == Some(true))
            });
        if ws
            && measured
                .iter()
                .any(|m| m.average().is_some_and(|ms| ms > 200))
        {
            record(
                report,
                progress,
                check(
                    "stability:ws",
                    text("check.websocket_config", "WebSocket configuration"),
                    TroubleshootingStatus::Warning,
                    text(
                        "line_ws",
                        "Some lines average over 200 ms. WebSocket is sensitive to network quality; try disabling WS if using these lines.",
                    ),
                ),
            )?;
        }
        Ok(())
    }
}
