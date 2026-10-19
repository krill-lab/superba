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
| macOS | `~/Library/LaunchAgents/com.krill-code.superba.autostart.plist` | A user LaunchAgent runs the absolute `Superba.app/Contents/MacOS/superba` path with `--autostart`. `AssociatedBundleIdentifiers` attributes the entry to `com.krill-code.superba` in macOS 13+ Login Items. It also supports an unbundled developer binary. |
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

On macOS, direct execution delivers the login flag to single-instance IPC
without creating an `open -n` application or sending the running app a reopen
event. A manual `open -a Superba`, Finder/Dock reopen, or second executable start
shows and focuses the existing window, including when it is hidden in the tray.
Disabling removes the plist and attempts `launchctl bootout gui/<uid>/<label>`;
an already absent job or a bootout failure does not make disabling fail.

Two Dock tiles do not by themselves mean two running instances. macOS can keep
recent-app bookmarks for older copies of a moved or replaced bundle. In the
macOS regression run, two old `/Applications/Superba.app` bookmarks remained
after disabling startup and rebooting with **no** Superba process running;
the active app had moved to `~/Applications`. Direct login execution removes
the unnecessary `open -n` launch, while single-instance IPC handles competing
starts. Obsolete recent-app tiles belong to the Dock's history and can be
removed there; Superba does not rewrite the user's Dock preferences.

The standalone `spb` installed in `~/.local/bin` finds the GUI in this order:
`SUPERBA_GUI_EXE`, a sibling binary/app, then the installed app by its
LaunchServices bundle identifier, with `~/Applications` and `/Applications`
as fallbacks. An explicit override must be an absolute executable file path.
The Superba overview uses the same macOS installation lookup for `spb`, with
`SUPERBA_SPB_EXE` as its override and `~/.local/bin/spb` before the shell PATH.
This works when Finder's PATH does not include the user's shell directories.

We retain the per-user LaunchAgent instead of `SMAppService` for this release:
the standalone CLI and unbundled builds can manage it without starting the
GUI, embedding a helper, or depending on macOS 13. `SMAppService.mainApp` is
appropriate for a future bundle-owned registration flow, but would require
the standalone CLI to hand registration to that bundle. Apple's
[service-management documentation](https://developer.apple.com/documentation/servicemanagement/smappservice)
describes the newer API; its
[legacy-agent guidance](https://developer.apple.com/documentation/servicemanagement/updating-helper-executables-from-earlier-versions-of-macos)
documents the bundle association used here. An ad-hoc build can show the
executable name `superba` and an unidentified developer in System Settings,
even when the background-item database records `Superba`. Publisher
attribution still requires release signing.

Normal GUI launches refresh an existing enabled registration after an update
or portable installation move. Updates restart the installed GUI, which performs
this refresh. MSI installs, repairs and upgrades also refresh existing GUI
registrations using `INSTALLFOLDER` even when the GUI is not opened. Disabled
registrations are never enabled by an update. A moved macOS app bundle needs
one manual launch to refresh its absolute login path, as does a manually moved
Linux/Windows portable installation.

macOS can also restore apps that were running at shutdown through **Reopen
windows when logging back in**. This is independent of Superba's login item:
turning off autostart does not turn off system session restoration. Quit
Superba before logging out, or clear that option in the logout/restart dialog,
to avoid a restored start. **Start in the tray only** applies to a restored
start even without `--autostart`, just as it applies to a normal first start.
We do not infer restoration from an absent flag: manual starts also lack it,
and AppKit's
[default-launch notification key](https://developer.apple.com/documentation/appkit/nsapplication/launchisdefaultuserinfokey)
also describes file/service launches, so it is not a unique restoration marker.
The tray preference controls initial visibility; a later explicit reopen shows
the window. See Apple's
[reopening-window settings](https://support.apple.com/en-us/102318).

Choosing **Skip setup** saves the dismissal on every platform. It does not
create a provider or change a coding tool's configuration. The wizard can
still be opened from Superba → Settings → Run setup again.

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
