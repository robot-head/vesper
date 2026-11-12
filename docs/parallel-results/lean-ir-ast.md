# Pure Lean CFG lowering through syntax nodes

The pure CFG/state-passing backend now exposes `extract_function_nodes(world, function) -> Option<Result<Vec<vesper_lean_syntax::Declaration>, String>>`. It constructs declarations, binders, types, terms, patterns, projections, mutable do statements, and lifted closure/block definitions through `vesper-lean-syntax`. The compatibility string extraction wrapper has been removed. Rendering belongs to the enclosing module boundary supplied by the effect-backend migration.

The migration preserves live-slot ordering, state-return ordering, mutable callbacks, disjoint field updates, closure capture/lifting, and checked-versus-wrapping arithmetic. Source names remain validated names rather than Lean source fragments. Remaining formatting in `src/lean_ir.rs` only creates generated identifiers. There are no term render/parse round trips or raw Lean nodes.

Existing eligibility boundaries remain: effectful or unsupported CFGs defer to the effect backend. Float patterns now return an explicit unsupported error: the previous emitted typed float pattern fails actual Lean dependent elimination. Float expressions use the shared scalar-literal helper. This change does not claim termination or correctness proofs for partial definitions.

The four added integration tests compare reference-evaluator output with actual Lean `#eval` for signed patterns, generic closure capture, CFG edges, mutable state ordering, disjoint updates, tuple destructuring, checked and wrapping scalars, and strings containing Lean commands/control characters. Standard-library references use the actual globals `Pure.pure` and `Decidable.decide`.

## Validation

All final builds used the isolated target `/workspace/vesper-targets/lean-ir-ast`; earlier shared-target runs were excluded because Cargo artifacts had crossed worktree boundaries. The commands below ran with:

```sh
export CARGO_TARGET_DIR=/workspace/vesper-targets/lean-ir-ast
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_INCREMENTAL=0
export VESPER_LEAN=/home/agent/.elan/bin/lean
```

```sh
rustfmt --edition 2024 src/lean_ir.rs tests/lean_ir_ast.rs
cargo test -p vesper --locked --jobs 2 --test lean --test lean_ir_ast --test lean_literal_syntax -- --test-threads=2
cargo test -p vesper --locked --jobs 2 --test checked_arithmetic -- --test-threads=2
cargo clippy -p vesper --locked --jobs 2 --all-targets -- -D warnings
git diff --check
```

Results: 52 existing Lean tests, four new CFG regressions, one scalar-literal test, and 18 checked-arithmetic tests passed (75 total), using Lean 4.24.0. Strict all-target Clippy and formatting checks passed. Checked-arithmetic coverage also exercises the evaluator, optimizer, JIT, and native object emission.

During isolated validation only, the old `src/lean.rs` caller had an uncommitted adapter to render the returned declarations and an allowance on its obsolete string update helper. Those temporary changes were restored and are not part of these commits; the coordinated effect-backend migration consumes the node API directly and replaces those old helpers. Applying this lane alone therefore requires the corresponding effect-backend caller migration.

Implementation commits: `181a3af` (structured pure CFG lowering and tests), `fbfda75` (remove compatibility rendering wrapper).
