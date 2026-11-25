"""Exercise the Aspect CLI and task registrations in an isolated checkout."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
ASPECT = shutil.which("aspect")


@unittest.skipUnless(ASPECT, "Aspect CLI unavailable")
class AspectTaskTests(unittest.TestCase):
    def setUp(self):
        scratch = self.enterContext(tempfile.TemporaryDirectory())
        self.root = Path(scratch).resolve() / "vesper workspace"
        self.nested = self.root / "nested directory"
        self.nested.mkdir(parents=True)
        shutil.copytree(ROOT / ".aspect", self.root / ".aspect")

    def write(self, relative, content, *, executable=False):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        if executable:
            path.chmod(0o755)
        return path

    def aspect(self, *args, status=0, stdin="", env=None):
        result = subprocess.run(
            [ASPECT, *args], cwd=self.nested, input=stdin, text=True,
            env={**os.environ, "CI": "true", **(env or {})},
            capture_output=True, timeout=30,
        )
        self.assertEqual(result.returncode, status, result.stderr)
        return result

    def powershell(self, script, *args, status=0, env=None):
        result = subprocess.run(
            [shutil.which("pwsh"), "-NoProfile", "-File", str(self.root / script), *args],
            cwd=self.nested, text=True, env={**os.environ, **(env or {})},
            capture_output=True, timeout=30,
        )
        self.assertEqual(result.returncode, status, result.stderr)
        return result

    def copy_fixture(self, path):
        self.write(path, (ROOT / path).read_text())

    def test_script_task_preserves_argv_cwd_environment_and_standard_streams(self):
        self.write("scripts/ci/merge_gate.py", """\
import json
import os
from pathlib import Path
import sys
print(json.dumps({
    "argv": sys.argv[1:],
    "cwd": str(Path.cwd()),
    "env": os.environ["VESPER_TASK_TEST_VALUE"],
    "stdin": sys.stdin.read(),
}))
print("child stderr", file=sys.stderr)
""")
        args = ["--help", "", "two words", "$HOME", "*.py", "--", "line\nbreak"]
        result = self.aspect(
            "ci", "merge-gate", "--", *args,
            stdin="input with spaces\n", env={"VESPER_TASK_TEST_VALUE": "value with spaces"},
        )
        self.assertEqual(json.loads(result.stdout), {
            "argv": args,
            "cwd": str(self.nested),
            "env": "value with spaces",
            "stdin": "input with spaces\n",
        })
        self.assertIn("child stderr", result.stderr)

    def test_rooted_checker_runs_from_workspace_root(self):
        self.write("scripts/check-doc-links.py", "from pathlib import Path\nprint(Path.cwd())\n")
        result = self.aspect("acceptance", "check-doc-links")
        self.assertEqual(result.stdout.splitlines(), [str(self.root)])

    def test_resource_oracle_fixtures_pass_the_mode_and_native_status(self):
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
import sys
from pathlib import Path
assert Path(".aspect").is_dir()
print(json.dumps(sys.argv[1:]))
if sys.argv[-1] == "crash":
    sys.exit(7)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(cargo.parent), os.environ["PATH"]])}
        flags = ["run", "--locked", "--quiet", "-p", "vesper-oracles", "--bin", "resource_oracle42", "--", "--fault-peer"]
        for args, mode, code in (("bye-eof-stall", "bye_eof_stall", 0), ("fault-peer", "crash", 7)):
            argv = (args, "--", mode) if args == "fault-peer" else (args,)
            result = self.aspect("fixture", "resource-oracle42", *argv, env=env, status=code)
            self.assertEqual(json.loads(result.stdout), flags + [mode])

    def test_acceptance_suites_forward_binary_root_and_scratch_and_run_examples(self):
        env = {
            "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
            "BAZEL": "/missing-bazel",
            "VESPER_BAZEL_EXECUTABLES": "executables",
        }
        # Use the same interpreter selection as the BuildBuddy acceptance job.
        (self.root / "bin").mkdir()
        (self.root / "bin/python").symlink_to(shutil.which("python3"))
        compiler = self.write("bin/compiler.exe", "")
        examples = {
            "canonical-protocols": ["vesper-grammar:example/" + name for name in ("protobuf", "der", "ipc")]
            + ["vesper-bench-pmu:example/probe"],
            "diagnostics": ["vesper-diagnostics:example/causal_diagnostics"],
            "editor": ["vesper-span:example/source_edits", "vesper-lsp:example/protocol_documents"],
            "source-tooling": ["vesper-grammar:example/grammar", "vesper-fmt:example/canonical",
                               "vesper-bench:example/paired_callbacks"],
        }
        rows = ["//:bin/vesper bin/compiler"]
        self.write("bin/compiler", "")
        for label in [label for values in examples.values() for label in values]:
            name = label.split("/")[-1]
            rows.append(f"//crates/{label} bin/{name}")
            body = '#!/bin/sh\nprintf "%s\\n" "' + name + '" >> order\n'
            if name == "probe":
                body += 'exit "${VESPER_TEST_PMU_STATUS:-0}"\n'
            self.write(f"bin/{name}", body, executable=True)
        self.write("executables", "\n".join(rows) + "\n")

        for task, override in (
            ("canonical-protocols", None),
            ("diagnostics", None),
            ("doc-links", "VESPER_DOC_LINKS_BINARY"),
            ("editor", None),
            ("format-proof", "VESPER_FORMAT_PROOF_BINARY"),
            ("macro-preview", "VESPER_MACRO_PREVIEW_BINARY"),
            ("recovery", "VESPER_RECOVERY_BINARY"),
            ("source-macro-parameters", "VESPER_SOURCE_MACRO_PARAMETERS_BINARY"),
            ("source-macro-tuples", "VESPER_SOURCE_MACRO_TUPLES_BINARY"),
            ("source-macros", "VESPER_SOURCE_MACROS_BINARY"),
            ("source-tooling", None),
            ("stylus", "VESPER_STYLUS_BINARY"),
        ):
            with self.subTest(task=task):
                checker = f"scripts/check-{task}.py"
                self.write(checker, """\
import json
from pathlib import Path
import sys
work = sys.argv[sys.argv.index("--work") + 1] if "--work" in sys.argv else None
if work:
    assert Path(work).is_dir()
print(json.dumps([str(Path.cwd()), sys.argv[1:]]))
Path("order").write_text("checker" + chr(10))
""")
                task_env = env.copy()
                expected_binary = compiler
                if override:
                    expected_binary = self.write(f"bin/{task} overridden compiler.exe", "")
                    task_env[override] = str(expected_binary.with_suffix(""))
                result = self.aspect("acceptance", "test-" + task, env=task_env)
                cwd, argv = json.loads(result.stdout.splitlines()[0])
                self.assertEqual(cwd, str(self.root))
                expected = ["--binary", str(expected_binary)]
                if task in ("canonical-protocols", "source-tooling"):
                    expected += ["--root", str(self.root)]
                if task != "doc-links":
                    work = Path(argv[-1])
                    self.assertTrue(work.name.startswith(f"vesper-{task}-"))
                    self.assertFalse(work.exists())
                    expected += ["--work", str(work)]
                self.assertEqual(argv, expected)
                order = (self.root / "order").read_text().splitlines()
                self.assertEqual(order, ["checker", *[label.split("/")[-1] for label in examples.get(task, [])]])
                if task == "canonical-protocols":
                    self.assertIn("positive counters", result.stdout)

        for status, stream, expected in ((2, "stdout", "reported unavailable"), (5, "stderr", "failed unexpectedly")):
            result = self.aspect("acceptance", "test-canonical-protocols",
                                 env={**env, "VESPER_TEST_PMU_STATUS": str(status)},
                                 status=0 if status == 2 else status)
            self.assertIn(expected, getattr(result, stream))

    def test_native_stylus_builds_with_cargo_defaults_and_preserves_overrides(self):
        (self.root / "bin").mkdir()
        (self.root / "bin/python").symlink_to(shutil.which("python3"))
        self.write("bin/cargo", """\
#!/bin/sh
test "$*" = "build --locked -p vesper --bin vesper" || exit 2
printf '%s %s %s %s %s\\n' "$CARGO_BUILD_JOBS" "$CARGO_PROFILE_DEV_DEBUG" "$CARGO_PROFILE_TEST_DEBUG" "$CARGO_INCREMENTAL" "$CARGO_TARGET_DIR" > build
""", executable=True)
        self.write("scripts/check-stylus.py", """\
import os
import sys
assert os.environ["CARGO_BUILD_JOBS"] == "3"
assert os.environ["CARGO_PROFILE_TEST_DEBUG"] == "0"
print(sys.argv[sys.argv.index("--binary") + 1])
""")
        env = {
            "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
            "VESPER_STYLUS_BINARY": "",
            "CARGO_BUILD_JOBS": "3",
            "CARGO_PROFILE_DEV_DEBUG": "",
            "CARGO_PROFILE_TEST_DEBUG": "",
            "CARGO_INCREMENTAL": "",
            "CARGO_TARGET_DIR": "relative target",
        }
        result = self.aspect("acceptance", "test-stylus", env=env)
        self.assertEqual(result.stdout.strip(), str(self.root / "relative target/debug/vesper"))
        self.assertEqual((self.root / "build").read_text().strip(), "3 0 0 0 relative target")

    def test_native_acceptance_removes_scratch_and_skips_examples_on_failure(self):
        (self.root / "bin").mkdir()
        (self.root / "bin/python").symlink_to(shutil.which("python3"))
        self.write("executables", "//:bin/vesper bin/compiler\n")
        self.write("bin/compiler", "")
        self.write("scripts/check-diagnostics.py", """\
