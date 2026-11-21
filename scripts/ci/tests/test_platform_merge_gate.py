import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "scripts/ci/merge_gate.py"


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], check=True,
                          capture_output=True, text=True).stdout.strip()


class MergeGateTest(unittest.TestCase):
    """scripts/ci/merge_gate.py: no CI for PRs that cannot merge yet (#1321)."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.repo = Path(self.temp.name)
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.name", "t")
        git(self.repo, "config", "user.email", "t@example.com")
        self.commit("a.txt", "base\n")
        # parent: one PR; child: its own commit, then merges the parent in.
        git(self.repo, "checkout", "-q", "-b", "parent")
        self.commit("p.txt", "parent\n")
        git(self.repo, "checkout", "-q", "-b", "child", "main")
        self.commit("c.txt", "child\n")
        git(self.repo, "merge", "-q", "--no-edit", "parent")
        # clash: edits a.txt, and main edits it differently.
        git(self.repo, "checkout", "-q", "-b", "clash", "main")
        self.commit("a.txt", "clash\n")
        git(self.repo, "checkout", "-q", "main")
        self.commit("a.txt", "main\n")
        self.prs = Path(self.temp.name) / "prs.json"
        self.prs.write_text(json.dumps([
            {"number": 1, "base": "main", "ref": "parent"},
            {"number": 2, "base": "main", "ref": "child"},
            {"number": 3, "base": "main", "ref": "clash"},
        ]))

    def tearDown(self):
        self.temp.cleanup()

    def commit(self, name, text):
        (self.repo / name).write_text(text)
        git(self.repo, "add", name)
        git(self.repo, "commit", "-q", "-m", name)

    def gate(self, number, head, base="main"):
        return subprocess.run(
            ["python3", str(GATE), "--number", str(number), "--base-ref", base,
             "--head", head, "--main", "main", "--open-prs", str(self.prs)],
            cwd=self.repo, capture_output=True, text=True)

    def test_parent_can_merge(self):
        run = self.gate(1, "parent")
        self.assertEqual(run.returncode, 0, run.stdout)

    def test_child_carrying_an_open_parent_cannot_merge(self):
        run = self.gate(2, "child")
        self.assertEqual(run.returncode, 1)
        self.assertIn("open PR #1", run.stdout)

    def test_child_can_merge_once_the_parent_is_in_main(self):
        git(self.repo, "merge", "-q", "--no-edit", "parent")
        run = self.gate(2, "child")
        self.assertEqual(run.returncode, 0, run.stdout)

    def test_conflict_with_main_cannot_merge(self):
        run = self.gate(3, "clash")
        self.assertEqual(run.returncode, 1)
        self.assertIn("conflicts with main", run.stdout)

    def test_base_other_than_main_cannot_merge(self):
        run = self.gate(1, "parent", base="child")
        self.assertEqual(run.returncode, 1)
        self.assertIn("not main", run.stdout)


if __name__ == "__main__":
    unittest.main()
