# One-click import from the Krill website (`superba://` links)

A Krill user clicks **Import into Superba** on the Krill website, the browser
opens Superba, and Superba asks once before it saves the key in its Krill
provider. This page defines the link format (version 1), what Superba does
with it, how Superba registers the `superba:` scheme on each system, and what
the website must keep in mind, since the link carries an API key.

## Link format, version 1

```text
superba://import/v1?key=<API key>[&name=<display name>]
```

| Part | Rule |
| --- | --- |
| scheme | `superba` (case-insensitive) |
| action (host) | `import` (case-insensitive) |
| version (path) | exactly `/v1`. Any other version, `/v1/` included, is refused with "update Superba". |
| query | `application/x-www-form-urlencoded`, as JS `URLSearchParams` writes it: `+` is a space, `%2B` is `+`, `%2F` is `/`, `%3D` is `=` |
| `key` | required. 8 to 512 visible ASCII characters with no spaces, `"`, `'` or `\` (the same characters the provider form and every Codex config file refuse). `sk-krill-…` keys fit. Surrounding spaces are trimmed. |
| `name` | optional. The provider's display name: 1 to 64 characters, no control characters, trimmed. The website always sends `Krill`. |
| anything else | refused: unknown parameters (including the earlier draft's `line` and `code`), a parameter given twice, a fragment (`#…`), user info, a port, or a link longer than 2048 bytes. A new parameter needs a new version. |

The link names no site, line or address. It always means **the built-in
Krill provider**, so a link cannot point Superba, or the key, at any other
server. Superba makes no network request to import the key.

### Building the link (website)

```js
button.addEventListener("click", async () => {
  const key = await createOrFetchSuperbaKey();          // the logged-in user's key
  const query = new URLSearchParams({ key, name: "Krill" });
  window.location.href = "superba://import/v1?" + query.toString();
});
```

Navigate from the click handler. Don't put the link in a static `href`, a
QR code, an e-mail or a chat message.

Sample: `superba://import/v1?key=sk-krill-FAKE0000example&name=Krill`.

## What Superba does

1. If Superba is already running, the link goes to that window, which comes
   to the front (also out of the tray). If it is not running, it starts with
   the link.
2. A confirmation dialog shows:
   - the provider (Krill);
   - the masked key (`••••` and its last four characters);
   - the Krill line it will use. For an existing provider, its current line is
     kept. A new provider gets the fastest reachable line Superba measures,
     else the default line, the same choice the setup wizard makes;
   - what changes: **Update its key** for an existing Krill provider (its lines
     and settings stay), or **Add as a new provider**;
   - **Switch Codex to this provider now** (on by default);
   - a warning to continue only if the user just clicked Import on the Krill
     website.
3. **Cancel** or Escape writes nothing. **Import** saves the key in Superba's
   key storage and the provider with API-key authentication, then switches
   Codex to it through the normal switch, or offers to switch.

A refused link opens a short message that says why. Nothing is imported.

The existing Krill provider is the one Codex currently uses, else the one last
selected, else the first Krill provider. Providers of custom sites are never
updated by a link, even when they are named "Krill".

## Registering the `superba:` scheme

| System | How |
| --- | --- |
| Linux | Each time the Superba window starts, it writes `~/.local/share/applications/superba-url-handler.desktop` (or under `$XDG_DATA_HOME`, hidden from menus, `Exec="<path to superba>" %u`, `MimeType=x-scheme-handler/superba;`). It then runs `update-desktop-database` on that folder and, if no other program is the scheme's default, `xdg-mime default superba-url-handler.desktop x-scheme-handler/superba`. The `superba.desktop` in the tarball also lists the scheme. To register by hand: copy it to `~/.local/share/applications/`, then run `update-desktop-database ~/.local/share/applications` and `xdg-mime default superba.desktop x-scheme-handler/superba`. |
| Windows | The MSI writes `HKCU\Software\Classes\superba` (`URL Protocol`, `shell\open\command` = `"…\superba.exe" "%1"`). Uninstalling removes it. |
| macOS | `Superba.app`'s `Info.plist` declares the scheme (`CFBundleURLTypes`). The link arrives as an Apple event. |

## Security notes

- **The key is in the URL.** It can end up in the browser's history, which
  may be synced, and in the browser's "Open Superba?" prompt. On Linux, every
  local user can briefly see it in the command line of the short-lived
  `xdg-open` / `superba` processes that hand it over. The website should:
  - show the button only to a logged-in user, on their own account pages,
    and create or fetch the key only when the button is clicked;
  - prefer a dedicated key named, for example, "Superba", so the user can
    revoke that key alone;
  - tell the user how to revoke the key if the link leaked;
  - never send the link by e-mail, chat or a QR code, and never log it.
    The navigation stays inside the browser; a `superba:` URL is never sent
    to a server.
- **Phishing.** Anyone can build a link with their own key. Importing it
  would make the user's Codex run on someone else's Krill account, and that
  account could see the user's requests. Superba therefore never imports
  silently. The dialog shows the masked key and warns. The website can help
  by showing the same last four characters next to the button.
- **What Superba does with the link:**
  - it never logs the link or the key. Logs and messages show at most
    `superba://import/v1?key=…&name=…`;
  - it keeps no history or recent list of links;
  - it hands a link to a running window only over its authenticated loopback
    connection, never through a file;
  - on Unix, a Superba started with a link on its command line restarts
    itself at once with the link in its private environment. That
    environment is cleared before anything else runs, so the long-running
    process's command line doesn't show the key, and Codex and other
    programs started by Superba don't inherit it;
  - the key is stored like any key the user types: in the system key store,
    or the file store where configured.