from pathlib import Path
import sys
Path("scratch").write_text(sys.argv[-1])
raise SystemExit(17)
""")
        env = {
            "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
            "BAZEL": "/missing-bazel",
            "VESPER_BAZEL_EXECUTABLES": "executables",
        }
        self.aspect("acceptance", "test-diagnostics", env=env, status=17)
        self.assertFalse(Path((self.root / "scratch").read_text()).exists())

    def test_fixture_builds_publish_payloads_and_hashes_only_after_cargo_succeeds(self):
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
assert args[:4] == ["build", "--locked", "--release", "--manifest-path"]
assert args[5] == "--target"
guest = Path(args[4]).parent
target = args[6]
hash_inputs = str(guest) + "|" + target
with open("fixtures.log", "a") as log:
    keys = ["CARGO_TARGET_DIR", "CARGO_BUILD_JOBS", "CARGO_INCREMENTAL", "RUSTFLAGS"]
    print(json.dumps([hash_inputs, {key: os.environ[key] for key in keys}]), file=log)
if target == os.environ.get("VESPER_FIXTURE_FAIL_TARGET"):
    sys.exit(12)
name = "vesper_sandbox_fixture" if "loom-sandbox" in str(guest) else "vesper_forge_runner_fixture"
if "quotation" in str(guest):
    name = "vesper_quotation_fixture"
out = Path(os.environ["CARGO_TARGET_DIR"]) / target / "release"
out.mkdir(parents=True, exist_ok=True)
(out / (name + ".wasm")).write_bytes(bytes([0, 128, 255]) + target.encode())
""", executable=True)
        env = {
            "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
            "CARGO_HOME": str(self.root / "cargo home"),
            "CARGO_TARGET_DIR": "shared out",
            "CARGO_BUILD_JOBS": "9",
            "CARGO_INCREMENTAL": "1",
        }
        calls = self.nested / "fixtures.log"
        cases = (
            ("forge-runner", "build", "", [("wasm32-wasip2", "macro.component.wasm")]),
            ("loom-sandbox", "build", "", [("wasm32-wasip2", "step.component.wasm"),
                                         ("wasm32-wasip1", "step.preview1.wasm")]),
            ("loom-sandbox", "build-quotation", "/quotation",
             [("wasm32-wasip2", "quotation.component.wasm")]),
        )
        for crate, task, qualifier, outputs in cases:
            with self.subTest(task=task, crate=crate):
                if calls.exists():
                    calls.unlink()
                result = self.aspect("crate", crate, task, env=env)
                fixture = self.root / f"crates/vesper-{crate}/fixtures{qualifier}"
                builds = [json.loads(line) for line in calls.read_text().splitlines()]
                self.assertEqual([item[0] for item in builds],
                                 [f"{fixture / 'guest'}|{target}" for target, _ in outputs])
                for _, variables in builds:
                    self.assertEqual(variables["CARGO_TARGET_DIR"], str(
                        self.nested / "shared out" if qualifier else fixture / "guest/target"
                    ))
                    self.assertEqual(variables["CARGO_BUILD_JOBS"], "1" if qualifier else "9")
                    self.assertEqual(variables["CARGO_INCREMENTAL"], "0" if qualifier else "1")
                    remap = "/quotation-fixture" if qualifier else "/fixture"
                    self.assertEqual(variables["RUSTFLAGS"],
                                     f"--remap-path-prefix={fixture / 'guest'}={remap} "
                                     f"--remap-path-prefix={env['CARGO_HOME']}=/cargo")
                expected = []
                for target, name in outputs:
                    payload = bytes([0, 128, 255]) + target.encode()
                    self.assertEqual((fixture / "bin" / name).read_bytes(), payload)
                    expected.append(hashlib.sha256(payload).hexdigest() + "  " + name)
                self.assertEqual(result.stdout, "\n".join(expected) + "\n")
                self.assertEqual(result.stdout, (fixture / "bin/SHA256SUMS").read_text())
        calls.unlink()
        self.aspect("crate", "loom-sandbox", "build-quotation",
                    env={**env, "CARGO_HOME": "", "CARGO_TARGET_DIR": ""})
        _, defaults = json.loads(calls.read_text())
        guest = self.root / "crates/vesper-loom-sandbox/fixtures/quotation/guest"
        self.assertEqual(defaults["CARGO_TARGET_DIR"], str(guest / "target"))
        self.assertIn(f"--remap-path-prefix={Path.home() / '.cargo'}=/cargo", defaults["RUSTFLAGS"])
        sandbox = self.root / "crates/vesper-loom-sandbox/fixtures/bin"
        checksum = (sandbox / "SHA256SUMS").read_bytes()
        self.aspect("crate", "loom-sandbox", "build",
                    env={**env, "VESPER_FIXTURE_FAIL_TARGET": "wasm32-wasip1"}, status=12)
        self.assertEqual((sandbox / "SHA256SUMS").read_bytes(), checksum)

    def test_lean4lean_task_aliases_forward_arguments_to_logos(self):
        self.write("tools/logos42/setup.py",
                   "import sys\nprint(*sys.argv[1:], sep=chr(10))\nraise SystemExit(19)\n")
        self.write("executables", "//crates/vesper-oracles:bin/logos42 bin/checker\n")
        checker = self.write("bin/checker", "")
        env = {
            "VESPER_LEAN": "/fixture/bin/lean",
            "VESPER_CARGO": "/fixture/bin/cargo",
            "VESPER_BAZEL_EXECUTABLES": "executables",
        }
        for alias, canonical, expected in (
            (("ci", "prepare-lean4lean42"), ("ci", "prepare-logos42"),
             ["--directory", "fixture with spaces", "--driver", str(checker), "--lean", "/fixture/bin/lean",
              "--lake", "/fixture/bin/lake", "--git", shutil.which("git"), "--cargo", "/fixture/bin/cargo"]),
            (("maintenance", "lean4lean42", "setup"), ("maintenance", "logos42", "setup"),
             ["--directory", "fixture with spaces"]),
        ):
            with self.subTest(alias=alias):
                forwarded = ("--", "--directory", "fixture with spaces") if alias[0] == "maintenance" else (
                    "--", "fixture with spaces")
                original = self.aspect(*canonical, *forwarded, status=19, env=env)
                compatible = self.aspect(*alias, *forwarded, status=19, env=env)
                self.assertEqual(compatible.stdout.splitlines(), expected)
                self.assertEqual(compatible.stdout, original.stdout)

    def test_native_logos_prepare_validates_inputs_and_discovers_cargo(self):
        self.write("tools/logos42/setup.py", "import sys\nprint(sys.argv[-1])\n")
        self.write("executables", "//crates/vesper-oracles:bin/logos42 bin/checker\n")
        self.write("bin/checker", "")
        for command in ("git", "python3"):
            (self.root / "bin" / command).symlink_to(shutil.which(command))
        cargo = self.write("bin/cargo", "#!/bin/sh\nexit 0\n", executable=True)
        rustup = self.write("bin/rustup", '#!/bin/sh\ntest "$*" = "which cargo" || exit 22\necho /fixture/cargo\n',
                            executable=True)
        env = {
            "PATH": str(self.root / "bin"),
            "VESPER_LEAN": "relative-lean",
            "VESPER_CARGO": "",
            "VESPER_BAZEL_EXECUTABLES": "executables",
        }
        command = ("ci", "prepare-logos42", "--", "missing")
        self.assertEqual(self.aspect(*command, env=env).stdout.strip(), "/fixture/cargo")
        rustup.unlink()
        self.assertEqual(self.aspect(*command, env=env).stdout.strip(), str(cargo))
        missing_lean = self.aspect(*command, env={**env, "VESPER_LEAN": ""}, status=2)
        self.assertIn("ABSENT_DIRECTORY", missing_lean.stderr)
        cargo.unlink()
        missing_cargo = self.aspect(*command, env=env, status=1)
        self.assertIn("requires Cargo and Git", missing_cargo.stderr)

    def test_native_logos_sweep_reports_results_even_when_the_checker_fails(self):
        self.write("manifest", "//crates/vesper-oracles:bin/logos42 bin/checker\n//:bin/vesper bin/vesper\n")
        checker = self.write("bin/checker", "")
        vesper = self.write("bin/vesper", "")
        self.write("tools/logos-sweep/sweep.py", """\
import json
import os
from pathlib import Path
import sys

args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
assert args["--root"] == "sealed fixture"
assert args["--lean"] == "/fixture/lean"
(Path("args.json")).write_text(json.dumps(args))
if not os.getenv("VESPER_TEST_NO_REPORT"):
    Path(args["--report"]).write_text(json.dumps({
        "status": "failed", "modules": 3, "batches": [{"logos_checked": 2}, {"logos_checked": 5}],
        "total_seconds": 1.25, "declined": [
            {"declaration": "A", "reason": "unsupported", "detail": "ignored"},
            {"declaration": "B", "reason": None, "detail": "detail fallback"},
        ],
        "problems": ["fixture problem"],
    }))
sys.exit(9)
""")
        env = {
            "VESPER_LEAN": "/fixture/lean",
            "LOGOS42_DRIVER": "",
            "VESPER_BIN": "",
            "LOGOS_SWEEP_BATCH_JOBS": "",
            "VESPER_BAZEL_EXECUTABLES": "manifest",
        }
        command = ("ci", "logos-sweep", "--", "sealed fixture", "report with spaces.json")
        report = self.aspect(*command, status=9, env=env)
        self.assertIn("3 modules, 7 declarations checked by Logos in 2 batches, 1.25 s", report.stdout)
        self.assertIn("A: unsupported", report.stdout)
        self.assertIn("B: detail fallback", report.stdout)
        self.assertIn("PROBLEM: fixture problem", report.stdout)
        args = json.loads((self.root / "args.json").read_text())
        self.assertEqual((args["--driver"], args["--vesper"], args["--batch-jobs"]),
                         (str(checker), str(vesper), "2"))
        (self.root / "report with spaces.json").unlink()
        no_report = self.aspect(*command, status=9, env={
            **env, "LOGOS42_DRIVER": "custom-checker", "VESPER_BIN": "custom-vesper",
            "LOGOS_SWEEP_BATCH_JOBS": "4", "VESPER_TEST_NO_REPORT": "1",
        })
        args = json.loads((self.root / "args.json").read_text())
        self.assertEqual((args["--driver"], args["--vesper"], args["--batch-jobs"]),
                         ("custom-checker", "custom-vesper", "4"))
        self.assertEqual(no_report.stdout, "")
        missing = self.aspect("ci", "logos-sweep", "--", "only one arg", status=2, env=env)
        self.assertIn("SEALED_ROOT REPORT", missing.stderr)

    def test_logos_workflow_replays_with_the_pinned_tools_and_retains_the_report(self):
        self.write(".ci-lean-path", "/installed lean/bin/lean\r\n")
        self.write(".ci-logos-cargo-path", "/pinned cargo/bin/cargo\n")
        self.write("tools/logos42/setup.py", """\
import json
import os
from pathlib import Path
import sys

args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
Path("setup.json").write_text(json.dumps(args))
if os.environ.get("SETUP_FAILURE"):
    sys.exit(17)
Path(args["--directory"]).mkdir()
""")
        self.write("tools/logos-sweep/sweep.py", """\
import json
from pathlib import Path
import sys

args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
assert Path(args["--root"]).is_dir()
Path("sweep.json").write_text(json.dumps(args))
Path(args["--report"]).write_text(json.dumps({
    "status": "failed", "modules": 3, "batches": [], "total_seconds": 1,
    "declined": [], "problems": ["fixture problem"],
}))
sys.exit(9)
""")
        command = ("ci", "replay-logos-models")
        no_temp = self.aspect(*command, env={"RUNNER_TEMP": ""}, status=2)
        self.assertIn("RUNNER_TEMP is required", no_temp.stderr)
        env = {"RUNNER_TEMP": "runner scratch", "LOGOS_SWEEP_BATCH_JOBS": "99"}
        self.aspect(*command, env={**env, "SETUP_FAILURE": "1"}, status=17)
        self.assertFalse((self.root / "sweep.json").exists())
        result = self.aspect(*command, env=env, status=9)
        temp = self.root / "runner scratch"
        checker = temp / "logos42/checker"
        driver = self.root / "target/debug/logos42"
        report = temp / "logos-sweep-report.json"
        self.assertEqual(json.loads((self.root / "setup.json").read_text()), {
            "--directory": str(checker), "--driver": str(driver),
            "--lean": "/installed lean/bin/lean", "--lake": "/installed lean/bin/lake",
            "--cargo": "/pinned cargo/bin/cargo", "--git": shutil.which("git"),
        })
        self.assertEqual(json.loads((self.root / "sweep.json").read_text()), {
            "--root": str(checker), "--driver": str(driver),
            "--vesper": str(self.root / "target/debug/vesper"), "--lean": "/installed lean/bin/lean",
            "--batch-jobs": "2", "--report": str(report),
        })
        self.assertIn("PROBLEM: fixture problem", result.stdout)
        self.assertTrue(report.is_file())
        self.write(".ci-logos-cargo-path", "\n")
        missing_cargo = self.aspect(*command, env=env, status=1)
        self.assertIn("The CI Lean and Cargo executable paths are required", missing_cargo.stderr)

    def test_native_port_check_requires_expected_failure_and_passing_control(self):
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import os
import sys

args = sys.argv[1:]
mode = os.environ["VESPER_TEST_PORT_MODE"]
assert args[:2] == ["test", "--locked"]
with open("port.log", "a") as log:
    print("|".join(args), file=log)
if args == ["test", "--locked", "-p", "vesper-simpulse", "-p", "vesper-simpulse-seed"]:
    sys.exit(37 if mode == "stable-failed" else 0)
assert args == ["test", "--locked", "-p", "vesper-simpulse", "--features",
                "vesper-port-divergent", "--test", "kernel", "--", "--test-threads=1"]
if mode != "missing-failure":
    print("test each_fixed_runnable_task_runs_once_per_round ... FAILED")
if mode != "missing-control":
    print("test virtual_clock_only_jumps_when_nothing_can_run ... ok", file=sys.stderr)
sys.exit(0 if mode == "divergent-passed" else 101)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        log = self.root / "port.log"
        for mode, code, calls in (
            ("normal", 0, 2),
            ("stable-failed", 37, 1),
            ("missing-failure", 1, 2),
            ("missing-control", 1, 2),
            ("divergent-passed", 1, 2),
        ):
            with self.subTest(mode=mode):
                log.unlink(missing_ok=True)
                result = self.aspect("crate", "simpulse", "check", status=code,
                                     env={**env, "VESPER_TEST_PORT_MODE": mode})
                self.assertEqual(len(log.read_text().splitlines()), calls)
                if code == 0:
                    self.assertIn("Detected divergent simpulse port (lib.rs)", result.stdout)
                elif code == 1:
                    self.assertIn("Divergence check did not fail as expected: simpulse port (lib.rs)",
                                  result.stderr)

        self.copy_fixture("scripts/port-harness-plan.json")
        log.unlink()
        result = self.aspect("acceptance", "test-port-harness", env={
            **env,
            "VESPER_TEST_PORT_MODE": "normal",
            "VESPER_PORT_COMPILER": "fixture-compiler",
            "VESPER_PORT_HARNESS_SCOPE": "all",
            "VESPER_PORT_HARNESS_SHARD": "3",
            "VESPER_PORT_HARNESS_CRATES": "vesper-simpulse",
        })
        self.assertEqual(len(log.read_text().splitlines()), 2)
        self.assertIn("Detected divergent simpulse port (lib.rs)", result.stdout)

    def test_indexed_port_checks_scope_mutation_to_each_cargo_child(self):
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import os
import stat
import sys

args = sys.argv[1:]
key = "VESPER_SILICON_DIVERGENT"
if args == ["test", "--locked", "-p", "vesper-silicon", "-p", "vesper-silicon-seed"]:
    assert os.environ.get(key) is None
    sys.exit(0)
index = int(os.environ[key])
cases = [
    ("parity", "imsic_files_match", "every_fixture_parses_identically"),
    ("parity", "aplic_domains_match", "imsic_files_match"),
    ("parity", "interrupt_fabrics_match", "imsic_files_match"),
    ("dtb", "qemu_riscv_virt_memory_harts_and_plic", "malformed_headers_are_typed_errors"),
]
suite, bad, good = cases[index]
assert args == ["test", "--locked", "-p", "vesper-silicon", "--features", "vesper-port-divergent",
                "--test", suite, "--", "--test-threads=1"]
assert stat.S_ISCHR(os.fstat(0).st_mode)
with open("port.log", "a") as log:
    print(index, file=log)
print(f"test {bad} ... FAILED")
print(f"test {good} ... ok")
sys.exit(101)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        result = self.aspect("crate", "silicon", "check", env=env)
        self.assertEqual((self.root / "port.log").read_text().splitlines(), list("0123"))
        for area in ("imsic", "aplic", "plic", "dtb"):
            self.assertIn(f"Detected divergent silicon port ({area})", result.stdout)

    def test_port_divergence_can_require_a_line_prefix(self):
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import sys

args = sys.argv[1:]
if "--features" in args and "vesper-port" in args:
    sys.exit(0)
if "--lib" in args:
    print("test model::link::tests::micro_burst_builds_queue_delay_then_tail_drops ... FAILED")
else:
    print("unexpected prefix before warp_wan: transcript differs")
sys.exit(101)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        result = self.aspect("crate", "sim-session", "test-port", status=1, env=env)
        self.assertEqual(result.stdout, "Detected divergent sim-session port (link)\n")
        self.assertIn("unexpected prefix before warp_wan:", result.stderr)

    def test_ignis_discovers_only_the_requested_test_artifact(self):
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
suite_names = ["planner", "multi_backend", "lifecycle"]
if "--no-run" in args:
    suite = args[args.index("--test") + 1]
    assert suite in suite_names
    assert "--message-format=json-render-diagnostics" in args
    assert "vesper-port-divergent" in args
    artifact = {
        "reason": "compiler-artifact", "executable": "/path with spaces/" + suite,
        "target": {"kind": ["test"], "name": suite},
    }
    print("null")
    print(json.dumps({**artifact, "target": {"kind": ["bin"], "name": suite}}))
    print(json.dumps({**artifact, "target": {"kind": ["test"], "name": "unrelated"}}))
    if os.environ.get("VESPER_TEST_ARTIFACT_MODE") != "missing":
        print(json.dumps(artifact))
    if os.environ.get("VESPER_TEST_ARTIFACT_MODE") == "duplicate":
        print(json.dumps(artifact))
elif "--test" in args:
    assert args[-4:] == ["--test", "divergence", "--", "--nocapture"]
    for suite in suite_names:
        assert os.environ["VESPER_IGNIS_DIVERGENT_" + suite.upper()] == "/path with spaces/" + suite
    Path("checked").touch()
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        self.aspect("crate", "ignis", "check", env=env)
        self.assertTrue((self.root / "checked").is_file())
        for mode, message in (("duplicate", "multiple executables"), ("missing", "every selected test suite")):
            with self.subTest(mode=mode):
                (self.root / "checked").unlink(missing_ok=True)
                result = self.aspect("crate", "ignis", "check", env={**env, "VESPER_TEST_ARTIFACT_MODE": mode},
                                     status=1)
                self.assertIn(message, result.stderr)
                self.assertFalse((self.root / "checked").exists())

    def test_resonance_checks_both_architectures_and_the_verifier_control(self):
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import sys

args = sys.argv[1:]
if "vesper-port-divergent" not in args:
    sys.exit(0)
