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
outputs and preserve source file bytes. The deep-refusal case compares raw HIR
bytes rather than imposing serde's reader recursion ceiling on valid output.
Original semantic errors must precede optimization and retain actual diagnostics.
The harness accepts `VESPER_REWRITE_WITNESS_BINARY` with the Cargo executable
fallback, using bounded child lifetime/output and disposable temporary sources.

`scripts/check-rewrite-witness.py --binary REAL_VESPER` repeats actual source
checking, evaluator/JIT and HIR comparisons with independent JSON decoding. Its
inline corpus covers the same four integer kinds, six folds, high unsigned
values, totality, shadowed/original intrinsic provenance, barrier order and
whole-island refusal. No existing `.ves` fixture is edited. No public serialized
certificate, proof flag, fabricated World, Lean source or approval is supplied.

**Execution is pending coherent source integration.** The contract reserves all
compiler builds/tests and script execution to the coordinator's integration
worktree/cache. This lane has authored and rustfmt-checked its source and checked
whitespace only; none of the runtime/native/CLI assertions above is reported as
executed yet. The report will be updated from the coordinator's actual results.
No hosted, Lean, formal refinement, timing, RSS or performance claim is made.
