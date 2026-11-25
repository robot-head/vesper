#!/usr/bin/env python3
"""Execute a declared native CI profile without changing test assertions.

Cargo builds once, then runs existing harnesses with bounded package-aware
parallelism. Bazel keeps non-libtest runners separate from filtered Rust tests.
The JSON summary records every deferred target/case and every execution result.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_POLICY = Path(__file__).with_name("platform_test_profiles.json")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")


def rust_test_cases(source: str) -> set[str]:
    """Read this catalog's top-level libtest cases, including intervening attrs."""
    return set(re.findall(
        r"#\[\s*test\s*\](?:(?!\bfn\b).)*?\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
        source, re.DOTALL,
    ))


def load_policy(path: Path | str = DEFAULT_POLICY, *, source_root: Path = ROOT) -> dict:
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


def _checked_groups(groups, policy: dict) -> set[str]:
    if isinstance(groups, str):
        raise ValueError("groups must be a sequence, not a string")
    result = set(groups)
    unknown = result - policy["groups"].keys()
    if unknown:
        raise ValueError(f"unknown escalation groups: {sorted(unknown)}")
    return result


def deferred_targets(profile: str, groups = ()) -> list[dict]:
    policy = load_policy()
    selected = _checked_groups(groups, policy)
    if profile not in {"pr", "full"}:
        raise ValueError(f"unknown platform test profile: {profile}")
    if profile == "full":
        return []
    return [dict(r) for r in policy["deferred_targets"] if r["group"] not in selected]



@dataclass(frozen=True)
class TestBinary:
    package: str
    target: str
    kind: tuple[str, ...]
    executable: Path
    cwd: Path
    manifest: Path
    harness: bool = True

    @property
    def identity(self) -> str:
        return f"{self.package}:{','.join(self.kind)}:{self.target}"


def command(args, *, cwd=ROOT, env=None, timeout=None):
    return subprocess.run(
        [str(a) for a in args], cwd=cwd, env=env, timeout=timeout,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", errors="replace", check=False,
    )


def require_success(result, description):
    if result.returncode:
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise RuntimeError(f"{description} exited {result.returncode}")


def _stop_test_tree(process):
    """Stop only the child tree/session this runner created."""
    if os.name == "nt":
        if process.poll() is None:
            try:
                subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               timeout=10, check=False)
            except (OSError, subprocess.TimeoutExpired):
                pass
        if process.poll() is None:
            process.kill()
    else:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass
        # Descendants may still own pipes after their parent exits.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def test_command(argv, *, cwd, env, cancellation):
    if cancellation.is_set():
        return subprocess.CompletedProcess(argv, -1, "", "cancelled before launch\n"), True
    options = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt" else {"start_new_session": True}
    with subprocess.Popen(argv, cwd=cwd, env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True, encoding="utf-8",
                          errors="replace", **options) as process:
        while True:
            try:
                stdout, stderr = process.communicate(timeout=0.2)
                return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr), False
            except subprocess.TimeoutExpired:
                if cancellation.is_set():
                    _stop_test_tree(process)
                    stdout, stderr = process.communicate()
                    return subprocess.CompletedProcess(argv, process.returncode, stdout,
                                                       stderr + "\ncancelled after another test binary failed\n"), True


def listed_tests(output: str) -> set[str]:
    # libtest's stable terse listing has one record per test, then a total.
    return {line[:-6] for line in output.splitlines() if line.endswith(": test")}


def validate_cases(output: str, names, label: str):
    actual = listed_tests(output)
    missing = sorted(set(names) - actual)
    if missing:
        raise RuntimeError(f"{label}: policy names tests absent from executable: {missing}")
    if not actual - set(names):
        raise RuntimeError(f"{label}: partial policy would execute zero test cases")