for bad, good in (
    ("bank_render_matches_reference_with_events", "host_reports_its_paths"),
    ("malformed_event_streams_are_typed_errors", "emergency_silence_zeroes_the_rest_and_drops_are_counted"),
    ("lease_must_be_fully_written_to_commit", "emergency_silence_zeroes_the_rest_and_drops_are_counted"),
    ("parallel_branches_get_compensation_delays", "buffer_liveness_reuses_buffers"),
    ("graph_outputs_match_the_rust_build", "events_and_ramps_match_the_rust_build"),
    ("delay_slab_read_write_wraps_and_clones_cleanly", "clones_are_realigned_and_hold_the_same_samples"),
    ("tick_is_bit_identical_over_filter_range", "host_reports_its_paths"),
    ("allocation_two_calls_deep_is_rejected_with_its_path", "loops_follow_the_policy"),
):
    print(f"test {bad} ... FAILED")
    print(f"test {good} ... ok")
sys.exit(101)
""", executable=True)
        compiler = self.write("bin/vesper", """\
#!/usr/bin/env python3
import os
import sys

args = sys.argv[1:]
mode = os.environ["VESPER_TEST_REALTIME_MODE"]
entry = args[args.index("--entry") + 1]
policy = args[args.index("--verify-policy") + 1]
assert "--verify-realtime" in args
with open("audio.log", "a") as out:
    if entry == "rt_audio":
        assert policy.endswith("/realtime-policy.json")
        print(args[args.index("--target") + 1], file=out)
    else:
        assert entry == "compile_schedule" and policy.endswith("/realtime-control.json")
        print("control", file=out)
if entry == "rt_audio":
    if mode == "unexpected":
        print("error[realtime/rt-unknown-callee]", file=sys.stderr)
        sys.exit(1)
    if mode == "crash":
        sys.exit(99)
    if mode == "no-findings":
        sys.exit(0)
    if mode == "panic":
        print("thread panicked", file=sys.stderr)
    if mode == "bare-error":
        print("error: could not verify", file=sys.stderr)
    if mode == "no-artifact":
        print("object was not emitted", file=sys.stderr)
    print("error[realtime/rt-unbounded-loop]", file=sys.stderr)
    print("1 native verifier finding(s)", file=sys.stderr)
elif mode != "missing-control":
    print("error[realtime/rt-unknown-callee]", file=sys.stderr)
sys.exit(1)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
               "VESPER_PORT_COMPILER": str(compiler)}
        log = self.root / "audio.log"
        for mode, status, calls in (("normal", 0, 3), ("no-findings", 0, 3), ("unexpected", 1, 1),
                                    ("crash", 1, 1), ("panic", 1, 1), ("bare-error", 1, 1),
                                    ("no-artifact", 1, 1), ("missing-control", 1, 3)):
            with self.subTest(mode=mode):
                log.unlink(missing_ok=True)
                result = self.aspect("crate", "resonance", "test-port", status=status,
                                     env={**env, "VESPER_TEST_REALTIME_MODE": mode})
                sequence = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "control"]
                self.assertEqual(log.read_text().splitlines(), sequence[:calls])
                if status == 0:
                    self.assertIn("accepted both audio targets", result.stdout)
                else:
                    self.assertIn("verifier", result.stderr)

    def test_native_harness_routes_all_shards_and_stops_at_failure(self):
        path = "scripts/port-harness-plan.json"
        self.copy_fixture(path)
        self.copy_fixture("scripts/port-abi-plan.json")
        plan = json.loads((self.root / path).read_text())
        for module in ("port-checks.axl", "port-special-checks.axl"):
            (self.root / ".aspect" / module).unlink()
        fixtures = """
def fixture(crate, kind):
    def run(ctx):
        env = ctx.std.env
        ctx.std.io.stdout.write(crate + " " + kind + "|" + env.var("VESPER_PORT_COMPILER") + "\\n")
        if env.var("VESPER_TEST_FAIL_PORT") == crate:
            return 27
        return 0
    return task(kind = kind, group = ["crate", crate], implementation = run)
"""
        # The mock tasks expose their own names and compiler paths to the parent.
        for shard in plan:
            for suite in shard:
                _, crate, kind = suite["task"]
                name = crate.replace("-", "_") + "_" + kind.replace("-", "_")
                fixtures += f'\n{name} = fixture("{crate}", "{kind}")'
        self.write(".aspect/dispatch-fixture.axl", fixtures)
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
from pathlib import Path
import sys

args = sys.argv[1:]
if args == ["build", "--locked", "--bin", "vesper"]:
    Path("compiler-built").touch()
    sys.exit(0)
plan = json.loads(Path("scripts/port-abi-plan.json").read_text())
for group in plan:
    for check in group["checks"]:
        if args == ["test", "--locked", *check["args"]]:
            print("abi|" + " ".join(args))
            if "description" not in check:
                sys.exit(0)
            for name in check.get("failed", []):
                print(f"test {name} ... FAILED")
            for name in check.get("passed", []):
                print(f"test {name} ... ok")
            if "diagnostic" in check:
                print(check["diagnostic"])
            sys.exit(101)
sys.exit(99)
""", executable=True)
        env = {
            "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
            "VESPER_PORT_COMPILER": "explicit compiler",
            "VESPER_PORT_HARNESS_SCOPE": "all",
            "VESPER_PORT_HARNESS_SHARD": "",
            "VESPER_PORT_HARNESS_CRATES": "all",
        }
        command = ("acceptance", "test-port-harness")

        def ports(result):
            return [line.split("|")[0] for line in result.stdout.splitlines() if "|explicit compiler" in line]

        def selected(shard):
            return [" ".join(suite["task"][1:]) for suite in shard]

        all_ports = self.aspect(*command, env=env)
        self.assertEqual(ports(all_ports), [item for shard in plan for item in selected(shard)])
        self.assertIn("abi|test --locked", all_ports.stdout)
        self.assertLess(all_ports.stdout.index("abi|test --locked"), all_ports.stdout.index("beacon check|"))
        self.assertFalse((self.root / "compiler-built").exists())
        for index, shard in enumerate(plan, 1):
            with self.subTest(shard=index):
                result = self.aspect(*command, env={**env, "VESPER_PORT_HARNESS_SHARD": str(index)})
                self.assertEqual(ports(result), selected(shard))
                self.assertEqual("abi|test --locked" in result.stdout, index == 1)
        filtered = self.aspect(*command, env={**env, "VESPER_PORT_HARNESS_CRATES": "vesper-beacon-arrow vesper-vault"})
        self.assertEqual(ports(filtered), ["beacon check", "beacon-arrow test-port", "vault check"])
        empty = self.aspect(*command, env={**env, "VESPER_PORT_HARNESS_CRATES": "all vesper-vault"})
        self.assertEqual(ports(empty), ["vault check"])
        abi_only = self.aspect(*command, env={**env, "VESPER_PORT_HARNESS_SCOPE": "abi"})
        self.assertIn("export ABI tier done", abi_only.stdout)
        self.assertFalse(ports(abi_only))
        failed = self.aspect(*command, status=27, env={**env, "VESPER_TEST_FAIL_PORT": "prism"})
        self.assertEqual(ports(failed), ["beacon check", "prism test-port"])
        for config, error in (
            ({"VESPER_PORT_HARNESS_SHARD": "01"}, "must be 1 to 7"),
            ({"VESPER_PORT_HARNESS_SHARD": "8"}, "must be 1 to 7"),
            ({"VESPER_PORT_HARNESS_SCOPE": "abi", "VESPER_PORT_HARNESS_SHARD": "1"}, "abi takes no shard"),
        ):
            result = self.aspect(*command, status=1, env={**env, **config})
            self.assertIn(error, result.stderr)
            self.assertFalse(ports(result))
            self.assertNotIn("abi|test --locked", result.stdout)
        built = self.aspect(*command, env={
            **env,
            "VESPER_PORT_COMPILER": "",
            "CARGO_TARGET_DIR": "build directory",
            "VESPER_PORT_HARNESS_CRATES": "vesper-vault",
        })
        self.assertTrue((self.root / "compiler-built").exists())
        self.assertIn("vault check|" + str(self.root / "build directory/debug/vesper"), built.stdout)

    def test_native_simd_oracle_preserves_the_binary_corpus_and_checks_tools(self):
        tool = self.write("bin/tool", """\
#!/usr/bin/env python3
import hashlib
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
root = Path(os.environ["VESPER_TEST_FIXTURE_ROOT"])
args = sys.argv[1:]
with (root / "tool-order").open("a") as log:
    print(name, file=log)
if name == "emit":
    assert len(args) == 1
    scratch = Path(args[0])
    (root / "scratch").write_text(str(scratch))
    (scratch / "aarch64.o").write_bytes(b"scanner object")
elif name == "assemble":
    assert args == ["start.s", "-o", "start.o"]
    assert hashlib.sha256(Path("start.s").read_bytes()).hexdigest() == (
        "b06a1ecbd3dba494ec4c384a85ff0ae7fef456f00bfb361afd6803967db90517")
    Path("start.o").write_bytes(b"start object")
elif name == "link":
    assert args[:2] == ["-z", "noexecstack"] and args[4] == "-o"
    assert Path(args[2]).name == "start.o" and Path(args[3]).name == "aarch64.o"
    assert Path(args[2]).read_bytes() == b"start object"
    Path(args[5]).write_bytes(b"scanner")
else:
    assert name == "emulate" and len(args) == 1 and Path(args[0]).read_bytes() == b"scanner"
    scratch = Path(args[0]).parent
    for file, length, digest in [
        ("input.bin", 131120, "9675030ae6483c3ae9c2e1a56a93bb7ac7c04b01c22c82a5c47ace1b27be6cf8"),
        ("cases.bin", 384000, "8f3f9664f5e5d4e18c981289c879cc3d82c4efbf3108c13a8104fa0e84ad725f"),
    ]:
        data = (scratch / file).read_bytes()
        assert len(data) == length and hashlib.sha256(data).hexdigest() == digest, file
    sys.exit(int(os.environ["VESPER_TEST_QEMU_STATUS"]))
