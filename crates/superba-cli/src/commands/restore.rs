use superba_core::{ProfileId, RestoreTarget, SwitchIntent, catalog};

use super::{Result, execute, request, usage};
use crate::{Output, RestoreArgs, RestoreTo, Runtime, text::text};

pub(super) async fn run(args: &RestoreArgs, rt: &Runtime, out: &mut Output) -> Result {
    if (args.provider.is_some() && args.to != RestoreTo::Original)
        || (args.generation.is_some() && args.to != RestoreTo::Generation)
    {
        return Err(usage(out, text("cli.error.restore_flags")));
    }
    let intent = match args.to {
        RestoreTo::Original => match args.provider {
            Some(provider) => SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: provider.into(),
                },
            },
            None => SwitchIntent::Profile {
                profile_id: ProfileId::new(catalog::PROFILE_ORIGINAL),
                line_id: None,
                auth_mode: None,
            },
        },
        RestoreTo::Official => SwitchIntent::Restore {
            target: RestoreTarget::Official,
        },
        RestoreTo::Generation => SwitchIntent::Restore {
            target: RestoreTarget::Generation {
                generation: args
                    .generation
                    .ok_or_else(|| usage(out, text("cli.error.generation")))?,
            },
        },
    };
    let mut request = request(intent, &args.exec);
    if args.take_over {
        // GH#5: restore also what another tool changed after Superba wrote it.
        superba_switch::ownership::take_over(&mut request);
    }
    execute::run(request, &args.exec, rt, out).await
}
