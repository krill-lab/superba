//! A command that cmd.exe, Git Bash and PowerShell can all launch unchanged.

use base64::{Engine as _, engine::general_purpose::STANDARD};

const LAUNCHER: &str = "powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ";
const SCRIPT_START: &str = "$ProgressPreference = 'SilentlyContinue'; $a = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('";
const SCRIPT_END: &str =
    "')) | ConvertFrom-Json; $LASTEXITCODE = 1; & $a[0] $a[1] $a[2] $a[3]; exit $LASTEXITCODE";

pub(super) fn command(program: &str, harness_id: &str) -> String {
    // Only shell-safe ASCII reaches the outer shell. JSON argv also keeps
    // PowerShell metacharacters (including smart quotes) out of its parser.
    // Native stdin is inherited, and the hook's exit code must survive -Command.
    // Suppress Windows PowerShell's first-use module progress on stderr.
    let argv = serde_json::to_vec(&[program, "notify", "hook", harness_id])
        .expect("serializing string arguments");
    let script = format!("{SCRIPT_START}{}{SCRIPT_END}", STANDARD.encode(argv));
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!("{LAUNCHER}{}", STANDARD.encode(utf16))
}

pub(super) fn argv(command: &str) -> Option<[String; 4]> {
    let bytes = STANDARD.decode(command.strip_prefix(LAUNCHER)?).ok()?;
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let utf16: Vec<u16> = pairs.iter().map(|b| u16::from_le_bytes(*b)).collect();
    let script = String::from_utf16(&utf16).ok()?;
    let encoded = script
        .strip_prefix(SCRIPT_START)?
        .strip_suffix(SCRIPT_END)?;
    serde_json::from_slice(&STANDARD.decode(encoded).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::is_our_hook_command;

    #[test]
    fn windows_hook_paths_round_trip_without_shell_interpolation() {
        for path in [
            r"C:\Tools\spb.exe",
            r"C:\Program Files\Superba\spb.exe",
            r"C:\O'Brien\$HOME %PATH% !name! & (x) `tick ; 中文 👋 ‘smart’\spb.exe",
            r"\\server\share with spaces\Superba\superba.exe",
        ] {
            for harness_id in ["codex", "claude-code"] {
                let cmd = command(path, harness_id);
                assert!(
                    cmd.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b" .-=+/".contains(&b))
                );
                assert_eq!(argv(&cmd).unwrap(), [path, "notify", "hook", harness_id]);
                assert!(is_our_hook_command(&cmd), "{path}");
            }
        }
    }

    #[test]
    fn encoded_hook_ownership_requires_the_exact_launcher_and_argv() {
        let ours = command(r"C:\Program Files\Superba\spb.exe", "codex");
        assert!(!is_our_hook_command(&command("other.exe", "codex")));
        assert!(!is_our_hook_command(&command("spb", "codex;other")));
        assert!(!is_our_hook_command(&format!("{ours} extra")));
        assert!(!is_our_hook_command(&ours.replace("-NoProfile", "-File")));
        for payload in ["!", "YQ==", "ANg=", "", "Zg=="] {
            assert!(!is_our_hook_command(&format!("{LAUNCHER}{payload}")));
        }
        for script in [
            "spb notify hook codex",
            &format!(
                "{SCRIPT_START}{}{SCRIPT_END}; Write-Output foreign",
                STANDARD.encode(b"[]")
            ),
            &format!(
                "{SCRIPT_START}{}{SCRIPT_END}",
                STANDARD.encode(br#"["spb","status","hook","codex"]"#)
            ),
            &format!(
                "{SCRIPT_START}{}{SCRIPT_END}",
                STANDARD.encode(br#"["spb","notify","hook","codex","extra"]"#)
            ),
        ] {
            let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            assert!(!is_our_hook_command(&format!(
                "{LAUNCHER}{}",
                STANDARD.encode(utf16)
            )));
        }
    }

    #[test]
    fn encoded_permission_hook_is_recognized_updated_and_removed() {
        use crate::codex_config::{read, set_permission_hook};

        let old = command(r"C:\Old Location\spb.exe", "codex");
        let config = format!(
            "[[hooks.PermissionRequest]]\n[[hooks.PermissionRequest.hooks]]\ntype = 'command'\ncommand = '{old}'\n"
        );
        assert!(read(&config, "config").unwrap().permission_hook);
        let updated = set_permission_hook(&config, "/new location/spb", true, false)
            .unwrap()
            .unwrap();
        assert!(read(&updated, "config").unwrap().permission_hook);
        assert_eq!(
            set_permission_hook(&updated, "/new location/spb", true, false).unwrap(),
            None
        );
        let removed = set_permission_hook(&updated, "/new location/spb", false, false)
            .unwrap()
            .unwrap();
        assert!(removed.trim().is_empty());
    }
}