""", executable=True)
        paths = {}
        for name in ("emit", "assemble", "link", "emulate"):
            paths[name] = self.root / "bin" / name
            paths[name].symlink_to(tool)
        self.write("manifest", "//crates/vesper-simd:example/emit_objects bin/emit\n")
        env = {
            "VESPER_TEST_FIXTURE_ROOT": str(self.root),
            "VESPER_BAZEL_EXECUTABLES": "manifest",
            "BAZEL": "/missing-bazel",
            "AARCH64_AS": str(paths["assemble"]),
            "AARCH64_LD": str(paths["link"]),
            "QEMU_AARCH64": str(paths["emulate"]),
        }
        for status in (0, 7):
            with self.subTest(status=status):
                order = self.root / "tool-order"
                order.unlink(missing_ok=True)
                result = self.aspect("acceptance", "test-simd-aarch64", status=status,
                                     env={**env, "VESPER_TEST_QEMU_STATUS": str(status)})
                self.assertEqual(order.read_text().splitlines(), ["emit", "assemble", "link", "emulate"])
                self.assertFalse(Path((self.root / "scratch").read_text()).exists())
                self.assertEqual("9600 scalar-reference comparisons passed" in result.stdout, status == 0)

    def test_platform_plan_binds_the_checkout_and_preserves_exact_git_paths(self):
        def git(*arguments):
            return subprocess.check_output(["git", *arguments], cwd=self.root, text=True, stderr=subprocess.DEVNULL).strip()

        git("init", "-q", "-b", "main")
        git("config", "user.name", "Fixture")
        git("config", "user.email", "fixture@example.invalid")
        git("config", "commit.gpgsign", "false")
        self.write("scripts/ci/platform_test_profiles.json", json.dumps({
            "version": 1, "shared_paths": ["Cargo.*"],
            "groups": {"frontend": {"paths": ["src/native*"]}, "proof": {"paths": ["lean/**"]}},
            "deferred_targets": [{"package": "vesper", "target": "suite", "bazel": "//:test/suite",
                                  "group": "proof", "source": "tests/suite.rs", "skip_tests": ["slow"]}],
        }))
        self.write("tests/suite.rs", "#[test]\n#[cfg(unix)]\nfn slow() {}\n#[test]\nfn fast() {}\n")
        git("add", ".")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        command = ("ci", "platform-test-plan", "--", "plan")
        empty = json.loads(self.aspect(*command, "--profile=pr", "--base", base, "--head", base).stdout)
        self.assertEqual(empty["groups"], [])
        self.assertEqual(empty["changed_paths"], [])
        branch_path = "src/native part λ\nodd.rs"
        git("switch", "-qc", "feature")
        self.write(branch_path, "fixture\n")
        git("add", "src")
        git("commit", "-qm", "feature")
        feature = git("rev-parse", "HEAD")
        body = json.loads(self.aspect(*command, "--profile", "pr", "--head", feature, "--base", base).stdout)
        self.assertEqual(body, {"profile": "pr", "groups": ["frontend"], "sha": feature,
                                "reason": "groups selected from changed paths", "changed_paths": [branch_path]})
        for unavailable in ("", "f" * 40, "bad-ref"):
            result = json.loads(self.aspect(*command, "--profile", "pr", "--head", feature, "--base", unavailable).stdout)
            self.assertEqual(result["groups"], ["frontend", "proof"])
            self.assertEqual(result["reason"], "base unavailable; full coverage selected")
        invalid = self.aspect(*command, "--profile", "full", "--head", "HEAD", status=2)
        self.assertIn("full 40-hex SHA", invalid.stderr)
        unavailable = self.aspect(*command, "--profile", "full", "--head", "f" * 40, status=2)
        self.assertIn("commit unavailable", unavailable.stderr)
        wrong_sha = self.aspect(*command, "--profile", "full", "--head", base, status=2)
        self.assertIn("checked-out commit", wrong_sha.stderr)
        self.aspect(*command, "--profile", "quick", "--head", feature, status=2)
        self.aspect(*command, "--profile", "full", "--head", status=2)
        self.aspect("ci", "verify-platform-dispatch", env={"PR_HEAD": feature})
        self.aspect("ci", "verify-platform-checkout", env={"EXPECTED_SHA": feature})
        self.aspect("ci", "verify-platform-checkout", env={"EXPECTED_SHA": base}, status=2)
        self.aspect("ci", "verify-platform-checkout", env={"EXPECTED_SHA": ""}, status=2)
        rejected = self.aspect("ci", "verify-platform-dispatch", env={"PR_HEAD": base}, status=2)
        self.assertIn("neither the dispatched PR head nor its test merge", rejected.stderr)
        self.aspect("ci", "verify-platform-dispatch", env={"PR_HEAD": ""}, status=2)
        git("switch", "-q", "main")
        git("merge", "--no-ff", "-qm", "test merge", "feature")
        self.aspect("ci", "verify-platform-dispatch", env={"PR_HEAD": feature})
        head = git("rev-parse", "HEAD")
        github = self.write("github output", "earlier=preserved\n")
        output = self.root / "runner temp"
        result = self.aspect("ci", "plan-platform-workflow", env={
            "GITHUB_OUTPUT": str(github), "RUNNER_TEMP": str(output), "REQUESTED_PROFILE": "pr", "BASE_SHA": base,
        })
        self.assertEqual(json.loads(result.stdout), json.loads((output / "platform-plan.json").read_text()))
        self.assertEqual(json.loads(result.stdout)["sha"], head)
        self.assertEqual(github.read_text(), f'earlier=preserved\nprofile=pr\ngroups=["frontend"]\nsha={head}\n')
        self.aspect("ci", "plan-platform-workflow", env={"GITHUB_OUTPUT": "", "RUNNER_TEMP": str(output)}, status=2)

        git("switch", "--orphan", "unrelated")
        git("commit", "--allow-empty", "-qm", "unrelated root")
        unrelated = git("rev-parse", "HEAD")
        git("switch", "-q", "main")
        result = json.loads(self.aspect(*command, "--profile", "pr", "--head", head, "--base", unrelated).stdout)
        self.assertEqual(result["reason"], "diff unavailable; full coverage selected")
        self.assertEqual(result["groups"], ["frontend", "proof"])

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable")
    def test_windows_platform_checkout_checks_the_exact_commit_from_any_directory(self):
        script = "scripts/windows/verify-platform-checkout.ps1"
        self.copy_fixture(script)
        def git(*args):
            return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()
        git("init", "-q")
        git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-qm", "base")
        head = git("rev-parse", "HEAD")
        self.powershell(script, env={"EXPECTED_SHA": head})
        mismatch = self.powershell(script, env={"EXPECTED_SHA": "f" * 40}, status=2)
        self.assertIn(f"found {head}", mismatch.stderr)
        self.powershell(script, env={"EXPECTED_SHA": ""}, status=2)

    def test_platform_plan_refuses_invalid_or_stale_catalog_entries(self):
        git = lambda *args: subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()
        git("init", "-q")
        git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-qm", "base")
        head = git("rev-parse", "HEAD")
        command = ("ci", "platform-test-plan", "--", "--profile", "full", "--head", head)
        source = self.write("tests/suite.rs", "#[test]\nfn slow() {}\n#[test]\nfn fast() {}\n")
        good = {"package": "vesper", "target": "suite", "bazel": "//:test/suite", "group": "proof",
                "source": "tests/suite.rs", "skip_tests": ["slow"]}
        policy = {"version": 1, "shared_paths": ["Cargo.lock"], "groups": {"proof": {"paths": ["lean/**"]}},
                  "deferred_targets": [good]}
        def reject(message, **overrides):
            self.write("scripts/ci/platform_test_profiles.json", json.dumps({**policy, **overrides}))
            output = self.aspect(*command, status=2)
            self.assertEqual(output.stdout, "")
            self.assertIn(message, output.stderr)
        reject("unsupported platform test policy version", version=2)
        reject("policy needs named escalation groups", groups={})
        reject("invalid paths for proof", groups={"proof": {"paths": [""]}})
        reject("invalid shared paths", shared_paths=[1])
        reject("invalid deferred-target record", deferred_targets=[{}])
        reject("unknown escalation group", deferred_targets=[{**good, "group": "missing"}])
        reject("selectors disagree", deferred_targets=[{**good, "bazel": "//:test/other"}])
        reject("duplicate deferred-target selector", deferred_targets=[good, good])
        reject("invalid exact test-case selectors", deferred_targets=[{**good, "skip_tests": ["slow", "slow"]}])
        reject("stale test cases", deferred_targets=[{**good, "skip_tests": ["renamed"]}])
        reject("partial exclusion leaves no PR tests", deferred_targets=[{**good, "skip_tests": ["slow", "fast"]}])
        source.unlink()
        reject("catalog source missing")

    def test_native_port_selection_handles_diff_failures_renames_and_workflow_output(self):
        def git(*arguments):
            return subprocess.check_output(["git", *arguments], cwd=self.root, text=True).strip()

        git("init", "-q", "-b", "main")
        git("config", "user.name", "Fixture")
        git("config", "user.email", "fixture@example.invalid")
        git("config", "commit.gpgsign", "false")
        self.write("crates/source/file with spaces λ.rs", "fixture\n")
        git("add", ".")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
from pathlib import Path
import sys
assert sys.argv[1:] == ["metadata", "--locked", "--no-deps", "--format-version", "1"]
print(Path("metadata.json").read_text())
""", executable=True)
        env = {"PATH": os.pathsep.join([str(cargo.parent), os.environ["PATH"]])}
        def package(name, path, dependencies=(), targets=()):
            return {"name": name, "manifest_path": str(self.root / path / "Cargo.toml"),
                    "dependencies": [{"path": str(self.root / directory), "kind": kind}
                                     for directory, kind in dependencies],
                    "targets": [{"src_path": str(self.root / file), "kind": [kind]} for file, kind in targets]}
        self.write("metadata.json", json.dumps({"workspace_root": str(self.root), "packages": [
            package("compiler", "", [("crates/seed", "build"), ("crates/source", "dev")]),
            package("source", "crates/source"),
            package("seed", "crates/seed", targets=[("crates/seed/../linked/src/lib.rs", "lib")]),
            package("linked", "crates/linked"),
            package("middle", "crates/middle", [("crates/source", "dev")]),
            package("last", "crates/last", [("crates/middle", "build")]),
        ]}))
        command = ("ci", "port-harness-selection", "--")
        for flags in (("--profile", "full"), ("--profile=pr", "--event=push"), ("--profile", "pr")):
            self.assertEqual(self.aspect(*command, *flags, "--head", "HEAD", env=env).stdout, "all\n")
        scoped = (*command, "--profile", "pr", "--base", base, "--head", "HEAD")
        self.assertEqual(self.aspect(*scoped, env=env).stdout, "all\n")
        self.assertEqual(self.aspect(*command, "--profile", "pr", "--base", "missing", "--head", "HEAD", env=env).stdout, "all\n")
        self.aspect(*command, "--profile", "invalid", "--head", "HEAD", env=env, status=2)
        self.aspect(*command, "--profile", "--head", "HEAD", env=env, status=2)

        self.write("crates/source/file with spaces λ.rs", "changed\n")
        git("commit", "-qam", "change a port")
        self.assertEqual(self.aspect(*scoped, env=env).stdout, "last middle source\n")
        output = self.write("step-output", "earlier=record\n")
        workflow_env = {**env, "GITHUB_OUTPUT": str(output), "PULL_REQUEST": "", "BASE_SHA": base,
                        "REQUESTED_PROFILE": "pr", "EVENT": "pull_request"}
        self.aspect("ci", "choose-platform-ports", env=workflow_env)
        self.aspect("ci", "choose-platform-ports", env={**workflow_env, "PULL_REQUEST": "42"})
        self.assertEqual(output.read_text(), "earlier=record\ncrates=last middle source\ncrates=none\n")
        self.aspect("ci", "choose-platform-ports", env={**workflow_env, "GITHUB_OUTPUT": ""}, status=2)
        self.aspect("ci", "choose-platform-ports", env={**workflow_env, "GITHUB_OUTPUT": str(output / "unwritable")}, status=1)
        self.aspect("ci", "choose-platform-ports", env={**workflow_env, "REQUESTED_PROFILE": "other"}, status=2)

        self.write("crates/linked/src/lib.rs", "linked library\n")
        git("add", "crates/linked")
        git("commit", "-qm", "change a compiler-linked file")
        self.assertEqual(self.aspect(*scoped, env=env).stdout, "all\n")
        git("reset", "--hard", base)
        (self.root / "docs").mkdir()
        git("mv", "crates/source/file with spaces λ.rs", "docs/renamed.md")
        git("commit", "-qm", "move a port file to documentation")
        self.assertEqual(self.aspect(*scoped, env=env).stdout, "last middle source\n")

    def test_blake3_generator_preserves_both_source_forms(self):
        command = ("maintenance", "gen-blake3", "--")
        self.aspect(*command, "rounds output.ves")
        committed = ROOT / "packages/vesper-std/hash/src/compress.ves"
        self.assertEqual((self.nested / "rounds output.ves").read_text(), committed.read_text())
        straight = self.nested / "straight output.ves"
        self.aspect(*command, str(straight), "--straight-line")
        self.assertEqual(hashlib.sha256(straight.read_bytes()).hexdigest(),
                         "f476483c6221f76bc39a7cb07084800277c0a3205c4231af8b4d58d4a66c005c")
        self.aspect(*command, status=2)
        self.aspect(*command, str(straight), "--unsupported", status=2)

    def test_serde_lean_generator_matches_the_committed_model(self):
        model = ROOT / "crates/vesper-serde/lean/SimdJsonQuotes.lean"
        result = self.aspect("crate", "serde", "gen-lean-quotes")
        self.assertEqual(result.stdout.encode("utf-8"), model.read_bytes())

    def test_default_cli_dependency_check_reports_exact_forbidden_packages(self):
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys
assert sys.argv[1:] == ["tree", "--locked", "-p", "vesper", "--edges", "normal", "--prefix", "none"]
assert Path(".aspect").is_dir()
print(os.environ["FIXTURE_CARGO_TREE"])
if os.environ.get("FIXTURE_CARGO_FAILURE"):
    print("failed to load dependency graph", file=sys.stderr)
    sys.exit(29)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(cargo.parent), os.environ["PATH"]])}
        command = ("ci", "check-default-cli-dependencies")
        allowed = "\nvesper v0.1.0 (path with wasmtime)\n wasmtime-adapter v1.0.0\n"
        self.aspect(*command, env={**env, "FIXTURE_CARGO_TREE": allowed})
        for package in ("wasmtime", "wasmtime-wasi", "vesper-loom-sandbox"):
            result = self.aspect(*command, env={**env, "FIXTURE_CARGO_TREE": allowed + f"{package} v1.0.0\n"}, status=1)
            self.assertIn("forbidden normal dependencies: " + package, result.stderr)
        output = "wasmtime v1.0.0\nvesper-loom-sandbox v0.0.1\nwasmtime v2.0.0\n"
        result = self.aspect(*command, env={**env, "FIXTURE_CARGO_TREE": output}, status=1)
        self.assertIn("forbidden normal dependencies: vesper-loom-sandbox, wasmtime\n", result.stderr)
        result = self.aspect(*command, env={**env, "FIXTURE_CARGO_TREE": "", "FIXTURE_CARGO_FAILURE": "1"}, status=29)
        self.assertIn("failed to load dependency graph", result.stderr)

    def test_native_tcp_fixture_matches_committed_bytes_and_reported_packets(self):
        import struct

        path = Path("crates/vesper-warp-tcp/fixtures/tcp_fixtures.pcap")
        (self.root / path.parent).mkdir(parents=True)
        result = self.aspect("crate", "warp-tcp", "gen-fixtures")
        data = (self.root / path).read_bytes()
        self.assertEqual(data, (ROOT / path).read_bytes())
        heading, *packets = result.stdout.splitlines()
        self.assertEqual(heading, f"{self.root / path} 8 packets")
        self.assertEqual(len(packets), 8)
        offset = 24
        for index, reported in enumerate(packets):
            timestamp, micros, captured, length = struct.unpack_from("<4I", data, offset)
            self.assertEqual((timestamp, micros, captured), (1_700_000_000 + index, 0, length))
            offset += 16
            self.assertEqual(bytes.fromhex(reported), data[offset:offset + length])
            offset += length
        self.assertEqual(offset, len(data))

    def test_stage1_compiles_and_reproduces_with_distinct_clean_targets(self):
        self.write("bootstrap/stage1.rs", "fixture")
        tools = self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import shutil
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
with (Path(os.environ["FIXTURE_ROOT"]) / "stage1.log").open("a") as log:
    print(json.dumps([name, args, os.getenv("CARGO_ENCODED_RUSTFLAGS"), os.getenv("SOURCE_DATE_EPOCH")]), file=log)
if name == "cargo":
    assert args[:4] == ["build", "--locked", "--profile", "dev"]
    assert args[6:] == ["--lib", "-p", "vesper", "-p", "vesper-codegen-verified"]
    target = Path(args[5])
    assert not (target / "stale").exists()
    for file in ("debug/libvesper.rlib", "debug/libvesper_codegen_verified.rlib",
                 "debug/deps/foo.rmeta", "debug/deps/lib.so", "debug/build/pkg/out/libbar.rlib",
                 "debug/apple/lib.dylib", "debug/windows/lib.dll",
                 "debug/unrelated/not-an-artifact.txt"):
        artifact = target / file
        artifact.parent.mkdir(parents=True, exist_ok=True)
        artifact.touch()
elif name == "rustc":
    if args == ["-vV"]:
        print("rustc fixture")
        print("host: fixture-host")
    else:
        assert args[:3] == ["--edition=2024", "-O", str(Path(sys.argv[0]).parent.parent / "bootstrap/stage1.rs")]
        assert args[3] == "-o"
        shutil.copy2(Path(sys.argv[0]), args[4])
else:
    assert name == "stage1-runner" and args[2] == "fixture-host"
    if args[4:] == ["--record"]:
        lock = Path(args[3])
        mismatch = os.getenv("REPRO_MISMATCH") and "repro-b" in str(lock)
        lock.write_text("changed" if mismatch else "same lock")
    else:
        sys.exit(int(os.getenv("FINAL_RUNNER_STATUS", "0")))
