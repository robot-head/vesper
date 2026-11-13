#!/usr/bin/env python3
"""Require every tracked .ves file to be canonically formatted.

Runs the real `vesper fmt --check --json` on every `git ls-files '*.ves'` path
except those listed in scripts/fmt-exclude.txt. Never writes source files.
"""
import argparse
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXCLUDE = ROOT / "scripts" / "fmt-exclude.txt"


def excluded():
    paths = set()
    for line in EXCLUDE.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            paths.add(line)
    return paths


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    tracked = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.ves"], cwd=ROOT, check=True, capture_output=True
    ).stdout.decode("utf-8").split("\0")
    tracked = sorted(p for p in tracked if p)
    skip = excluded()
    stale = sorted(skip - set(tracked))
    if stale:
        sys.exit("fmt-exclude.txt lists paths that are not tracked .ves files: " + ", ".join(stale))
    files = [p for p in tracked if p not in skip]
    if not files:
        sys.exit("no .ves files to check")
    result = subprocess.run(
        [str(args.binary), "fmt", "--check", "--json", "--", *files],
        cwd=ROOT,
        capture_output=True,
        timeout=600,
    )
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        sys.exit(f"vesper fmt did not report JSON (exit {result.returncode}): {result.stderr.decode(errors='replace')}")
    entries = report["files"] if "files" in report else [dict(report, path=files[0])]
    bad = [e for e in entries if e["status"] != "unchanged"]
    for entry in bad:
        detail = entry.get("error", entry["status"])
        print(f"{entry['path']}: {detail}")
    if bad or result.returncode != 0:
        print(
            f"{len(bad)} of {len(files)} .ves files are not canonically formatted. "
            "Run `vesper fmt <file>` on them, or add a reviewed entry to scripts/fmt-exclude.txt.",
            file=sys.stderr,
        )
        sys.exit(1)
    print(f"vesper fmt --check: {len(files)} files canonical, {len(skip)} excluded")


if __name__ == "__main__":
    main()
