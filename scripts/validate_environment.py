#!/usr/bin/env python3
"""Fail required CI lanes before tests can silently skip their filesystem cases."""

import argparse
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import subprocess
import tempfile


def run(args):
    return subprocess.check_output(args, text=True).strip()


def filesystem(path):
    path = path.resolve(strict=True)
    info = {"path": str(path), "device": path.stat().st_dev}
    usage = os.statvfs(path)
    info["capacity_bytes"] = usage.f_blocks * usage.f_frsize
    if platform.system() == "Darwin":
        device = run(["df", "-P", str(path)]).splitlines()[1].split()[0]
        detail = plistlib.loads(subprocess.check_output(["diskutil", "info", "-plist", device]))
        info.update({"filesystem": detail.get("FilesystemType"), "device_identifier": detail.get("DeviceIdentifier"), "mountpoint": detail.get("MountPoint"), "filesystem_personality": detail.get("FileSystemPersonality")})
    else:
        detail = json.loads(run(["findmnt", "-J", "-T", str(path), "-o", "FSTYPE,SOURCE,TARGET"]))["filesystems"][0]
        info.update({"filesystem": detail["fstype"], "device_identifier": detail["source"], "mountpoint": detail["target"]})
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--filesystem", choices=("apfs", "btrfs", "xfs"))
    parser.add_argument("--require-volumes", action="store_true")
    parser.add_argument("--git-series", help="require exact Git major.minor series, e.g. 2.39")
    args = parser.parse_args()
    git = run(["git", "--version"])
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", git)
    if match is None or tuple(map(int, match.groups()[:2])) < (2, 39):
        raise RuntimeError(f"Git 2.39 or later required: {git}")
    if args.git_series and ".".join(match.groups()[:2]) != args.git_series:
        raise RuntimeError(f"expected Git {args.git_series}, found {git}")
    report = {"os": platform.platform(), "git": git, "rust": run(["rustc", "--version"]), "temporary": filesystem(Path(tempfile.gettempdir())), "required_clone": os.environ.get("GIT_THIN_REQUIRE_CLONE") == "1"}
    with tempfile.TemporaryDirectory(prefix="git-thin-case-probe-") as temporary:
        root = Path(temporary)
        (root / "case-probe").touch()
        report["temporary"]["case_sensitive"] = not (root / "CASE-PROBE").exists()
        (root / "caf\u00e9").touch()
        report["temporary"]["unicode_normalization_distinct"] = not (root / "cafe\u0301").exists()
    if args.filesystem:
        if report["temporary"]["filesystem"] != args.filesystem:
            raise RuntimeError(f"required {args.filesystem} TMPDIR, found {report['temporary']}")
        if not report["required_clone"]:
            raise RuntimeError("native lane must export GIT_THIN_REQUIRE_CLONE=1")
    if args.require_volumes:
        for name in ("GIT_THIN_CROSS_VOLUME_ROOT", "GIT_THIN_EXHAUSTION_ROOT"):
            value = os.environ.get(name)
            if not value:
                raise RuntimeError(f"required test volume is missing: {name}")
            report[name] = filesystem(Path(value))
        devices = [report[key]["device"] for key in ("temporary", "GIT_THIN_CROSS_VOLUME_ROOT", "GIT_THIN_EXHAUSTION_ROOT")]
        if len(set(devices)) != 3:
            raise RuntimeError(f"test, cross-volume, and exhaustion paths must use three distinct filesystems: {devices}")
        exhaustion = report["GIT_THIN_EXHAUSTION_ROOT"]
        if Path(exhaustion["path"]) != Path(exhaustion["mountpoint"]).resolve():
            raise RuntimeError("exhaustion root must be the root of its dedicated mounted filesystem")
        if not 0 < exhaustion["capacity_bytes"] <= 256 * 1024 * 1024:
            raise RuntimeError("exhaustion filesystem must have positive capacity at most 256 MiB")
        if args.filesystem and exhaustion["filesystem"] != args.filesystem:
            raise RuntimeError("exhaustion filesystem must use the native lane's filesystem type")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