""", executable=True)
        (self.root / "bin/rustc").symlink_to(tools)
        env = {"PATH": os.pathsep.join([str(tools.parent), os.environ["PATH"]]),
               "FIXTURE_ROOT": str(self.root), "WORK": "work scratch", "OUT": "my lock", "PROFILE": "dev",
               "CARGO_HOME": "cargo home", "CARGO_TARGET_DIR": "other target",
               "SOURCE_DATE_EPOCH": "", "CARGO_ENCODED_RUSTFLAGS": ""}
        work = self.root / "work scratch"
        self.write("work scratch/repro-a/target/stale", "")
        command = ("bootstrap", "run", "--", "stage1")
        result = self.aspect(*command, "--reproduce", "--outcome", "--require-cross-backend",
                             status=36, env={**env, "FINAL_RUNNER_STATUS": "36"})
        self.assertIn("two clean builds recorded identical locks", result.stdout)
        log = self.root / "stage1.log"
        rows = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual([row[0] for row in rows],
                         ["rustc", "cargo", "rustc", "stage1-runner", "cargo", "rustc", "stage1-runner", "stage1-runner"])
        for side, cargo, rustc, runner in (("a", *rows[1:4]), ("b", *rows[4:7])):
            directory = work / f"repro-{side}"
            target = directory / "target"
            self.assertFalse(target.exists())
            self.assertEqual(cargo[1][5], str(target))
            self.assertEqual(cargo[2].split("\x1f"), [
                f"--remap-path-prefix={self.root}=/vesper",
                f"--remap-path-prefix={self.root / 'cargo home'}=/cargo",
                f"--remap-path-prefix={target}=/target",
            ])
            self.assertEqual(cargo[3], "1794650795")
            profile = str(target / "debug")
            self.assertEqual(rustc[1][5:], [
                f"--remap-path-prefix={self.root}=/vesper", f"--remap-path-prefix={target}=/target",
                "--extern", f"vesper={profile}/libvesper.rlib",
                "--extern", f"vesper_codegen_verified={profile}/libvesper_codegen_verified.rlib",
                "-L", f"dependency={profile}", "-L", f"dependency={profile}/apple",
                "-L", f"dependency={profile}/build/pkg/out", "-L", f"dependency={profile}/deps",
                "-L", f"dependency={profile}/windows",
            ])
            self.assertEqual(runner[1], [str(self.root), str(directory / "work"), "fixture-host",
                                         str(directory / "stage1.lock"), "--record"])
        self.assertEqual(rows[-1][1], [str(self.root), str(work), "fixture-host", "my lock",
                                       "--outcome", "--require-cross-backend"])
        mismatch = self.aspect(*command, "--reproduce", "--outcome", env={**env, "REPRO_MISMATCH": "1"}, status=1)
        self.assertIn("ReproducibilityMismatch", mismatch.stderr)
        log.unlink()
        self.aspect(*command, "--record", env=env)
        rows = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(rows[1][1][5], str(self.root / "other target"))
        self.assertEqual(rows[-1][1][-1], "--record")
        self.aspect("bootstrap", "compile-stage1", "--", str(self.root / "other target/debug"),
                    str(self.root / "bootstrap/stage1.rs"), "adapter", "--test", env=env)
        self.assertIn("--test", json.loads(log.read_text().splitlines()[-1])[1])
        self.aspect(*command, "--unknown", env=env, status=2)
        self.aspect(*command, env={**env, "PROFILE": "invalid"}, status=2)
        self.aspect("bootstrap", "compile-stage1", "--", "p", "s", "o", "--invalid", env=env, status=2)

    def test_stage0_builds_two_seeds_and_compares_the_paths_via_seed_lock(self):
        tool = self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import shutil
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
root = Path(os.environ["FIXTURE_ROOT"])
with (root / "stage0.log").open("a") as log:
    print(json.dumps([name, args, os.getenv("SOURCE_DATE_EPOCH"), os.getenv("CARGO_ENCODED_RUSTFLAGS")]), file=log)
if name == "cargo":
    if args[0] == "build":
        assert args[1:6] == ["--locked", "--profile", "dev", "--bin", "vesper"]
        target = Path(args[7])
        assert not target.exists()
        output = target / "debug/vesper"
        output.parent.mkdir(parents=True)
        shutil.copy2(sys.argv[0], output)
    else:
        assert args[:2] == ["run", "--quiet"] and "seed-lock" in args
        opts = dict(zip(args[args.index("seed-lock") + 1::2], args[args.index("seed-lock") + 2::2]))
        assert Path(opts["--results-a"]).read_text() == Path(opts["--results-b"]).read_text() == "a 3\\nb 7\\n"
        assert [p.name for p in Path(opts["--objects-a"]).iterdir()] == [p.name for p in Path(opts["--objects-b"]).iterdir()]
        Path(opts["--out"]).write_text(json.dumps(opts))
elif name in ("seed-a", "seed-b"):
    output = Path(args[-1])
    if args[0] == "emit":
        assert args[2:5] == ["--emit", "object", "-o"]
        output.write_text(args[1])
    else:
        assert args[0] == "build"
        shutil.copy2(sys.argv[0], output)
elif name in ("a", "b"):
    sys.exit({"a": 3, "b": 7}[name])
elif name == "lean":
    if args == ["--print-prefix"]:
        print(root / "lean toolchain")
    elif args == ["--version"]:
        print("Lean (version 4.35.0, Release)")
    else:
        assert args[0] == "-c" and args[2] == "BootstrapDdc/Reference.lean"
        assert Path.cwd() == root / "lean"
        Path(args[1]).write_text("C source")
elif name == "gcc":
    if args == ["--version"]:
        print("gcc fixture\\nsecond line")
    else:
        path_b = root / "seed scratch/path-b"
        toolchain = root / "lean toolchain"
        if args[0].startswith("-O"):
            assert args == ["-O2", "-I" + str(toolchain / "include"), "-c",
                            str(path_b / "reference.c"), "-o", str(path_b / "reference.o")]
        else:
            library = str(toolchain / "lib/lean")
            assert args == [str(path_b / "reference.o"), "-L" + library, "-lleanshared",
                            "-Wl,-rpath," + library, "-o", str(path_b / "vesperc-lean-ddc")]
        shutil.copy2(sys.argv[0], args[-1])
elif name == "rustc":
    print("rustc 1.100.0 (fixture)")
else:
    assert name == "vesperc-lean-ddc" and args == ["bootstrap/corpus/a.ves", "bootstrap/corpus/b.ves"]
    print("a 3\\nb 7")
""", executable=True)
        for binary in ("lean", "gcc", "rustc"):
            (tool.parent / binary).symlink_to(tool)
        self.write(".ci-lean-path", str(tool.parent / "lean") + "\n")
        self.write("bootstrap/corpus/b.ves", "b")
        self.write("bootstrap/corpus/a.ves", "a")
        self.write("lean/BootstrapDdc/Reference.lean", "fixture")
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                        "-c", "commit.gpgsign=false", "commit", "-qm", "base", "--allow-empty"],
                       cwd=self.root, check=True)
        env = {"PATH": os.pathsep.join([str(tool.parent), os.environ["PATH"]]),
               "FIXTURE_ROOT": str(self.root), "WORK": "seed scratch", "OUT": "recorded lock",
               "PROFILE": "dev", "SOURCE_DATE_EPOCH": "1717", "CC_B": "gcc", "VESPER_LEAN": ""}
        self.aspect("bootstrap", "run", env=env)
        self.assertFalse((self.root / "seed scratch/target").exists())
        lock = json.loads((self.root / "recorded lock").read_text())
        self.assertEqual({name: lock[name] for name in ("--rustc", "--profile", "--epoch", "--lean", "--cc")}, {
            "--rustc": "rustc 1.100.0", "--profile": "dev", "--epoch": "1717",
            "--lean": "Lean 4.35.0", "--cc": "gcc fixture",
        })
        rows = [json.loads(row) for row in (self.root / "stage0.log").read_text().splitlines()]
        self.assertEqual([row[0] for row in rows], [
            "lean", "cargo", "cargo", "seed-a", "seed-b", "seed-a", "a", "seed-a", "seed-b", "seed-a", "b",
            "lean", "gcc", "gcc", "vesperc-lean-ddc", "rustc", "lean", "gcc", "cargo",
        ])
        self.assertTrue(all(row[2] == "1717" for row in rows))
        self.assertIn(f"--remap-path-prefix={self.root}=/vesper", rows[1][3].split("\x1f"))
        self.aspect("bootstrap", "run", env={**env, "VESPER_LEAN": "missing-lean"}, status=2)
        self.aspect("bootstrap", "run", env={**env, "CC_B": "missing-cc"}, status=2)

    def test_stage1_cross_backend_ratchet_enforces_only_a_strict_baseline(self):
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
import sys
assert sys.argv[1:] == ["build", "--locked", "-p", "vesper-codegen-verified", "--bin", "vesper-bootstrap"]
""", executable=True)
        self.write("custom target/debug/vesper-bootstrap", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys
assert sys.argv[1] == "check-stage1-lock"
with open("gate.log", "a") as log:
    print(sys.argv[2], file=log)
code = "COMMITTED_STATUS" if Path(sys.argv[2]).name == "stage1.lock" else "FRESH_STATUS"
sys.exit(int(os.environ[code]))
""", executable=True)
        env = {"PATH": os.pathsep.join([str(cargo.parent), os.environ["PATH"]]),
               "WORK": "gate scratch", "CARGO_TARGET_DIR": "custom target"}
        for baseline, fresh, status, message in (
            ("0", "39", 39, "the fresh lock must be too"),
            ("1", "0", 0, "passes the strict gate"),
            ("1", "1", 0, "not yet satisfiable"),
        ):
            result = self.aspect("ci", "stage1-cross-backend", status=status,
                                 env={**env, "COMMITTED_STATUS": baseline, "FRESH_STATUS": fresh})
            self.assertIn(message, result.stdout)
        self.assertEqual((self.root / "gate.log").read_text().splitlines(),
                         ["bootstrap/stage1.lock", str(self.root / "gate scratch/stage1.fresh.lock")] * 3)

    def test_stage2_wasmtime_modes_keep_paths_preopens_and_sidecars(self):
        self.check_stage2_modes(lambda *args, **kwargs: self.aspect("bootstrap", "stage2", "--", *args, **kwargs))

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable")
    def test_windows_stage2_preserves_the_same_wasmtime_contract(self):
        script = "scripts/windows/bootstrap-stage2.ps1"
        self.copy_fixture(script)
        self.check_stage2_modes(lambda *args, **kwargs: self.powershell(script, *args, **kwargs))

    def check_stage2_modes(self, run):
        tool = self.write("bin/wt", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
with open("wasmtime.log", "a") as log:
    print(json.dumps(args), file=log)
mode = os.environ.get("TEST_STAGE2_MODE")
if args == ["--version"]:
    print("wasmtime " + os.environ.get("TEST_WASMTIME_VERSION", "50.0.1"))
elif args[0] == "compile":
    assert args[2] == "-o" and not Path(args[3]).exists()
    Path(args[3]).write_bytes(Path(args[1]).read_bytes())
else:
    assert args[0] == "run"
    preopens = [args[index + 1] for index, value in enumerate(args) if value == "--dir"]
    assert preopens[0] == str(Path.cwd().parent) + "::."
    filename = args[-2].removeprefix("/out/")
    output = Path(preopens[1].removesuffix("::/out")) / filename
    suffixes = ["", ".err.txt", ".stage", ".syms"]
    assert all(not Path(str(output) + suffix).exists() for suffix in suffixes)
    if mode == "diagnostic":
        Path(str(output) + ".err.txt").write_text("compiler diagnostic")
    elif mode == "empty":
        Path(str(output) + ".stage").write_text("reached lowering")
        output.touch()
    elif mode == "wasmtime-failure":
        sys.exit(43)
    else:
        output.write_bytes(b"changed" if mode == "different" else Path(args[-4]).read_bytes())
""", executable=True)
        self.write("nested directory/input wasm.wasm", "wasm test bytes")
        env = {"WASMTIME_BIN": "../bin/wt", "WASMTIME": "/unused", "VESPER_STAGE2_SYMS": "",
               "VESPER_ALLOW_UNPINNED_WASMTIME": ""}
        expected = self.nested / "input wasm.cwasm"
        expected.write_text("stale")
        self.assertIn("../bin/wt", run("which", env=env).stdout)
        rejected = run("which", env={**env, "TEST_WASMTIME_VERSION": "50.0.10"}, status=1)
        self.assertIn("Expected wasmtime 50.0.1", rejected.stderr)
        run("which", env={
            **env, "WASMTIME_BIN": "", "WASMTIME": str(tool), "TEST_WASMTIME_VERSION": "50.0.10",
            "VESPER_ALLOW_UNPINNED_WASMTIME": "1",
        })
        compiled = run("precompile", "input wasm.wasm", env=env)
        self.assertEqual(compiled.stdout, "input wasm.cwasm\n")
        self.assertEqual(expected.read_text(), "wasm test bytes")
        output = "output space/stage3.wasm"
        for suffix in ("", ".err.txt", ".stage", ".syms"):
            self.write("nested directory/" + output + suffix, "stale")
        run("run", "--precompiled", "--", str(expected), "package path", output, "custom entry",
            env={**env, "VESPER_STAGE2_SYMS": "2"})
        log = self.nested / "wasmtime.log"
        argv = json.loads(log.read_text().splitlines()[-1])
        self.assertEqual(argv, ["run", "--allow-precompiled", "--dir", f"{self.root}::.", "--dir",
                                f"{self.nested / 'output space'}::/out", "--env", "VESPER_STAGE2_SYMS=2",
                                "--argv0", "stage2", str(expected), "package path", "/out/stage3.wasm", "custom entry"])
        for mode, status, diagnostic in (
            ("diagnostic", 1, "compiler diagnostic"), ("empty", 1, "reached lowering"),
            ("wasmtime-failure", 43, "failed"),
        ):
            with self.subTest(mode=mode):
                result = run("run", str(expected), "package", output, status=status,
                             env={**env, "TEST_STAGE2_MODE": mode})
                self.assertIn(diagnostic, result.stderr)
        for mode, flags in (("verify", []), ("fixed-point", ["--precompile"])):
            result = run(mode, *flags, "input wasm.wasm", "verify scratch", env=env)
            self.assertIn("stage2 == stage3 (15 bytes)", result.stdout)
            args = json.loads(log.read_text().splitlines()[-1])
            self.assertEqual("--allow-precompiled" in args, bool(flags))
            self.assertEqual(args[-3:], ["packages/vesper-stage2", "/out/stage3.wasm", "main"])
            self.assertIn("VESPER_STAGE2_SYMS=1", args)
        mismatch = run("verify", "input wasm.wasm", status=1, env={**env, "TEST_STAGE2_MODE": "different"})
        self.assertIn("fixed-point mismatch", mismatch.stderr)
        for args in ((), ("run", "--unknown"), ("run", "only one"), ("verify",), ("precompile",)):
            run(*args, env=env, status=2)

    def test_stage2_discovers_wasmtime_on_path_with_bazel_and_in_runfiles(self):
        self.check_stage2_discovery(lambda **kwargs: self.aspect("bootstrap", "stage2", "--", "which", **kwargs))

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable")
    def test_windows_stage2_discovers_wasmtime_on_path_with_bazel_and_in_runfiles(self):
        script = "scripts/windows/bootstrap-stage2.ps1"
        self.copy_fixture(script)
        self.check_stage2_discovery(lambda **kwargs: self.powershell(script, "which", **kwargs))

    def check_stage2_discovery(self, find_wasmtime):
        tools = self.root / "bin"
        tools.mkdir()
        for name in ("git", "python3"):
            (tools / name).symlink_to(shutil.which(name))
        env = {"PATH": str(tools), "WASMTIME": "", "WASMTIME_BIN": ""}
        missing = find_wasmtime(env=env, status=1)
        self.assertIn("Could not find Wasmtime", missing.stderr)
        contents = "#!/usr/bin/env python3\nprint('wasmtime 50.0.1')\n"
        wasmtime = self.write("bin/wasmtime", contents, executable=True)
        self.assertEqual(find_wasmtime(env=env).stdout, str(wasmtime) + "\n")
        wasmtime.unlink()
        queried = self.write("built wasmtime/bin", contents, executable=True)
        self.write("bin/bazelisk", """\
#!/usr/bin/env python3
from pathlib import Path
import sys
args = sys.argv[1:]
assert args[-1] == "@wasmtime//:wasmtime"
assert (Path.cwd() / ".aspect").is_dir()
if args[0] == "build":
    print("building")
else:
    assert args == ["cquery", "--output=files", "@wasmtime//:wasmtime"]
    print("built wasmtime/bin")
""", executable=True)
        result = find_wasmtime(env=env)
        self.assertEqual(result.stdout, str(queried) + "\n")
        self.assertIn("building", result.stderr)
        queried.unlink()
        cached = self.write("bazel-bin/external/+wasmtime+wasmtime_test/wasmtime", contents, executable=True)
        self.assertEqual(find_wasmtime(env=env).stdout, str(cached) + "\n")

    def test_ci_wasmtime_installer_checks_archive_before_extraction(self):
        tool = self.write("bin/curl", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import shutil
import sys

args = sys.argv[1:]
name = Path(sys.argv[0]).name
if name == "curl":
    assert args[:3] == ["-fsSL", "https://github.com/bytecodealliance/wasmtime/releases/download/v50.0.1/wasmtime-v50.0.1-x86_64-linux.tar.xz", "-o"]
    Path(args[3]).write_text("archive fixture")
elif name == "sha256sum":
    assert Path(args[0]).read_text() == "archive fixture"
    digest = os.environ.get("TEST_ARCHIVE_DIGEST", "685466455dd42a14774a86c36c8f2e45cbb9bd6d92a850139315df60a75498a5")
    print(digest, args[0])
elif name == "tar":
    assert args[0] == "-xJf" and args[2:4] == ["--strip-components=1", "-C"]
    assert Path(args[1]).parent == Path(args[4])
    Path("extracted").touch()
    shutil.copy2(sys.argv[0], Path(args[4]) / "wasmtime")
else:
    assert name == "wasmtime" and args == ["--version"]
    print("wasmtime " + os.environ.get("TEST_WASMTIME_VERSION", "50.0.1"))
""", executable=True)
        for name in ("sha256sum", "tar"):
            (tool.parent / name).symlink_to(tool)
        stale = self.write("runner temp/wasmtime/wasmtime", "previous good install")
        env = {"PATH": os.pathsep.join([str(tool.parent), os.environ["PATH"]]),
               "RUNNER_TEMP": "runner temp", "VESPER_ALLOW_UNPINNED_WASMTIME": ""}
        command = ("ci", "install-wasmtime")
        self.aspect(*command, env={**env, "RUNNER_TEMP": ""}, status=2)
        bad_hash = self.aspect(*command, env={**env, "TEST_ARCHIVE_DIGEST": "bad"}, status=1)
        self.assertIn("archive checksum mismatch", bad_hash.stderr)
        self.assertFalse((self.root / "extracted").exists())
        self.assertEqual(stale.read_text(), "previous good install")
        self.aspect(*command, env={**env, "TEST_WASMTIME_VERSION": "49.0.0"}, status=1)
        self.assertTrue((self.root / "extracted").exists())
        self.assertEqual(stale.read_text(), "previous good install")
        self.aspect(*command, env=env)
        self.assertTrue(os.access(stale, os.X_OK))
        self.aspect("bootstrap", "stage2", "--", "which", env={**env, "WASMTIME_BIN": str(stale)})

    def test_native_disk_cleanup_preserves_ownership_depth_and_symlink_targets(self):
        outside = self.write("outside/keep", "outside scratch")
        keep = [self.write("vesper-tmp scratch/unrelated/stage1-nested/keep", "nested"),
                self.write("vesper-tmp scratch/keep", "unrelated")]
        if os.geteuid() == 0:
            foreign = self.write("vesper-tmp scratch/vesper-foreign", "foreign")
            os.chown(foreign, 65534, -1)
            keep.append(foreign)
        purge = [self.write(f"vesper-tmp scratch/{name}/artifact", "stale").parent for name in (
            "vesper-check", "stage1-test", "blake3-case", "native-probe", ".tmp fixture",
        )]
        link = self.root / "vesper-tmp scratch/vesper-symlink"
        link.symlink_to(outside.parent, target_is_directory=True)
        find = shutil.which("find")
        self.write("bin/find", f"""\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys
args = sys.argv[1:]
if args[1:7] == ["-mindepth", "1", "-maxdepth", "1", "-user", str(os.getuid())]:
    Path("cleanup-ownership-verified").touch()
os.execv({find!r}, [{find!r}, *args])
""", executable=True)
        self.write("bin/df", """\
#!/usr/bin/env python3
import sys
if sys.argv[1:] == ["-Pk", "."]:
    print("header")
    print("/dev/fake 0 100 10485759 99% /")
else:
    assert sys.argv[1] == "-h" and sys.argv[2] == "." and sys.argv[3].endswith("tmp scratch")
    print("disk output")
    print("disk output")
""", executable=True)
        self.write("bin/bazel", """\
#!/usr/bin/env python3
from pathlib import Path
import sys
with Path("bazel.log").open("a") as log:
    log.write(" ".join(sys.argv[1:]) + "\\n")
if sys.argv[1:] == ["info", "output_base"]:
    print("bazel output directory")
if sys.argv[1:] == ["clean"]:
    sys.exit(23)
""", executable=True)
        self.write("bin/du", "#!/usr/bin/env python3\nimport sys\nprint(repr(sys.argv[1:]))\n", executable=True)
        command = ("ci", "reclaim-disk")
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
               "TMPDIR": "vesper-tmp scratch", "VESPER_CI_MIN_FREE_GB": "9"}
        self.aspect(*command, "--", "invalid", env=env, status=2)
        report = self.aspect(*command, "--", "report", env=env)
        self.assertEqual(report.stdout.count("disk output\n"), 1)
        self.assertIn("bazel output directory", report.stdout)
        self.assertTrue(all(path.exists() for path in purge))
        log = self.root / "bazel.log"
        self.assertEqual(log.read_text().splitlines(), ["info output_base"])
        log.unlink()
        self.aspect(*command, env=env)
        self.assertTrue((self.root / "cleanup-ownership-verified").is_file())
        self.assertTrue(all(path.exists() for path in [outside, *keep]))
        self.assertTrue(all(not path.exists() for path in purge))
        self.assertFalse(link.is_symlink())
        self.assertEqual(log.read_text().splitlines(), ["info output_base", "info output_base"])
        log.unlink()
        result = self.aspect(*command, env={**env, "VESPER_CI_MIN_FREE_GB": ""})
        self.assertIn("9 GB free, below 20 GB", result.stdout)
        self.assertEqual(log.read_text().splitlines(), ["info output_base", "clean", "info output_base"])
        self.assertLess(result.stdout.index("before cleanup"), result.stdout.index("after cleanup"))

    def test_native_disk_watch_and_bazel_lane_preserve_status_and_lean(self):
        self.write(".ci-lean-path", "/actual lean/bin/lean\r\n")
        self.write("bin/df", """\
#!/usr/bin/env python3
import sys
if sys.argv[1] == "-Pk":
    print("header")
    print("/dev/fake 0 500 12582912 0% /")
else:
    print("report")
""", executable=True)
        self.write("bin/sleep", """\
#!/usr/bin/env python3
from pathlib import Path
import sys
assert sys.argv[1:] == ["120"]
if Path("slept").exists():
    sys.exit(22)
Path("slept").touch()
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        watch = self.aspect("ci", "reclaim-disk", "--", "watch", env=env, status=22)
        self.assertRegex(watch.stdout, r"reclaim-disk: \d\d:\d\d:\d\d 12 GB free")
        (self.root / "slept").unlink()
        self.write("bin/bazel", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys
import time
if sys.argv[1] == "test":
    assert sys.argv[2:] == ["--config=ci", "//:target name"]
    assert os.environ["VESPER_LEAN"] == "/actual lean/bin/lean"
    for _ in range(300):
        if Path("slept").exists():
            break
        time.sleep(0.01)
    assert Path("slept").exists()
    print("Bazel test")
    sys.exit(37)
assert sys.argv[1:] == ["info", "output_base"]
sys.exit(1)
""", executable=True)
        self.write("bin/du", "#!/usr/bin/env python3\nraise SystemExit(124)\n", executable=True)
        args = ("ci", "bazel-tests", "--", "--config=ci", "//:target name")
        result = self.aspect(*args, env=env, status=37)
        self.assertLess(result.stdout.index("Bazel test"), result.stdout.index("reclaim-disk: disk use"))
        self.write(".ci-lean-path", "\n")
        self.assertIn("Lean executable path is required", self.aspect(*args, env=env, status=1).stderr)

    def test_format_proof_uses_the_ci_lean_path_when_available(self):
        self.write("scripts/check-format-proof.py", "import os\nprint(os.getenv('VESPER_LEAN', 'missing'))\n")
        self.write("bin/vesper", "")
        env = {"VESPER_FORMAT_PROOF_BINARY": "bin/vesper", "VESPER_LEAN": "",
               "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        (self.root / "bin/python").symlink_to(shutil.which("python3"))
        command = ("acceptance", "test-format-proof")
        self.assertEqual(self.aspect(*command, env=env).stdout.strip(), "")
        self.write(".ci-lean-path", "/installed lean/bin/lean\r\n")
        self.assertEqual(self.aspect(*command, env=env).stdout.strip(), "/installed lean/bin/lean")
        self.assertEqual(self.aspect(*command, env={**env, "VESPER_LEAN": "/explicit/lean"}).stdout.strip(),
                         "/explicit/lean")

    def test_lean_replay_scope_uses_the_git_branch_history_and_exact_paths(self):
        def git(*args):
            result = subprocess.run(
                ["git", *args], cwd=self.root, text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

        command = ("ci", "lean-replay-scope")
        env = {"GIT_BRANCH": ""}
        skip = "--test_tag_filters=-lean_replay\n"
        git("init", "-b", "main")
        git("config", "user.name", "Fixture")
        git("config", "user.email", "fixture@example.invalid")
        git("config", "commit.gpgsign", "false")
        git("commit", "--allow-empty", "-m", "Base")
        git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.assertEqual(self.aspect(*command, env=env).stdout, "")
        git("switch", "-c", "feature")
        self.write("docs/file with spaces.md", "Unrelated")
        git("add", "docs")
        git("commit", "-m", "Documentation")
        self.assertEqual(self.aspect(*command, env=env).stdout, skip)
        self.assertEqual(self.aspect(*command, "--", "--base", "main", env=env).stdout, skip)
        self.assertEqual(self.aspect(*command, "--", "--base=main", env=env).stdout, skip)
        self.assertEqual(self.aspect(*command, env={**env, "GIT_BRANCH": "main"}).stdout, "")
        self.assertEqual(self.aspect(*command, "--", "--base", "missing", env=env).stdout, "")
        self.aspect(*command, "--", "--unknown", env=env, status=2)
        git_path = shutil.which("git")
        self.write("bin/git", f"""\
#!/usr/bin/env python3
import os
import sys
args = sys.argv[1:]
failure = os.environ.get("FIXTURE_GIT_FAILURE")
if args[:2] in ([failure, "--name-only"], [failure, "--format=%B"]):
    sys.exit(45)
os.execv({git_path!r}, [{git_path!r}, *args])
""", executable=True)
        for failure in ("diff", "log"):
            with self.subTest(git_failure=failure):
                full = self.aspect(*command, env={
                    **env, "FIXTURE_GIT_FAILURE": failure,
                    "PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
                })
                self.assertEqual(full.stdout, "")
                self.assertIn(f"git {failure} failed; running the full suite", full.stderr)
        git("commit", "--allow-empty", "-m", "Run CI [ci full]")
        self.assertEqual(self.aspect(*command, env=env).stdout, "")
        git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.assertEqual(self.aspect(*command, env=env).stdout, skip)
        self.write("lean/Model with spaces λ.lean", "import Lean")
        git("add", "lean")
        git("commit", "-m", "Update Lean")
        full = self.aspect(*command, env=env)
        self.assertEqual(full.stdout, "")
        self.assertIn("Model with spaces λ.lean", full.stderr)
        git("switch", "--detach")
        self.assertEqual(self.aspect(*command, env=env).stdout, "")

    def test_native_lean_replay_preserves_scope_and_cleans_up_on_failure(self):
        (self.root / ".aspect/lean-ci.axl").unlink()
        (self.root / ".aspect/lean-replay-scope.axl").unlink()
        disk_task = self.root / ".aspect/disk-ci.axl"
        disk_task.write_text(disk_task.read_text().replace('kind = "reclaim-disk"', 'kind = "unused-reclaim-disk"'))
        self.write(".aspect/replay-fixture.axl", """
def _scope(ctx):
    if ctx.std.env.var("VESPER_TEST_REPLAY_MODE") == "skip":
        ctx.std.io.stdout.write("--skip-lean\\n")
    return 0

def _install(ctx):
    root = ctx.std.env.aspect_root_dir()
    ctx.std.fs.write(root + "/.ci-lean-path", "mock-lean\\n")
    ctx.std.fs.write(root + "/.ci-logos-cargo-path", "mock-cargo\\n")
    return 0

def _reclaim(ctx):
    root = ctx.std.env.aspect_root_dir()
    if ctx.args.tool_args == ["watch"]:
        return ctx.std.process.command("sleep").args(["30"]).status().code or 0
    checker = ctx.std.fs.read_to_string(root + "/checker-path")
    ctx.std.fs.write(root + "/report-clean", str(not ctx.std.fs.exists(checker)))
    return 0

scope = task(kind = "lean-replay-scope", group = ["ci"], summary = "Mock replay scope", implementation = _scope)
install = task(kind = "install-logos-rust", group = ["ci"], summary = "Mock replay installer", implementation = _install)
reclaim = task(kind = "reclaim-disk", group = ["ci"], summary = "Mock disk watcher", implementation = _reclaim,
               args = {"tool_args": args.trailing_var_args(description = "watch or report")})
""")
        self.write("manifest", "//crates/vesper-oracles:bin/logos42 bin/checker\n")
        self.write("bin/checker", "")
        self.write("tools/logos42/setup.py", """\
import os
from pathlib import Path
import sys

args = dict(zip(sys.argv[1::2], sys.argv[2::2]))
checker = Path(args["--directory"])
assert str(checker) == os.environ["VESPER_LOGOS42_ROOT"]
Path("checker-path").write_text(str(checker))
assert args["--lean"] == "mock-lean" and args["--cargo"] == "mock-cargo"
checker.mkdir()
if os.environ["VESPER_TEST_REPLAY_MODE"] == "prepare-fail":
    sys.exit(17)
""")
        self.write("bin/bazel", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys

args = sys.argv[1:]
mode = os.environ["VESPER_TEST_REPLAY_MODE"]
if args[0] == "query":
    assert args[1] == "--config=ci" and "lean_replay" in args[2]
    Path("queried").touch()
    if mode != "empty":
        print("//:first")
        print("//:second")
    sys.exit(0)
assert args == ["test", "--config=ci", "--config=rbe_build", "--config=ci_tests", "//:first", "//:second"]
assert os.environ["VESPER_LEAN"] == "mock-lean"
assert os.environ["VESPER_CARGO"] == "mock-cargo"
assert Path(os.environ["VESPER_LOGOS42_ROOT"]).is_dir()
Path("tested").touch()
sys.exit(23 if mode == "test-fail" else 0)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]]),
               "VESPER_BAZEL_EXECUTABLES": "manifest"}
        for mode, status, queried, tested, cleaned in (
            ("skip", 0, False, False, False),
            ("empty", 1, True, False, False),
            ("prepare-fail", 17, True, False, True),
            ("test-fail", 23, True, True, True),
            ("normal", 0, True, True, True),
        ):
            with self.subTest(mode=mode):
                for path in ("queried", "tested", "report-clean", "checker-path"):
                    (self.root / path).unlink(missing_ok=True)
                result = self.aspect("ci", "test-lean-replay", status=status,
                                     env={**env, "VESPER_TEST_REPLAY_MODE": mode})
                if mode == "empty":
                    self.assertIn("No Lean replay test targets found", result.stderr)
                self.assertEqual((self.root / "queried").exists(), queried)
                self.assertEqual((self.root / "tested").exists(), tested)
                self.assertEqual((self.root / "report-clean").exists(), cleaned)
                if cleaned:
                    scratch = Path((self.root / "checker-path").read_text()).parent
                    if scratch.parent == Path("/tmp") and scratch.name.startswith("vesper-logos42-ci-"):
                        self.addCleanup(shutil.rmtree, scratch, ignore_errors=True)
                    self.assertEqual((self.root / "report-clean").read_text(), "True")

    def test_lean_model_plan_checks_order_version_and_private_imports(self):
        self.copy_fixture("lean-toolchain")
        self.copy_fixture("scripts/lean-model-plan.json")
        elan_home = self.root / "elan home"
        self.write("elan home/bin/lean", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
with open("lean.log", "a") as out:
    print(json.dumps([args, os.environ.get("LEAN_PATH")]), file=out)
if args == ["--version"]:
    print("Lean (version " + os.environ["VESPER_TEST_LEAN_VERSION"] + ", fixture)")
    sys.exit(0)
if args[0] == "-o":
    output = Path(args[1])
    assert output.parent.is_dir()
    output.touch()
if args[-1] == os.environ.get("VESPER_TEST_LEAN_FAIL"):
    sys.exit(23)
""", executable=True)
        env = {
            "ELAN_HOME": str(elan_home), "LEAN_PATH": "/inherited/path",
            "VESPER_TEST_LEAN_VERSION": "4.35.0",
        }
        command = ("ci", "check-lean-models")
        self.aspect(*command, env={**env, "VESPER_TEST_LEAN_VERSION": "4.35.01"}, status=1)
        log = self.root / "lean.log"
        self.assertEqual(len(log.read_text().splitlines()), 1)
        log.unlink()
        self.aspect(*command, env=env)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        plan = json.loads((self.root / "scripts/lean-model-plan.json").read_text())
        models = ["lean/" + model + ".lean" for model in plan["standalone"]]
        self.assertEqual(calls[:1 + len(models)],
                         [[["--version"], "/inherited/path"], *[[[path], "/inherited/path"] for path in models]])
        dependencies = calls[1 + len(models):]
        self.assertEqual(len(dependencies), len(plan["dependencies"]))
        self.assertEqual(dependencies[0][0][0], "-o")
        scratch = Path(dependencies[0][0][1]).parent
        self.assertFalse(scratch.exists())
        for (argv, imports), row in zip(dependencies, plan["dependencies"], strict=True):
            model = row["model"]
            expected = ["-o", str(scratch / (model + ".olean"))] if row.get("compile") else []
            self.assertEqual(argv, expected + ["lean/" + model + ".lean"])
            self.assertEqual(imports, str(scratch) if row.get("imports") else "/inherited/path")
        encoding = next(i for i, (args, _) in enumerate(dependencies) if args[-1] == "lean/CodegenRv64Encoding.lean")
        semantics = next(i for i, (args, _) in enumerate(dependencies) if args[-1] == "lean/CodegenRv64Semantics.lean")
        self.assertLess(encoding, semantics)
        log.unlink()
        self.aspect(*command, env={**env, "VESPER_TEST_LEAN_FAIL": "lean/ArithmeticOracle.lean"}, status=23)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(calls[-1][0][-1], "lean/ArithmeticOracle.lean")
        self.assertFalse(Path(calls[-1][1]).exists())

    def test_ci_toolchain_installers_publish_paths_only_after_success(self):
        tools = self.write("fixtures/tool", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
config = os.environ["ELAN_HOME"] if name in ("elan", "lean") else os.environ["CARGO_HOME"]
assert os.environ["PATH"].split(os.pathsep)[0] == str(Path(config) / "bin")
with open("toolchain.log", "a") as out:
    print(json.dumps([name, args]), file=out)
if name == "elan":
    assert args == ["which", "lean"]
    if os.environ.get("VESPER_TEST_EMPTY_LEAN") != "1":
        print("/fixture/lean binary")
elif name == "rustup":
    assert args in (["toolchain", "install", "1.99.0", "--profile", "minimal"],
                    ["which", "--toolchain", "1.99.0", "cargo"])
    if args[0] == "which" and os.environ.get("VESPER_TEST_EMPTY_CARGO") != "1":
        print(Path(config) / "bin" / "cargo")
else:
    assert args == ["--version"]
    if os.environ.get("VESPER_TEST_FAIL_VERSION") == name:
        sys.exit(17)
    print(name + " fixture version")
""", executable=True)
        for directory, commands in (("elan home", ("lean", "elan")), ("cargo home", ("rustup", "cargo"))):
            (self.root / directory / "bin").mkdir(parents=True)
            for name in commands:
                (self.root / directory / "bin" / name).symlink_to(tools)
        env = {
            "ELAN_HOME": str(self.root / "elan home"),
            "CARGO_HOME": str(self.root / "cargo home"),
        }
        self.aspect("ci", "install-lean", env=env)
        self.aspect("ci", "install-logos-rust", env=env)
        self.assertEqual((self.root / ".ci-lean-path").read_text(), "/fixture/lean binary\n")
        cargo = self.root / "cargo home/bin/cargo"
        self.assertEqual((self.root / ".ci-logos-cargo-path").read_text(), str(cargo) + "\n")
        log = [json.loads(line) for line in (self.root / "toolchain.log").read_text().splitlines()]
        self.assertEqual(log, [
            ["lean", ["--version"]],
            ["elan", ["which", "lean"]],
            ["rustup", ["toolchain", "install", "1.99.0", "--profile", "minimal"]],
            ["rustup", ["which", "--toolchain", "1.99.0", "cargo"]],
            ["cargo", ["--version"]],
        ])
        for task, path, expected_error, override in (
            ("install-lean", ".ci-lean-path", 17, {"VESPER_TEST_FAIL_VERSION": "lean"}),
            ("install-lean", ".ci-lean-path", 1, {"VESPER_TEST_EMPTY_LEAN": "1"}),
            ("install-logos-rust", ".ci-logos-cargo-path", 17, {"VESPER_TEST_FAIL_VERSION": "cargo"}),
            ("install-logos-rust", ".ci-logos-cargo-path", 1, {"VESPER_TEST_EMPTY_CARGO": "1"}),
        ):
            with self.subTest(task=task, override=override):
                published_path = self.root / path
                published_path.write_text("previous good path\n")
                self.aspect("ci", task, status=expected_error, env={**env, **override})
                self.assertEqual(published_path.read_text(), "previous good path\n")

    def test_missing_toolchains_use_native_release_installers(self):
        import platform
        import sys

        installer = self.write("fixtures/installer", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import shutil
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "elan-init":
    assert args == ["-y", "--default-toolchain", "none"]
    destination = Path(os.environ["ELAN_HOME"]) / "bin"
    tools = ["elan", "lean"]
else:
    assert name == "rustup-init"
    assert args == ["-y", "--no-modify-path", "--profile", "minimal", "--default-toolchain", "1.99.0"]
    destination = Path(os.environ["CARGO_HOME"]) / "bin"
    tools = ["rustup", "cargo"]
destination.mkdir(parents=True)
for tool in tools:
    shutil.copy2("fixtures/installed-tool", destination / tool)
with open("installers.log", "a") as log:
    print(json.dumps([name, str(Path(sys.argv[0]).parent)]), file=log)
""", executable=True)
        self.write("fixtures/installed-tool", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "elan":
    assert args == ["which", "lean"]
    print(Path(os.environ["ELAN_HOME"]) / "bin/lean")
elif name == "rustup":
    assert args == ["which", "--toolchain", "1.99.0", "cargo"]
    print(Path(os.environ["CARGO_HOME"]) / "bin/cargo")
else:
    assert args == ["--version"]
""", executable=True)
        self.write("bin/curl", """\
#!/usr/bin/env python3
from pathlib import Path
import shutil
import sys

args = sys.argv[1:]
assert args[:4] == ["--proto", "=https", "--tlsv1.2", "-fsSL"]
url, flag, output = args[4:]
assert flag == "-o"
with open("downloads", "a") as log:
    print(url, file=log)
if output.endswith("elan.tar.gz"):
    Path(output).write_text("fake native archive")
else:
    shutil.copyfile("fixtures/installer", output)
""", executable=True)
        self.write("bin/tar", f"""\
#!/usr/bin/env python3
from pathlib import Path
import shutil
import sys

archive, flag, directory, entry = sys.argv[2:]
assert sys.argv[1] == "-xzf" and flag == "-C" and entry == "elan-init"
assert Path(archive).read_text() == "fake native archive"
shutil.copy2({str(installer)!r}, Path(directory) / entry)
""", executable=True)
        (self.root / "bin/python3").symlink_to(shutil.which("python3"))
        (self.root / "bin/git").symlink_to(shutil.which("git"))
        env = {
            "PATH": str(self.root / "bin"), "ELAN_HOME": str(self.root / "new elan"),
            "CARGO_HOME": str(self.root / "new cargo"),
            "RUSTUP_UPDATE_ROOT": "https://fixture.invalid/rustup/", "RUSTUP_VERSION": "3.1.4",
        }
        self.aspect("ci", "install-lean", env=env)
        self.aspect("ci", "install-logos-rust", env=env)
        self.aspect("ci", "install-logos-rust", env={
            **env, "CARGO_HOME": str(self.root / "default rustup channel"),
            "RUSTUP_UPDATE_ROOT": "", "RUSTUP_VERSION": "",
        })
        arch = "aarch64" if platform.machine() == "arm64" else platform.machine()
        triple = arch + ("-apple-darwin" if sys.platform == "darwin" else "-unknown-linux-gnu")
        self.assertEqual((self.root / "downloads").read_text().splitlines(), [
            "https://github.com/leanprover/elan/releases/latest/download/elan-" + triple + ".tar.gz",
            "https://fixture.invalid/rustup/archive/3.1.4/" + triple + "/rustup-init",
            "https://static.rust-lang.org/rustup/dist/" + triple + "/rustup-init",
        ])
        installers = [json.loads(line) for line in (self.root / "installers.log").read_text().splitlines()]
        self.assertEqual([name for name, _ in installers], ["elan-init", "rustup-init", "rustup-init"])
        self.assertTrue(all(not Path(path).exists() for _, path in installers))

    @unittest.skipUnless(shutil.which("openssl"), "OpenSSL unavailable")
    def test_native_tls_fixtures_preserve_names_keys_and_trust(self):
        def openssl(*args, status=0):
            result = subprocess.run(["openssl", *args], capture_output=True)
            self.assertEqual(result.returncode, status, result.stderr.decode())
            return result.stdout

        chorus = [(f"node{node}", f"node{node}.chorus.test", f"node{node}.chorus.test",
                   "chorus-ca") for node in range(16)]
        chorus.append(("rogue-node1", "node1.chorus.test", "node1.chorus.test", "rogue-ca"))
        for crate, directory, trusted, leaves in (
            ("chorus", "pki", "chorus-ca", chorus),
            ("warp-tls", "", "test-ca", [
                ("server", "server", "warp.test", "test-ca"),
                ("rogue-server", "rogue-server", "warp.test", "rogue-ca"),
            ]),
        ):
            with self.subTest(crate=crate):
                out = self.root / f"crates/vesper-{crate}/tests/fixtures" / directory
                self.aspect("crate", crate, "gen")
                self.assertEqual({file.name for file in out.iterdir()}, {
                    f"{trusted}.der", "rogue-ca.der",
                    *(f"{leaf[0]}{suffix}" for leaf in leaves for suffix in (".der", ".key.der")),
                })
                auxiliary = self.root / f"{crate} verification"
                auxiliary.mkdir()
                for authority in (trusted, "rogue-ca"):
                    ca = out / f"{authority}.der"
                    details = openssl("x509", "-inform", "DER", "-in", ca, "-noout", "-text")
                    self.assertIn(b"CA:TRUE", details)
                    self.assertIn(b"Certificate Sign, CRL Sign", details)
                    openssl("x509", "-inform", "DER", "-in", ca, "-out", auxiliary / f"{authority}.pem")
                for name, subject, host, authority in leaves:
                    cert = out / f"{name}.der"
                    key = out / f"{name}.key.der"
                    details = openssl("x509", "-inform", "DER", "-in", cert, "-noout", "-text")
                    self.assertRegex(details, rb"Subject: CN\s*=\s*" + subject.encode() + rb"\n")
                    self.assertIn(f"DNS:{host}".encode(), details)
                    self.assertIn(b"CA:FALSE", details)
                    self.assertIn(b"Digital Signature", details)
                    self.assertIn(b"TLS Web Server Authentication, TLS Web Client Authentication", details)
                    self.assertIn(b"ASN1 OID: prime256v1", details)
                    self.assertEqual(
                        openssl("pkey", "-inform", "DER", "-in", key, "-pubout"),
                        openssl("x509", "-inform", "DER", "-in", cert, "-pubkey", "-noout"),
                    )
                    pem = auxiliary / f"{name}.pem"
                    openssl("x509", "-inform", "DER", "-in", cert, "-out", pem)
                    openssl("verify", "-CAfile", auxiliary / f"{authority}.pem", pem)
                    if authority == "rogue-ca":
                        openssl("verify", "-CAfile", auxiliary / f"{trusted}.pem", pem, status=2)

    def test_native_tls_fixtures_leave_existing_files_on_openssl_failure(self):
        path = "crates/vesper-warp-tls/tests/fixtures/test-ca.der"
        prior = self.write(path, "previous certificate")
        self.write("bin/openssl", "#!/bin/sh\nexit 23\n", executable=True)
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        self.aspect("crate", "warp-tls", "gen", env=env, status=23)
        self.assertEqual(list(prior.parent.iterdir()), [prior])
        self.assertEqual(prior.read_text(), "previous certificate")

    def test_script_exit_codes_and_signals_reach_the_cli(self):
        for body, status in (
            ("raise SystemExit(23)\n", 23),
            ("import os, signal\nos.kill(os.getpid(), signal.SIGTERM)\n", 128 + signal.SIGTERM),
        ):
            with self.subTest(status=status):
                self.write("scripts/ci/merge_gate.py", body)
                result = self.aspect("ci", "merge-gate", status=status)
                self.assertEqual(result.stdout, "")

    def test_missing_script_has_a_nonzero_status_and_diagnostic(self):
        result = self.aspect("ci", "merge-gate", status=2)
        self.assertEqual(result.stdout, "")
        self.assertIn("merge_gate.py", result.stderr)

    def test_platform_suite_discovers_tests_from_workspace_root(self):
        self.write("scripts/ci/tests/test_platform_fixture.py", """\
from pathlib import Path
import unittest

class ExampleTest(unittest.TestCase):
    def test_workspace_root(self):
        self.assertTrue(Path(".aspect/version.axl").is_file())
""")
        result = self.aspect("ci", "platform-tests", "--", "-q")
        self.assertIn("Ran 1 test", result.stderr)

    def test_native_host_runs_expected_statuses_from_shared_cases(self):
        self.write("scripts/native-example-cases.txt", "fixture:42\n")
        self.write("manifest with spaces", "//:bin/vesper bin/vesper compiler\n")
        self.write("bin/vesper compiler", """\
#!/bin/sh
test "$1" = build && test "$2" = examples/fixture.ves && test "$3" = -o || exit 2
printf '#!/bin/sh\\nexit 42\\n' > "$4"
chmod +x "$4"
""", executable=True)
        result = self.aspect("acceptance", "test-native-host",
                             env={"VESPER_BAZEL_EXECUTABLES": "manifest with spaces"})
        self.assertEqual(result.stdout, "fixture: host exit 42\n")

    def test_native_fmt_checks_tracked_paths_and_exclusions(self):
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        files = ["test with spaces.ves", "skip.ves"]
        for path in files:
            self.write(path, "")
        subprocess.run(["git", "add", "--", *files], cwd=self.root, check=True)
        self.write("scripts/fmt-exclude.txt", "# fixture exclusion\nskip.ves\n")
        binary = self.write("bin/vesper", """\
#!/bin/sh
test "$#" = 5 && test "$5" = 'test with spaces.ves' || exit 2
printf '{"status":"unchanged"}\\n'
""", executable=True)
        result = self.aspect("acceptance", "check-fmt", "--", "--binary", str(binary))
        self.assertIn("1 files canonical, 1 excluded", result.stdout)
        self.write("scripts/fmt-exclude.txt", "missing.ves\n")
        result = self.aspect("acceptance", "check-fmt", "--", "--binary", str(binary), status=1)
        self.assertIn("missing.ves", result.stderr)

    def test_native_abba_alternates_arms_and_emits_flip_gate_pairs(self):
        rust = self.write("bin/rust arm", """\
#!/bin/sh
echo A >> order
echo rust 8 digest
""", executable=True)
        vesper = self.write("bin/vesper arm", """\
#!/bin/sh
echo B >> order
echo vesper 6 digest
""", executable=True)
        result = self.aspect("crate", "gpu-compute", "abba", "--", str(rust), str(vesper), "2", "sample")
        self.assertEqual((self.nested / "order").read_text().split(), list("ABBAABBA"))
        self.assertIn("flip-gate pairs sample 16:12 16:12", result.stdout)
        self.assertIn("Vesper/Rust mean 0.750", result.stdout)
        self.write("bin/vesper arm", "#!/bin/sh\necho vesper 6 different\n", executable=True)
        result = self.aspect("crate", "gpu-compute", "abba", "--", str(rust), str(vesper), "2", status=1)
        self.assertIn("digests DIFFER: different digest", result.stdout)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable")
    def test_powershell_native_examples_accept_the_expected_exit_code(self):
        script = "scripts/windows/test-native-executables.ps1"
        self.copy_fixture(script)
        self.write("scripts/native-example-cases.txt", "example:42\n")
        compiler = self.write("bin/compiler.exe", """\
#!/bin/sh
case "$4" in *.exe) ;; *) exit 2 ;; esac
printf '#!/bin/sh\\nexit 42\\n' > "$4"
chmod +x "$4"
""", executable=True)
        env = {"VESPER_NATIVE_BINARY": str(compiler), "VESPER_NATIVE_SUFFIX": ".exe"}
        result = self.powershell(script, env=env)
        self.assertIn("example: host exit 42", result.stdout)
        self.write("scripts/native-example-cases.txt", "example:41\n")
        result = self.powershell(script, status=1, env=env)
        self.assertIn("expected 41, got 42", result.stderr)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable")
    def test_port_abi_plan_matches_between_aspect_and_powershell(self):
        script = "scripts/windows/test-port-abi.ps1"
        self.copy_fixture(script)
        self.copy_fixture("scripts/port-abi-plan.json")
        self.copy_fixture("scripts/port-harness-plan.json")
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
assert os.environ["VESPER_PORT_COMPILER"] == "fixture-compiler"
assert args[:2] == ["test", "--locked"]
with open("invocations", "a") as out:
    print(json.dumps(args[2:]), file=out)
