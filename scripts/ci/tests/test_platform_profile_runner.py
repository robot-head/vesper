"""Selection, process isolation, and failure semantics of the native CI runner."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("platform_profile_runner", SCRIPTS / "test-platform-profile.py")
runner = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = runner
spec.loader.exec_module(runner)


def result(stdout="", stderr="", code=0):
    return subprocess.CompletedProcess([], code, stdout, stderr)


def binary(package="ordinary", target="suite", *, harness=True):
    return runner.TestBinary(package, target, ("test",), Path("/fake") / package / target,
                             Path("/workspace") / package,
                             Path("/workspace") / package / "Cargo.toml", harness)


def record(package="ordinary", target="suite", skips=None):
    return {"package": package, "target": target, "bazel": "//crates/ordinary:test/suite",
            "group": "faults", "skip_tests": skips}


class SelectionTests(unittest.TestCase):
    def test_unclassified_targets_remain_and_full_has_no_tier_skips(self):
        items = [binary(), binary(target="new_unclassified")]
        self.assertEqual(runner.plan_cargo(items, []), [(b, []) for b in items])
        self.assertEqual(runner.plan_cargo(items, [record()]), [(items[1], [])])

    def test_partial_selection_preserves_exact_names(self):
        b = binary()
        names = ["sweep"]
        self.assertEqual(runner.plan_cargo([b], [record(skips=names)]), [(b, names)])
        runner.validate_cases("sweep: test\nsweep_extra: test\nordinary: test\n", names, "target")
        with self.assertRaisesRegex(RuntimeError, "absent"):
            runner.validate_cases("renamed_sweep: test\n", names, "target")

    def test_partial_cannot_report_green_with_every_case_excluded(self):
        with self.assertRaisesRegex(RuntimeError, "zero test cases"):
            runner.validate_cases("sweep: test\n", ["sweep"], "target")

    def test_missing_target_and_custom_harness_filters_fail(self):
        with self.assertRaisesRegex(RuntimeError, "absent"):
            runner.plan_cargo([binary()], [record(target="renamed")])
        with self.assertRaisesRegex(RuntimeError, "custom test harness"):
            runner.plan_cargo([binary(harness=False)], [record(skips=["sweep"])])

    def test_cargo_binary_and_library_unit_names_are_not_confused_with_integration(self):
        b = binary()
        lib = runner.TestBinary(b.package, b.target, ("lib",), b.executable,
                                b.cwd, b.manifest)
        self.assertEqual(runner.plan_cargo([b, lib], [record()]), [(lib, [])])


class SchedulerTests(unittest.TestCase):
    def test_two_packages_overlap_but_package_binaries_do_not(self):
        items = [(binary("a", "one"), []), (binary("a", "two"), []),
                 (binary("b", "one"), []), (binary("c", "one"), [])]
        lock = threading.Lock()
        running, peaks = {}, []
        overlapped = threading.Event()

        def execute(item):
            b, _ = item
            with lock:
                self.assertNotIn(b.package, running)
                running[b.package] = True
                peaks.append(len(running))
                if len(running) == 2:
                    overlapped.set()
            self.assertTrue(overlapped.wait(2), "different packages never overlapped")
            time.sleep(0.01)
            with lock:
                running.pop(b.package)
            return {"target": b.identity, "status": "passed"}

        results, remaining = runner.scheduled(items, 2, execute)
        self.assertEqual(len(results), 4)
        self.assertEqual(remaining, [])
        self.assertEqual(max(peaks), 2)
        self.assertTrue(all(r["status"] == "passed" for r in results))

    def test_root_and_lean_targets_hold_all_process_slots(self):
        items = [(binary("a"), []), (binary("vesper"), []),
                 (binary("b", "lean_model"), []), (binary("c"), [])]
        lock = threading.Lock()
        active = []
        violations = []

        def execute(item):
            b, _ = item
            heavy = b.package == "vesper" or "lean" in b.target
            with lock:
                if heavy and active or any(x for x in active):
                    violations.append(b.identity)
                active.append(heavy)
            time.sleep(0.01)
            with lock:
                active.remove(heavy)
            return {"target": b.identity, "status": "passed"}

        results, remaining = runner.scheduled(items, 2, execute)
        self.assertEqual(violations, [])
        self.assertEqual(len(results), 4)
        self.assertFalse(remaining)

    def test_failure_stops_queued_work_and_exceptions_fail(self):
        items = [(binary("a"), []), (binary("b"), [])]
        results, remaining = runner.scheduled(items, 1, lambda _: {"status": "failed"})
        self.assertEqual(len(results), 1)
        self.assertEqual(remaining, [items[1][0].identity])

        def raises(_):
            raise OSError("cannot start executable")

        results, remaining = runner.scheduled(items, 1, raises)
        self.assertEqual(results[0]["status"], "failed")
        self.assertIn("cannot start", results[0]["error"])
        self.assertEqual(len(remaining), 1)


class CargoTests(unittest.TestCase):
    def test_library_harness_applies_to_proc_macros_and_nondefault_crate_types(self):
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / "Cargo.toml"
            manifest.write_text("[lib]\nharness=false\n", encoding="utf-8")
            pkg = {"manifest_path": str(manifest)}
            for kind in ["lib", "rlib", "proc-macro", "cdylib", "staticlib", "dylib"]:
                with self.subTest(kind=kind):
                    self.assertFalse(runner._harness(pkg, {"name": "fixture", "kind": [kind]}))
            self.assertTrue(runner._harness(pkg, {"name": "fixture", "kind": ["test"]}))

    @unittest.skipUnless(shutil.which("cargo"), "Cargo unavailable for the owned fixture")
    def test_real_cargo_artifacts_custom_harness_and_doctests(self):
        # Three tiny std-only executables exercise the real Cargo JSON contract;
        # this never builds the repository workspace or downloads dependencies.
        with tempfile.TemporaryDirectory(prefix="platform-profile-fixture-") as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            (root / "tests").mkdir()
            (root / "Cargo.toml").write_text(
                '[package]\nname="ci-profile-fixture"\nversion="0.1.0"\nedition="2024"\n'
                '[[test]]\nname="custom"\nharness=false\n', encoding="utf-8")
            (root / "src/lib.rs").write_text(
                '/// ```\n/// assert_eq!(2+2,4);\n/// ```\n'
                'pub fn value() {}\n#[test] fn unit() {}\n', encoding="utf-8")
            (root / "tests/ordinary.rs").write_text(
                '#[test] fn sweep() {}\n#[test] fn sweep_extra() {}\n#[test] fn ordinary() {}\n',
                encoding="utf-8")
            (root / "tests/custom.rs").write_text(
                'fn main() { assert_eq!(std::env::args().count(),1); '
                'assert_eq!(std::env::var("CARGO_MANIFEST_DIR").unwrap(),env!("CARGO_MANIFEST_DIR")); }\n',
                encoding="utf-8")
            actual_command = runner.command

            def owned_command(argv, **kwargs):
                kwargs.setdefault("cwd", root)
                return actual_command(argv, **kwargs)

            with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(root / "target"), "CARGO_NET_OFFLINE": "true"}):
                subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root,
                               check=True, capture_output=True)
                for profile, skips, expected in [("pr", [record("ci-profile-fixture", "ordinary", ["sweep"])], 2), ("full", [], 3)]:
                    args = types.SimpleNamespace(summary=root / (profile + ".json"), jobs=2, test_threads=2)
                    summary = {}
                    with mock.patch.object(runner, "command", side_effect=owned_command), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                        runner.run_cargo(skips, args, summary)
                    self.assertEqual(len(summary["results"]), 3)
                    self.assertTrue(all(r["status"] == "passed" for r in summary["results"]))
                    ordinary = next(r for r in summary["results"] if r["target"].endswith(":ordinary"))
                    self.assertIn(f"{expected} passed; 0 failed; 0 ignored", Path(ordinary["log"]).read_text())
                    self.assertEqual(summary["doctests_exit_code"], 0)

    def test_environment_preserves_host_settings_and_adds_loader_paths(self):
        b = binary("pkg")
        pkg = {"pkg": {"version": "1.2.3-rc.1", "authors": ["One", "Two"]}}
        with mock.patch.dict(os.environ, {"PATH": "existing", "VESPER_LEAN": "pinned-lean"}):
            env = runner.cargo_environment(b, pkg, [Path("/deps"), Path("/native")])
        self.assertEqual(env["VESPER_LEAN"], "pinned-lean")
        self.assertEqual(env["CARGO_MANIFEST_DIR"], str(b.cwd))
        self.assertEqual(env["CARGO_PKG_VERSION_PRE"], "rc.1")
        self.assertEqual(env["CARGO_PKG_AUTHORS"], "One:Two")
        self.assertTrue(env["PATH"].startswith(str(b.executable.parent) + os.pathsep))
        self.assertIn(str(Path("/deps")), env["PATH"])
        self.assertTrue(env["PATH"].endswith("existing"))

    def test_discovery_uses_real_artifact_ids_and_handles_custom_harness(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            manifest = root / "Cargo.toml"
            manifest.write_text('[[test]]\nname="custom"\nharness=false\n', encoding="utf-8")
            pkg = {"id": "opaque package id", "name": "pkg", "version": "0.1.0",
                   "manifest_path": str(manifest)}
            metadata = {"workspace_members": [pkg["id"]], "packages": [pkg],
                        "target_directory": str(root / "target")}
            artifact = {"reason": "compiler-artifact", "package_id": pkg["id"],
                        "target": {"name": "custom", "kind": ["test"]},
                        "profile": {"test": False}, "executable": str(root / "custom.exe")}
            records = [artifact, artifact,
                       {"reason": "compiler-artifact", "executable": None,
                        "filenames": [str(root / "dynamic" / "library.dll"), str(root / "static" / "library.rlib")]},
                       {"reason": "build-script-executed", "linked_paths": ["native=" + str(root / "native=dir")]},
                       {"reason": "build-finished", "success": True}]
            output = "\n".join(json.dumps(r) for r in records)
            with mock.patch.object(runner, "command", side_effect=[result(json.dumps(metadata)), result(output), result(str(root / "rustlib"))]) as execute:
                bins, packages, paths = runner.cargo_discover()
            self.assertEqual(len(bins), 1)
            self.assertFalse(bins[0].harness)
            self.assertEqual(bins[0].cwd, root)
            self.assertEqual(packages["pkg"], pkg)
            self.assertIn(root / "native=dir", paths)
            self.assertIn(root / "rustlib", paths)
            self.assertIn(root / "dynamic", paths)
            self.assertNotIn(root / "static", paths)
            self.assertNotIn(root, paths, "unrelated executable directories must not inflate Windows PATH")
            self.assertIn("--no-run", execute.call_args_list[1].args[0])

    def test_failed_build_does_not_turn_into_an_empty_success(self):
        metadata = {"workspace_members": [], "packages": [], "target_directory": "/target"}
        for build in [result(code=1), result('{"reason":"build-finished","success":true}')]:
            with mock.patch.object(runner, "command", side_effect=[result(json.dumps(metadata)), build]):
                with self.assertRaises(RuntimeError):
                    runner.cargo_discover()

    def test_actual_harness_arguments_and_doctests_are_separate(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = types.SimpleNamespace(summary=Path(tmp) / "summary.json", jobs=1, test_threads=2)
            bins = [binary()]
            packages = {"ordinary": {"version": "1.0.0"}}
            calls = []

            def execute(argv, **kwargs):
                calls.append((list(argv), kwargs))
                if "--list" in argv:
                    return result("sweep: test\nordinary: test\n")
                return result("test result: ok. 1 passed; 0 failed\n")

            summary = {}
            def run_test(argv, **kwargs):
                return execute(argv, **kwargs), False

            with mock.patch.object(runner, "cargo_discover", return_value=(bins, packages, [])), mock.patch.object(runner, "command", side_effect=execute), mock.patch.object(runner, "test_command", side_effect=run_test), contextlib.redirect_stdout(io.StringIO()):
                runner.run_cargo([record(skips=["sweep"])], args, summary)
            command = calls[1][0]
            self.assertIn("--exact", command)
            self.assertEqual(command[-2:], ["--skip", "sweep"])
            self.assertEqual(calls[-1][0], ["cargo", "test", "--workspace", "--locked", "--doc"])
            self.assertEqual(summary["doctests_exit_code"], 0)
            self.assertTrue(Path(summary["results"][0]["log"]).is_file())

    def test_failure_cancels_an_owned_running_subprocess(self):
        cancellation = threading.Event()
        ready = threading.Event()
        processes = []
        original = runner.subprocess.Popen

        def opened(*args, **kwargs):
            p = original(*args, **kwargs)
            processes.append(p)
            ready.set()
            return p

        timer = threading.Thread(target=lambda: (ready.wait(3), cancellation.set()))
        timer.start()
        with mock.patch.object(runner.subprocess, "Popen", side_effect=opened):
            output, cancelled = runner.test_command(
                [sys.executable, "-c", "import time; time.sleep(30)"],
                cwd=SCRIPTS, env=dict(os.environ), cancellation=cancellation,
            )
        timer.join(3)
        self.assertTrue(cancelled)
        self.assertNotEqual(output.returncode, 0)
        self.assertIsNotNone(processes[0].poll())
        self.assertIn("cancelled after", output.stderr)

    def test_precancelled_work_never_starts_a_child(self):
        cancellation = threading.Event()
        cancellation.set()
        with mock.patch.object(runner.subprocess, "Popen") as start:
            output, cancelled = runner.test_command(["unused"], cwd=SCRIPTS, env={}, cancellation=cancellation)
        self.assertTrue(cancelled)
        self.assertNotEqual(output.returncode, 0)
        start.assert_not_called()


class BazelTests(unittest.TestCase):
    def test_full_is_the_original_unfiltered_suite(self):
        with mock.patch.object(runner, "_bazel") as execute:
            runner.run_bazel([], types.SimpleNamespace(), {})
        self.assertEqual(execute.call_args_list, [mock.call(["test", "--test_summary=short", "//..."])])

    def test_pr_keeps_build_coverage_and_only_rust_receives_filters(self):
        records = [record(target="whole"), record(target="partial", skips=["sweep"])]
        records[0]["bazel"] = "//:test/whole"
        records[1]["bazel"] = "//:test/partial"
        summary = {}
        with mock.patch.object(runner, "_bazel", return_value=result("sweep: test\nordinary: test\n")) as execute:
            runner.run_bazel(records, types.SimpleNamespace(test_threads=2), summary)
        calls = [c.args[0] for c in execute.call_args_list]
        self.assertEqual(calls[0], ["build", "//..."])
        self.assertEqual(calls[1][:2], ["run", "//:test/partial"])
        ordinary = calls[2]
        self.assertIn("-//:test/whole", ordinary)
        self.assertIn("-//:test/partial", ordinary)
        self.assertFalse(any(a.startswith("--test_arg") for a in ordinary))
        self.assertIn("--test_arg=--exact", calls[3])
        self.assertNotIn("//...", calls[3])
        self.assertEqual(calls[3][-1], "//:test/partial")

    def test_stale_partial_case_fails_before_running_tests(self):
        with mock.patch.object(runner, "_bazel", return_value=result("renamed: test\n")) as execute:
            with self.assertRaisesRegex(RuntimeError, "absent"):
                runner.run_bazel([record(skips=["sweep"])], types.SimpleNamespace(test_threads=2), {})
        self.assertEqual(len(execute.call_args_list), 2)


class CommandLineTests(unittest.TestCase):
    def test_wrong_commit_fails_before_backend_and_records_actual_sha(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.json"
            with mock.patch.dict(os.environ, {"EXPECTED_SHA": "b" * 40}), mock.patch.object(runner, "command", return_value=result("a" * 40 + "\n")), mock.patch.object(runner, "run_cargo") as execute, contextlib.redirect_stderr(io.StringIO()):
                code = runner.main(["--backend", "cargo", "--profile", "full", "--summary", str(summary)])
            self.assertEqual(code, 1)
            execute.assert_not_called()
            self.assertEqual(json.loads(summary.read_text())["sha"], "a" * 40)

    def test_success_writes_coverage_to_job_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.json"
            markdown = Path(tmp) / "summary.md"
            with mock.patch.dict(os.environ, {"EXPECTED_SHA": "a" * 40, "GITHUB_STEP_SUMMARY": str(markdown)}), mock.patch.object(runner, "command", return_value=result("a" * 40 + "\n")), mock.patch.object(runner, "load_policy"), mock.patch.object(runner, "deferred_targets", return_value=[record(skips=["sweep"])]), mock.patch.object(runner, "run_cargo"):
                code = runner.main(["--backend", "cargo", "--profile", "pr", "--summary", str(summary)])
            self.assertEqual(code, 0)
            rendered = markdown.read_text()
            self.assertIn("**passed**", rendered)
            self.assertIn("a" * 40, rendered)
            self.assertIn("sweep", rendered)

    def test_failure_writes_failed_summary_and_returns_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.json"
            with mock.patch.object(runner, "load_policy"), mock.patch.object(runner, "deferred_targets", return_value=[]), mock.patch.object(runner, "run_cargo", side_effect=RuntimeError("broken test")), contextlib.redirect_stderr(io.StringIO()):
                code = runner.main(["--backend", "cargo", "--profile", "pr", "--summary", str(summary)])
            self.assertEqual(code, 1)
            data = json.loads(summary.read_text())
            self.assertEqual(data["status"], "failed")
            self.assertEqual(data["error"], "broken test")

    def test_nonarray_groups_fail_without_starting_a_backend(self):
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.json"
            with mock.patch.object(runner, "run_cargo") as execute, contextlib.redirect_stderr(io.StringIO()):
                code = runner.main(["--backend", "cargo", "--profile", "pr", "--groups", "{}", "--summary", str(summary)])
            self.assertEqual(code, 1)
            execute.assert_not_called()


if __name__ == "__main__":
    unittest.main()
