"""Exercise the Aspect CLI and task registrations in an isolated checkout."""
import hashlib
import json
import os
from pathlib import Path
import shutil
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

    def test_logos_maintenance_alias_uses_the_native_argument_parser(self):
        canonical = self.aspect("maintenance", "logos42", "setup", "--", "--help")
        alias = self.aspect("maintenance", "lean4lean42", "setup", "--", "--help")
        self.assertEqual(alias.stdout, canonical.stdout)
        self.assertIn("--directory DIRECTORY", alias.stdout)

    def mock_logos_setup(self, body):
        path = self.root / ".aspect/logos-ci.axl"
        path.write_text(path.read_text().replace(
            'load("./logos-setup.axl", "setup")',
            'load("./logos-setup-fixture.axl", "setup")',
        ))
        self.write(".aspect/logos-setup-fixture.axl", body)

    def test_logos_workflow_prepares_with_the_pinned_tools_before_sweeping(self):
        self.write(".ci-lean-path", "/installed lean/bin/lean\r\n")
        self.write(".ci-logos-cargo-path", "/pinned cargo/bin/cargo\n")
        self.mock_logos_setup(r"""
def setup(ctx, root, cwd, directory, driver, lean, lake, git, cargo):
    if root != cwd:
        fail("Setup did not receive the repository root")
    ctx.std.fs.write(root + "/setup.json", json.encode({
        "--directory": directory, "--driver": driver, "--lean": lean,
        "--lake": lake, "--git": git, "--cargo": cargo,
    }))
    if ctx.std.env.var("SETUP_FAILURE"):
        ctx.std.process.exit(17, "fixture setup failure")
    ctx.std.fs.create_dir(directory)
""")
        command = ("ci", "replay-logos-models")
        no_temp = self.aspect(*command, env={"RUNNER_TEMP": ""}, status=2)
        self.assertIn("RUNNER_TEMP is required", no_temp.stderr)
        env = {"RUNNER_TEMP": "runner scratch", "LOGOS_SWEEP_BATCH_JOBS": "99"}
        self.aspect(*command, env={**env, "SETUP_FAILURE": "1"}, status=17)
        self.assertFalse((self.root / "runner scratch/logos42/checker").exists())
        result = self.aspect(*command, env=env, status=1)
        temp = self.root / "runner scratch"
        checker = temp / "logos42/checker"
        driver = self.root / "target/debug/logos42"
        report = temp / "logos-sweep-report.json"
        self.assertEqual(json.loads((self.root / "setup.json").read_text()), {
            "--directory": str(checker), "--driver": str(driver),
            "--lean": "/installed lean/bin/lean", "--lake": "/installed lean/bin/lake",
            "--cargo": "/pinned cargo/bin/cargo", "--git": shutil.which("git"),
        })
        self.assertIn("setup-receipt.json", result.stderr)
        self.assertFalse(report.exists())
        self.write(".ci-logos-cargo-path", "\n")
        missing_cargo = self.aspect(*command, env=env, status=1)
        self.assertIn("The CI Lean and Cargo executable paths are required", missing_cargo.stderr)

    def test_lint_port_checks_new_modes_suite_with_an_independent_control(self):
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys
args = sys.argv[1:]
area = os.environ.get("VESPER_LINT_DIVERGE")
assert args[:4] == ["test", "--locked", "-p", "vesper-lint"]
if not area:
    if args[4:6] == ["-p", "vesper-lint-seed"]:
        assert args[6:] == ["--", "--skip", "port_frees_its_allocations"]
    sys.exit(0)
assert args[4:] == ["--features", "vesper-port,vesper-port-divergent", "--no-fail-fast",
                    "--test", "differential", "--test", "typed", "--test", "policy", "--test", "modes",
                    "--", "--skip", "port_frees_its_allocations"]
failed, passed = {
    "effects": ("fuzzed_facts_lint_identically", "fuzzed_documents_parse_identically"),
    "projections": ("fuzzed_facts_lint_identically", "fuzzed_documents_parse_identically"),
    "toml": ("fuzzed_documents_parse_identically", "fuzzed_facts_lint_identically"),
    "edits": ("fuzzed_edits_apply_identically", "fuzzed_facts_lint_identically"),
    "modes": ("repository_sources_match_the_definition", "fuzzed_documents_parse_identically"),
}[area]
with Path("lint.log").open("a") as log:
    print(area, file=log)
print(f"test {failed} ... FAILED")
if area != "modes" or not os.environ.get("DROP_MODES_CONTROL"):
    print(f"test {passed} ... ok")