for group in json.loads(Path("scripts/port-abi-plan.json").read_text()):
    for check in group["checks"]:
        if check["args"] != args[2:]:
            continue
        if "description" not in check:
            sys.exit(0)
        if os.getenv("VESPER_ABI_TEST_BAD_DIAGNOSTIC"):
            sys.exit(2)
        for name in check.get("failed", []):
            print(f"test {name} ... FAILED")
        for name in check.get("passed", []):
            print(f"test {name} ... ok")
        if "diagnostic" in check:
            print(check["diagnostic"], file=sys.stderr)
        sys.exit(1)
sys.exit(99)
""", executable=True)
        env = {
            "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"],
            "VESPER_PORT_COMPILER": "fixture-compiler",
            "VESPER_PORT_HARNESS_CRATES": "all",
            "VESPER_PORT_HARNESS_SHARD": "",
        }
        commands = self.root / "invocations"
        self.aspect("ci", "test-port-abi", env=env)
        axl_commands = commands.read_text().splitlines()
        commands.unlink()
        self.aspect("acceptance", "test-port-harness", env={**env, "VESPER_PORT_HARNESS_SCOPE": "abi"})
        self.assertEqual(commands.read_text().splitlines(), axl_commands)
        commands.unlink()
        self.powershell(script, env=env)
        self.assertEqual(commands.read_text().splitlines(), axl_commands)
        self.assertEqual(len(axl_commands), 9)
        bad_env = {**env, "VESPER_ABI_TEST_BAD_DIAGNOSTIC": "1"}
        self.assertIn("Divergence check", self.aspect("ci", "test-port-abi", status=1, env=bad_env).stderr)
        self.assertIn("Divergence check", self.powershell(script, status=1, env=bad_env).stderr)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable")
    def test_hybrid_restores_default_compiler_after_a_failing_suite(self):
        script = "scripts/windows/test-hybrid-cli.ps1"
        self.copy_fixture(script)
        self.write("tests/fixture_cli.rs", "")
        self.write("tests/skip_cli.rs", "")
        self.write("bin/cargo", """\
