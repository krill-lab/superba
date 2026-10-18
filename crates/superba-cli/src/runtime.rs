//! Production construction and injectable process inputs.

use std::sync::Arc;
use superba_core::{Clock, EnvSource, ProcessEnv, Result, SwitchService, SystemClock};

/// All external services used by the CLI.
pub struct Runtime {
    pub service: Arc<dyn SwitchService>,
    pub clock: Arc<dyn Clock>,
    pub env: Arc<dyn EnvSource>,
    /// Read one secret line, trimmed. Tests inject a reader without touching stdin.
    pub read_stdin_line: Arc<dyn Fn() -> std::io::Result<String> + Send + Sync>,
    /// stdin and stderr are terminals: a switch may ask the "stop the Codex
    /// processes for you?" question and read the answer with
    /// [`Self::read_stdin_line`].
    pub interactive: bool,
}

impl Runtime {
    /// Construct the production service. This is the only production wiring point.
    pub fn production() -> Result<Self> {
        let ctx = superba_switch::detect_context()?;
        Ok(Self::with_service(superba_switch::default_service(ctx)))
    }

    /// Embed a service with the real clock, environment and stdin by default.
    pub fn with_service(service: Arc<dyn SwitchService>) -> Self {
        Self {
            service,
            clock: Arc::new(SystemClock::new()),
            env: Arc::new(ProcessEnv),
            read_stdin_line: Arc::new(|| {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                Ok(line.trim().to_owned())
            }),
            interactive: {
                use std::io::IsTerminal;
                std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
            },
        }
    }
}

pub(crate) fn initial_locale(explicit: Option<&str>) -> superba_core::Locale {
    crate::resolve_locale(explicit, &ProcessEnv)
}
