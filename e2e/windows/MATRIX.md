# Windows UI regression additions for alpha.7

Run `python3 e2e/windows/run.py list` for the full registered matrix. The rows below
cover the Superba navigation and tool integration changes. A **SKIP** means the
feature was not exercised; screenshot review items still need a human verdict.

| ID | Entry / coverage | Required integration |
|---|---|---|
| W36 | Superba → Diagnostics; installed coding tools and reasons | M2; tool table from M5 |
| W40 | Overview → Actions → Restore original, located by locale key | M2 |
| W50 | Superba → Updates → ChatGPT desktop app; expand the row in the unified list | M2 / M3 |
| W51 | Superba → Updates; expand desktop Codex pin and local CLI details | M2 / M3 |
| W52 | GUI self-update from an older MSI, then verify Superba → Updates in the new MSI | `--old-msi`; target MSI newer than source |
| W53 | Failed MSI update: toast, expanded Superba row, log and CLI report | M2 / M3 |
| W60 | 19 destinations in English and Chinese | M2 |
| W61 | Themes; Superba → Settings → Appearance for backgrounds | M2 |
| W62 | Three window sizes; pinned Superba, five tabs and four fixed settings anchors at 960×640 | M2b |
| W64 | Tray lifecycle; close behavior in Superba → Settings → General | M2 |
| W91 | Tools → opencode: apply Krill, manually change an owned key with CRLF, detect drift, restore original bytes | M3 + M4 + t1; mock network |
| W92 | Same configuration workflow for pi | M3 + M4 + t1; mock network |
| W93 | Same configuration workflow for ZCode | M3 + M4 + t1; mock network |
| W94 | Same configuration workflow for WorkBuddy | M3 + M4 + t1; mock network |
| W95 | Superba → Settings → Notifications; generated Codex approval and Claude lifecycle commands executed unchanged by PowerShell, with ordinary paths and paths containing spaces | M3 + M4 + t1; mock network |
| W96 | Superba → Updates: install / uninstall Claude Code, compare `spb tools list` | M3 + M4 + t1; vendor downloads |
| W97 | Same install / uninstall workflow for opencode | M3 + M4 + t1; vendor downloads |
| W98 | Same install / uninstall workflow for pi | M3 + M4 + t1; vendor downloads |
| W99 | Same install / uninstall workflow for dsh; standalone variant without Node | M3 + M4 + t1; vendor downloads |
| W100 | Same install / uninstall workflow for ZCode; signature log review | M3 + M4 + t1; vendor downloads |
| W101 | Same install / uninstall workflow for WorkBuddy; signature log review | M3 + M4 + t1; vendor downloads |

The 19 destinations are Overview, Tools, Providers, Models, Router, Usage, Codex
Settings, Sessions, Hosts, Claude Code, Claude Code Settings, and the eight Superba
sections: Home, Updates, Diagnostics, Data & backups, General, Appearance, Network,
Notifications. Superba has five top tabs: Overview, Updates, Settings, Diagnostics,
Data. General, Appearance, Network and Notifications are anchors on the scrolling
Settings page. The driver scopes top tabs to the UIA Tab named Superba to avoid
the main sidebar’s same-named Overview and Settings destinations. Anchor completion
requires a visible heading below the fixed jump row and stable bounds.

A “ChatGPT is still running” dialog is reported by its localized name immediately.
When force quit is allowed, the harness tries “End these processes” once and records
a warning; a repeated dialog fails explicitly. Result toasts are polled across
frames. An observed WSL switch timeout remains **WARN** after later success in the
same run; a failed check remains **FAIL**.

W88 explicitly enables following before testing stopped distributions, so a timed
out first probe cannot leave its pending-update precondition unset. W82 covers the
default follow behavior for a running distribution.

W50's large app download, W51's destructive pin/version changes and the layout
judgments remain explicit manual checks in their reports. An installation check
must not be called verified while its row is SKIP or has outstanding manual items.
