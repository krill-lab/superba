//! User-triggered observations. Repairs remain ordinary, confirmed switches.

mod cleanup;
mod stability;
mod versions;

use superba_config::codex_files::{CheckContext, validate};
use superba_core::*;

use crate::Switcher;

fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(format!("troubleshooting.{key}"), fallback)
}

fn check(
    id: impl Into<String>,
    title: LocalizedText,
    status: TroubleshootingStatus,
    detail: LocalizedText,
) -> TroubleshootingCheck {
    TroubleshootingCheck {
        id: id.into(),
        title,
        detail,
        status,
        retry: None,
    }
}

fn retryable(mut check: TroubleshootingCheck, retry: TroubleshootingRetry) -> TroubleshootingCheck {
    if check.status != TroubleshootingStatus::Passed {
        check.retry = Some(retry);
    }
    check
}

fn record(
    report: &mut TroubleshootingReport,
    progress: &TroubleshootingProgressSink,
    check: TroubleshootingCheck,
) -> Result<()> {
    report.checks.push(check.clone());
    if progress(TroubleshootingProgress::Check(check)) {
        Ok(())
    } else {
        Err(Error::Cancelled)
    }
}

impl Switcher {
    pub(crate) async fn troubleshooting_check_impl(
        &self,
        request: TroubleshootingRequest,
        progress: TroubleshootingProgressSink,
    ) -> Result<TroubleshootingReport> {
        use TroubleshootingIssue as Issue;
        use TroubleshootingStatus as Status;
        let home = self.deps.ctx.codex_home();
        let state = self.deps.config.read_state(&home).await?;
        let set = self.deps.profiles.load().await?;
        let profile = state
            .active
            .as_ref()
            .and_then(|a| a.profile_id.as_ref())
            .and_then(|id| set.profile(id));
        let mut report = TroubleshootingReport {
            issue: request.issue,
            checked_at: self.deps.clock.now(),
            profile_id: profile.map(|p| p.id.clone()),
            line_id: state.active.as_ref().and_then(|a| a.line_id.clone()),
            routed: set.settings.router.enabled,
            checks: Vec::new(),
            models: Vec::new(),
            legacy_providers: Vec::new(),
            reapply: false,
            config_ok: false,
            restart_recommended: false,
            can_reinitialize: profile.is_some_and(|p| p.builtin != Some(BuiltinProfile::Original)),
            cleanup: None,
        };
        if matches!(request.issue, Issue::Policy | Issue::Server | Issue::Other) {
            return Ok(report);
        }
        if let Some(retry) = &request.retry {
            let provider = set
                .profile(retry.profile())
                .ok_or_else(|| Error::invalid("The supplier no longer exists."))?;
            let mut actual = provider.clone();
            if report.profile_id.as_ref() == Some(&provider.id) && !report.routed {
                actual.line_id = report.line_id.clone().or(actual.line_id);
            }
            self.troubleshooting_provider(&set, &state, &actual, &request, &mut report, &progress)
                .await?;
            return Ok(report);
        }
        if !progress(TroubleshootingProgress::Configuration) {
            return Err(Error::Cancelled);
        }
        self.monitor.record(EnvironmentEvent::Refresh);
        self.troubleshooting_versions(&mut report, &progress)
            .await?;
        // The first missing-model step refreshes first; configuration is checked
        // after the user identifies a model, or asks for the normal request check.
        let configuration = request.issue != Issue::Sessions
            && (request.issue != Issue::Models || request.model.is_some());
        if configuration {
            self.troubleshooting_config(&state, profile, request.speed, &mut report, &progress)
                .await?;
        }
        if profile.is_none() && request.issue != Issue::Sessions {
            record(
                &mut report,
                &progress,
                check(
                    "provider",
                    text("check.provider", "Applied provider"),
                    Status::Unknown,
                    text(
                        "provider_missing",
                        "Select and apply a provider before running provider checks.",
                    ),
                ),
            )?;
        }
        if matches!(request.issue, Issue::Sessions | Issue::All) {
            match self.deps.sessions.scan(&home).await {
                Ok(sessions) => {
                    report.legacy_providers = sessions
                        .distribution()
                        .into_iter()
                        .filter(|(id, n)| id != codex::PROVIDER_OPENAI && *n > 0)
                        .collect();
                    let count: u32 = report.legacy_providers.iter().map(|(_, n)| n).sum();
                    let status = if count > 0 || !sessions.warnings.is_empty() {
                        Status::Warning
                    } else {
                        Status::Passed
                    };
                    record(&mut report, &progress, check("sessions", text("check.sessions", "Conversation providers"), status,
                        text("sessions_found", "{count} conversations still use a provider other than openai; {warnings} scan notices.")
                            .arg("count", count).arg("warnings", sessions.warnings.len())))?;
                }
                Err(e) => record(
                    &mut report,
                    &progress,
                    check(
                        "sessions",
                        text("check.sessions", "Conversation providers"),
                        Status::Unknown,
                        e.to_text(),
                    ),
                )?,
            }
        }
        if matches!(request.issue, Issue::Sessions) {
            return Ok(report);
        }
        let profiles: Vec<_> = if report.routed && request.issue != Issue::Stability {
            set.settings
                .router
                .members
                .iter()
                .filter_map(|id| set.profile(id))
                .collect()
        } else {
            profile.into_iter().collect()
        };
        if !progress(TroubleshootingProgress::Backend) {
            return Err(Error::Cancelled);
        }
        for provider in profiles {
            if provider.is_builtin() {
                record(
                    &mut report,
                    &progress,
                    check(
                        format!("backend:{}", provider.id),
                        text("check.backend", "Supplier backend").arg("provider", &provider.name),
                        Status::Unknown,
                        text(
                            "official_check",
                            "The official login is managed by Codex. Check service status and try a new conversation.",
                        ),
                    ),
                )?;
                continue;
            }
            let mut actual = provider.clone();
            if report.profile_id.as_ref() == Some(&provider.id) && !report.routed {
                actual.line_id = report.line_id.clone().or(actual.line_id);
            }
            self.troubleshooting_provider(&set, &state, &actual, &request, &mut report, &progress)
                .await?;
            if request.issue == Issue::Stability {
                self.troubleshooting_stability(&actual, &state, &mut report, &progress)
                    .await?;
            }
        }
        if report.routed && configuration {
            match self.router_status_impl().await {
                Ok(router) => {
                    let ok = router.codex_routed && router.running;
                    record(
                        &mut report,
                        &progress,
                        check(
                            "router",
                            text("check.router", "Local router"),
                            if ok { Status::Passed } else { Status::Failed },
                            text(
                                if ok { "router_ok" } else { "router_failed" },
                                if ok {
                                    "Codex points at the running router."
                                } else {
                                    "The router is not healthy or Codex is not using it. Check the Router page."
                                },
                            ),
                        ),
                    )?;
                }
                Err(e) => record(
                    &mut report,
                    &progress,
                    check(
                        "router",
                        text("check.router", "Local router"),
                        Status::Unknown,
                        e.to_text(),
                    ),
                )?,
            }
        }
        let feature = matches!(request.issue, Issue::Speed | Issue::WebSocket);
        report.restart_recommended = feature
            && report.config_ok
            && !report.checks.is_empty()
            && report
                .checks
                .iter()
                // Update availability does not change feature compatibility.
                .filter(|c| !c.id.starts_with("update:"))
                .all(|c| c.status == Status::Passed);
        Ok(report)
    }
}

