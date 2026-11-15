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
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass

from platform_test_plan import deferred_targets, load_policy


ROOT = Path(__file__).resolve().parents[2]


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