sys.exit(101)
""", executable=True)
        env = {"PATH": os.pathsep.join([str(cargo.parent), os.environ["PATH"]])}
        result = self.aspect("crate", "lint", "test-port", env=env)
        self.assertIn("Detected divergent lint port (modes)", result.stdout)
        self.assertEqual((self.root / "lint.log").read_text().splitlines(),
                         ["effects", "projections", "toml", "edits", "modes"])
        rejected = self.aspect("crate", "lint", "test-port", env={**env, "DROP_MODES_CONTROL": "1"}, status=1)
        self.assertIn("Divergence check did not fail as expected: lint port (modes)", rejected.stderr)

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
# The plain builds (default and `vesper-port`) pass; only the divergent one fails.
if "vesper-port-divergent" not in args:
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
        invalid_shard = f"must be 1 to {len(plan)}"
        for config, error in (
            ({"VESPER_PORT_HARNESS_SHARD": "01"}, invalid_shard),
            ({"VESPER_PORT_HARNESS_SHARD": str(len(plan) + 1)}, invalid_shard),
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

    def test_divergent_feature_guard_requires_the_cfg_selected_library(self):
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
import sys
from pathlib import Path
assert sys.argv[1:] == ["metadata", "--locked", "--no-deps", "--format-version", "1"]
print(Path("fixture metadata.json").read_text())
""", executable=True)
        self.write("crates/guarded/src/nested/module.rs", '#[cfg(feature\n =\n "vesper-port")]\nfn port() {}\n')
        self.write("crates/unconditional/src/lib.rs", "fn port() {}\n")
        self.write("crates/unconditional/tests/fixture.rs", '#[cfg(feature = "vesper-port")]\nfn fixture() {}\n')
        def metadata(features):
            self.write("fixture metadata.json", json.dumps({
                "workspace_root": str(self.root),
                "packages": [{"manifest_path": str(self.root / "crates" / name / "Cargo.toml"),
                              "features": {"vesper-port-divergent": flags}}
                             for name, flags in [("guarded", features), ("unconditional", [])]],
            }))
        env = {"PATH": os.pathsep.join([str(cargo.parent), os.environ["PATH"]])}
        command = ("ci", "tests", "port-divergent-features")
        metadata(["vesper-port"])
        self.assertIn("1 feature-gated ports", self.aspect(*command, env=env).stdout)
        metadata([])
        result = self.aspect(*command, env=env, status=1)
        self.assertIn("vesper-port-divergent must enable vesper-port", result.stderr)
        self.assertIn("/guarded/Cargo.toml", result.stderr)

    def test_bazel_workspace_sync_checks_and_creates_only_missing_members(self):
        cargo = self.write("bin/cargo", """\
#!/usr/bin/env python3
import os
from pathlib import Path
import sys
assert sys.argv[1:] == ["metadata", "--no-deps", "--format-version=1", "--quiet", "--offline"]
if os.environ.get("METADATA_FAILURE"):
    print("metadata failure", file=sys.stderr)
    sys.exit(29)
print(Path("cargo metadata.json").read_text())
""", executable=True)
        build = self.write("crates/owned/BUILD.bazel", "manually edited\n")
        for path in ("crates/first crate", "crates/second", "outside"):
            (self.root / path).mkdir(parents=True)
        self.write("cargo metadata.json", json.dumps({
            "workspace_root": str(self.root),
            "workspace_members": ["root", "owned", "first", "second"],
            "packages": [{"id": name, "manifest_path": str(self.root / path / "Cargo.toml")}
                         for name, path in [("root", ""), ("owned", "crates/owned"), ("outside", "outside"),
                                            ("second", "crates/second"), ("first", "crates/first crate")]],
        }))
        for binary in ("git", "python3"):
            (cargo.parent / binary).symlink_to(shutil.which(binary))
        env = {"PATH": str(cargo.parent)}
        command = ("maintenance", "bazel", "sync-build-files")
        expected = ["crates/first crate/BUILD.bazel", "crates/second/BUILD.bazel"]
        check = self.aspect(*command, "--", "--check", env=env, status=1)
        self.assertEqual(check.stdout.splitlines(),
                         [f"missing {path}; run aspect maintenance bazel sync-build-files" for path in expected])
        self.assertTrue(all(not (self.root / path).exists() for path in expected))
        repair = self.aspect(*command, env=env)
        self.assertEqual(repair.stdout.splitlines(), [f"created {path}" for path in expected])
        for path in expected:
            created = self.root / path
            self.assertEqual(created.read_text(), 'load("//tools/bazel:defs.bzl", "vesper_crate")\n\nvesper_crate()\n')
            self.assertEqual(list(created.parent.iterdir()), [created])
        self.assertFalse((self.root / "BUILD.bazel").exists())
        self.assertFalse((self.root / "outside/BUILD.bazel").exists())
        self.assertEqual(build.read_text(), "manually edited\n")
        self.assertEqual(self.aspect(*command, "--", "--check", env=env).stdout, "")
        invalid = self.aspect(*command, "--", "--invalid", env=env, status=2)
        self.assertIn("usage:", invalid.stderr)
        failure = self.aspect(*command, "--", "--check", env={**env, "METADATA_FAILURE": "1"}, status=29)
        self.assertIn("metadata failure", failure.stderr)

        self.write("bin/bazelisk", """\
#!/usr/bin/env python3
import os
import sys
assert sys.argv[1:] == ["query", "@vesper_cargo//:packages.bzl"]
print("Bazel checked the generated workspace repository")
sys.exit(int(os.environ.get("BAZEL_CODE", "0")))
""", executable=True)
        fallback = self.aspect(*command, "--", "--check", env={**env, "METADATA_FAILURE": "1"})
        self.assertIn("Bazel checked", fallback.stdout)
        cargo.unlink()
        self.assertIn("Bazel checked", self.aspect(*command, "--", "--check", env=env).stdout)
        self.aspect(*command, "--", "--check", env={**env, "BAZEL_CODE": "43"}, status=43)
        generation = self.aspect(*command, env=env, status=1)
        self.assertIn("Cargo is required", generation.stderr)
        (cargo.parent / "bazelisk").unlink()
        absent = self.aspect(*command, "--", "--check", env=env, status=1)
        self.assertIn("without working Cargo or Bazel", absent.stderr)

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
        (self.root / ".aspect/lean-toolchain-test.axl").unlink()
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
        self.mock_logos_setup(r"""
def setup(ctx, root, cwd, directory, driver, lean, lake, git, cargo):
    if directory != ctx.std.env.var("VESPER_LOGOS42_ROOT") or lean != "mock-lean" or cargo != "mock-cargo":
        fail("Setup did not receive the pinned build tools and checker directory")
    ctx.std.fs.write(root + "/checker-path", directory)
    ctx.std.fs.create_dir(directory)
    if ctx.std.env.var("VESPER_TEST_REPLAY_MODE") == "prepare-fail":
        ctx.std.process.exit(17, "fixture setup failure")
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
