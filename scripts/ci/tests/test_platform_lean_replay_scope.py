"""PR scoping of the `lean_replay` Bazel tests (scripts/ci/lean_replay_scope.py)."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import re
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
ROOT = SCRIPTS.parents[1]
spec = importlib.util.spec_from_file_location("lean_replay_scope", SCRIPTS / "lean_replay_scope.py")
scope = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = scope
spec.loader.exec_module(scope)


class LeanReplayScope(unittest.TestCase):
    def test_main_always_runs_everything(self):
        self.assertEqual(scope.flags(branch="main", changed=["README.md"], messages=""), [])

    def test_unknown_diff_runs_everything(self):
        self.assertEqual(scope.flags(branch="feature", changed=None, messages=""), [])
        self.assertEqual(scope.flags(branch="", changed=[], messages=""), [])

    def test_unrelated_pr_skips_replays(self):
        changed = ["crates/vesper-kinetic/src/lib.rs", "docs/x.md"]
        self.assertEqual(scope.flags(branch="feature", changed=changed, messages=""), [scope.SKIP_FLAG])

    def test_lean_facing_pr_runs_replays(self):
        for path in ["lean/Foundations.lean", "tools/logos42/pins.json", "src/lean_ir.rs",
                     "tests/lean4lean_map42.rs", "MODULE.bazel", "src/optimize/witness_lean.rs",
                     "tests/fixtures/resource_oracle42/fault_peer.py",
                     "docs/rfcs/resource-model42-source-manifest.json",
                     "scripts/ci/test-lean-replay.sh"]:
            with self.subTest(path=path):
                self.assertEqual(scope.flags(branch="feature", changed=[path], messages=""), [])

    def test_marker_forces_full_suite(self):
        self.assertEqual(scope.flags(branch="feature", changed=["docs/x.md"], messages="fix\n[ci full]\n"), [])

    def test_every_tagged_test_has_a_trigger(self):
        build = (ROOT / "BUILD.bazel").read_text(encoding="utf-8")
        tagged = re.findall(r'"test/([a-z0-9_]+)": \[[^\]]*"lean_replay"', build)
        self.assertGreaterEqual(len(tagged), 18)
        for name in tagged:
            with self.subTest(test=name):
                self.assertTrue((ROOT / "tests" / f"{name}.rs").is_file())
                self.assertTrue(scope.triggered([f"tests/{name}.rs"]))


if __name__ == "__main__":
    unittest.main()
