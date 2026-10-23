"""alpha.8 Windows acceptance harness fixes (H1): the mock release server and
the version the CI fixture MSI's programs report."""
import json
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import flows
from scenarios.more import _Release


class ReleaseServerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        asset = Path(self.tmp.name) / "superba-9.0.0-windows-x86_64.msi"
        asset.write_bytes(b"msi")
        self.srv = ThreadingHTTPServer(("127.0.0.1", 0), _Release)
        self.srv.tag, self.srv.files, self.srv.hits = "v9.0.0", [asset], []
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.srv.server_address[1]}"

    def tearDown(self):
        self.srv.shutdown()
        self.srv.server_close()
        self.tmp.cleanup()

    def get(self, path):
        with urllib.request.urlopen(self.base + path, timeout=10) as r:
            return r.read()

    def test_update_now_reads_the_release_by_its_tag(self):
        release = json.loads(self.get("/api/releases/tags/v9.0.0"))
        self.assertEqual(release["tag_name"], "v9.0.0")
        self.assertEqual([a["name"] for a in release["assets"]], ["superba-9.0.0-windows-x86_64.msi"])
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.get("/api/releases/tags/v1.0.0")
        self.assertEqual(e.exception.code, 404)

    def test_latest_and_list_still_answer(self):
        self.assertEqual(json.loads(self.get("/api/releases/latest"))["tag_name"], "v9.0.0")
        self.assertEqual(json.loads(self.get("/api/releases"))[0]["tag_name"], "v9.0.0")


class ProgramVersionTests(unittest.TestCase):
    def test_the_fixture_msi_installs_the_workspace_version(self):
        version = flows.workspace_version()
        self.assertRegex(version, r"^\d+\.\d+\.\d+")
        self.assertEqual(flows.program_version("9.0.0"), version)
        self.assertEqual(flows.program_version("0.1.1-alpha.9"), "0.1.1-alpha.9")


if __name__ == "__main__":
    unittest.main()
