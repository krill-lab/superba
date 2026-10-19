# Start Superba at login

Preferences → General contains **Start Superba at login** (off by default)
and **Start in the tray only**. The latter reuses the window's `start_in_tray`
preference and also applies to manual starts. Without a usable tray host the
window opens so Superba remains accessible.

The same login setting is available without the GUI:

```sh
spb autostart status
spb autostart on
spb autostart off
spb --json autostart status
```

The setting applies to the current user and needs no administrator privileges.
The registration is the source of truth; a saved preferences file cannot
recreate a registration removed by an uninstaller. Failures are reported by
the CLI or the GUI settings notification.

| Platform | Registration | Why |
| --- | --- | --- |
| Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `Superba` | The standard per-user login mechanism; independent of the router's `SuperbaRouter` value. It runs the quoted, absolute `superba.exe` path with `--autostart`. |
| macOS | `~/Library/LaunchAgents/com.krill-code.superba.autostart.plist` | A user LaunchAgent works with the app bundle and unbundled developer builds without an administrator or a separate login helper. The packaged app uses `/usr/bin/open -g -b com.krill-code.superba --args --autostart`; LaunchServices resolves its identity when Finder moves or an updater replaces the bundle. |
| Linux | `$XDG_CONFIG_HOME/autostart/superba.desktop`, normally `~/.config/autostart/superba.desktop` | The XDG desktop-session convention works across desktop environments. The absolute executable path is quoted and desktop field codes are escaped. AppImage registrations use the original image, not its temporary mount. |

The Windows Run command has a documented 260-character limit; enabling reports
an error if the chosen install path exceeds it. `spb` resolves its sibling GUI
first, with the MSI's current-user `InstallDir` as a fallback. No default install
directory is assumed. Linux relative `XDG_CONFIG_HOME` values are ignored.

A login start obeys the tray preference from the first native window frame.
If Superba is already running, authenticated single-instance IPC acknowledges
the login request without raising its window. A process-lifetime lock also
prevents duplicate windows during simultaneous starts. A manual second start
still brings the existing window forward.

Normal GUI launches refresh an existing enabled registration after an update
or portable installation move. Updates restart the installed GUI, which performs
this refresh. MSI installs, repairs and upgrades also refresh existing GUI
registrations using `INSTALLFOLDER` even when the GUI is not opened. Disabled
registrations are never enabled by an update. macOS app bundles use their
bundle identity; an unbundled developer binary needs one manual launch after a
move, as does a manually moved Linux/Windows portable installation.

MSI removal through its Remove page or `msiexec /x` deletes the GUI Run value.
The cleanup lives in the MSI's embedded DLL and works even if installed
`spb.exe` is missing. An upgrade's removal of the old MSI preserves the choice.
Superba's own uninstall and successful restore-original flows also remove the
GUI registration. This does not remove the router's independent registration
except where uninstall already removes the router itself.

Platform references: [Windows Run keys](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys),
[Apple user LaunchAgents](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html),
[XDG autostart](https://specifications.freedesktop.org/autostart/latest/) and
[desktop Exec escaping](https://specifications.freedesktop.org/desktop-entry/latest/exec-variables.html).
