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
import tempfile
import time

_TIME_PROBE = None


def run(args, cwd=None, env=None):
    result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {args!r}\n"
            + result.stderr.decode("utf-8", errors="replace")
            + result.stdout.decode("utf-8", errors="replace")
        )
    return result.stdout


def measured_run(args, cwd, env, report_directory):
    """Use the platform's wait4-based time tool; do not infer RSS from allocation."""
    time_binary = Path("/usr/bin/time")
    memory = {
        "peak_rss_bytes": None,
        "method": "unavailable",
        "limitation": "maximum RSS reported by time, not summed simultaneous process-tree memory",
    }
    global _TIME_PROBE
    with tempfile.TemporaryDirectory(prefix="git-thin-measure-", dir=report_directory) as temporary:
        statistics = Path(temporary) / "time.txt"
        command = list(args)
        if time_binary.exists() and sys.platform in ("darwin", "linux"):
            prefix = [str(time_binary), "-l" if sys.platform == "darwin" else "-v", "-o", str(statistics)]
            if _TIME_PROBE is None:
                # A sandbox may permit the measured command but deny time's
                # sysctl queries afterwards. Probe with a harmless command first;
                # never retry a creation operation that may already have run.
                probe = subprocess.run([*prefix, "/usr/bin/true"], cwd=cwd, env=env, capture_output=True, check=False)
                _TIME_PROBE = (probe.returncode == 0, probe.stderr.decode("utf-8", errors="replace"))
                if statistics.exists():
                    statistics.unlink()
            if _TIME_PROBE[0]:
                command = [*prefix, *args]
            else:
                memory["warning"] = "time capability probe failed: " + _TIME_PROBE[1]
        started = time.perf_counter()
        output = run(command, cwd, env)
        elapsed = time.perf_counter() - started
        if statistics.exists():
            memory["raw"] = statistics.read_text(encoding="utf-8")
            memory["method"] = "/usr/bin/time -l" if sys.platform == "darwin" else "/usr/bin/time -v"
            for line in memory["raw"].splitlines():
                if sys.platform == "darwin" and "maximum resident set size" in line:
                    memory["peak_rss_bytes"] = int(line.split()[0])
                elif sys.platform == "linux" and "Maximum resident set size (kbytes):" in line:
                    memory["peak_rss_bytes"] = int(line.rsplit(":", 1)[1].strip()) * 1024
            if memory["peak_rss_bytes"] is None:
                memory["warning"] = "time output did not contain a recognized peak RSS counter"
    return output, elapsed, memory


def optional_command(args, env):
    try:
        return run(args, env=env).decode("utf-8", errors="replace").strip()
    except (OSError, RuntimeError) as error:
        return {"unavailable": str(error)}


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
    parser.add_argument("--build-features", default="unspecified", help="record exact features used to build the measured binary")
    parser.add_argument("--environment-notes", type=Path, help="JSON object recording hardware, filesystem settings, snapshots, encryption, cache and background activity")
    parser.add_argument("--reserve-bytes", type=int, default=1024 * 1024 * 1024, help="additional free filesystem reserve beyond fixture estimates")
    parser.add_argument("--run", action="store_true", help="required acknowledgement that fixtures consume disk")
    args = parser.parse_args()
    for field in ("bytes", "small_file_bytes", "worktrees", "edit_bytes"):
        if getattr(args, field) <= 0:
            parser.error(f"--{field.replace('_', '-')} must be positive")
    if args.reserve_bytes < 0:
        parser.error("--reserve-bytes must be nonnegative")
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
    reserve = args.bytes * (3 + (args.worktrees if args.method == "checkout" else 0)) + args.reserve_bytes
    if shutil.disk_usage(root.parent).free < reserve:
        raise RuntimeError(f"Need at least {reserve:,} free bytes in the benchmark parent.")
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("GIT_"):
            del env[key]
    env.update({"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "LC_ALL": "C"})
    report = {
        "schema_version": 2,
        "status": "running",
        "parameters": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
        "platform": platform.platform(),
        "python": platform.python_version(),
        "hardware": {"machine": platform.machine(), "processor": platform.processor(), "logical_cpus": os.cpu_count()},
        "git": run(["git", "--version"], env=env).decode().strip(),
        "rust": optional_command(["rustc", "--version"], env),
        "tool_commit": optional_command(["git", "-C", str(Path(__file__).resolve().parent.parent), "rev-parse", "HEAD"], env),
        "tool_checkout_status": optional_command(["git", "-C", str(Path(__file__).resolve().parent.parent), "status", "--porcelain"], env),
        "worktrees": [],
        "measurements": [],
        "limitations": [
            "Allocation counters include filesystem metadata, snapshots, delayed allocation, and other activity.",
            "Logical file bytes and file st_blocks/du sums are not exact shared-extent accounting.",
            "Warm caches and a synthetic fixture affect elapsed times; record cold/warm conditions separately.",
        ],
    }
    if args.environment_notes:
        report["environment_notes"] = json.loads(args.environment_notes.read_text(encoding="utf-8"))
        if not isinstance(report["environment_notes"], dict):
            raise RuntimeError("--environment-notes must contain a JSON object")
    else:
        report["limitations"].append("Operator environment notes were not supplied; snapshot, cache, encryption and background-activity conditions are unverified.")
    if binary:
        report["git_thin"] = run([binary, "--version"], env=env).decode().strip()
        report["binary"] = {"path": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(), "features": args.build_features}
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
            if args.method == "thin":
                raw, creation_seconds, creation_memory = measured_run([binary, "--json", "add", str(destination), "--branch", branch, "--write", "fixture"], source, env, output.parent)
                creation = json.loads(raw)
            else:
                raw, creation_seconds, creation_memory = measured_run(["git", "worktree", "add", "-b", branch, str(destination), report["creation_commit"]], source, env, output.parent)
                creation = {"git_output": raw.decode("utf-8", errors="replace")}
            verification_seconds = verify_contents(destination, entries, object_format)
            report["worktrees"].append({
                "path": str(destination),
                "creation_wall_seconds": creation_seconds,
                "creation_memory": creation_memory,
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
            worktree["pre_edit_independence_verification_seconds"] = verify_contents(destination, entries, object_format)
            worktree["requested_edit_bytes"] = args.edit_bytes
            worktree["actual_edit_bytes"] = edit_fixture(destination, entries, args.edit_bytes, args.seed + number)
            report["measurements"].append({"phase": f"edited_{number}", **sync_and_measure(root)})
            if args.method == "thin":
                raw, elapsed, memory = measured_run([binary, "--json", "check", str(destination)], source, env, output.parent)
                worktree["scope_check"] = json.loads(raw)
                worktree["scope_check_wall_seconds"] = elapsed
                worktree["scope_check_memory"] = memory
        report["source_independence_verification_seconds"] = verify_contents(source, entries, object_format)
        report["total_independent_verification_seconds"] = report["source_independence_verification_seconds"] + sum(
            worktree["independent_full_content_verification_seconds"]
            + worktree["pre_edit_independence_verification_seconds"]
            for worktree in report["worktrees"]
        )
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
