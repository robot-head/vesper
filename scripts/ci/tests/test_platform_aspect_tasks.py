"""Exercise the Aspect CLI and task registrations in an isolated checkout."""
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

    def install_bazel_helpers(self):
        destination = self.root / "scripts/bazel-lib.sh"
        destination.parent.mkdir()
        shutil.copyfile(ROOT / "scripts/bazel-lib.sh", destination)

    def test_script_task_preserves_argv_cwd_environment_and_standard_streams(self):
        self.write("scripts/ci/platform_test_plan.py", """\
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
            "ci", "platform-test-plan", "--", *args,
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

    def test_shell_tasks_select_the_registered_interpreter(self):
        for shell in ("sh", "bash"):
            self.write(
                f"bin/{shell}",
                f'#!/bin/sh\nVESPER_TASK_TEST_SHELL={shell} exec /bin/sh "$@"\n',
                executable=True,
            )
        env = {"PATH": os.pathsep.join([str(self.root / "bin"), os.environ["PATH"]])}
        for task, path, shell in (
            (("crate", "warp-wheel", "test-port"), "crates/vesper-warp-wheel/port/test-port.sh", "sh"),
            (("acceptance", "test-doc-links"), "scripts/test-doc-links.sh", "bash"),
        ):
            with self.subTest(task=task):
                self.write(path, 'printf "%s|%s\\n" "$VESPER_TASK_TEST_SHELL" "$1"\n')
                result = self.aspect(*task, "--", "two words", env=env)
                self.assertEqual(result.stdout, f"{shell}|two words\n")

    def test_script_exit_codes_and_signals_reach_the_cli(self):
        for body, status in (
            ("raise SystemExit(23)\n", 23),
            ("import os, signal\nos.kill(os.getpid(), signal.SIGTERM)\n", 128 + signal.SIGTERM),
        ):
            with self.subTest(status=status):
                self.write("scripts/ci/platform_test_plan.py", body)
                result = self.aspect("ci", "platform-test-plan", status=status)
                self.assertEqual(result.stdout, "")

    def test_missing_script_has_a_nonzero_status_and_diagnostic(self):
        result = self.aspect("ci", "platform-test-plan", status=2)
        self.assertEqual(result.stdout, "")
        self.assertIn("platform_test_plan.py", result.stderr)

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
        self.copy_fixture("scripts/test-port-harness.sh")
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

    def test_bazel_helpers_resolve_and_run_prebuilt_executable(self):
        self.install_bazel_helpers()
        self.write("bin/cached", '#!/bin/sh\nprintf "%s\\n" "$1"\nexit 19\n', executable=True)
        self.write("bazel-cache", "//:cached bin/cached\n")
        env = {"BAZEL": "/missing-bazel", "VESPER_BAZEL_EXECUTABLES": "bazel-cache"}
        command = ("maintenance", "bazel-helpers", "--")
        result = self.aspect(*command, "bazel_executable", "//:cached", env=env)
        self.assertEqual(result.stdout, "bin/cached\n")
        result = self.aspect(*command, "bazel_exec", "//:cached", "two words", status=19, env=env)
        self.assertEqual(result.stdout, "two words\n")

    def test_bazel_prebuild_writes_the_manifest_in_workspace_root(self):
        self.install_bazel_helpers()
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
        self.install_bazel_helpers()
        for args in ((), ("touch", "unexpected")):
            with self.subTest(args=args):
                result = self.aspect("maintenance", "bazel-helpers", "--", *args, status=2)
                self.assertIn("usage: aspect maintenance bazel-helpers", result.stderr)
                self.assertEqual(result.stdout, "")
        self.assertFalse((self.root / "unexpected").exists())


if __name__ == "__main__":
    unittest.main()