def cargo_environment(binary: TestBinary, packages, search_paths):
    env = dict(os.environ)
    pkg = packages[binary.package]
    version = pkg["version"]
    numeric, _, pre = version.partition("-")
    parts = numeric.split(".")
    env.update({
        "CARGO_MANIFEST_DIR": str(binary.cwd),
        "CARGO_MANIFEST_PATH": str(binary.manifest),
        "CARGO_PKG_NAME": binary.package,
        "CARGO_PKG_VERSION": version,
        "CARGO_PKG_VERSION_MAJOR": parts[0],
        "CARGO_PKG_VERSION_MINOR": parts[1],
        "CARGO_PKG_VERSION_PATCH": parts[2],
        "CARGO_PKG_VERSION_PRE": pre,
        "CARGO_PKG_AUTHORS": ":".join(pkg.get("authors", [])),
        "CARGO_CRATE_NAME": binary.target.replace("-", "_"),
    })
    for key in ("description", "homepage", "repository", "license", "license_file", "rust_version", "readme"):
        env["CARGO_PKG_" + key.upper()] = str(pkg.get(key) or "")
    if "bin" in binary.kind:
        env["CARGO_BIN_NAME"] = binary.target
    else:
        env.pop("CARGO_BIN_NAME", None)
    # Cargo normally supplies these directories to the platform's dynamic loader.
    # Native build-script paths are included, not just target/debug.
    paths = list(dict.fromkeys(str(p) for p in [binary.executable.parent, *search_paths]))
    for key in (["PATH"] if os.name == "nt" else ["PATH", "DYLD_FALLBACK_LIBRARY_PATH" if sys.platform == "darwin" else "LD_LIBRARY_PATH"]):
        env[key] = os.pathsep.join([*paths, *([env[key]] if env.get(key) else [])])
    return env


def _harness(pkg, target):
    manifest = Path(pkg["manifest_path"])
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    kind = target["kind"]
    table = next((k for k in ("test", "bin", "example", "bench") if k in kind), "lib")
    if table == "lib":
        return data.get("lib", {}).get("harness", True)
    for entry in data.get(table or "", []):
        if entry.get("name") == target["name"]:
            return entry.get("harness", True)
    return True


def cargo_discover():
    metadata_result = command(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"])
    require_success(metadata_result, "cargo metadata")
    metadata = json.loads(metadata_result.stdout)
    members = set(metadata["workspace_members"])
    by_id = {p["id"]: p for p in metadata["packages"] if p["id"] in members}
    packages = {p["name"]: p for p in by_id.values()}
    if len(packages) != len(by_id):
        raise RuntimeError("workspace package names must be unique")
    built = command(["cargo", "test", "--workspace", "--locked", "--no-run", "--message-format=json"])
    # Preserve Cargo diagnostics without flooding the console with artifact JSON.
    sys.stderr.write(built.stderr)
    records = []
    for line in built.stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            print(line)
            continue
        records.append(record)
        if record.get("reason") == "compiler-message":
            sys.stderr.write(record.get("message", {}).get("rendered") or "")
    if built.returncode or not any(r.get("reason") == "build-finished" and r.get("success") for r in records):
        raise RuntimeError(f"cargo test --no-run failed ({built.returncode})")
    binaries = {}
    target_dir = Path(metadata["target_directory"])
    search_paths = [target_dir / "debug" / "deps", target_dir / "debug"]
    for record in records:
        if record.get("reason") == "build-script-executed":
            for path in record.get("linked_paths", []):
                kind, separator, directory = path.partition("=")
                search_paths.append(Path(directory if separator and kind in {"native", "dependency", "crate", "all", "framework"} else path))
        if record.get("reason") == "compiler-artifact":
            for filename in record.get("filenames", []):
                if Path(filename).suffix.lower() in {".dll", ".so", ".dylib"}:
                    search_paths.append(Path(filename).parent)
        if record.get("reason") != "compiler-artifact" or not record.get("executable"):
            continue
        target = record["target"]
        integration = "test" in target["kind"] and target.get("test", True)
        if not record.get("profile", {}).get("test") and not integration:
            continue
        pkg = by_id.get(record["package_id"])
        if pkg is None:
            raise RuntimeError(f"test artifact is not a workspace member: {record['package_id']}")
        binary = TestBinary(
            pkg["name"], target["name"], tuple(target["kind"]),
            Path(record["executable"]), Path(pkg["manifest_path"]).parent,
            Path(pkg["manifest_path"]), _harness(pkg, target),
        )
        if binary.identity in binaries and binaries[binary.identity] != binary:
            raise RuntimeError(f"ambiguous Cargo artifact: {binary.identity}")
        binaries[binary.identity] = binary
    if not binaries:
        raise RuntimeError("Cargo produced zero workspace test executables")
    rustc_args = [os.environ.get("RUSTC", "rustc"), "--print", "target-libdir"]
    if os.environ.get("CARGO_BUILD_TARGET"):
        rustc_args += ["--target", os.environ["CARGO_BUILD_TARGET"]]
    rustc = command(rustc_args)
    require_success(rustc, "rustc target library directory")
    search_paths.append(Path(rustc.stdout.strip()))
    return list(binaries.values()), packages, search_paths


def plan_cargo(binaries, deferred):
    records = {(r["package"], r["target"]): r for r in deferred}
    actual = {(b.package, b.target) for b in binaries if "test" in b.kind}
    missing = sorted(set(records) - actual)
    if missing:
        raise RuntimeError(f"policy targets absent from Cargo test artifacts: {missing}")
    planned = []
    for binary in binaries:
        record = records.get((binary.package, binary.target)) if "test" in binary.kind else None
        if record is not None and record["skip_tests"] is None:
            continue
        skips = record["skip_tests"] if record else []
        if skips and not binary.harness:
            raise RuntimeError(f"cannot filter custom test harness {binary.identity}")
        planned.append((binary, skips))
    if not planned:
        raise RuntimeError("profile selected zero executable test targets")
    return planned


def scheduled(items, jobs, execute, cancellation=None):
    """Bound processes; serialize a package, and reserve all slots for heavy roots."""
    pending = list(items)
    running = {}
    results = []
    failed = False
    cancellation = cancellation or threading.Event()
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=jobs)
    try:
        while pending or running:
            packages = {item[0].package for item, _ in running.values()}
            used = sum(weight for _, weight in running.values())
            for item in list(pending):
                binary, _ = item
                weight = jobs if binary.package == "vesper" or "lean" in binary.target else 1
                if failed or binary.package in packages or used + weight > jobs:
                    continue
                pending.remove(item)
                running[pool.submit(execute, item)] = (item, weight)
                packages.add(binary.package)
                used += weight
            if not running:
                if failed:
                    break
                raise RuntimeError("test scheduler cannot make progress")
            done, _ = concurrent.futures.wait(running, return_when=concurrent.futures.FIRST_COMPLETED)
            for future in done:
                item, _ = running.pop(future)
                try:
                    result = future.result()
                except Exception as error:  # Record worker failures; never report partial green.
                    result = {"target": item[0].identity, "status": "failed", "error": str(error)}
                results.append(result)
                failed |= result["status"] != "passed"
                if failed:
                    cancellation.set()
    except BaseException:
        cancellation.set()
        raise
    finally:
        pool.shutdown(wait=True)
    return results, [item[0].identity for item in pending]


