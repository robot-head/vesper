#!/usr/bin/env python3
"""Create the BUILD.bazel file of every Cargo workspace member that lacks one.

Bazel derives each member's targets and dependencies from Cargo metadata, so
the file is the same one-line macro call for almost every member. Members
that need extra inputs (files outside their directory, host tools) edit their
own BUILD.bazel by hand afterwards; this script never rewrites an existing
file.

    python3 tools/bazel/sync_build_files.py          # create missing files
    python3 tools/bazel/sync_build_files.py --check  # fail if any is missing
"""

import pathlib
import sys
import tomllib

TEMPLATE = '''load("//tools/bazel:defs.bzl", "vesper_crate")

vesper_crate()
'''

def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[2]
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    missing = []
    for member in workspace["members"]:
        if member == ".":
            continue  # The root package's BUILD.bazel is maintained by hand.
        path = root / member / "BUILD.bazel"
        if not path.exists():
            missing.append(path)
    if "--check" in sys.argv[1:]:
        for path in missing:
            print(f"missing {path.relative_to(root)}; run python3 tools/bazel/sync_build_files.py")
        return 1 if missing else 0
    for path in missing:
        path.write_text(TEMPLATE)
        print(f"created {path.relative_to(root)}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
