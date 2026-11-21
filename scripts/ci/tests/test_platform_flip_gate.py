from fractions import Fraction
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "scripts/ci/flip_gate.py"
SPEC = importlib.util.spec_from_file_location("flip_gate", GATE)
flip_gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(flip_gate)
PY = shlex.quote(sys.executable)


def py(code):
    return f"{PY} -c {shlex.quote(code)}"


class MedianIntervalTest(unittest.TestCase):
    """The interval is vesper-bench's analyze_pairs (statistics.rs)."""

    def test_ranks_match_vesper_bench(self):
        # Binomial(n, 1/2) ranks with at most 2.5% in each tail: rank 1 at
        # n = 6..8, 2 at 9..11, 10 at n = 30 (cumulative tail 2,140,768,
        # 2.0% of 2^30).
        for n, rank in ((6, 1), (8, 1), (9, 2), (11, 2), (12, 3), (30, 10)):
            pairs = [(100, 100 + i) for i in range(n)]
            lower, upper, _ = flip_gate.median_interval(pairs)
            self.assertEqual(lower, Fraction(100 + rank - 1, 100), n)
            self.assertEqual(upper, Fraction(100 + n - rank, 100), n)

    def test_too_few_epochs_has_no_interval(self):
        lower, upper, median = flip_gate.median_interval([(10, 11)] * 5)
        self.assertIsNone(lower)
        self.assertIsNone(upper)
        self.assertEqual(median, Fraction(11, 10))
        self.assertEqual(flip_gate.decide(lower, upper, Fraction(3, 2)), "inconclusive")

    def test_decisions_compare_exact_ratios(self):
        bar = Fraction(3, 2)
        self.assertEqual(flip_gate.decide(Fraction(1), Fraction(3, 2), bar), "pass")
        self.assertEqual(flip_gate.decide(Fraction(1), Fraction(150001, 100000), bar),
                         "inconclusive")
        self.assertEqual(flip_gate.decide(Fraction(151, 100), Fraction(2), bar), "regression")