def run_cargo(deferred, args, summary):
    binaries, packages, search_paths = cargo_discover()
    planned = plan_cargo(binaries, deferred)
    summary["built_test_binaries"] = len(binaries)
    logs = args.summary.parent / (args.summary.stem + "-logs")
    logs.mkdir(parents=True, exist_ok=True)
    cancellation = threading.Event()

    def execute(item):
        binary, skips = item
        env = cargo_environment(binary, packages, search_paths)
        if skips:
            listing = command([binary.executable, "--list", "--format", "terse"], cwd=binary.cwd, env=env)
            require_success(listing, f"list {binary.identity}")
            validate_cases(listing.stdout, skips, binary.identity)
        argv = [str(binary.executable)]
        if binary.harness:
            argv += ["--test-threads", str(args.test_threads), "--exact"]
            for name in skips:
                argv += ["--skip", name]
        name = re.sub(r"[^A-Za-z0-9_.-]", "_", binary.identity)
        digest = hashlib.sha256(binary.identity.encode()).hexdigest()[:10]
        log = logs / f"{name}-{digest}.log"
        started = time.monotonic()
        result, cancelled = test_command(argv, cwd=binary.cwd, env=env, cancellation=cancellation)
        log.write_text(result.stdout + result.stderr, encoding="utf-8")
        status = "cancelled" if cancelled else "passed" if result.returncode == 0 else "failed"
        print(f"{status}: {binary.identity} ({time.monotonic()-started:.2f}s), log={log}", flush=True)
        if status == "failed":
            sys.stdout.write(result.stdout)
            sys.stderr.write(result.stderr)
        return {"target": binary.identity, "status": status, "exit_code": result.returncode,
                "seconds": round(time.monotonic() - started, 3), "skipped_tests": skips,
                "cwd": str(binary.cwd), "log": str(log)}

    results, unexecuted = scheduled(planned, args.jobs, execute, cancellation)
    summary["results"] = results
    summary["unexecuted_after_failure"] = unexecuted
    if unexecuted or any(r["status"] != "passed" for r in results):
        raise RuntimeError("one or more Cargo test binaries failed; queued binaries were not started")
    # `--no-run` does not compile or execute doctests. Keep Cargo's own driver.
    docs = command(["cargo", "test", "--workspace", "--locked", "--doc"])
    sys.stdout.write(docs.stdout)
    sys.stderr.write(docs.stderr)
    summary["doctests_exit_code"] = docs.returncode
    require_success(docs, "workspace doctests")


