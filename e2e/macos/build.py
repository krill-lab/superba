#!/usr/bin/env python3
"""Archive a git ref, rsync to macOS, build arm64; use an exact-ref CI DMG if full."""

import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from e2e.macos.lib.vm import Blocked, VmError, configured_vm  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
MIN_BYTES = 25 * 1024**3


def local(argv, **kwargs):
    return subprocess.run(argv, check=True, text=True, capture_output=True, **kwargs).stdout.strip()


def extract_archive(archive, destination):
    with tarfile.open(archive) as tar:
        for member in tar.getmembers():
            path = Path(member.name)
            if path.is_absolute() or ".." in path.parts or any(part in (".private", ".secrets") for part in path.parts):
                raise ValueError("Archive contains a forbidden path")
            if not member.isfile() and not member.isdir() and not member.issym():
                raise ValueError("Archive contains a special file")
        # Extract manually for Python 3.11, checking resolved paths after each
        # entry. A later member must not follow a previous symlink outside.
        for member in tar.getmembers():
            target = destination / member.name
            if not target.resolve().is_relative_to(destination.resolve()):
                raise ValueError("Archive contains an escaping path")
            target.parent.mkdir(parents=True, exist_ok=True)
            if member.isdir():
                target.mkdir(exist_ok=True)
            elif member.issym():
                link = target.parent / member.linkname
                if not link.resolve().is_relative_to(destination.resolve()):
                    raise ValueError("Archive contains an escaping link")
                target.symlink_to(member.linkname)
            else:
                with tar.extractfile(member) as source, target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(member.mode & 0o777)


def download_ci(ref, out, run_id=None):
    env = dict(os.environ)
    helper = env.get("SUPERBA_GH_TOKEN_HELPER")
    if not helper:
        raise Blocked("Set SUPERBA_GH_TOKEN_HELPER for the CI DMG fallback")
    env["GH_TOKEN"] = local([helper])
    if not env["GH_TOKEN"]:
        raise ValueError("Token helper returned no token")

    def gh(*args):
        return local(["gh", *args], env=env)

    if run_id:
        run = json.loads(gh("run", "view", str(run_id), "--repo", "krill-lab/superba",
                            "--json", "databaseId,headSha,url"))
        candidates = [run]
    else:
        candidates = json.loads(gh("run", "list", "--repo", "krill-lab/superba", "--commit", ref,
                                   "--limit", "50", "--json", "databaseId,headSha,url"))
    for run in candidates:
        if run["headSha"] != ref:
            continue
        artifacts = json.loads(gh("api", f"repos/krill-lab/superba/actions/runs/{run['databaseId']}/artifacts"))
        if not any(a["name"] == "dmg-test" and not a["expired"] for a in artifacts["artifacts"]):
            continue
        destination = out / ("ci-" + str(run["databaseId"]))
        if destination.exists():
            raise ValueError("CI download directory already exists; choose a fresh output directory")
        gh("run", "download", str(run["databaseId"]), "--repo", "krill-lab/superba",
           "--name", "dmg-test", "--dir", str(destination))
        dmgs = list(destination.rglob("*.dmg"))
        if len(dmgs) != 1:
            raise ValueError("Expected exactly one DMG in CI artifact")
        result = {"mode": "ci-dmg", "ref": ref, "run": run, "dmg": str(dmgs[0])}
        (out / "build.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        return result
    raise Blocked("No unexpired dmg-test artifact matches the requested ref; no workflow was triggered")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", default="origin/main")
    parser.add_argument("--out", required=True)
    parser.add_argument("--ci-run", type=int, help="fallback run; must match the requested git ref")
    parser.add_argument("--ci-only", action="store_true")
    parser.add_argument("--wait-lock", type=int, default=0)
    opts = parser.parse_args()
    ref = local(["git", "rev-parse", "--verify", opts.ref + "^{commit}"], cwd=REPO)
    out = Path(opts.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    vm, lease = configured_vm()
    lease.wait = opts.wait_lock
    with lease:
        if shutil.disk_usage(REPO).free < 30 * 1024**3:
            raise Blocked("Host filesystem has less than 30 GiB free")
        free = int(vm.shell("/bin/df -k /Users/Shared | /usr/bin/awk 'NR==2 {print $4}'").stdout.strip()) * 1024
        if free < MIN_BYTES or opts.ci_only:
            reason = f"VM has {free / 1024**3:.1f} GiB free; build requires at least 25 GiB"
            (out / "disk-check.txt").write_text(reason + "\n")
            print(reason + "; selecting CI DMG" if not opts.ci_only else "CI-only requested")
            download_ci(ref, out, opts.ci_run)
            return 0
        destination = vm.root + "/build/" + ref
        with tempfile.TemporaryDirectory(prefix="macos-archive-", dir=out) as staging:
            staging = Path(staging)
            archive = staging / "source.tar"
            local(["git", "archive", "--format=tar", "--output", str(archive), ref], cwd=REPO)
            tree = staging / "src"
            tree.mkdir()
            extract_archive(archive, tree)
            vm.shell("mkdir -p " + shlex.quote(destination))
            local(["rsync", "-az", "--delete", "--exclude=.private", "--exclude=.secrets",
                   "-e", str(REPO / "e2e/macos/rsync-ssh.py"), str(tree) + "/", "vm:" + destination + "/"])
        # Run the reviewed driver from this checkout, while building the archived ref.
        vm.put(REPO / "e2e/macos/guest/build.sh", destination + "/macos-e2e-build.sh")
        command = shlex.join(["/bin/bash", destination + "/macos-e2e-build.sh", destination])
        result = vm.shell(command, timeout=7200, check=False)
        (out / "build.log").write_text(result.stdout + result.stderr)
        if result.returncode:
            if result.returncode == 75:
                download_ci(ref, out, opts.ci_run)
                return 0
            raise VmError("Guest build failed; see build.log")
        remote = destination + "/dist/superba-dev-arm64.dmg"
        local_dmg = out / "superba-dev-arm64.dmg"
        vm.get(remote, local_dmg)
        result = {"mode": "vm-build", "ref": ref, "dmg": str(local_dmg), "guest_source": destination}
        (out / "build.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Blocked as error:
        print("BLOCKED: " + str(error), file=sys.stderr)
        sys.exit(2)
    except (VmError, ValueError, subprocess.CalledProcessError) as error:
        # CalledProcessError includes argv; token is only in the child's env.
        print(str(error), file=sys.stderr)
        sys.exit(1)
