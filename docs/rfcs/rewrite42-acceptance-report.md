# Independent AST rewrite runtime and CLI acceptance (#235)

Public claim #235 and the published `rewrite42-contracts.md` contract preceded
writes. The coordinator's `c42edb0` amendment is copied as `9989218` here; it is
not acceptance-owned work. Owned paths are only the two new Rust tests, Python
harness and this report. Existing fixtures, source implementation, main/native,
Cargo/dependencies and build/workflow files remain outside this lane.

The actual CLI enables AST optimization by default and supports `--no-opt`.
There is no new `--opt` alias. `check` returns before optimization, so acceptance
uses it only for baseline source validity; actual `run`, `jit` and `emit hir`
exercise the default gate and its original-source comparison path.

Eight runtime tests use real parsed and checked source, the real matching
TypeMap, actual `optimize::optimize`, and a fresh check of the result. Both
baseline and rewritten Worlds run through evaluator and native JIT with their
existing IR optimization disabled and enabled. Structural assertions require:

- All six constant-fold families actually become the expected literal at each
  of u8/u32/u64/i64. Runtime agreement alone cannot pass via an inert optimizer.
- Checked identities and a prior-totality-supported erasure genuinely simplify.
  Erasure of a possibly overflowing checked subterm must remain impossible for
  checked multiply/zero/self-subtraction and wrapping multiply/self-subtraction.
- High-u64 products exceeding signed i128's product range, wide in-range folded
  constants, modulo products and signed endpoints retain exact values/failure.
- A surviving wrapping call after nested identity simplification copies an
  actual original same-operation callee name/span, including original UTF8 byte
  association and unchanged call argument conventions.
- All three locally shadowed intrinsic spellings remain typed source calls;
  their original callee expression stays intact while pure constant arguments
  independently fold. No synthetic callee span can authorize an intrinsic.
- Pure child islands inside division/call/handler barriers may rewrite while
  mutable calls and distinct failure kinds retain strict observable order.
- A source-admitted but depth-refused maximal island remains byte-for-byte AST
  equal, including child nodes/spans. It cannot silently retry smaller children.
  Identical checked inputs produce identical output modules.

Five actual executable CLI tests independently compare default execution against
`--no-opt`, inspect serialized HIR folds and callee identity, test deterministic
outputs and preserve source file bytes. The input-node-refusal case uses a
balanced addition tree with 257 leaves and 256 operators. Its actual parsed HIR
must contain 513 expression occurrences at depth 10, exceeding the default
512-input-node admission limit while staying shallow. It compares raw HIR
bytes and requires the complete original island, including children and spans.
Original semantic errors must precede optimization and retain actual diagnostics.
The harness accepts `VESPER_REWRITE_WITNESS_BINARY` with the Cargo executable
fallback, using bounded child lifetime/output and disposable temporary sources.

`scripts/check-rewrite-witness.py --binary REAL_VESPER` repeats actual source
checking, evaluator/JIT and HIR comparisons with independent JSON decoding. Its
inline corpus covers the same four integer kinds, six folds, high unsigned
values, totality, shadowed/original intrinsic provenance, barrier order and
whole-island refusal. No existing `.ves` fixture is edited. No public serialized
certificate, proof flag, fabricated World, Lean source or approval is supplied.

Actual execution is performed by the coordinator, as required by the published
contract. The earlier Linux critical integration head `2260c6a` passed:

- All 129 critical tests across six targets, including all eight runtime tests
  and five actual CLI tests, with zero failures or ignores. Genuine nested
  wrapping simplification and original callee provenance assertions pass.
- The actual executable Python harness's 340 commands across 49 inline programs,
  covering checked baseline validity, evaluator/JIT agreement, installed HIR,
  deterministic output and unchanged whole-island refusal.
- Strict package all-target Clippy with `-D warnings`. Writer formatting and
  whitespace checks also pass. This writer performs no local compiler build or
  executable run; these runtime results come from the coordinator's actual
  integration log and final binary.

The initial coordinator pass exposed ill-typed positive fixtures and a genuine
producer-budget refusal on a required simplification. The fixture correction
adds existing effect rows/lexical handlers around checked variable arithmetic,
preserving every structural expectation. The direct in-process runtime test
still retains its depth-70 original-AST comparison. Successful identity and
resource-refusal cases use a distinct failure fallback.
The producer owner separately fixes its bounded accounting. No compiler effect
policy, optimization expectation or witness cap is relaxed.

PR238's Windows run subsequently exposed a stack overflow in the CLI's original
left-associated depth-70 fixture. The other four CLI tests and strict Clippy
passed in that run; this was not a passing Windows acceptance result. The CLI
and independent Python refusal cases now use the balanced 513-node fixture
described above. Both inspect actual unoptimized HIR occurrence/depth counts,
retain deterministic byte equality, and compare evaluator/JIT results with and
without optimization. CLI success failures now identify command mode, options
and exit status. No main-thread stack policy, compiler resource limit or
workflow changes are part of this correction.

Before this fixture update, the coordinator executed the exact balanced source
with a Linux 1 MiB stack: JSON checking, unoptimized/optimized HIR, evaluator and
JIT all passed (seven commands). Both HIR byte streams were identical at 377771
bytes, and all four execution commands returned 42. This evidence belongs to the
coordinator's actual binary probe, not a writer build. The updated Rust/Python
test runs and Windows rerun remain pending at this commit; the low-stack Linux
probe does not claim Windows success.

The earlier 129-test targeted result does not claim whole-workspace execution.
No hosted, Lean, full Rust/source refinement,
self-hosting/DDC, timing, RSS or production-performance claim is made.