def _bazel(args, *, capture=False):
    result = command(["bazel", *args])
    if not capture or result.returncode:
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
    require_success(result, "bazel " + " ".join(str(a) for a in args[:2]))
    return result


def run_bazel(deferred, args, summary):
    if not deferred:
        _bazel(["test", "--test_summary=short", "//..."])
        summary["batches"] = [{"kind": "full", "target": "//..."}]
        return
    # Explicitly retain compile coverage for all targets, including deferred tests.
    _bazel(["build", "//..."])
    partial = [r for r in deferred if r["skip_tests"] is not None]
    for record in partial:
        # Bazel supplies the launcher's working directory, PATH and runfiles.
        listing = _bazel(["run", record["bazel"], "--", "--list", "--format", "terse"], capture=True)
        validate_cases(listing.stdout, record["skip_tests"], record["bazel"])
    excluded = [r["bazel"] for r in deferred]
    _bazel(["test", "--test_summary=short", "--", "//...", *["-" + label for label in excluded]])
    batches = [{"kind": "ordinary", "excluded": excluded}]
    if partial:
        names = sorted({name for r in partial for name in r["skip_tests"]})
        flags = ["--test_arg=--exact", f"--test_arg=--test-threads={args.test_threads}"]
        for name in names:
            flags += ["--test_arg=--skip", "--test_arg=" + name]
        _bazel(["test", "--test_summary=short", *flags, *[r["bazel"] for r in partial]])
        batches.append({"kind": "partial-libtest", "targets": [r["bazel"] for r in partial], "skipped_tests": names})
    summary["batches"] = batches


def write_job_summary(summary):
    destination = os.environ.get("GITHUB_STEP_SUMMARY")
    if not destination:
        return
    lines = [f"### Native tests: {summary['backend']} / {summary['profile']}", "",
             f"Status: **{summary['status']}**; commit: `{summary.get('sha', 'unavailable')}`.", "",
             "Full groups: " + (", ".join(summary.get("full_groups", [])) or "none") + ".", "",
             "Deferred coverage:", ""]
    for record in summary.get("deferred", []):
        cases = record["skip_tests"]
        scope = "whole test target" if cases is None else ", ".join(cases)
        lines.append(f"- `{record['package']}:{record['target']}`: {scope}")
    if not summary.get("deferred"):
        lines.append("None selected for deferral; see status above for completion.")
    lines += ["", "Execution details and errors are in the uploaded JSON summary.", ""]
    with open(destination, "a", encoding="utf-8") as stream:
        stream.write("\n".join(lines))


def main(argv=None):
    # Windows redirected consoles can default to cp1252. A failed Lean test
    # may print Unicode theorem/source text; preserve its original failure.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="backslashreplace")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["cargo", "bazel"], required=True)
    parser.add_argument("--profile", choices=["pr", "full"], required=True)
    parser.add_argument("--groups", default="[]", help="JSON array of groups escalated to full coverage")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--test-threads", type=int, default=2)
    parser.add_argument("--summary", type=Path, required=True)
    args = parser.parse_args(argv)
    if args.jobs < 1 or args.test_threads < 1:
        parser.error("jobs and test-threads must be positive")
    summary = {"schema_version": 1, "backend": args.backend, "profile": args.profile, "status": "failed"}
    args.summary.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    try:
        revision = command(["git", "rev-parse", "HEAD"])
        require_success(revision, "resolve checked-out commit")
        sha = revision.stdout.strip()
        if not re.fullmatch(r"[0-9a-f]{40}", sha):
            raise ValueError("checked-out commit must be a full SHA")
        summary["sha"] = sha
        expected = os.environ.get("EXPECTED_SHA")
        if expected and sha != expected:
            raise ValueError("checked-out commit does not match EXPECTED_SHA")
        groups = json.loads(args.groups)
        if not isinstance(groups, list) or not all(isinstance(g, str) for g in groups):
            raise ValueError("groups must be a JSON array of strings")
        load_policy()  # Validate policy/source associations before compiling or executing.
        records = deferred_targets(args.profile, groups)
        summary.update({"full_groups": groups, "deferred": records})
        if args.backend == "cargo":
            run_cargo(records, args, summary)
        else:
            run_bazel(records, args, summary)
        summary["status"] = "passed"
    except (KeyError, TypeError, ValueError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        summary["error"] = str(error)
        print(f"platform profile failed: {error}", file=sys.stderr)
    finally:
        summary["seconds"] = round(time.monotonic() - started, 3)
        args.summary.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        write_job_summary(summary)
    return 0 if summary["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
