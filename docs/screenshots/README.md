# Documentation screenshots

Capture the real Superba application with synthetic profiles and
usage records. The fixture credentials do not authenticate to a real service.
Linux uses an isolated HOME and process namespace; the Windows capture tool uses a disposable VM
and a dedicated SuperbaDemo data directory. Images are not retouched.

The Superba page has five tabs: Overview, Updates, Settings, Diagnostics and Data.
General, Appearance, Network and Notifications are jump destinations on the same
scrolling Settings page. The main sidebar's Codex Settings page remains separate.

| Set | Capture tool | Coverage |
| --- | --- | --- |
| Linux (this directory) | `crates/superba-gui/tools/screenshots/shoot.py` and `codex_files_shots.py` | 19 destinations, actions, palette, wizard and Codex file editor; English/Chinese and light/dark |
| Windows (publish under `windows/` after review) | `e2e/windows/screenshots.py` | Eight Superba destinations; English/Chinese and light/dark |

Use a 1280×800 application window. Retain the existing Linux filenames for the
README links and place Windows screenshots in their own subdirectory. The capture
tools save raw frames; review the actual images for
private data, clipped content and the intended page before publishing them.

The Windows capture command requires the MSI's full source commit and records its
SHA256 with the screenshot hashes in the external evidence directory. Reproduce
using the instructions in [the Windows harness](../../e2e/windows/README.md#documentation-screenshots).

The refresh captured from `d1c95a77ca79fdcf3d762887ca6efa25658f6540` is not
published here: that build exposes missing service delegation on Tools and
Updates. Existing images remain until replacement captures from the designated
fixed commit have been reviewed. Do not treat the existing set as evidence of
the five-tab layout described above.
