"""Archive a ref, run the CI test job over SSH, and collect durable evidence."""

import argparse
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def output(*args):
    return run(*args, stdout=subprocess.PIPE, text=True).stdout.strip()


def configuration(src, commit):
    workflow = yaml.safe_load((src / ".github/workflows/ci.yml").read_text())
    job = workflow["jobs"]["test"]
    steps = job["steps"]
    tests = [s for s in steps if "cargo test " in s.get("run", "")]
    if len(tests) != 1:
        raise ValueError("Expected exactly one cargo test step in jobs.test")
    step = tests[0]
    command = step["run"].strip()
    if "${{" in command or step.get("if") or job.get("container"):
        raise ValueError(
            "Conditional/expression/container test steps need manual review"
        )
    env = {}
    for values in (workflow.get("env", {}), job.get("env", {}), step.get("env", {})):
        env.update({k: str(v) for k, v in values.items()})
    for key, value in env.items():
        if not re.fullmatch(r"[A-Z_][A-Z_0-9]*", key) or "${{" in value:
            raise ValueError(f"Unsupported workflow environment: {key}")
        if key in {
            "HOME",
            "PATH",
            "TMPDIR",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_TARGET_DIR",
        }:
            raise ValueError(f"Workflow overrides isolation variable: {key}")
    toolchain = src / "rust-toolchain.toml"
    legacy = src / "rust-toolchain"
    if toolchain.exists():
        channel = tomllib.loads(toolchain.read_text())["toolchain"]["channel"]
    elif legacy.exists():
        data = legacy.read_text().strip()
        channel = (
            tomllib.loads(data)["toolchain"]["channel"]
            if "[toolchain]" in data
            else data
        )
    else:
        # Older refs have no rust-toolchain file: use their CI pin, never host default.
        installs = "\n".join(s.get("run", "") for s in steps)
        pins = re.findall(r"rustup toolchain install ([\w.\-]+)", installs)
        if len(pins) != 1:
            raise ValueError("No unambiguous toolchain file or CI Rust pin")
        channel = pins[0]
    env.update(RUSTUP_TOOLCHAIN=channel, TEST_COMMAND=command, GIT_COMMIT=commit)
    return env


