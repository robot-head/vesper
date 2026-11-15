#!/usr/bin/env python3
"""Plan native-platform CI exclusions; unclassified tests always run.

The catalog names only expensive suites/cases. Affected groups are escalated
back to full coverage. Missing diff information selects every group, and a
stale catalog is an error rather than an apparently successful empty run.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
from typing import Iterable

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT = Path(__file__).with_name("platform_test_profiles.json")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")
_SHA = re.compile(r"[0-9a-fA-F]{40}\Z")


def rust_test_cases(source: str) -> set[str]:
    """Read this catalog's top-level libtest cases, including intervening attrs."""
    return set(re.findall(
        r"#\[\s*test\s*\](?:(?!\bfn\b).)*?\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
        source, re.DOTALL,
    ))


def load_policy(path: Path | str = DEFAULT, *, source_root: Path = REPO_ROOT) -> dict:
    policy = json.loads(Path(path).read_text(encoding="utf-8"))
    if policy.get("version") != 1:
        raise ValueError("unsupported platform test policy version")
    groups = policy.get("groups")
    if not isinstance(groups, dict) or not groups:
        raise ValueError("policy needs named escalation groups")
    for name, settings in groups.items():
        if not _IDENTIFIER.fullmatch(name) or not isinstance(settings, dict):
            raise ValueError("invalid escalation group")
        if not isinstance(settings.get("paths"), list) or not all(
            isinstance(p, str) and p for p in settings["paths"]
        ):
            raise ValueError(f"invalid paths for {name}")
    if not isinstance(policy.get("shared_paths"), list) or not all(
        isinstance(p, str) and p for p in policy["shared_paths"]
    ):
        raise ValueError("invalid shared paths")
    records = policy.get("deferred_targets")
    if not isinstance(records, list) or not records:
        raise ValueError("policy needs an explicit deferred-target catalog")
    seen_cargo, seen_bazel, seen_source = set(), set(), set()
    for record in records:
        if not isinstance(record, dict) or set(record) != {
            "package", "target", "bazel", "group", "source", "skip_tests"
        }:
            raise ValueError("invalid deferred-target record")
        package, target = record["package"], record["target"]
        if not isinstance(package, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", package):
            raise ValueError("invalid Cargo package selector")
        if not isinstance(target, str) or not _IDENTIFIER.fullmatch(target):
            raise ValueError("invalid Cargo test selector")
        if record["group"] not in groups:
            raise ValueError("target references unknown escalation group")
        source = record["source"]
        if not isinstance(source, str) or PurePosixPath(source).is_absolute() or ".." in PurePosixPath(source).parts:
            raise ValueError("source must be repository-relative")
        expected = f"tests/{target}.rs" if package == "vesper" else f"crates/{package}/tests/{target}.rs"
        expected_bazel = f"//:test/{target}" if package == "vesper" else f"//crates/{package}:test/{target}"
        if source != expected or record["bazel"] != expected_bazel:
            raise ValueError("Cargo/Bazel/source selectors disagree")
        cargo = (package, target)
        if cargo in seen_cargo or record["bazel"] in seen_bazel or source in seen_source:
            raise ValueError("duplicate deferred-target selector")
        seen_cargo.add(cargo)
        seen_bazel.add(record["bazel"])
        seen_source.add(source)
        source_file = source_root / source
        if not source_file.is_file():
            raise ValueError(f"catalog source missing: {source}")
        cases = rust_test_cases(source_file.read_text(encoding="utf-8"))
        if not cases:
            raise ValueError(f"catalog source has no libtest cases: {source}")
        skipped = record["skip_tests"]
        if skipped is not None:
            if not isinstance(skipped, list) or not skipped or not all(
                isinstance(case, str) and _IDENTIFIER.fullmatch(case) for case in skipped
            ) or len(set(skipped)) != len(skipped):
                raise ValueError("invalid exact test-case selectors")
            missing = set(skipped) - cases
            if missing:
                raise ValueError(f"stale test cases in {source}: {sorted(missing)}")
            if not cases - set(skipped):
                raise ValueError(f"partial exclusion leaves no PR tests in {source}")
    return policy


def _checked_groups(groups: Iterable[str], policy: dict) -> set[str]:
    if isinstance(groups, str):
        raise ValueError("groups must be a sequence, not a string")
    result = set(groups)
    unknown = result - policy["groups"].keys()
    if unknown:
        raise ValueError(f"unknown escalation groups: {sorted(unknown)}")
    return result


def deferred_targets(profile: str, groups: Iterable[str] = ()) -> list[dict]:
    policy = load_policy()
    selected = _checked_groups(groups, policy)
    if profile not in {"pr", "full"}:
        raise ValueError(f"unknown platform test profile: {profile}")
    if profile == "full":
        return []
    return [dict(r) for r in policy["deferred_targets"] if r["group"] not in selected]


def affected_groups(paths: Iterable[str], policy: dict | None = None) -> list[str]:
    policy = load_policy() if policy is None else policy
    paths = list(paths)
    if any(fnmatch.fnmatchcase(path, pattern) for path in paths for pattern in policy["shared_paths"]):
        return sorted(policy["groups"])
    result = set()
    for group, settings in policy["groups"].items():
        if any(fnmatch.fnmatchcase(path, pattern) for path in paths for pattern in settings["paths"]):
            result.add(group)
    # Editing a cataloged suite always restores all of its own cases.
    result.update(r["group"] for r in policy["deferred_targets"] if r["source"] in paths)
    return sorted(result)


def _commit(value: str, repo: Path) -> str:
    if not _SHA.fullmatch(value):
        raise ValueError("commit must be a full 40-hex SHA")
    result = subprocess.run(
        ["git", "rev-parse", "--verify", value + "^{commit}"], cwd=repo,
        capture_output=True, text=True, check=False,
    )
    if result.returncode:
        raise ValueError(f"commit unavailable: {value}")
    return result.stdout.strip()


def make_plan(profile: str, base: str | None, head: str, *, repo: Path = REPO_ROOT) -> dict:
    policy = load_policy()
    if profile not in {"pr", "full"}:
        raise ValueError(f"unknown platform test profile: {profile}")
    sha = _commit(head, repo)  # Never silently substitute a different candidate.
    checked_out = subprocess.run(
        ["git", "rev-parse", "--verify", "HEAD^{commit}"], cwd=repo,
        capture_output=True, text=True, check=False,
    )
    if checked_out.returncode or checked_out.stdout.strip() != sha:
        raise ValueError("requested head does not match the checked-out commit")
    groups = sorted(policy["groups"])
    reason = "full profile requested"
    changed = []
    if profile == "pr":
        reason = "base unavailable; full coverage selected"
        if base:
            try:
                base_sha = _commit(base, repo)
                result = subprocess.run(
                    ["git", "diff", "--name-only", "--no-renames", "-z", base_sha + "..." + sha, "--"],
                    cwd=repo, capture_output=True, check=False,
                )
                if result.returncode:
                    reason = "diff unavailable; full coverage selected"
                else:
                    changed = [p.decode("utf-8", "surrogateescape") for p in result.stdout.split(b"\0") if p]
                    groups = affected_groups(changed, policy)
                    reason = "groups selected from changed paths"
            except ValueError:
                pass
    return {"profile": profile, "groups": groups, "sha": sha, "reason": reason, "changed_paths": changed}


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if args and args[0] == "plan":
        args.pop(0)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, choices=["pr", "full"])
    parser.add_argument("--base")
    parser.add_argument("--head", required=True)
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--output", type=Path)
    options = parser.parse_args(args)
    try:
        plan = make_plan(options.profile, options.base, options.head)
    except (ValueError, OSError, json.JSONDecodeError) as error:
        parser.error(str(error))
    encoded = json.dumps(plan, indent=2, ensure_ascii=True) + "\n"
    if options.output:
        options.output.parent.mkdir(parents=True, exist_ok=True)
        options.output.write_text(encoded, encoding="utf-8")
    if options.github_output:
        with options.github_output.open("a", encoding="utf-8") as output:
            for key in ["profile", "groups", "sha"]:
                value = json.dumps(plan[key], separators=(",", ":")) if key == "groups" else plan[key]
                output.write(f"{key}={value}\n")
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
