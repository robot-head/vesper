#!/usr/bin/env python3
"""Decide whether a pull request can merge yet (#1321).

Project rule: no CI for pull requests that cannot merge yet. A pull request
cannot merge yet when

- its base branch is not main,
- its branch carries another open pull request's own commits (it merged in
  a parent that has not landed), or
- it conflicts with current main.

A commit belongs to an open pull request when it is on that branch's
first-parent chain past main. A child that merges its parent in carries those
commits through a merge's second parent, so the parent itself is not flagged.

Usage (from .github/workflows/merge-gate.yml):
  merge_gate.py --number N --base-ref main --head SHA --main origin/main \
      --open-prs prs.json
prs.json is a list of {"number", "base", "ref"} for every open pull request,
where "ref" is a local ref holding that pull request's head. Prints one line
per reason and exits 1 when the pull request cannot merge yet, else 0.
"""

import argparse
import json
import subprocess
import sys


def git(*args, check=True):
    return subprocess.run(["git", *args], check=check, capture_output=True, text=True)


def commits(*args):
    return set(git("rev-list", *args).stdout.split())


def blockers(number, base_ref, head, main, open_prs):
    reasons = []
    if base_ref != "main":
        reasons.append(f"its base is {base_ref}, not main")
    carried = commits(f"{main}..{head}")
    own = commits("--first-parent", f"{main}..{head}")
    for pr in open_prs:
        if pr["number"] == number or pr.get("base", "main") != "main":
            continue
        theirs = commits("--first-parent", f"{main}..{pr['ref']}")
        shared = (carried - own) & theirs
        if shared:
            reasons.append(f"it carries {len(shared)} commit(s) of open PR #{pr['number']}")
    if git("merge-tree", "--write-tree", main, head, check=False).returncode != 0:
        reasons.append("it conflicts with main")
    return reasons


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--number", type=int, required=True)
    parser.add_argument("--base-ref", required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--main", default="origin/main")
    parser.add_argument("--open-prs", required=True)
    args = parser.parse_args()
    with open(args.open_prs) as f:
        open_prs = json.load(f)
    reasons = blockers(args.number, args.base_ref, args.head, args.main, open_prs)
    for reason in reasons:
        print(f"PR #{args.number} cannot merge yet: {reason}")
    if not reasons:
        print(f"PR #{args.number} can merge: CI runs")
    return 1 if reasons else 0


if __name__ == "__main__":
    sys.exit(main())
