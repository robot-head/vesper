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
        for name, shell in (("test-native-host", "sh"), ("test-doc-links", "bash")):
            with self.subTest(task=name):
                self.write(f"scripts/{name}.sh", 'printf "%s|%s\\n" "$VESPER_TASK_TEST_SHELL" "$1"\n')
                result = self.aspect("acceptance", name, "--", "two words", env=env)
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
