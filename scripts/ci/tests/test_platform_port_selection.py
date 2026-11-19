"""Tests for scripts/ci/port_harness_selection.py (#1103)."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import port_harness_selection as selection  # noqa: E402


class PortHarnessSelectionTests(unittest.TestCase):
    def test_compiler_and_shared_paths_run_every_port(self):
        for path in ("src/lib.rs", "tests/fixtures/self_host/driver.rs", "Cargo.lock",
                     "Cargo.toml", "scripts/test-port-harness.sh", ".github/workflows/platforms.yml",
                     "packages/vesper-std/src/lib.ves", "crates/vesper-syntax/src/lib.rs"):
            self.assertEqual(selection.select([path]), "all", path)

    def test_a_seed_the_compiler_links_runs_every_port(self):
        package_dir, _, link, _ = selection.load_workspace()
        compiler = next(n for n, d in package_dir.items() if d == "")
        seeds = sorted(n for n in selection.closure({compiler}, link) if n.endswith("-seed"))
        self.assertTrue(seeds)
        self.assertEqual(selection.select([package_dir[seeds[0]] + "/src/lib.rs"]), "all")

    def test_port_sources_a_compiler_seed_shares_run_every_port(self):
        # vesper-alloc-seed's library is ../src/lib.rs, and the compiler links it.
        self.assertEqual(selection.select(["crates/vesper-alloc/src/lib.rs"]), "all")
        self.assertEqual(selection.select(["crates/vesper-alloc/vesper/src/region.ves"]), "vesper-alloc")

    def test_a_port_runs_itself_and_its_dependents(self):
        self.assertEqual(selection.select(["crates/vesper-prism-h3/vesper/src/message.ves"]),
                         "vesper-prism-h3")
        picked = selection.select(["crates/vesper-beacon/tests/behavior.rs", "docs/x.md"]).split()
        self.assertIn("vesper-beacon", picked)
        self.assertIn("vesper-beacon-arrow", picked)

    def test_the_harness_library_reaches_every_port(self):
        picked = selection.select(["crates/vesper-port-harness/src/lib.rs"]).split()
        for crate in ("vesper-ebpf", "vesper-ignis", "vesper-serde", "vesper-port-pilot"):
            self.assertIn(crate, picked)

    def test_documentation_only_runs_nothing(self):
        self.assertEqual(selection.select(["docs/ci-platforms.md", "README.md"]), "none")

    def test_main_nightly_and_full_run_everything(self):
        self.assertEqual(selection.main(["--profile", "full", "--base", "HEAD", "--head", "HEAD"]), 0)
        for args in (["--profile", "full"], ["--profile", "pr", "--event", "push"],
                     ["--profile", "pr", "--event", "schedule"], ["--profile", "pr"]):
            with self.subTest(args=args):
                from io import StringIO
                from contextlib import redirect_stdout
                out = StringIO()
                with redirect_stdout(out):
                    selection.main(args + ["--head", "HEAD"])
                self.assertEqual(out.getvalue().strip(), "all")

    def test_every_harness_guard_names_a_crate_directory(self):
        import re
        script = (selection.ROOT / "scripts/test-port-harness.sh").read_text()
        names = set()
        for line in script.splitlines():
            m = re.match(r"(?:if ported|skip_port) ([a-z0-9 -]+?)(?:; then| \|\|)", line)
            if m:
                names.update(m.group(1).split())
        self.assertGreater(len(names), 20)
        for name in names:
            self.assertTrue((selection.ROOT / "crates" / name / "Cargo.toml").is_file(), name)


if __name__ == "__main__":
    unittest.main()
