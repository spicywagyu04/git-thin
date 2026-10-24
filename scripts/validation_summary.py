#!/usr/bin/env python3
"""Render executed, unavailable and missing CI evidence without counting skips as passes."""

import argparse
import json
import os
from pathlib import Path
import re


def scenario_messages(text):
    unavailable = set()
    inapplicable = set()
    for line in text.splitlines():
        line = line.strip()
        if re.search(r"\bINAPPLICABLE\b", line):
            inapplicable.add(line)
        elif re.search(r"\b(SKIP|UNAVAILABLE)\b", line):
            # Older APFS tests used SKIP for a filesystem-impossible filename.
            # Preserve that evidence's meaning while accepting explicit markers.
            if "filesystem rejects non-UTF-8 filenames" in line:
                inapplicable.add(line)
            else:
                unavailable.add(line)
    return unavailable, inapplicable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--lane", required=True)
    args = parser.parse_args()
    print(f"## {args.lane}\n\nCandidate: `{os.environ.get('GITHUB_SHA', 'local working tree')}`\n")
    environment = args.evidence / "environment.json"
    if environment.exists() and environment.stat().st_size:
        info = json.loads(environment.read_text(encoding="utf-8"))
        print(f"- {info['os']}; {info['git']}; {info['rust']}")
        print(f"- Temporary filesystem: `{info['temporary']}`")
    else:
        print("- Capability report missing: environment validation did not complete.")
    unavailable = set()
    inapplicable = set()
    for name in ("all-features.log", "production-tests.log"):
        path = args.evidence / name
        if not path.exists():
            print(f"- `{name}` missing: this suite did not complete its invocation.")
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        summaries = re.findall(r"^test result: .+$", text, flags=re.MULTILINE)
        print(f"\n### {name}\n")
        for summary in summaries:
            print(f"- `{summary}`")
        if not summaries:
            print("- No completed Rust test results found.")
        missing, excluded = scenario_messages(text)
        unavailable.update(missing)
        inapplicable.update(excluded)
    print("\n### Unavailable scenarios\n")
    if unavailable:
        print("Rust may count early-returning tests as passed; these scenarios did not execute.\n")
        for line in sorted(unavailable):
            print(f"- {line}")
    else:
        print("No unavailable messages in the retained logs. Inspect missing-suite notes above before inferring coverage.")
    print("\n### Inapplicable scenarios\n")
    if inapplicable:
        print("These assertions do not apply to this filesystem or configuration; they are not executed passes.\n")
        for line in sorted(inapplicable):
            print(f"- {line}")
    else:
        print("No inapplicable messages in the retained logs.")
    for name in ("production-smoke.json", "smoke-summary.json"):
        path = args.evidence / name
        print(f"\n### {name}\n")
        if not path.exists() or not path.stat().st_size:
            print("No completed report retained.")
            continue
        report = json.loads(path.read_text(encoding="utf-8"))
        if name == "production-smoke.json":
            print(f"Executed controls: {len(report.get('executed', []))}; unavailable scenarios: {len(report.get('unavailable', []))}.")
        else:
            print(f"Status: {report['status']}; complete trials: {report['complete_trials']}/{report['expected_trials']}. Small smoke trials do not establish full-scale storage savings.")


if __name__ == "__main__":
    main()
