"""Regression cases for revision/inventory consistency, not source authenticity."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("snapshot_check", ROOT / "scripts/check-specification-snapshot.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="vesper-spec-check-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "docs").mkdir()
        for name in ["specification.txt", "specification-source.json"]:
            shutil.copyfile(ROOT / "docs" / name, self.root / "docs" / name)
        self.path = self.root / "docs/specification-source.json"
        self.metadata = json.loads(self.path.read_text(encoding="utf-8"))

    def check(self):
        self.path.write_text(json.dumps(self.metadata, ensure_ascii=False), encoding="utf-8")
        with contextlib.redirect_stdout(io.StringIO()):
            MODULE.check(self.root)

    def test_recorded_source_and_section_order_pass(self):
        self.check()

    def test_duplicate_inventory_cannot_stand_in_for_section_37(self):
        self.metadata["sections"]["37"] = dict(self.metadata["sections"]["36"])
        with self.assertRaisesRegex(ValueError, "numbered heading identity"):
            self.check()

    def test_missing_numbered_section_fails(self):
        del self.metadata["sections"]["37"]
        with self.assertRaisesRegex(ValueError, "sections 1 through 37"):
            self.check()

    def test_duplicate_or_missing_heading_fails(self):
        self.metadata["document_heading_order"].append(self.metadata["document_heading_order"][0])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.check()
        self.metadata["document_heading_order"].pop()
        self.metadata["document_heading_order"].remove(self.metadata["sections"]["37"]["title"])
        with self.assertRaisesRegex(ValueError, "numbered inventory"):
            self.check()

    def test_changed_snapshot_bytes_fail(self):
        path = self.root / "docs/specification.txt"
        with path.open("ab") as stream:
            stream.write(b"\nchanged\n")
        with self.assertRaisesRegex(ValueError, "specification bytes"):
            self.check()

    def test_changed_section_digest_fails(self):
        self.metadata["sections"]["37"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "section 37 differs"):
            self.check()

    def test_reordered_heading_inventory_fails(self):
        self.metadata["document_heading_order"].reverse()
        with self.assertRaisesRegex(ValueError, "reordered"):
            self.check()


if __name__ == "__main__":
    unittest.main()
