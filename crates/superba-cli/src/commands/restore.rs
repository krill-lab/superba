use superba_core::{RestoreTarget, SwitchIntent};

use super::{Result, execute, request, usage};
use crate::{Output, ProviderArg, RestoreArgs, RestoreTo, Runtime, text::text};

pub(super) async fn run(args: &RestoreArgs, rt: &Runtime, out: &mut Output) -> Result {
    if (args.provider.is_some() && args.to != RestoreTo::Original)
        || (args.generation.is_some() && args.to != RestoreTo::Generation)
    {
        return Err(usage(out, text("cli.error.restore_flags")));
    }
    let intent = match args.to {
        RestoreTo::Original => SwitchIntent::Restore {
            target: RestoreTarget::Original {
                provider: args.provider.unwrap_or(ProviderArg::ReverseMigrate).into(),
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