class FlipGateTest(unittest.TestCase):
    """scripts/ci/flip_gate.py end to end, on stand-in commands."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.dir = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def gate(self, config, *args):
        path = self.dir / "flip-gate.toml"
        path.write_text(config)
        out = self.dir / "result.json"
        proc = subprocess.run([sys.executable, str(GATE), "demo", "--config", str(path),
                               "--json", str(out), "--quiet", *args],
                              capture_output=True, text=True)
        result = json.loads(out.read_text()) if out.exists() else None
        return proc, result

    @staticmethod
    def pairs_harness(workload, pairs):
        line = " ".join(f"{r}:{v}" for r, v in pairs)
        return py(f"print('warming up'); print('flip-gate pairs {workload} {line}')")

    def config(self, *, tests=("pass",), diff="print('ok')", pairs=None, extra=""):
        lines = ['name = "demo"', 'lane = "SH6-1"']
        for t in tests:
            code = "pass" if t == "pass" else "raise SystemExit(1)"
            lines += ["[[tests]]", f"run = {json.dumps(py(code))}"]
        if diff is not None:
            lines += ["[differential]", f"run = {json.dumps(py(diff))}"]
        if pairs is not None:
            lines += ["[[abba]]", 'workload = "w"',
                      f"run = {json.dumps(self.pairs_harness('w', pairs))}"]
        return "\n".join(lines) + "\n" + extra

    def test_pass(self):
        pairs = [(1000, 1100 + 10 * i) for i in range(30)]
        proc, result = self.gate(self.config(
            diff="print('flip-gate differential differences=0 cases=1234')", pairs=pairs))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(result["verdict"], "PASS")
        self.assertEqual(result["differential"]["cases"], 1234)
        self.assertEqual(result["abba"][0]["decision"], "pass")
        verdict, row = proc.stdout.strip().splitlines()
        self.assertTrue(verdict.startswith("flip-gate demo: PASS"), verdict)
        self.assertIn("0 differences on 1,234 cases", verdict)
        self.assertIn("w 1.25x [1.19x, 1.30x] n=30", verdict)
        self.assertEqual(row.count("|"), 7, row)
        self.assertTrue(row.startswith("| `demo` (PASS) | SH6-1 |"), row)

    def test_slow_port_is_a_regression(self):
        proc, result = self.gate(self.config(pairs=[(100, 200)] * 10))
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(result["abba"][0]["decision"], "regression")

    def test_interval_across_the_bar_is_inconclusive(self):
        pairs = [(100, 130 + 4 * i) for i in range(10)]
        proc, result = self.gate(self.config(pairs=pairs))
        self.assertEqual(proc.returncode, 2, proc.stdout)
        self.assertEqual(result["verdict"], "INCONCLUSIVE")

    def test_failing_test_fails(self):
        proc, result = self.gate(self.config(tests=("pass", "fail"), pairs=[(1, 1)] * 6))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("tests 1/2 pass", proc.stdout)

    def test_reported_differences_fail_even_with_exit_zero(self):
        proc, result = self.gate(self.config(
            diff="print('flip-gate differential differences=3 cases=50')", pairs=[(1, 1)] * 6))
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(result["differential"]["differences"], 3)

    def test_failing_differential_fails(self):
        proc, _ = self.gate(self.config(diff="raise SystemExit(4)", pairs=[(1, 1)] * 6))
        self.assertEqual(proc.returncode, 1)
        self.assertIn("differential failed (exit 4)", proc.stdout)

    def test_missing_pairs_line_fails(self):
        extra = '[[abba]]\nworkload = "other"\nrun = ' + json.dumps(py("print('nothing')")) + "\n"
        proc, result = self.gate(self.config(pairs=[(1, 1)] * 6, extra=extra))
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(result["abba"][1]["decision"], "error")

    def test_skipped_check_is_inconclusive(self):
        proc, result = self.gate(self.config(pairs=[(1, 1)] * 6), "--only", "tests,differential")
        self.assertEqual(proc.returncode, 2)
        self.assertEqual(result["skipped"], ["abba"])

    def test_gate_times_two_commands(self):
        fast = py("pass")
        slow = py("import time; time.sleep(0.25)")
        extra = ("[[abba]]\nworkload = \"t\"\n"
                 f"rust = {json.dumps(fast)}\nvesper = {json.dumps(slow)}\n")
        proc, result = self.gate(self.config(extra=extra), "--epochs", "6")
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        bench = result["abba"][0]
        self.assertEqual(bench["epochs"], 6)
        self.assertEqual(bench["decision"], "regression")
        self.assertGreater(bench["lower"], 1.5)

    def test_configuration_errors(self):
        proc, _ = self.gate('[[abba]]\nworkload = "w"\nrust = "true"\n')
        self.assertEqual(proc.returncode, 3)
        self.assertIn("needs rust and vesper, or run", proc.stderr)
        proc, _ = self.gate(self.config(), "--only", "bogus")
        self.assertEqual(proc.returncode, 3)
        proc, _ = self.gate('rust = ["no/such/dir"]\n' + self.config())
        self.assertEqual(proc.returncode, 3)
        self.assertIn("rust path no/such/dir does not exist", proc.stderr)
        proc = subprocess.run([sys.executable, str(GATE), "no-such-crate"],
                              capture_output=True, text=True)
        self.assertEqual(proc.returncode, 3)
        self.assertIn("no flip-gate.toml", proc.stderr)

    def test_rust_lines_are_counted(self):
        src = self.dir / "src"
        src.mkdir()
        (src / "lib.rs").write_text("fn a() {}\n\n// x\n")
        (src / "notes.txt").write_text("ignored\n")
        extra = f"rust = [{json.dumps(str(src))}]\n"
        # `rust` is a top-level key, so it goes before the first table.
        path = self.dir / "flip-gate.toml"
        path.write_text(extra + self.config(pairs=[(1, 1)] * 6))
        proc = subprocess.run([sys.executable, str(GATE), "demo", "--config", str(path),
                               "--quiet"], capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertTrue(proc.stdout.strip().endswith("| 2 | |"), proc.stdout)


if __name__ == "__main__":
    unittest.main()