impl Switcher {
    async fn troubleshooting_provider(
        &self,
        set: &ProfileSet,
        state: &ConfigState,
        profile: &Profile,
        request: &TroubleshootingRequest,
        report: &mut TroubleshootingReport,
        progress: &TroubleshootingProgressSink,
    ) -> Result<()> {
        use TroubleshootingStatus as Status;
        let retry = request.retry.as_ref();
        let models_only = matches!(retry, Some(TroubleshootingRetry::Models { .. }));
        let backend_only = matches!(retry, Some(TroubleshootingRetry::Backend { .. }));
        let websocket_only = matches!(retry, Some(TroubleshootingRetry::WebSocket { .. }));
        let title = text("check.provider_named", "Supplier backend · {provider}")
            .arg("provider", &profile.name);
        let (site, resolved) = match self.direct_resolution(set, profile).await {
            Ok(r) => r,
            Err(e) => {
                report.config_ok = false;
                record(
                    report,
                    progress,
                    retryable(
                        check(
                            if models_only {
                                format!("models:{}", profile.id)
                            } else if websocket_only {
                                match retry {
                                    Some(TroubleshootingRetry::WebSocket {
                                        line: Some(line),
                                        ..
                                    }) => format!("websocket:{}:{line}", profile.id),
                                    _ => format!("websocket:{}", profile.id),
                                }
                            } else {
                                format!("backend:{}", profile.id)
                            },
                            title,
                            Status::Failed,
                            e.to_text(),
                        ),
                        retry
                            .cloned()
                            .unwrap_or_else(|| TroubleshootingRetry::Backend {
                                profile: profile.id.clone(),
                                model: request.model.clone(),
                            }),
                    ),
                )?;
                return Ok(());
            }
        };
        let ResolvedMode::Token { endpoints, key, .. } = &resolved.mode else {
            return Ok(());
        };
        let mut upstream = self
            .model_lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&profile.id)
            .cloned();
        if retry.is_none() || models_only {
            let facts = self.monitor.facts().await;
            let installs = self.effective_installs(facts.installs.clone(), set).await;
            let (app, _) = crate::models::local_codexes(
                set.settings.preferred_app_id.as_deref(),
                &installs,
                &facts.binaries,
            );
            let version = app.as_ref().and_then(|a| a.version.as_ref());
            let fetched = self
                .deps
                .prober
                .model_list(ModelListRequest {
                    site: &site,
                    base_url: &endpoints.openai_base_url,
                    key,
                    client_version: version,
                    refresh: request.refresh_models || models_only,
                    source: profile.model_source,
                    standard_base: None,
                })
                .await;
            let mut models = TroubleshootingModels {
                profile_id: profile.id.clone(),
                name: profile.name.clone(),
                list: None,
                error: None,
                refreshed: false,
            };
            let (status, detail) = match fetched {
                Ok(list) => {
                    models.refreshed = (request.refresh_models || models_only)
                        && matches!(list.source, ModelListSource::Upstream { .. })
                        && list.fallback_reason.is_none()
                        && (list.sources.is_empty()
                            || list.sources.iter().any(|s| s.error.is_none()));
                    let failed = list.sources.iter().filter(|s| s.error.is_some()).count();
                    let status = if !matches!(list.source, ModelListSource::Upstream { .. }) {
                        Status::Unknown
                    } else if failed > 0 {
                        Status::Warning
                    } else {
                        Status::Passed
                    };
                    let detail = text("models_fetched", "{count} models returned by this supplier; {failed} source requests failed.")
                        .arg("count", list.models.len()).arg("failed", failed);
                    self.model_lists
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(profile.id.clone(), list.clone());
                    upstream = Some(list.clone());
                    models.list = Some(list);
                    (status, detail)
                }
                Err(e) => {
                    models.error = Some(e.to_text());
                    (Status::Failed, e.to_text())
                }
            };
            report.models.push(models);
            record(
                report,
                progress,
                retryable(
                    check(
                        format!("models:{}", profile.id),
                        text("check.models_named", "Upstream model list · {provider}")
                            .arg("provider", &profile.name),
                        status,
                        detail,
                    ),
                    TroubleshootingRetry::Models {
                        profile: profile.id.clone(),
                    },
                ),
            )?;
        }
        if models_only
            || (request.issue == TroubleshootingIssue::Models
                && request.model.is_none()
                && retry.is_none())
        {
            return Ok(());
        }
        let retry_model = match retry {
            Some(TroubleshootingRetry::Backend { model, .. }) => model.as_deref(),
            _ => None,
        };
        let model = retry_model
            .or(request.model.as_deref())
            .or(profile.model.model.as_deref())
            .or(state.model.as_deref())
            .or_else(|| {
                upstream
                    .as_ref()
                    .and_then(|l| l.models.first())
                    .map(|m| m.slug.as_str())
            });
        if !websocket_only {
            let retry_backend = TroubleshootingRetry::Backend {
                profile: profile.id.clone(),
                model: model.map(str::to_owned),
            };
            if let Some(model) = model {
                match self
                    .deps
                    .prober
                    .probe_responses(endpoints, key, site.site_model(model))
                    .await
                {
                    Ok(result) => {
                        let detail = text(
                            "backend_response",
                            "Responses probe: HTTP {status}. {detail}",
                        )
                        .arg(
                            "status",
                            result.status.map_or_else(|| "—".into(), |s| s.to_string()),
                        )
                        .arg("detail", result.detail.unwrap_or_default());
                        record(
                            report,
                            progress,
                            retryable(
                                check(
                                    format!("backend:{}", profile.id),
                                    title,
                                    if result.ok {
                                        Status::Passed
                                    } else {
                                        Status::Failed
                                    },
                                    detail,
                                ),
                                retry_backend,
                            ),
                        )?;
                    }
                    Err(e) => record(
                        report,
                        progress,
                        retryable(
                            check(
                                format!("backend:{}", profile.id),
                                title,
                                Status::Failed,
                                e.to_text(),
                            ),
                            retry_backend,
                        ),
                    )?,
                }
            } else {
                record(
                    report,
                    progress,
                    check(
                        format!("backend:{}", profile.id),
                        title,
                        Status::Unknown,
                        text(
                            "model_for_probe",
                            "No model is available for the Responses probe. Select a model first.",
                        ),
                    ),
                )?;
            }
        }
        if backend_only {
            return Ok(());
        }
        if websocket_only
            || matches!(
                request.issue,
                TroubleshootingIssue::WebSocket | TroubleshootingIssue::All
            )
        {
            match self
                .line_transports_impl(LineTransportsRequest {
                    profile: profile.id.clone(),
                    lines: match retry {
                        Some(TroubleshootingRetry::WebSocket { line, .. }) => {
                            line.clone().into_iter().collect()
                        }
                        _ => resolved.line_id.clone().into_iter().collect(),
                    },
                    refresh: TransportRefresh::All,
                })
                .await
            {
                Ok(lines) if !lines.is_empty() => {
                    for line in lines {
                        let status = match line.websocket.support {
                            TransportSupport::Supported => Status::Passed,
                            TransportSupport::Unsupported => Status::Warning,
                            _ => Status::Unknown,
                        };
                        record(
                            report,
                            progress,
                            retryable(
                                check(
                                    format!("websocket:{}:{}", profile.id, line.line_id),
                                    text(
                                        "check.websocket_named",
                                        "WebSocket · {provider} / {line}",
                                    )
                                    .arg("provider", &profile.name)
                                    .arg("line", &line.line_id),
                                    status,
                                    text("websocket_result", "{support}. {detail}")
                                        .arg_text("support", line.websocket.support.text())
                                        .arg("detail", line.websocket.detail.unwrap_or_default()),
                                ),
                                TroubleshootingRetry::WebSocket {
                                    profile: profile.id.clone(),
                                    line: Some(line.line_id),
                                },
                            ),
                        )?;
                    }
                }
                Ok(_) => record(
                    report,
                    progress,
                    retryable(
                        check(
                            format!("websocket:{}", profile.id),
                            text("check.websocket", "WebSocket handshake"),
                            Status::Unknown,
                            text(
                                "websocket_not_checked",
                                "No enabled upstream line was checked.",
                            ),
                        ),
                        TroubleshootingRetry::WebSocket {
                            profile: profile.id.clone(),
                            line: None,
                        },
                    ),
                )?,
                Err(e) => record(
                    report,
                    progress,
                    retryable(
                        check(
                            match retry {
                                Some(TroubleshootingRetry::WebSocket {
                                    line: Some(line), ..
                                }) => format!("websocket:{}:{line}", profile.id),
                                _ => format!("websocket:{}", profile.id),
                            },
                            text("check.websocket", "WebSocket handshake"),
                            Status::Unknown,
                            e.to_text(),
                        ),
                        retry
                            .cloned()
                            .unwrap_or_else(|| TroubleshootingRetry::WebSocket {
                                profile: profile.id.clone(),
                                line: None,
                            }),
                    ),
                )?,
            }
        }
        if websocket_only {
            return Ok(());
        }
        if request.issue == TroubleshootingIssue::Speed {
            let target = request.speed.unwrap_or(SpeedTier::Fast);
            let spec = upstream
                .as_ref()
                .and_then(|l| model.and_then(|m| l.model(m)));
            let api_mode = state
                .auth
                .mode()
                .or_else(|| state.active.as_ref().and_then(|a| a.auth_mode))
                == Some(AuthMode::ApiKey);
            let (status, detail) = if api_mode {
                if state
                    .model_provider
                    .as_deref()
                    .unwrap_or(codex::PROVIDER_OPENAI)
                    == codex::PROVIDER_OPENAI
                {
                    (
                        Status::Passed,
                        text(
                            "speed_openai",
                            "API mode uses the openai provider and supports Fast / UltraFast.",
                        ),
                    )
                } else {
                    (
                        Status::Warning,
                        text(
                            "speed_provider",
                            "API mode currently supports Standard speed only. To enable Fast / UltraFast, set model_provider to openai (an OpenAI-compatible provider).",
                        ),
                    )
                }
            } else {
                match (upstream.as_ref(), spec) {
                    (Some(list), Some(spec)) if !spec.service_tiers.is_empty() => {
                        let support = crate::models::speed_support(
                            target,
                            list,
                            model,
                            false,
                            state
                                .active
                                .as_ref()
                                .and_then(|a| a.auth_mode)
                                .is_some_and(|m| m == AuthMode::Pat),
                            site.capabilities.tier_model_suffix,
                        );
                        let (_, app) = support.effect(target);
                        let ok = matches!(
                            app,
                            SpeedEffect::Applies
                                | SpeedEffect::ViaModelName { .. }
                                | SpeedEffect::AppMenu
                        ) && spec.speed_tiers().contains(&target);
                        (
                            if ok { Status::Passed } else { Status::Warning },
                            app.text(),
                        )
                    }
                    _ => (
                        Status::Unknown,
                        text(
                            "speed_unknown",
                            "The supplier did not publish enough metadata to confirm this model's speed mode. Confirm support with the supplier.",
                        ),
                    ),
                }
            };
            record(
                report,
                progress,
                check(
                    format!("speed:{}", profile.id),
                    text("check.speed", "Model speed support · {tier}")
                        .arg_text("tier", target.text()),
                    status,
                    detail,
                ),
            )?;
        }
        Ok(())
    }
}

