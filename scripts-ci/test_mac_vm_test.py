"""Guard the CI gate against incorrect counts and workflow drift."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import mac_vm_test as vmci


class TestGate(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def config(
        self, extra_env="", command="cargo test --workspace --locked --no-fail-fast"
    ):
        workflow = self.root / ".github/workflows/ci.yml"
        workflow.parent.mkdir(parents=True, exist_ok=True)
        workflow.write_text(
            'env:\n  CARGO_INCREMENTAL: "0"\n'
            f"{extra_env}"
            "jobs:\n  test:\n    steps:\n"
            "      - run: rustup toolchain install 1.98 --profile minimal\n"
            "      - env:\n          SUPERBA_SECRET_BACKEND: file\n"
            '          RUST_BACKTRACE: "1"\n'
            f"        run: {command}\n"
        )
        return vmci.configuration(self.root, "a" * 40)

    def report(self, log, code, error=None):
        (self.root / "tests.log").write_text(log)
        with contextlib.redirect_stdout(io.StringIO()):
            status = vmci.summarize(self.root, "a" * 40, "test-ref", code, error)
        return status, json.loads((self.root / "result.json").read_text())

    def test_ci_pin_env_and_feature_flags(self):
        command = "cargo test --workspace --locked --no-fail-fast --features extra"
        env = self.config(command=command)
        self.assertEqual(env["TEST_COMMAND"], command)
        self.assertEqual(env["RUSTUP_TOOLCHAIN"], "1.98")
        self.assertEqual(env["CARGO_INCREMENTAL"], "0")
        self.assertEqual(env["SUPERBA_SECRET_BACKEND"], "file")
        self.assertEqual(env["RUST_BACKTRACE"], "1")

    def test_toolchain_file_takes_precedence(self):
        (self.root / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.99.0"\n'
        )
        self.assertEqual(self.config()["RUSTUP_TOOLCHAIN"], "1.99.0")

    def test_legacy_toolchain(self):
        (self.root / "rust-toolchain").write_text("1.99.0\n")
        self.assertEqual(self.config()["RUSTUP_TOOLCHAIN"], "1.99.0")

    def test_isolation_override_rejected(self):
        with self.assertRaisesRegex(ValueError, "isolation"):
            self.config(extra_env="  HOME: /somewhere\n")

    def test_expression_requires_review(self):
        with self.assertRaisesRegex(ValueError, "manual review"):
            self.config(command="cargo test --features ${{ matrix.features }}")

    def test_counts_multiple_suites_and_failure_names(self):
        status, result = self.report(
            "test result: ok. 5 passed; 0 failed; 2 ignored; 0 measured;\n"
            "test suite::broken ... \x1b[31mFAILED\x1b[0m\n"
            "test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured;\n",
            101,
        )
        self.assertEqual(status, 101)
        self.assertEqual(
            (result["passed"], result["failed"], result["ignored"]), (8, 1, 2)
        )
        self.assertEqual(result["failed_tests"], ["suite::broken"])

    def test_compile_failure_with_zero_tests_is_failure(self):
        status, result = self.report("error: could not compile example\n", 101)
        self.assertEqual(status, 101)
        self.assertEqual(result["passed"], 0)
        self.assertIn("FAIL", (self.root / "RESULT.md").read_text())

    def test_empty_success_is_rejected(self):
        status, _ = self.report("", 0)
        self.assertNotEqual(status, 0)

    def test_log_failure_overrides_exit_zero(self):
        status, _ = self.report(
            "test result: FAILED. 3 passed; 1 failed; 0 ignored;\n", 0
        )
        self.assertNotEqual(status, 0)

    def test_success(self):
        status, result = self.report(
            "test result: ok. 8 passed; 0 failed; 1 ignored;\n", 0
        )
        self.assertEqual(status, 0)
        self.assertEqual(result["passed"], 8)

    def test_infrastructure_error_overrides_success_marker(self):
        status, _ = self.report(
            "test result: ok. 8 passed; 0 failed; 1 ignored;\n", 0, "lost logs"
        )
        self.assertNotEqual(status, 0)


if __name__ == "__main__":
    unittest.main()
