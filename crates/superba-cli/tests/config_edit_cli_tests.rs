//! `spb config edit|check|backups|restore` (GitHub issue 9) against the real
//! switching service in a temporary home, with an injected editor: the copy
//! the editor gets, the check after it closes, the diff, the question, the
//! encrypted backup and the restore; `auth.json` keys never reach the output
//! unless asked for.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::Parser;
use superba_cli::{Cli, Output, Runtime};
use superba_core::{DataDir, EnvSource, LocalContext, MapEnv};
use superba_switch::{Switcher, SwitcherDeps};

const CONFIG: &str =
    "# my settings\nmodel = \"gpt-5.5\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\n";
const KEY: &str = "sk-cli-test-0000000000000042";

/// What the injected editor does with the file, one entry per run.
type Edit = Box<dyn Fn(&str) -> String + Send>;

struct World {
    tmp: tempfile::TempDir,
    edits: Arc<Mutex<VecDeque<Edit>>>,
    /// (editor command, file name, private directory) of each run.
    runs: Arc<Mutex<Vec<(String, String, bool)>>>,
    answers: Arc<Mutex<VecDeque<String>>>,
    /// The directories the editor's copies were in.
    dirs: Arc<Mutex<Vec<PathBuf>>>,
    interactive: bool,
    env: Vec<(&'static str, &'static str)>,
}

impl World {
    fn new() -> Self {
        let w = Self {
            tmp: tempfile::tempdir().unwrap(),
            edits: Arc::default(),
            runs: Arc::default(),
            answers: Arc::default(),
            dirs: Arc::default(),
            interactive: false,
            env: Vec::new(),
        };
        std::fs::create_dir_all(w.codex()).unwrap();
        std::fs::create_dir_all(w.data()).unwrap();
        std::fs::write(w.codex().join("config.toml"), CONFIG).unwrap();
        w
    }

    fn codex(&self) -> PathBuf {
        self.tmp.path().join("home/.codex")
    }

    fn data(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.codex().join(name)).unwrap_or_default()
    }

    fn edit(&self, f: impl Fn(&str) -> String + Send + 'static) {
        self.edits.lock().unwrap().push_back(Box::new(f));
    }

    fn answer(&self, a: &str) {
        self.answers.lock().unwrap().push_back(a.to_owned());
    }

    fn runtime(&self) -> Runtime {
        let mut map = MapEnv::new()
            .with("SUPERBA_SECRET_BACKEND", "file")
            .with("SUPERBA_LOCALE", "en");
        for (k, v) in &self.env {
            map = map.with(k, v);
        }
        let env: Arc<dyn EnvSource> = Arc::new(map);
        let ctx = LocalContext::new(
            self.tmp.path().join("home"),
            DataDir::new(self.data()),
            env.clone(),
        );
        let service = Arc::new(Switcher::new(SwitcherDeps::with_defaults(ctx)));
        let mut rt = Runtime::with_service(service);
        rt.env = env;
        rt.interactive = self.interactive;
        let answers = self.answers.clone();
        rt.read_stdin_line = Arc::new(move || {
            Ok(answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("an unexpected question"))
        });
        let (edits, runs, dirs) = (self.edits.clone(), self.runs.clone(), self.dirs.clone());
        rt.edit_file = Arc::new(move |command: &str, path: &Path| {
            let private = is_private(path.parent().unwrap());
            dirs.lock()
                .unwrap()
                .push(path.parent().unwrap().to_path_buf());
            runs.lock().unwrap().push((
                command.to_owned(),
                path.file_name().unwrap().to_string_lossy().into_owned(),
                private,
            ));
            let edit = edits
                .lock()
                .unwrap()
                .pop_front()
                .expect("an unexpected edit");
            let text = std::fs::read_to_string(path)?;
            std::fs::write(path, edit(&text))?;
            Ok(true)
        });
        rt
    }

    async fn spb(&self, args: &[&str]) -> (u8, String, String) {
        let cli = Cli::try_parse_from(std::iter::once("spb").chain(args.iter().copied())).unwrap();
        let (mut out, stdout, stderr) = Output::capture();
        let code = superba_cli::run(&cli, &self.runtime(), &mut out).await;
        (code, stdout.contents(), stderr.contents())
    }
}

#[cfg(unix)]
fn is_private(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir).unwrap().permissions().mode() & 0o777 == 0o700
}

#[cfg(not(unix))]
fn is_private(_dir: &Path) -> bool {
    true
}

