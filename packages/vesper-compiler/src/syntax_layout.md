# Self-hosted syntax buffer, `vesper.syntax.flat/1`

The parser accepts `source_len()` and `source_byte(i)` as before. It lexes
once, retains every token (including trivia and EOF), and writes a flat CST,
lexical name tables and diagnostics. No Rust parser participates in producing
these records.

`parse_transport() -> i64 eff [SyntaxMemory]` is the embedding entry point.
`SyntaxMemory` is a linear, synchronous memory interface: the caller must
resume every `read(index)` and `write(index, value)` exactly once. The test
adapter implements only these two operations over bounded storage; it has no
grammar, tokenization or diagnostic-message logic.

`syntax_run(consume)` implements that same interface entirely in Vesper. It
allocates a sparse radix-16 directory of region boxes inside a 256 MiB bounded
region, calls `consume`, then releases the region. `parse_main()` uses it with
`parse_digest`. A checker can instead supply a callback that parses and reads
the records before returning its own copy result. Region handles cannot escape.
The package's original five grammar files and every `parse_*.ves` and
`syntax_*.ves` file form one compilation unit; declaration order is immaterial.

## Byte representation

The logical buffer contains 1,048,576 nonnegative i64 cells (8,388,608 bytes).
The contiguous embedding buffer and the Vesper sparse backing implement the
same indices. Each cell's eight little-endian bytes are available through
`syntax_byte(cell_index, byte_in_cell)`. Addresses outside the buffer, negative
writes, and byte indices outside 0..8 raise typed `Diag::Limit`.

All indices below are **cell indices**, so multiply by eight for byte offsets.
Records never contain host pointers. Source ranges are half-open UTF-8 byte
ranges. Node IDs are zero-based; parent/owner links encode `id + 1`, with zero
meaning no parent/owner.

| Area | Base cell | Record cells | Maximum records |
| --- | ---: | --- | ---: |
| Metadata | 0 | See below | 16 cells |
| Tokens | 16 | kind, start, end, owning node + 1 | 65,537, including EOF |
| CST nodes | 270,000 | kind, start, end, parent + 1 | 131,072 |
| Lexical names | 800,000 | start, end, previous binding ID | 65,536 |
| Current generic bounds | 997,000 | start, end | 128 |
| Diagnostics | 1,000,000 | code, message template, start, end, arg0, arg1 | 128 |

Metadata cells 0..4 hold token count, node count, diagnostic count, name count
and current open node + 1. Cell 5 is reserved. Cell 6 holds the first error's
compatibility code, and cell 7 the current generic-bound count. Cells 8..16 are
reserved. Parsing resets this metadata; unused records are outside the counts.

Node zero is MODULE (200). The other batch kinds are ITEM (201), EXPR (202),
PATTERN (203) and TYPE (204). Nodes are stored in preorder. Parent links, byte
ranges, token order and token ownership reconstruct the complete ordered tree,
including trivia and empty nodes. EOF is stored in the token buffer but has
owner zero and is never a CST leaf. The tests compare every record with Rowan,
without relying on the compatibility digest. Editor-only BLOCK, ERROR_NODE
and INCOMPLETE wrappers are not part of the seed's batch CST.

## Diagnostics and outcomes

Diagnostic code cells contain 1 for E0001 and 2 for E0002. Template 1 renders
`expected {TokenKind}, found {TokenKind}` using its two arguments. The other
templates are static seed messages. `syntax_message_len` and
`syntax_message_byte` render the text in Vesper, including the seed token kind
names. Consumers need no separate message table. Literal errors deliberately
use the seed's post-consumption spans.

The compatibility return remains a nonnegative CST digest on success, or
`-1 - (code * 2^32 + offset)` on failure: lexical error 1, syntax error 2,
arithmetic overflow 3, literal error 4, limit 5. `Diag::Unsupported` remains
reserved. A limit is a typed failure, not a successful partial tree. Buffers
from a failed or interrupted phase must not be treated as checked syntax;
ordinary syntax errors retain recovered records and diagnostic text. A dangling
`copy`, `affine` or `linear` modifier at EOF reports E0002; this deliberately
rejects an incomplete declaration that the seed's batch progress guard currently
accepts. Its lossless CST is still retained.

The source ceiling is 1 MiB, the raw token ceiling 65,536 excluding EOF, the
seed descent ceiling 128, and the diagnostic ceiling 128. Node, name, generic
bound and storage ceilings above are additional explicit resource limits.
No allocation or compiler-correctness theorem is claimed.

## Execution boundary

The evaluator runs the complete parser, both with the embedding buffer and
with `syntax_run`'s Vesper region implementation. The seed's Cranelift backend
cannot currently compile this recursive parser with its storage effects:
recursive effect specialization exceeds its 64-inline-frame limit. The
native session backend also explicitly rejects recursive suspending call
cycles. The corpus therefore checks every parser record on the evaluator and
runs the lexer on the JIT for every fourth source and every large source.
This does not establish full-parser JIT or Stage 1 native execution.
