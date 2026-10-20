"""Download OpenAI's full ChatGPT DMG and verify it before installation.

The current app contains Codex; the separate ChatGPT Classic DMG is not used.
The official download page is https://chatgpt.com/download/.
"""

import hashlib
import json
import plistlib
import re
import time
import urllib.parse
import urllib.request

DOWNLOAD = "https://persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg"
# https://help.openai.com/en/articles/20001159-how-to-add-openai-apps-to-your-macos-application-allowlist
TEAM = "2DC432GLL2"


def official_url(url):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != "https" or parsed.hostname != "persistent.oaistatic.com"
            or parsed.username or parsed.password or parsed.port not in (None, 443)
            or not parsed.path.startswith("/codex-app-prod/")):
        raise ValueError("Download left the official OpenAI distribution origin")
    return url


class OfficialRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        official_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(destination):
    opener = urllib.request.build_opener(OfficialRedirect())
    digest = hashlib.sha256()
    size = 0
    started = time.monotonic()
    with opener.open(official_url(DOWNLOAD), timeout=60) as response, destination.open("xb") as stream:
        resolved = official_url(response.geturl())
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > 2 * 1024**3 or time.monotonic() - started > 900:
                raise RuntimeError("Official desktop download exceeded its size/time limit")
            stream.write(chunk)
            digest.update(chunk)
    return {"source_page": "https://chatgpt.com/download/", "url": DOWNLOAD,
            "resolved_url": resolved, "bytes": size, "sha256": digest.hexdigest()}


def install(root, guest):
    destination = guest.Path("/Applications/ChatGPT.app")
    if destination.exists() or destination.is_symlink() or guest.Path("/Applications/Codex.app").exists():
        raise guest.Blocked("M02 needs empty ChatGPT/Codex installation slots")
    if guest.desktop_user_data():
        raise guest.Blocked("M02 cannot use existing OpenAI desktop user data")
    report = {"status": "STARTED", "checks": [], "account_login": False}
    evidence = root / "desktop-verification.json"

    def save_report():
        evidence.write_text(json.dumps(report, indent=2))

    def check(name, argv):
        result = guest.command(argv, timeout=180)
        report["checks"].append({"name": name, "code": result.returncode,
                                 "stdout": result.stdout, "stderr": result.stderr})
        save_report()
        if result.returncode:
            raise RuntimeError("Official desktop verification failed: " + name)
        return result.stdout + result.stderr

    save_report()
    mount = root / "chatgpt-mount"
    attached = False
    try:
        report["download"] = download(root / "ChatGPT.dmg")
        save_report()
        check("dmg-integrity", ["/usr/bin/hdiutil", "verify", root / "ChatGPT.dmg"])
        mount.mkdir()
        st = guest.state(root)
        st["mounts"].append(str(mount))
        guest.save(root, st)
        check("mount-read-only", ["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse",
                                  "-noautoopen", "-mountpoint", mount, root / "ChatGPT.dmg"])
        attached = True
        app = mount / "ChatGPT.app"
        info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
        if info.get("CFBundleIdentifier") != "com.openai.codex":
            raise RuntimeError("DMG is not the current ChatGPT app containing Codex")
        identity = check("identity", ["/usr/bin/codesign", "-dv", "--verbose=4", app])
        if not re.search(r"^Authority=Developer ID Application: OpenAI [^\n]+\(" + TEAM + r"\)$",
                         identity, re.M) or "TeamIdentifier=" + TEAM not in identity:
            raise RuntimeError("Unexpected ChatGPT signing identity")
        check("signature", ["/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2",
                            "-R", 'anchor apple generic and certificate leaf[subject.OU] = "' + TEAM + '"', app])
        assessment = check("notarization", ["/usr/sbin/spctl", "--assess", "--type", "execute", "-vv", app])
        if "accepted" not in assessment or "source=Notarized Developer ID" not in assessment:
            raise RuntimeError("ChatGPT lacks a notarized Developer ID assessment")
        check("stapled-ticket", ["/usr/bin/xcrun", "stapler", "validate", app])
        # Claim before ditto, so an interrupted partial copy is also owned.
        st = guest.state(root)
        st["desktop_installed_by_session"] = str(destination)
        guest.save(root, st)
        check("install", ["/usr/bin/ditto", app, destination])
        check("installed-signature", ["/usr/bin/codesign", "--verify", "--deep", "--strict", destination])
        report.update(status="VERIFIED", team=TEAM, bundle_id=info["CFBundleIdentifier"],
                      version=info.get("CFBundleShortVersionString"), build=info.get("CFBundleVersion"))
        return report
    except Exception as error:
        report.update(status="FAIL", failed_step=report["checks"][-1]["name"] if report["checks"] else "download",
                      error=type(error).__name__)
        raise
    finally:
        if attached:
            result = guest.command(["/usr/bin/hdiutil", "detach", mount])
            report["detach_code"] = result.returncode
            if result.returncode == 0:
                st = guest.state(root)
                st["mounts"].remove(str(mount))
                guest.save(root, st)
        save_report()