#!/usr/bin/env python3
import json
from pathlib import Path
import sys
if sys.argv[1] == "test":
    assert sys.argv[-2:] == ["--test", "fixture_cli"]
    print(json.dumps({
        "reason": "compiler-artifact",
        "executable": str(Path("bin/test suite").resolve()),
        "target": {"kind": ["test"], "name": "fixture_cli"},
    }))
""", executable=True)
        env = {
            "PATH": str(self.root / "bin") + os.pathsep + os.environ["PATH"],
            "CARGO_TARGET_DIR": "build output",
            "CLI_SUITES": "fixture_cli",
        }
        for suffix in ("", ".exe"):
            with self.subTest(suffix=suffix):
                default = self.write("build output/debug/vesper" + suffix, "default")
                self.write("build output/debug/vesper-hybrid" + suffix, "hybrid")
                self.write("bin/test suite", f"""\
#!/bin/sh
test "$1" = 'one arg' || exit 3
test "$(cat 'build output/debug/vesper{suffix}')" = hybrid || exit 4
echo ran >> exercised
exit 19
""", executable=True)
                if suffix:
                    result = self.powershell(script, "one arg", status=1, env=env)
                else:
                    result = self.aspect("crate", "hybrid", "cli-suites", "--", "one arg", status=1, env=env)
                self.assertIn("1 suites on the hybrid, 1 failed", result.stdout)
                self.assertEqual(default.read_text(), "default")
        self.assertEqual((self.root / "exercised").read_text().splitlines(), ["ran", "ran"])

    def test_native_prebuild_records_outputs_and_clears_stale_manifest_on_failure(self):
        bazel = self.write("bin/mock bazel", """\
