//! Prepare guided recoveries on the backend thread. File writes, conversation
//! migration and restarts always execute through the global switch Flow.

use superba_core::*;

use crate::pages::troubleshooting::Repair;

/// Resolve the current local recovery without selecting a different provider.
pub(super) async fn prepare_troubleshooting(
    service: &dyn SwitchService,
    repair: Repair,
) -> Result<SwitchRequest> {
    let overview = service.overview().await?;
    let set = service.profile_set().await?;
    let active = overview.active.as_ref();
    let profile = active
        .and_then(|a| a.profile_id.as_ref())
        .and_then(|id| set.profile(id));
    let mut request =
        SwitchRequest::profile(profile.map_or_else(|| ProfileId::new("-"), |p| p.id.clone()));
    request.targets = TargetSelection {
        local: true,
        remotes: Vec::new(),
    };
    request.local_only = true;
    request.intent = match repair {
        Repair::Cleanup(_) => {
            return Err(Error::invalid(
                "Configuration cleanup uses the file editor save operation.",
            ));
        }
        Repair::Reapply => SwitchIntent::ReapplyConfig,
        Repair::Reinitialize => SwitchIntent::ReinitializeConfig,
        Repair::Restart => SwitchIntent::RestartApp { install_id: None },
        Repair::Official => SwitchIntent::Restore {
            target: RestoreTarget::Official,
        },
        Repair::Migrate => {
            let (sessions, _) = service.sessions().await?;
            let selectors = sessions
                .distribution()
                .into_iter()
                .filter(|(provider, n)| provider != codex::PROVIDER_OPENAI && *n > 0)
                .map(|(provider, _)| MigrationSelector::Provider { provider })
                .collect();
            SwitchIntent::MigrateSessions {
                request: MigrationRequest {
                    to_provider: codex::PROVIDER_OPENAI.into(),
                    selectors,
                    include_archived: true,
                    exclude: Vec::new(),
                },
            }
        }
        Repair::Model {
            profile: supplier,
            model,
        } => {
            let routed = set.settings.router.enabled;
            let allowed = if routed {
                set.settings.router.members.contains(&supplier)
            } else {
                profile.is_some_and(|p| p.id == supplier)
            };
            if !allowed {
                return Err(Error::invalid(
                    "the applied provider changed; run the model check again",
                ));
            }
            if routed {
                let old = set.settings.router.catalog.clone();
                let mut settings = old.clone();
                settings.visibility.insert(model.clone(), true);
                if settings.mode == CatalogMode::Codex {
                    settings.mode = CatalogMode::Extended;
                }
                service.save_router_catalog(settings).await?;
                let view = service
                    .models_view(ModelsViewRequest {
                        profile_id: Some(supplier.clone()),
                        refresh: false,
                    })
                    .await?;
                let alias = view
                    .router_catalog
                    .as_ref()
                    .and_then(|catalog| catalog.model(&model))
                    .and_then(|row| {
                        row.sources
                            .iter()
                            .find(|source| source.provider == supplier.as_str())
                    })
                    .and_then(|source| source.alias.clone());
                let tested = match alias {
                    Some(alias) => service.test_router_model(alias).await,
                    None => Err(Error::invalid(
                        "the selected supplier did not publish this model in the router catalog",
                    )),
                };
                match tested {
                    Ok(test) if test.passed => {}
                    result => {
                        service.save_router_catalog(old).await?;
                        return Err(match result {
                            Ok(test) => Error::invalid(test.detail),
                            Err(e) => e,
                        });
                    }
                }
            } else {
                let view = service
                    .models_view(ModelsViewRequest {
                        profile_id: Some(supplier.clone()),
                        refresh: false,
                    })
                    .await?;
                let spec = view.list.model(&model).ok_or_else(|| {
                    Error::invalid("the supplier no longer lists this model; refresh again")
                })?;
                if !spec.supported_in_api
                    || !matches!(
                        spec.visibility,
                        ModelVisibility::List | ModelVisibility::Other
                    )
                {
                    return Err(Error::invalid(
                        "the supplier marks this model unavailable or hidden; confirm access with the supplier",
                    ));
                }
                let mut value = view.overrides.get(&model).cloned().unwrap_or_default();
                value.hidden = false;
                // A named override makes the existing catalog builder publish
                // the supplier entry, including models outside Codex's bundle.
                value
                    .display_name
                    .get_or_insert_with(|| spec.label().to_owned());
                service
                    .save_models(ModelsEdit {
                        profile_id: Some(supplier),
                        set: [(model, value)].into(),
                        picker: view.picker_available.then_some(ModelPicker::Provider),
                        ..Default::default()
                    })
                    .await?;
            }
            request.refresh_models = true;
            SwitchIntent::ReapplyConfig
        }
    };
    Ok(request)
}
