#!/usr/bin/env python3
"""Print the Bazel flag that scopes the `lean_replay` tests for this CI run.

"Test all targets" runs the whole `//...` suite on pushes to main. On a pull
request it skips targets tagged `lean_replay` (the Lean/Logos replay suite in
BUILD.bazel) unless the change touches a path those tests read. Anything this
script cannot determine selects the full suite: it prints nothing.

Usage: lean_replay_scope.py [--base REF]   (prints flags on stdout)
"""
from __future__ import annotations

import argparse
import fnmatch
import os
import subprocess
import sys
from typing import Iterable

SKIP_FLAG = "--test_tag_filters=-lean_replay"
FULL_MARKER = "[ci full]"

# Paths whose change makes a PR run the Lean replay suite. Globs use fnmatch
# (`*` also matches `/`).
TRIGGERS = (
    # Lean models and the replay tooling.
    "lean/*",
    "tools/lean4lean*",
    "tools/logos42/*",
    "tools/resource-model42/*",
    "src/bin/logos42*",
    "src/bin/arithmetic_oracle42*",
    "src/bin/resource_oracle42*",
    "src/bin/lean4lean42*",
    # Compiler modules the replay tests import or that emit Lean.
    "src/lean.rs",
    "src/lean_*.rs",
    "src/lean/*",
    "src/optimize/witness*",
    "crates/vesper-lean-syntax/*",
    # The tests themselves.
    "tests/lean4lean_*42.rs",
    "tests/logos42_acceptance.rs",
    "tests/oracle42_acceptance.rs",
    "tests/resource_oracle42.rs",
    "tests/resource_model42.rs",
    "tests/replay42_kernel.rs",
    "tests/common/lean4lean*",
    # Toolchain, Bazel and CI configuration.
    "BUILD.bazel",
    "MODULE.bazel",
    ".bazelrc",
    ".bazelversion",
    "Cargo.lock",
    "rust-toolchain*",
    "buildbuddy.yaml",
    "tools/bazel/*",
    "scripts/ci/install-lean.sh",
    "scripts/ci/install-logos-rust.sh",
    "scripts/ci/prepare-logos42.sh",
    "scripts/ci/prepare-lean4lean42.sh",
    "scripts/ci/lean_replay_scope.py",
)


def triggered(paths: Iterable[str]) -> list[str]:
    return [p for p in paths if any(fnmatch.fnmatchcase(p, g) for g in TRIGGERS)]


def flags(*, branch: str, changed: list[str] | None, messages: str) -> list[str]:
    """Flags for one run; `changed` is None when the diff is unknown."""
    if branch in ("", "main", "HEAD") or changed is None:
        return []
    if FULL_MARKER in messages or triggered(changed):
        return []
    return [SKIP_FLAG]


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", default="origin/main")
    args = parser.parse_args(argv)
    # BuildBuddy checks out the PR branch by name and merges main into it.
    branch = os.environ.get("GIT_BRANCH", "")
    try:
        if not branch:
            branch = _git("rev-parse", "--abbrev-ref", "HEAD").strip()
        changed = [p for p in _git("diff", "--name-only", args.base, "HEAD").splitlines() if p]
        messages = _git("log", "--format=%B", f"{args.base}..HEAD")
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"lean_replay_scope: {error}; running the full suite", file=sys.stderr)
        changed, messages = None, ""
    result = flags(branch=branch, changed=changed, messages=messages)
    hits = triggered(changed or [])
    if result:
        print(f"lean_replay_scope: {branch}: no Lean-facing changes; skipping lean_replay tests", file=sys.stderr)
    elif hits:
        print(f"lean_replay_scope: {branch}: running lean_replay tests for {hits[:5]}", file=sys.stderr)
    else:
        print(f"lean_replay_scope: {branch or 'unknown'}: running the full suite", file=sys.stderr)
    print(" ".join(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
