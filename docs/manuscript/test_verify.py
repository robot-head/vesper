"""Regression tests for manuscript source citations and sparse PDF pages."""
import tempfile
import unittest
from pathlib import Path

import pymupdf

from verify import check_lean_references, sparse_body


class ManuscriptChecksTest(unittest.TestCase):
    def test_lean_references_require_a_source_and_declared_theorem(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'lean').mkdir()
            (root / 'lean/Example.lean').write_text('theorem actual : True := by trivial\n')
            source = '```lean\n-- lean/Example.lean\ntheorem missing : True := by trivial\n```\n'
            self.assertEqual(check_lean_references(source, root),
                             ['Missing theorem missing in lean/Example.lean'])
            self.assertEqual(check_lean_references(source.replace('missing', 'actual'), root), [])
            self.assertEqual(check_lean_references(source.replace('Example', 'Absent'), root),
                             ['Missing Lean source: lean/Absent.lean'])
            self.assertEqual(check_lean_references(source.replace('lean/Example', 'illustrative/Example'), root), [])

    def test_sparse_body_ignores_header_and_footer(self):
        document = pymupdf.open()
        page = document.new_page(width=595, height=842)
        page.insert_text((70, 25), 'Running header ' * 10)
        page.insert_text((70, 810), 'Footer ' * 10)
        for i in range(4):
            page.insert_text((70, 100 + i * 14), '    }')
        self.assertTrue(sparse_body(page))
        for i in range(4, 8):
            page.insert_text((70, 100 + i * 14), 'More body text')
        self.assertFalse(sparse_body(page))


if __name__ == '__main__':
    unittest.main()