#[tokio::test]
async fn edit_saves_after_a_backup_and_restore_puts_it_back() {
    let mut w = World::new();
    w.env = vec![("EDITOR", "nano"), ("VISUAL", "code --wait")];
    w.edit(|t| t.replace("gpt-5.5", "gpt-6"));
    let (code, stdout, stderr) = w.spb(&["config", "edit", "--yes"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(w.file("config.toml"), CONFIG.replace("gpt-5.5", "gpt-6"));
    let runs = w.runs.lock().unwrap().clone();
    assert_eq!(
        runs,
        vec![("code --wait".to_owned(), "config.toml".to_owned(), true)],
        "VISUAL wins over EDITOR; a private copy named like the file"
    );
    for expected in [
        "-model = \"gpt-5.5\"",
        "+model = \"gpt-6\"",
        "Saved ",
        "Backup of the previous file: ",
    ] {
        assert!(stdout.contains(expected), "{expected:?} missing:\n{stdout}");
    }
    let id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Backup of the previous file: "))
        .and_then(|l| l.split(' ').next())
        .unwrap()
        .to_owned();

    let (code, stdout, _) = w.spb(&["config", "backups"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains(&id), "{stdout}");
    assert!(stdout.contains("Before an edit"), "{stdout}");

    let (code, stdout, stderr) = w.spb(&["config", "restore", &id, "--yes"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("+model = \"gpt-5.5\""), "{stdout}");
    assert_eq!(w.file("config.toml"), CONFIG);
    // The restore kept a backup of the file it replaced.
    let (_, stdout, _) = w.spb(&["config", "backups", "--file", "config"]).await;
    assert!(
        stdout.contains(&format!("Before restoring {id}")),
        "{stdout}"
    );
}

#[tokio::test]
async fn a_broken_edit_is_never_saved_and_can_be_edited_again() {
    let mut w = World::new();
    // Without a terminal: refused with the position, nothing written.
    w.edit(|t| t.replace("model = \"gpt-5.5\"", "model = "));
    let (code, stdout, _) = w.spb(&["config", "edit", "--yes"]).await;
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("config.toml:2:"), "{stdout}");
    assert!(stdout.contains("TOML syntax error"), "{stdout}");
    assert!(stdout.contains("Not saved."), "{stdout}");
    assert_eq!(w.file("config.toml"), CONFIG);

    // At a terminal: edit again (Enter), fix it, confirm.
    w.interactive = true;
    w.edit(|t| t.replace("model = \"gpt-5.5\"", "model = "));
    w.edit(|t| t.replace("model = \n", "model = \"gpt-6\"\n"));
    w.answer("");
    w.answer("y");
    let (code, stdout, stderr) = w.spb(&["config", "edit"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(stdout.contains("Edit again? [Y/n]"), "{stdout}");
    assert!(stdout.contains("Save config.toml?"), "{stdout}");
    assert_eq!(w.file("config.toml"), CONFIG.replace("gpt-5.5", "gpt-6"));
    assert_eq!(w.runs.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn without_yes_or_a_terminal_nothing_is_saved() {
    let w = World::new();
    w.edit(|t| format!("{t}# more\n"));
    let (code, stdout, _) = w.spb(&["config", "edit"]).await;
    assert_eq!(code, 4, "{stdout}");
    assert!(stdout.contains("add --yes"), "{stdout}");
    assert_eq!(w.file("config.toml"), CONFIG);
    // Unchanged after the editor: nothing to do.
    w.edit(|t| t.to_owned());
    let (code, stdout, _) = w.spb(&["config", "edit"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("No changes"), "{stdout}");
}

#[tokio::test]
async fn auth_keys_stay_hidden_unless_asked_for() {
    let w = World::new();
    w.edit(|_| format!("{{\n  \"OPENAI_API_KEY\": \"{KEY}\",\n  \"auth_mode\": \"apikey\"\n}}\n"));
    let (code, stdout, stderr) = w.spb(&["config", "edit", "auth", "--yes"]).await;
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert!(!stdout.contains(KEY) && !stderr.contains(KEY), "{stdout}");
    assert!(stdout.contains("keys are hidden"), "{stdout}");
    assert!(stdout.contains("••••0042"), "{stdout}");
    assert!(w.file("auth.json").contains(KEY));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(w.codex().join("auth.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The JSON result carries no key either.
    w.edit(|t| t.replace('}', "  }"));
    let (_, stdout, _) = w.spb(&["--json", "config", "edit", "auth", "--yes"]).await;
    assert!(!stdout.contains(KEY), "{stdout}");
    // Revealed on request.
    w.edit(|t| t.replace(KEY, "sk-cli-test-new-0000000000099"));
    let (code, stdout, _) = w
        .spb(&["config", "edit", "auth", "--yes", "--show-secrets"])
        .await;
    assert_eq!(code, 0);
    assert!(
        stdout.contains(&format!("-  \"OPENAI_API_KEY\": \"{KEY}\"")),
        "{stdout}"
    );
    // No copy of the key is left where the editor worked.
    for dir in w.dirs.lock().unwrap().iter() {
        assert!(!dir.exists(), "{} left behind", dir.display());
    }
}

#[tokio::test]
async fn check_reports_the_file_as_it_is() {
    let w = World::new();
    let (code, stdout, _) = w.spb(&["config", "check"]).await;
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("no problems found"), "{stdout}");
    std::fs::write(w.codex().join("auth.json"), "{\"OPENAI_API_KEY\": 5,}").unwrap();
    let (code, stdout, _) = w.spb(&["config", "check", "auth"]).await;
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("auth.json:1:"), "{stdout}");
}
