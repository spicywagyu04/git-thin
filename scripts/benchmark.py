#!/usr/bin/env python3
"""Controlled, opt-in git-thin benchmark; retains all fixtures and changes."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import random
import shutil
import subprocess
import sys
import time


def run(args, cwd=None, env=None):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {args!r}\n"
            + result.stderr.decode("utf-8", errors="replace")
            + result.stdout.decode("utf-8", errors="replace")
        )
    return result.stdout


def sync_and_measure(path):
    # This intentionally sits outside timed creation/content-verification sections.
    os.sync()
    stat = os.statvfs(path)
    result = {
        "time_unix_seconds": time.time(),
        "filesystem_capacity_bytes": stat.f_blocks * stat.f_frsize,
        "filesystem_used_bytes": (stat.f_blocks - stat.f_bfree) * stat.f_frsize,
        "filesystem_available_bytes": stat.f_bavail * stat.f_frsize,
        "measurement": "filesystem counters; includes metadata and unrelated activity",
    }
    if sys.platform == "darwin":
        try:
            # diskutil accepts a device or mount point, not every nested directory.
            # POSIX df keeps the device in the first column even when mount paths
            # contain spaces. Pass the directory as an argument, never through a shell.
            rows = run(["df", "-P", str(path)]).splitlines()
            if len(rows) < 2 or not rows[1].split():
                raise RuntimeError("Could not resolve the benchmark filesystem device with df.")
            device = os.fsdecode(rows[1].split(maxsplit=1)[0])
            if not device.startswith("/dev/"):
                raise RuntimeError(f"Benchmark filesystem is not a local disk device: {device}")
            info = plistlib.loads(run(["diskutil", "info", "-plist", device]))
            listing = plistlib.loads(run(["diskutil", "apfs", "list", "-plist"]))
            for container in listing.get("Containers", []):
                volumes = container.get("Volumes", [])
                volume = next(
                    (v for v in volumes if v.get("DeviceIdentifier") == info.get("DeviceIdentifier")),
                    None,
                )
                if volume is None:
                    continue
                result["apfs"] = {
                    "container": container.get("ContainerReference"),
                    "volume": volume.get("DeviceIdentifier"),
                    "volume_capacity_in_use_bytes": volume.get("CapacityInUse"),
                    "container_capacity_bytes": container.get("CapacityCeiling"),
                    "container_free_bytes": container.get("CapacityFree"),
                    "sibling_volumes": [
                        {
                            "device": v.get("DeviceIdentifier"),
                            "capacity_in_use_bytes": v.get("CapacityInUse"),
                        }
                        for v in volumes
                    ],
                }
                break
            if "apfs" not in result:
                result["apfs_measurement_warning"] = "No matching APFS volume found."
        except (RuntimeError, plistlib.InvalidFileException, OSError) as error:
            result["apfs_measurement_warning"] = str(error)
    return result


def write_random(path, count, generator):
    with path.open("xb") as handle:
        remaining = count
        while remaining:
            size = min(remaining, 1024 * 1024)
            handle.write(generator.randbytes(size))
            remaining -= size


def tree_entries(source, env):
    entries = []
    for record in run(["git", "ls-tree", "-rz", "--full-tree", "HEAD"], source, env).split(b"\0"):
        if record:
            metadata, relative = record.split(b"\t", 1)
            mode, kind, oid = metadata.split(b" ")
            if kind != b"blob" or mode not in (b"100644", b"100755"):
                raise RuntimeError("Benchmark fixture must contain only regular tracked files.")
            entries.append((os.fsdecode(relative), oid.decode("ascii")))
    return entries


def verify_contents(root, entries, object_format):
    started = time.perf_counter()
    for relative, expected_oid in entries:
        path = root / relative
        digest = hashlib.new(object_format)
        digest.update(f"blob {path.stat().st_size}\0".encode("ascii"))
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
        if digest.hexdigest() != expected_oid:
            raise RuntimeError(f"Content differs from creation commit: {path}")
    return time.perf_counter() - started


def edit_fixture(root, entries, count, seed):
    generator = random.Random(seed)
    remaining = count
    for relative, _ in entries:
        if not remaining:
            break
        path = root / relative
        size = min(remaining, path.stat().st_size)
        with path.open("r+b") as handle:
            written = 0
            while written < size:
                block_size = min(size - written, 1024 * 1024)
                handle.write(generator.randbytes(block_size))
                written += block_size
        remaining -= size
    return count - remaining


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="new directory on the filesystem being measured")
    parser.add_argument("--output", type=Path, required=True, help="new JSON result file outside root")
    parser.add_argument("--layout", choices=("large", "small"), required=True)
    parser.add_argument("--method", choices=("thin", "checkout"), default="thin")
    parser.add_argument("--bytes", type=int, default=8_000_000_000, help="logical fixture bytes; default 8 GB")
    parser.add_argument("--small-file-bytes", type=int, default=64 * 1024)
    parser.add_argument("--worktrees", type=int, default=3)
    parser.add_argument("--edit-bytes", type=int, default=16 * 1024 * 1024, help="in-place writes per worktree")
    parser.add_argument("--seed", type=int, default=20261022)
    parser.add_argument("--git-thin", default="git-thin", help="binary path or executable in PATH")
    parser.add_argument("--run", action="store_true", help="required acknowledgement that fixtures consume disk")
    args = parser.parse_args()
    for field in ("bytes", "small_file_bytes", "worktrees", "edit_bytes"):
        if getattr(args, field) <= 0:
            parser.error(f"--{field.replace('_', '-')} must be positive")
    if not args.run:
        parser.error("pass --run to create the benchmark fixture; read docs/benchmarks.md first")
    return args


def main():
    args = parse_args()
    if args.root.is_symlink() or args.output.is_symlink():
        raise RuntimeError("Benchmark root and report must not be symbolic links.")
    root = args.root.resolve()
    output = args.output.resolve()
    if root.exists() or root.is_symlink():
        raise RuntimeError(f"Refusing to reuse an existing directory: {root}")
    if root in output.parents or output == root:
        raise RuntimeError("Keep the report outside the measured root (preferably on another volume).")
    if output.exists() or output.is_symlink():
        raise RuntimeError(f"Refusing to overwrite a report: {output}")
    if not output.parent.is_dir():
        raise RuntimeError(f"Report parent directory does not exist: {output.parent}")
    binary = shutil.which(args.git_thin)
    if args.method == "thin" and binary is None:
        raise RuntimeError(f"git-thin executable not found: {args.git_thin}")
    if binary is not None:
        binary = os.path.abspath(binary)
    # Reserve room for the source, its Git blobs, changes and ordinary checkout controls.
    reserve = args.bytes * (3 + (args.worktrees if args.method == "checkout" else 0))
    if shutil.disk_usage(root.parent).free < reserve:
        raise RuntimeError(f"Need at least {reserve:,} free bytes in the benchmark parent.")
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("GIT_"):
            del env[key]
    env.update({"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"})
    report = {
        "schema_version": 1,
        "status": "running",
        "parameters": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
        "platform": platform.platform(),
        "python": platform.python_version(),
        "git": run(["git", "--version"], env=env).decode().strip(),
        "worktrees": [],
        "measurements": [],
        "limitations": [
            "Allocation counters include filesystem metadata, snapshots, delayed allocation, and other activity.",
            "Logical file bytes and file st_blocks/du sums are not exact shared-extent accounting.",
            "Warm caches and a synthetic fixture affect elapsed times; record cold/warm conditions separately.",
        ],
    }
    if binary:
        report["git_thin"] = run([binary, "--version"], env=env).decode().strip()
    root.mkdir()  # Exclusive creation; the harness never deletes data, even on failure.
    try:
        report["measurements"].append({"phase": "before_source", **sync_and_measure(root)})
        source = root / "source"
        source.mkdir()
        run(["git", "init", "--initial-branch=main", str(source)], env=env)
        for key, value in (
            ("user.name", "git-thin benchmark"),
            ("user.email", "benchmark@example.invalid"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.compression", "0"),
            ("gc.auto", "0"),
            ("core.hooksPath", os.devnull),
        ):
            run(["git", "config", key, value], source, env)
        run(["git", "commit", "--allow-empty", "-m", "Initialize benchmark"], source, env)
        if args.method == "thin":
            report["doctor"] = json.loads(run([binary, "--json", "doctor"], source, env))
        fixture = source / "fixture"
        fixture.mkdir()
        generator = random.Random(args.seed)
        remaining = args.bytes
        index = 0
        print(f"Creating {args.bytes:,} logical fixture bytes ({args.layout}) in {root}", file=sys.stderr)
        while remaining:
            count = remaining if args.layout == "large" else min(remaining, args.small_file_bytes)
            # Shard small files to avoid making a single huge directory the only workload.
            directory = fixture if args.layout == "large" else fixture / f"{index // 1000:06d}"
            directory.mkdir(exist_ok=True)
            write_random(directory / f"{index:09d}.bin", count, generator)
            remaining -= count
            index += 1
        report["source_logical_bytes"] = args.bytes
        report["source_file_count"] = index
        run(["git", "add", "--all"], source, env)
        run(["git", "commit", "-m", "Add reproducible benchmark fixture"], source, env)
        report["creation_commit"] = run(["git", "rev-parse", "HEAD"], source, env).decode().strip()
        object_format = run(["git", "rev-parse", "--show-object-format"], source, env).decode().strip()
        entries = tree_entries(source, env)
        report["measurements"].append({"phase": "source_committed", **sync_and_measure(root)})
        for number in range(1, args.worktrees + 1):
            destination = root / f"worktree-{number}"
            branch = f"benchmark/{number}"
            started = time.perf_counter()
            if args.method == "thin":
                raw = run([binary, "--json", "add", str(destination), "--branch", branch, "--write", "fixture"], source, env)
                creation = json.loads(raw)
            else:
                raw = run(["git", "worktree", "add", "-b", branch, str(destination), report["creation_commit"]], source, env)
                creation = {"git_output": raw.decode("utf-8", errors="replace")}
            creation_seconds = time.perf_counter() - started
            verification_seconds = verify_contents(destination, entries, object_format)
            report["worktrees"].append({
                "path": str(destination),
                "creation_wall_seconds": creation_seconds,
                "independent_full_content_verification_seconds": verification_seconds,
                "logical_file_bytes": args.bytes,
                "creation_result": creation,
            })
            if args.method == "thin":
                # Keep phase metrics alongside the complete output for future schemas.
                for field in ("clone_ms", "verification_ms"):
                    if isinstance(creation.get(field), (int, float)):
                        report["worktrees"][-1][field] = creation[field]
            report["measurements"].append({"phase": f"created_{number}", **sync_and_measure(root)})
            print(f"Created {destination.name}: {creation_seconds:.3f}s; independent verification {verification_seconds:.3f}s", file=sys.stderr)
        for number, worktree in enumerate(report["worktrees"], 1):
            destination = Path(worktree["path"])
            # Recheck before editing, so earlier worktree edits must not leak here.
            verify_contents(destination, entries, object_format)
            worktree["requested_edit_bytes"] = args.edit_bytes
            worktree["actual_edit_bytes"] = edit_fixture(destination, entries, args.edit_bytes, args.seed + number)
            report["measurements"].append({"phase": f"edited_{number}", **sync_and_measure(root)})
            if args.method == "thin":
                started = time.perf_counter()
                worktree["scope_check"] = json.loads(run([binary, "--json", "check", str(destination)], source, env))
                worktree["scope_check_wall_seconds"] = time.perf_counter() - started
        report["source_independence_verification_seconds"] = verify_contents(source, entries, object_format)
        report["status"] = "complete"
    except BaseException as error:
        report["status"] = "failed"
        report["error"] = str(error)
        raise
    finally:
        with output.open("x", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, ensure_ascii=False)
            handle.write("\n")
        print(f"Report: {output}; retained benchmark data: {root}", file=sys.stderr)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"benchmark: {error}", file=sys.stderr)
        sys.exit(1)
