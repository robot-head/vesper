#!/usr/bin/env python3
"""Regenerates src/qpack/static_table.rs and src/qpack/huffman_table.rs.

Usage: gen_tables.py RFC9204_MD H2_HUFFMAN_TABLE_RS CRATE_DIR

RFC9204_MD is rfc9204.md from github.com/quicwg/base-drafts (main);
H2_HUFFMAN_TABLE_RS is src/hpack/huffman/table.rs of the h2 0.4.12 crate.
"""
import re
import sys

rfc, h2, crate = sys.argv[1:4]
rows = []
on = False
for line in open(rfc):
    if line.startswith("# Static Table"):
        on = True
    if on and line.startswith("{: title"):
        break
    m = re.match(r"\|\s*(\d+)\s*\|\s*(\S+)\s*\|(.*)\|\s*$", line) if on else None
    if m:
        # Undo Markdown escapes such as \* and \'.
        value = re.sub(r"\\(.)", r"\1", m.group(3).strip())
        rows.append((int(m.group(1)), m.group(2), value))
assert [r[0] for r in rows] == list(range(99)), len(rows)


def lit(s):
    return 'b"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


out = [
    "//! The QPACK static table (RFC 9204 Appendix A), generated from the RFC",
    "//! text (quicwg/base-drafts `rfc9204.md`) by `tests/fixtures/gen_tables.py`.",
    "//! Do not edit by hand.",
    "",
    "/// `(name, value)` for static indices 0..=98.",
    "pub const STATIC_TABLE: [(&[u8], &[u8]); 99] = [",
]
out += [f"    ({lit(n)}, {lit(v)})," for _, n, v in rows]
out.append("];")
open(crate + "/src/qpack/static_table.rs", "w").write("\n".join(out) + "\n")

txt = open(h2).read()
enc = txt.split("ENCODE_TABLE")[1].split("];")[0]
pairs = re.findall(r"\((\d+),\s*(0x[0-9a-f_]+)\)", enc)
assert len(pairs) == 257
codes = [(int(n), int(b.replace("_", ""), 16)) for n, b in pairs]
out = [
    "//! The HPACK Huffman code (RFC 7541 Appendix B), shared by QPACK",
    "//! (RFC 9204 §4.1.2). Generated from the `h2` 0.4.12 crate's `ENCODE_TABLE`",
    "//! by `tests/fixtures/gen_tables.py`; the unit tests check that the code",
    "//! is canonical and complete. Do not edit by hand.",
    "",
    "/// `(bit length, code)` per symbol; symbol 256 is EOS.",
    "pub const CODES: [(u8, u32); 257] = [",
]
out += [f"    ({l}, 0x{c:x})," for l, c in codes]
out.append("];")
open(crate + "/src/qpack/huffman_table.rs", "w").write("\n".join(out) + "\n")