impl Switcher {
    async fn troubleshooting_config(
        &self,
        state: &ConfigState,
        profile: Option<&Profile>,
        speed: Option<SpeedTier>,
        report: &mut TroubleshootingReport,
        progress: &TroubleshootingProgressSink,
    ) -> Result<()> {
        use TroubleshootingStatus as Status;
        let doc = self
            .codex_file_impl(TargetId::local(), CodexFileKind::ConfigToml)
            .await?;
        let Some(contents) = doc.text() else {
            record(
                report,
                progress,
                check(
                    "configuration",
                    text("check.config", "Configuration syntax and compatibility"),
                    Status::Failed,
                    text(
                        "config_unreadable",
                        "config.toml is not readable UTF-8. Open the configuration editor.",
                    ),
                ),
            )?;
            return Ok(());
        };
        let problems = validate(
            CodexFileKind::ConfigToml,
            &contents,
            &CheckContext {
                schema: Some(&doc.schema),
                owned: &doc.owned,
                ..Default::default()
            },
        );
        let errors: Vec<_> = problems
            .iter()
            .filter(|p| p.severity == ProblemSeverity::Error)
            .collect();
        // The current desktop app determines compatibility, with CLI as the
        // fallback when no desktop is installed. An unrelated CLI probe must
        // not turn a clean recheck into "unknown" after warnings are removed.
        let current_consumer = doc
            .schema
            .consumers
            .iter()
            .position(|c| c.consumer == ConsumerKind::DesktopApp)
            .or_else(|| (!doc.schema.consumers.is_empty()).then_some(0));
        let mut known_version = false;
        for (index, consumer) in doc.schema.consumers.iter().enumerate() {
            let snapshot = consumer.version.as_ref().and_then(|version| {
                doc.schema
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.snapshot(Some(version)))
            });
            let (status, detail) = match (&consumer.version, snapshot) {
                (Some(version), Some(snapshot)) => {
                    let too_old = matches!(snapshot.matched, CatalogMatch::BelowFloor);
                    let detail = if too_old {
                        text("config_catalog_below_floor", "Codex {version} predates the oldest settings catalog ({catalog}); compatibility could not be confirmed.")
                    } else if snapshot.matched.is_inexact() {
                        text("config_version_approximate", "Codex {version}; checked against the nearest available settings catalog ({catalog}).")
                    } else {
                        text("config_version_known", "Codex {version}; settings catalog {catalog}.")
                    };
                    (
                        if too_old { Status::Unknown } else { Status::Passed },
                        detail.arg("version", version).arg("catalog", &snapshot.codex_version),
                    )
                }
                (Some(version), None) => (
                    Status::Unknown,
                    text("config_catalog_missing", "Codex {version} was detected, but no settings catalog is available for compatibility checks.")
                        .arg("version", version),
                ),
                (None, _) => (
                    Status::Unknown,
                    text("config_consumer_version_unknown", "The installed version for {consumer} could not be read. Configuration syntax was checked.")
                        .arg("consumer", &consumer.label),
                ),
            };
            if current_consumer == Some(index) {
                known_version = status == Status::Passed;
            }
            record(
                report,
                progress,
                check(
                    format!("configuration:version:{index}"),
                    text("check.config_version", "Codex version · {consumer}")
                        .arg("consumer", &consumer.label),
                    status,
                    detail,
                ),
            )?;
        }
        let warnings: Vec<_> = problems
            .iter()
            .filter(|p| p.severity == ProblemSeverity::Warning)
            .collect();
        let cleanup_entries = cleanup::entries(&doc, &problems);
        report.cleanup = doc
            .sha256
            .as_ref()
            .filter(|_| !cleanup_entries.is_empty() && doc.read_only.is_none())
            .map(|sha256| TroubleshootingCleanup {
                sha256: sha256.clone(),
                entries: cleanup_entries,
            });
        let status = if !errors.is_empty() {
            Status::Failed
        } else if !warnings.is_empty() {
            Status::Warning
        } else if !known_version {
            Status::Unknown
        } else {
            Status::Passed
        };
        let detail = match errors.first() {
            Some(p) => text("config_invalid", "{count} configuration errors; first at line {line}, column {column}. Open the configuration editor.")
                .arg("count", errors.len()).arg("line", p.line).arg("column", p.column),
            None if !warnings.is_empty() => text("config_warnings", "{count} configuration compatibility warnings. Review the items below.")
                .arg("count", warnings.len()),
            None if !known_version => text("config_version_unknown", "Syntax checked; configuration compatibility requires the installed Codex version and settings catalog."),
            None => text("config_valid", "Configuration syntax and the installed Codex settings catalog passed."),
        };
        record(
            report,
            progress,
            check(
                "configuration",
                text("check.config", "Configuration syntax and compatibility"),
                status,
                detail,
            ),
        )?;
        report.config_ok = status == Status::Passed;
        for (index, problem) in problems
            .iter()
            .filter(|problem| problem.severity != ProblemSeverity::Info)
            .enumerate()
        {
            record(
                report,
                progress,
                check(
                    format!(
                        "configuration:{}:{index}",
                        if cleanup::is_cleanup_notice(problem) {
                            "ignored"
                        } else {
                            "problem"
                        }
                    ),
                    text("check.config_entry", "{path}")
                        .arg("path", problem.path.as_deref().unwrap_or("config.toml")),
                    if problem.severity == ProblemSeverity::Error {
                        Status::Failed
                    } else {
                        Status::Warning
                    },
                    problem.message.clone(),
                ),
            )?;
        }
        if state.parse_error.is_some()
            || problems
                .iter()
                .any(|problem| problem.kind == ProblemKind::Syntax)
        {
            return Ok(());
        }
        let managed_bad = state
            .model_provider
            .as_deref()
            .is_some_and(|p| p != codex::PROVIDER_OPENAI)
            || !state.drift.is_empty()
            || doc.contents.is_none()
            || state.auth_drift.is_some()
            || profile.is_some_and(|p| {
                !p.is_builtin()
                    && (state.auth.mode().is_none()
                        || state
                            .openai_base_url
                            .as_deref()
                            .and_then(|u| url::Url::parse(u).ok())
                            .is_none())
            });
        record(
            report,
            progress,
            check(
                "managed",
                text("check.managed", "Provider, endpoint and authentication"),
                if managed_bad {
                    Status::Warning
                } else {
                    Status::Passed
                },
                if managed_bad {
                    text(
                        "managed_changed",
                        "The provider, endpoint, credential or managed settings differ from the applied configuration. Reapply it to review and repair the differences.",
                    )
                } else {
                    text(
                        "managed_ok",
                        "No unexpected change was found in the applied provider, endpoint or authentication configuration.",
                    )
                },
            ),
        )?;
        report.reapply =
            managed_bad && profile.is_some_and(|p| !p.is_builtin()) && !state.journal_pending;
        report.config_ok &= !managed_bad;
        let values = superba_config::settings::read_config_values(Some(contents.as_bytes()))?;
        if report.issue == TroubleshootingIssue::Speed {
            let feature = if speed == Some(SpeedTier::Ultrafast) {
                "features.ultrafast_mode"
            } else {
                "features.fast_mode"
            };
            if values.get(feature).and_then(SettingValue::as_bool) == Some(false) {
                report.config_ok = false;
                record(report, progress, check("speed_config", text("check.speed_config", "Speed configuration"), Status::Warning,
                    text("speed_disabled", "{feature} is explicitly disabled. Enable it in the configuration editor before trying this speed mode.").arg("feature", feature)))?;
            }
        }
        if let Some(path) = values
            .get(codex::config_keys::MODEL_CATALOG_JSON)
            .and_then(SettingValue::as_str)
        {
            let path = self.deps.ctx.codex_home().root().join(path);
            let ok = tokio::fs::read(path)
                .await
                .ok()
                .is_some_and(|b| serde_json::from_slice::<serde_json::Value>(&b).is_ok());
            record(
                report,
                progress,
                check(
                    "catalog",
                    text("check.catalog", "Applied model catalog"),
                    if ok { Status::Passed } else { Status::Failed },
                    if ok {
                        text(
                            "catalog_ok",
                            "The configured model catalog exists and is valid JSON.",
                        )
                    } else {
                        text(
                            "catalog_invalid",
                            "The configured model catalog is missing or unreadable. Reapply the provider to regenerate it.",
                        )
                    },
                ),
            )?;
            report.config_ok &= ok;
            report.reapply |= !ok && profile.is_some_and(|p| !p.is_builtin());
        }
        if report.issue == TroubleshootingIssue::WebSocket
            && values
                .get("model_providers.openai.supports_websockets")
                .and_then(SettingValue::as_bool)
                == Some(false)
        {
            report.config_ok = false;
            record(
                report,
                progress,
                check(
                    "websocket_config",
                    text("check.websocket_config", "WebSocket configuration"),
                    Status::Warning,
                    text(
                        "websocket_disabled",
                        "The openai provider explicitly disables supports_websockets. Check this setting and your Codex version in the configuration editor.",
                    ),
                ),
            )?;
        }
        for finding in self.doctor_impl().await? {
            if matches!(
                finding.id.as_str(),
                "journal_pending"
                    | "profile_override"
                    | "missing_env"
                    | "codex_home_mismatch"
                    | "openai_env_conflict"
                    | "authapi_other_line"
                    | "cli_too_old"
            ) {
                let status = match finding.severity {
                    Severity::Info => Status::Passed,
                    Severity::Warning => Status::Warning,
                    Severity::Danger => Status::Failed,
                };
                report.config_ok &= status == Status::Passed;
                record(report, progress, check(format!("doctor:{}", finding.id), finding.title, status,
                    finding.detail.unwrap_or_else(|| text("doctor_manual", "Review this finding before continuing. Open diagnostics or the configuration editor."))))?;
            }
        }
        Ok(())
    }
}
