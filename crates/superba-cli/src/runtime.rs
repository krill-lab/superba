//! Production construction and injectable process inputs.

use std::path::Path;
use std::sync::Arc;
use superba_core::{Clock, EnvSource, ProcessEnv, Result, SwitchService, SystemClock};

/// Opens a file in a text editor and waits (see [`Runtime::edit_file`]).
pub type EditFile = Arc<dyn Fn(&str, &Path) -> std::io::Result<bool> + Send + Sync>;

/// All external services used by the CLI.
pub struct Runtime {
    pub service: Arc<dyn SwitchService>,
    pub clock: Arc<dyn Clock>,
    pub env: Arc<dyn EnvSource>,
    /// Read one secret line, trimmed. Tests inject a reader without touching stdin.
    pub read_stdin_line: Arc<dyn Fn() -> std::io::Result<String> + Send + Sync>,
    /// Read all of stdin (at most 4 MiB): the JSON a harness hook gets
    /// (`spb notify hook`). Tests inject the payload.
    pub read_stdin_all: Arc<dyn Fn() -> std::io::Result<String> + Send + Sync>,
    /// stdin and stderr are terminals: a switch may ask the "stop the Codex
    /// processes for you?" question and read the answer with
    /// [`Self::read_stdin_line`].
    pub interactive: bool,
    /// Open a file in a text editor and wait until it is closed (`spb config
    /// edit`): the editor command (`$VISUAL`, `$EDITOR`, notepad on Windows,
    /// `vi` elsewhere) and the file. `Ok(false)`: the editor failed. Tests
    /// inject an editor that changes the file.
    pub edit_file: EditFile,
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
            read_stdin_all: Arc::new(|| {
                use std::io::Read;
                let mut text = String::new();
                std::io::stdin()
                    .take(4 * 1024 * 1024)
                    .read_to_string(&mut text)?;
                Ok(text)
            }),
            interactive: {
                use std::io::IsTerminal;
                std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
            },
            edit_file: Arc::new(run_editor),
        }
    }
}

/// The words of an editor command (`code --wait`, `"C:\Program Files\x.exe" -n`):
/// split at spaces outside double quotes.
pub fn editor_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

/// Run `command` on `path` with this terminal and wait for it.
pub fn run_editor(command: &str, path: &Path) -> std::io::Result<bool> {
    let words = editor_words(command);
    let Some((program, args)) = words.split_first() else {
        return Err(std::io::Error::other("no editor command"));
    };
    let mut cmd = if cfg!(windows)
        && !program.to_ascii_lowercase().ends_with(".exe")
        && !program.eq_ignore_ascii_case("notepad")
    {
        // `code`, `subl`: batch launchers only cmd.exe finds.
        let mut c = superba_core::process::terminal_command("cmd");
        c.arg("/C").arg(program);
        c
    } else {
        superba_core::process::terminal_command(program)
    };
    let status = cmd.args(args).arg(path).status()?;
    Ok(status.success())
}

pub(crate) fn initial_locale(explicit: Option<&str>) -> superba_core::Locale {
    crate::resolve_locale(explicit, &ProcessEnv)
}
