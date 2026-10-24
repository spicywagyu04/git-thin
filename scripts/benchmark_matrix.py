#!/usr/bin/env python3
"""Plan, run, or summarize a randomized benchmark matrix without deleting fixtures."""

import argparse
import json
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import sys


def distribution(values):
    values = [value for value in values if value is not None]
    return {"count": len(values), "median": statistics.median(values), "minimum": min(values), "maximum": max(values)} if values else {"count": 0}


def summarize(trials, output, expected):
    groups = {}
    failures = []
    missing = []
    contexts = []
    for trial in trials:
        path = output / f"{trial['name']}.json"
        if not path.exists():
            missing.append(trial["name"])
            continue
        report = json.loads(path.read_text(encoding="utf-8"))
        parameters = report.get("parameters", {})
        mismatches = [field for field, value in {**expected, "layout": trial["layout"], "method": trial["method"]}.items() if parameters.get(field) != value]
        if mismatches:
            failures.append({"trial": trial["name"], "error": f"report parameters differ from this matrix: {', '.join(mismatches)}"})
            continue
        if report.get("status") != "complete":
            failures.append({"trial": trial["name"], "error": report.get("error", "incomplete report")})
            continue
        trees = report["worktrees"]
        context = {field: report.get(field) for field in ("platform", "python", "git")}
        context["binary_sha256"] = report.get("binary", {}).get("sha256")
        if context not in contexts:
            contexts.append(context)
        measurements = {row["phase"]: row for row in report["measurements"]}
        source = measurements["source_committed"]
        created = measurements[f"created_{len(trees)}"]
        edited = measurements[f"edited_{len(trees)}"]
        row = {
            "creation_wall_seconds": sum(tree["creation_wall_seconds"] for tree in trees),
            "clone_ms": sum(tree["clone_ms"] for tree in trees) if all("clone_ms" in tree for tree in trees) else None,
            "verification_ms": sum(tree["verification_ms"] for tree in trees) if all("verification_ms" in tree for tree in trees) else None,
            "independent_verification_seconds": sum(tree["independent_full_content_verification_seconds"] for tree in trees),
            "scope_check_wall_seconds": sum(tree["scope_check_wall_seconds"] for tree in trees) if all("scope_check_wall_seconds" in tree for tree in trees) else None,
            "creation_peak_rss_bytes": max((tree.get("creation_memory", {}).get("peak_rss_bytes") or 0 for tree in trees), default=0) or None,
            "scope_peak_rss_bytes": max((tree.get("scope_check_memory", {}).get("peak_rss_bytes") or 0 for tree in trees), default=0) or None,
            "actual_edit_bytes": sum(tree["actual_edit_bytes"] for tree in trees),
            "filesystem_creation_delta_bytes": created["filesystem_used_bytes"] - source["filesystem_used_bytes"],
            "filesystem_edit_delta_bytes": edited["filesystem_used_bytes"] - created["filesystem_used_bytes"],
        }
        for field, earlier, later in (("apfs_creation_delta_bytes", source, created), ("apfs_edit_delta_bytes", created, edited)):
            before = earlier.get("apfs", {}).get("volume_capacity_in_use_bytes")
            after = later.get("apfs", {}).get("volume_capacity_in_use_bytes")
            row[field] = after - before if before is not None and after is not None else None
        groups.setdefault(f"{trial['layout']}-{trial['method']}", []).append(row)
    return {
        "status": "complete" if not missing and not failures and len(contexts) == 1 else "incomplete",
        "expected_trials": len(trials),
        "complete_trials": sum(map(len, groups.values())),
        "missing_trials": missing,
        "failed_trials": failures,
        "execution_contexts": contexts,
        "context_warning": "Reports have different platforms, Python/Git versions or measured binaries; do not combine them." if len(contexts) > 1 else None,
        "groups": {name: {field: distribution([row[field] for row in rows]) for field in rows[0]} for name, rows in groups.items()},
        "limitations": ["Each observation is a trial total across its worktrees, except memory, which is the largest reported command RSS.", "Filesystem deltas include metadata and background activity; they do not identify exact shared extents.", "An absent APFS counter is unavailable, never zero.", "Review raw reports for identical build, settings, cache conditions and fixture parameters before comparing trials."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="existing dedicated filesystem directory; trial directories must be new")
    parser.add_argument("--output-dir", required=True, type=Path, help="existing report directory outside the measured filesystem")
    parser.add_argument("--git-thin", default="git-thin")
    parser.add_argument("--bytes", type=int, default=8_000_000_000)
    parser.add_argument("--small-file-bytes", type=int, default=64 * 1024)
    parser.add_argument("--worktrees", type=int, default=3)
    parser.add_argument("--edit-bytes", type=int, default=16 * 1024 * 1024)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--seed", type=int, default=20261024)
    parser.add_argument("--reserve-bytes", type=int, default=1024 * 1024 * 1024)
    parser.add_argument("--environment-notes", type=Path)
    parser.add_argument("--build-features", default="unspecified")
    parser.add_argument("--trial", type=int, help="run only this one-based randomized trial, allowing an operator to reset a disposable volume between trials")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--run", action="store_true", help="otherwise print plan without writing anything")
    mode.add_argument("--summarize-only", action="store_true", help="read existing reports and print summary without running trials")
    args = parser.parse_args()
    expected = {field: getattr(args, field) for field in ("bytes", "small_file_bytes", "worktrees", "edit_bytes", "seed", "build_features")}
    for field in ("bytes", "small_file_bytes", "worktrees", "edit_bytes", "repetitions"):
        if getattr(args, field) <= 0:
            parser.error(f"--{field.replace('_', '-')} must be positive")
    if args.reserve_bytes < 0:
        parser.error("--reserve-bytes must be nonnegative")
    trials = [{"name": f"{layout}-{method}-{repetition}", "layout": layout, "method": method} for layout in ("large", "small") for method in ("thin", "checkout") for repetition in range(1, args.repetitions + 1)]
    random.Random(args.seed).shuffle(trials)
    for number, trial in enumerate(trials, 1):
        trial["order"] = number
        trial["estimated_free_bytes"] = args.bytes * (3 + (args.worktrees if trial["method"] == "checkout" else 0)) + args.reserve_bytes
    if args.trial is not None and not 1 <= args.trial <= len(trials):
        parser.error(f"--trial must be between 1 and {len(trials)}")
    if args.summarize_only:
        print(json.dumps(summarize(trials, args.output_dir, expected), indent=2))
        return
    selected = [trials[args.trial - 1]] if args.trial is not None else trials
    required = sum(trial["estimated_free_bytes"] for trial in selected)
    if not args.run:
        print(json.dumps({"seed": args.seed, "minimum_free_bytes_for_retained_trials": required, "selected_trials": selected, "note": "No fixtures created. --run acknowledges real disk consumption; no automatic cleanup is performed."}, indent=2))
        return
    root = args.root.resolve(strict=True)
    output = args.output_dir.resolve(strict=True)
    if root == output or root in output.parents:
        parser.error("reports must be outside the measured root")
    if shutil.disk_usage(root).free < required:
        parser.error(f"selected retained trials require at least {required:,} free bytes; use --trial for operator-controlled volume resets")
    for trial in selected:
        if (root / trial["name"]).exists() or (output / f"{trial['name']}.json").exists():
            parser.error(f"refusing to reuse fixture or report for {trial['name']}")
    for trial in selected:
        command = [sys.executable, str(Path(__file__).with_name("benchmark.py")), str(root / trial["name"]), "--output", str(output / f"{trial['name']}.json"), "--layout", trial["layout"], "--method", trial["method"], "--git-thin", args.git_thin, "--build-features", args.build_features, "--run"]
        for field in ("bytes", "small_file_bytes", "worktrees", "edit_bytes", "seed", "reserve_bytes"):
            command.extend(["--" + field.replace("_", "-"), str(getattr(args, field))])
        if args.environment_notes:
            command.extend(["--environment-notes", str(args.environment_notes)])
        result = subprocess.run(command, check=False)
        if result.returncode:
            raise RuntimeError(f"trial {trial['name']} failed ({result.returncode}); evidence and fixtures retained")
    print(json.dumps(summarize(trials, output, expected), indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError) as error:
        print(f"benchmark matrix: {error}", file=sys.stderr)
        sys.exit(1)
