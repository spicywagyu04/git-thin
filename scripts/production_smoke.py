#!/usr/bin/env python3
"""Exercise a production binary with hostile test controls; never enable test features."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--require-native", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update({"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C"})
    source_text = "\n".join(path.read_text(encoding="utf-8") for path in Path(__file__).resolve().parent.parent.joinpath("src").glob("*.rs"))
    variables = set(re.findall(r"GIT_THIN_TEST_[A-Z_]+", source_text))
    points = set(re.findall(r'fault\("([^"\n]+)"\)', source_text))
    # Cover the original controls even if refactoring moves their spelling.
    points.update({"after-registration", "before-clone", "before-verification", "no-space"})
    controls = {prefix + point for point in points for prefix in ("", "crash-", "pause-")}
    controls.update(f"{state}metadata-{stage}" for state in ("", "creating-", "indexing-", "verifying-", "publishing-", "ready-", "removing-") for stage in ("serialize", "write", "file-sync", "persist", "directory-sync"))
    controls.update(f"{error}-after-partial-clone" for error in ("ENOSPC", "EACCES", "EIO"))
    controls = sorted(controls)
    report = {"binary": str(binary), "feature_expectation": "production (no fault-injection)", "executed": [], "unavailable": []}
    with tempfile.TemporaryDirectory(prefix="git-thin-production-") as temporary:
        root = Path(temporary).resolve()
        source = root / "source"
        source.mkdir()

        def run(command, settings=env, check=True):
            result = subprocess.run(command, cwd=source, env=settings, capture_output=True, text=True, timeout=20)
            if check and result.returncode:
                raise RuntimeError(f"command failed ({result.returncode}): {command!r}\n{result.stdout}\n{result.stderr}")
            return result

        for command in (["git", "init", "-b", "main"], ["git", "config", "user.name", "Production smoke"], ["git", "config", "user.email", "smoke@example.invalid"], ["git", "config", "core.autocrlf", "false"], ["git", "config", "core.hooksPath", os.devnull], ["git", "config", "commit.gpgsign", "false"]):
            run(command)
        (source / "tracked").write_text("baseline\n", encoding="utf-8")
        run(["git", "add", "."])
        run(["git", "commit", "-m", "baseline"])
        baseline = run(["git", "rev-parse", "HEAD"]).stdout
        index = (source / ".git" / "index").read_bytes()
        doctor = run([str(binary), "--json", "doctor"], check=False)
        if doctor.returncode:
            diagnostic = doctor.stdout + doctor.stderr
            unsupported = "native copy-on-write cloning is unsupported" in diagnostic or "no supported clone backend" in diagnostic
            if args.require_native or not unsupported:
                raise RuntimeError(f"required native production smoke unavailable: {doctor.stdout}{doctor.stderr}")
            report["unavailable"].append({"scenario": "native production controls", "diagnostic": diagnostic})
        else:
            for number, point in enumerate(controls):
                settings = env.copy()
                for variable in variables:
                    settings[variable] = "invalid-test-control"
                ready = root / "unexpected-ready"
                settings.update({"GIT_THIN_TEST_FAILPOINT": point, "GIT_THIN_TEST_READY_FILE": str(ready), "GIT_THIN_TEST_CONTINUE_FILE": str(root / "never-continue")})
                destination = root / f"agent-{number}"
                created = json.loads(run([str(binary), "--json", "add", str(destination), "--branch", f"smoke/{number}", "--write", "tracked"], settings).stdout)
                if (destination / "tracked").read_text(encoding="utf-8") != "baseline\n" or ready.exists():
                    raise RuntimeError(f"production binary honored test control {point}")
                run([str(binary), "--json", "check", str(destination)], settings)
                run([str(binary), "--json", "remove", str(destination)], settings)
                if destination.exists():
                    raise RuntimeError("production removal left destination")
                report["executed"].append({"control": point, "creation": created})
            if (source / "tracked").read_text(encoding="utf-8") != "baseline\n" or (source / ".git" / "index").read_bytes() != index or run(["git", "rev-parse", "HEAD"]).stdout != baseline:
                raise RuntimeError("production smoke changed source contents, index or HEAD")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