def summarize(log_dir, commit, ref, exit_code, error=None):
    path = log_dir / "tests.log"
    log = path.read_text(errors="replace") if path.exists() else ""
    log = re.sub(r"\x1b\[[0-9;]*m", "", log)
    totals = re.findall(
        r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", log
    )
    passed, failed, ignored = (sum(int(row[i]) for row in totals) for i in range(3))
    failures = re.findall(r"^test (.+?) \.\.\. FAILED\s*$", log, re.MULTILINE)
    # A compiler/SSH/bootstrap failure must never become a green zero-test run.
    if exit_code == 0 and failed:
        exit_code = 1
        error = "Test failures contradict successful remote exit status"
    if error and exit_code == 0:
        exit_code = 1
    if exit_code == 0 and (not totals or passed == 0):
        exit_code = 1
        error = "No completed, nonempty Rust test results found"
    result = {
        "commit": commit,
        "ref": ref,
        "exit_code": exit_code,
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "test_suites": len(totals),
        "failed_tests": failures,
        "error": error,
    }
    (log_dir / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    report = (
        f"# macOS VM test: {'PASS' if exit_code == 0 else 'FAIL'}\n\n"
        f"- Commit: `{commit}`\n- Requested ref: `{ref}`\n"
        f"- Passed: **{passed}**; failed: **{failed}**; ignored: **{ignored}**\n"
        f"- Completed test suites: {len(totals)}\n- Exit code: {exit_code}\n"
        "- Platform: macOS arm64 (version recorded in full.log)\n"
        "- Command/environment: archived workflow jobs.test; see config.json\n"
        "- Logs: full.log, tests.log, controller.log; machine result: result.json\n"
    )
    if failures:
        report += (
            "\nFailed tests:\n\n" + "\n".join(f"- `{name}`" for name in failures) + "\n"
        )
    if error:
        report += f"\nInfrastructure/report error: {error}\n"
    (log_dir / "RESULT.md").write_text(report)
    print(report, flush=True)
    return exit_code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("git_ref")
    args = parser.parse_args()
    commit = output(
        "git",
        "-C",
        str(ROOT),
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{args.git_ref}^{{commit}}",
    )
    sha = commit[:7]
    common = Path(
        output(
            "git",
            "-C",
            str(ROOT),
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        )
    ).parent
    helper = Path(
        os.environ.get("MACVM_SSH_HELPER", common / ".secrets" / "macvm-ssh.sh")
    ).resolve()
    if not helper.is_file() or not os.access(helper, os.X_OK):
        parser.error(
            "Set MACVM_SSH_HELPER to an executable SSH helper (remote command as argument)"
        )
    log_root = Path(
        os.environ.get("MACVM_LOG_ROOT", common / ".private" / "vmci")
    ).resolve()
    log_dir = log_root / sha
    log_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Preserve earlier evidence rather than overwrite a previous test of this ref.
    if (log_dir / "RESULT.md").exists():
        parser.error(
            f"Results already exist at {log_dir}; use a different MACVM_LOG_ROOT for a rerun"
        )
    rc, error, remote_root, reserved, launched = 1, None, None, False, False
    with tempfile.TemporaryDirectory(prefix="stage-", dir=log_root) as staging:
        staging = Path(staging)
        adapter = staging / "ssh-rsync"
        adapter.write_text('#!/bin/sh\nshift\nexec "$MACVM_SSH_HELPER" "$@"\n')
        adapter.chmod(0o700)
        transport_env = dict(os.environ, MACVM_SSH_HELPER=str(helper))

        def ssh(command, **kwargs):
            return run(str(helper), command, **kwargs)

        def sync(source, destination):
            run(
                "rsync",
                "-a",
                "--exclude=.private",
                "--exclude=.secrets",
                "-e",
                str(adapter),
                source,
                destination,
                env=transport_env,
            )

        try:
            archive = staging / "source.tar"
            with archive.open("wb") as stream:
                run(
                    "git",
                    "-C",
                    str(ROOT),
                    "archive",
                    "--format=tar",
                    commit,
                    stdout=stream,
                )
            src = staging / "src"
            src.mkdir()
            with tarfile.open(archive) as tar:
                members = [
                    m
                    for m in tar.getmembers()
                    if not {".private", ".secrets"}.intersection(Path(m.name).parts)
                ]
                # Git archives contain only directories, files and symlinks.
                # Extract explicitly for Python 3.11 versions without tar filters.
                for member in members:
                    dest = src / member.name
                    if not dest.resolve().is_relative_to(src.resolve()):
                        raise ValueError("Archive path escapes staging directory")
                    if member.isdir():
                        dest.mkdir(parents=True, exist_ok=True)
                    elif member.isfile():
                        dest.parent.mkdir(parents=True, exist_ok=True)
                        with (
                            tar.extractfile(member) as source,
                            dest.open("wb") as target,
                        ):
                            shutil.copyfileobj(source, target)
                        dest.chmod(member.mode & 0o777)
                    elif member.issym():
                        target = (dest.parent / member.linkname).resolve()
                        if not target.is_relative_to(src.resolve()):
                            raise ValueError(
                                "Archive symlink escapes staging directory"
                            )
                        dest.parent.mkdir(parents=True, exist_ok=True)
                        dest.symlink_to(member.linkname)
                    else:
                        raise ValueError("Unsupported archive entry")
            env = configuration(src, commit)
            (log_dir / "config.json").write_text(json.dumps(env, indent=2) + "\n")
            (staging / "config.sh").write_text(
                "".join(
                    f"export {key}={shlex.quote(value)}\n" for key, value in env.items()
                )
            )
            remote_home = ssh(
                'printf "%s" "$HOME"', stdout=subprocess.PIPE, text=True
            ).stdout
            if not re.fullmatch(r"/[A-Za-z0-9_./-]+", remote_home):
                raise ValueError("Unsupported remote home path")
            remote_root = f"{remote_home}/ci"
            remote_src = f"{remote_root}/src-{sha}"
            remote_log = f"{remote_root}/logs-{sha}"
            remote_home = f"{remote_root}/home-{sha}"
            # Separate from the GUI lock: protects the shared Cargo cache from other unit runs.
            ssh(f"mkdir -p {remote_root} && mkdir {remote_root}/unit-test.lock")
            reserved = True
            # The cache lock is ours: discard stale per-run files, never caches.
            ssh(
                f"rm -rf {remote_src} {remote_home} {remote_log} && "
                f"mkdir -p {remote_src} {remote_log} {remote_home}/tmp"
            )
            sync(f"{src}/", f"vm:{remote_src}/")
            sync(str(staging / "config.sh"), f"vm:{remote_log}/config.sh")
            sync(
                str(ROOT / "scripts-ci/mac-vm-test-remote.sh"),
                f"vm:{remote_log}/run.sh",
            )
            command = (
                f"env -i HOME={remote_home} TMPDIR={remote_home}/tmp "
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin "
                f"/bin/bash {remote_log}/run.sh {sha} {remote_root}"
            )
            with (log_dir / "controller.log").open("w") as stream:
                launched = True
                proc = subprocess.run(
                    [str(helper), command],
                    stdout=stream,
                    stderr=subprocess.STDOUT,
                    check=False,
                )
            rc = proc.returncode
        except (
            OSError,
            ValueError,
            KeyError,
            TypeError,
            yaml.YAMLError,
            subprocess.CalledProcessError,
            tarfile.TarError,
        ) as exc:
            error = str(exc)
            print(error, file=sys.stderr)
        finally:
            if reserved and not launched:
                # Upload/setup failed before any remote build could start.
                try:
                    ssh(
                        f"rm -rf {remote_root}/src-{sha} {remote_root}/home-{sha} && "
                        f"rmdir {remote_root}/unit-test.lock"
                    )
                except subprocess.CalledProcessError as exc:
                    error = f"{error}; pre-launch cleanup failed: {exc}"
            if reserved and launched:
                try:
                    sync(f"vm:{remote_root}/logs-{sha}/", f"{log_dir}/")
                    status = log_dir / "exit-code"
                    if status.exists():
                        rc = int(status.read_text().strip())
                    else:
                        rc, error = 1, error or "Remote completion marker missing"
                    # Remote EXIT trap cleans source/HOME and releases the cache lock.
                    # Never remove an active run's lock after a broken connection.
                    if not status.exists():
                        error = (
                            error or ""
                        ) + "; inspect remote unit-test.lock before retrying"
                except (OSError, ValueError, subprocess.CalledProcessError) as exc:
                    rc, error = 1, f"Could not collect complete remote logs: {exc}"
    return summarize(log_dir, commit, args.git_ref, rc, error)


if __name__ == "__main__":
    sys.exit(main())