#!/bin/sh
case "$1" in
    query) echo //crates/example:example/test ;;
    build)
        test "$4" = //crates/example:example/test || exit 2
        test -z "$VESPER_FIXTURE_BUILD_FAIL" || exit 8
        ;;
    cquery) printf '@@//:bin/vesper bin/vesper\\n@//crates/example:example/test bin/test\\n' ;;
    *) exit 99 ;;
esac
""", executable=True)
        env = {"BAZEL": str(bazel), "BAZEL_FLAGS": ""}
        task = ("ci", "prebuild-acceptance", "--", "manifest with spaces")
        result = self.aspect(*task, env=env)
        manifest = self.root / "manifest with spaces"
        self.assertEqual(manifest.read_text(),
                         "//:bin/vesper bin/vesper\n//crates/example:example/test bin/test\n")
        self.assertIn("Prebuilt 2 executables", result.stdout)
        result = self.aspect(*task, env={**env, "VESPER_FIXTURE_BUILD_FAIL": "1"})
        self.assertIn("each task will build its own", result.stderr)
        self.assertFalse(manifest.exists())

    def test_bazel_helpers_query_uncached_builds_and_report_failures(self):
        bazel = self.write("bin/mock bazel", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys

verb, *args = sys.argv[1:]
with open("bazel-calls", "a") as log:
    print(verb, file=log)
mode = os.environ["VESPER_TEST_BAZEL_MODE"]
if verb == "build":
    assert args == ["--config=ci", "//:uncached"]
    print("build diagnostic")
    sys.exit(19 if mode == "build-fail" else 0)
assert verb == "cquery" and args == ["--config=ci", "--output=files", "//:uncached"]
if mode != "empty-query":
    print("bin/output with spaces")
    print("bin/second output")
""", executable=True)
        env = {"BAZEL": str(bazel), "BAZEL_FLAGS": "--config=ci", "VESPER_BAZEL_EXECUTABLES": "missing"}
        command = ("maintenance", "bazel-helpers", "--", "bazel_executable", "//:uncached")
        calls = self.root / "bazel-calls"
        for mode, status, invocations in (("normal", 0, ["build", "cquery"]),
                                          ("build-fail", 19, ["build"]),
                                          ("empty-query", 1, ["build", "cquery"])):
            with self.subTest(mode=mode):
                calls.unlink(missing_ok=True)
                result = self.aspect(*command, status=status, env={**env, "VESPER_TEST_BAZEL_MODE": mode})
                self.assertEqual(calls.read_text().splitlines(), invocations)
                self.assertIn("build diagnostic", result.stderr)
                self.assertEqual(result.stdout, "bin/output with spaces\n" if status == 0 else "")

    def test_bazel_helpers_resolve_and_run_prebuilt_executable(self):
        self.write("bin/cached", '#!/bin/sh\nprintf "%s\\n" "$1"\nexit 19\n', executable=True)
        self.write("bazel-cache", "//:cached bin/cached\n")
        env = {"BAZEL": "/missing-bazel", "VESPER_BAZEL_EXECUTABLES": "bazel-cache"}
        command = ("maintenance", "bazel-helpers", "--")
        result = self.aspect(*command, "bazel_executable", "//:cached", env=env)
        self.assertEqual(result.stdout, "bin/cached\n")
        result = self.aspect(*command, "bazel_exec", "//:cached", "two words", status=19, env=env)
        self.assertEqual(result.stdout, "two words\n")

    def test_bazel_prebuild_writes_the_manifest_in_workspace_root(self):
        bazel = self.write("bin/mock-bazel", """\
#!/bin/sh
case "$1" in
    build) printf '%s\\n' "$@" > bazel-build-args ;;
    cquery) printf '@@//:one bin/one\\n@//:two bin/two\\n' ;;
    *) exit 99 ;;
esac
""", executable=True)
        self.aspect(
            "maintenance", "bazel-helpers", "--", "bazel_prebuild",
            "output with spaces", "//:one", "//:two",
            env={"BAZEL": str(bazel), "BAZEL_FLAGS": "--config=test"},
        )
        self.assertEqual((self.root / "bazel-build-args").read_text().splitlines(),
                         ["build", "--config=test", "//:one", "//:two"])
        self.assertEqual((self.root / "output with spaces").read_text(),
                         "//:one bin/one\n//:two bin/two\n")
        self.assertFalse((self.root / "output with spaces.tmp").exists())

    def test_bazel_helpers_reject_missing_and_unlisted_functions(self):
        for args in ((), ("touch", "unexpected")):
            with self.subTest(args=args):
                result = self.aspect("maintenance", "bazel-helpers", "--", *args, status=2)
                self.assertIn("usage: aspect maintenance bazel-helpers", result.stderr)
                self.assertEqual(result.stdout, "")
        self.assertFalse((self.root / "unexpected").exists())


if __name__ == "__main__":
    unittest.main()
