# @vesper/check-tensor-types

The checker half of `src/tensor_types` in Vesper (self-hosting wave SH2,
lane 14, #1186): shape kinds, symbolic dimensions, dtypes, substitution,
inference and the per-function obligations. Rust stays the default checker;
nothing here replaces Rust code.

## Entry point

`check_tensor_types(in input: Str) -> ChunkSeq[u64]` takes one source file
plus the results of the Rust wire layout checker (`src/tensor_types/wire.rs`
stays in Rust) and returns a text report packed with
`@vesper/check-data::pack_text`. The input layout and the report format are
written down in `src/lib.ves`. The report matches `tensor_types::analyze`
line for line: `inactive` for a file with no tensor use, else the
diagnostics (code, span, message), the ADT kinds and every signature with
its generics, parameters, result, KV pool and obligations.

## Files

| File | Rust | Vesper |
|---|---|---|
| `src/state.ves` | `dim.rs`, `types.rs` (`Dim`, `DType`, `Ty`, `Display`) | hash-consed dimension and type ids, dtype words, diagnostics |
| `src/kinds.ves` | `infer.rs` kind collection, `mentions_tensors`, scopes | `adt_kinds` fixed point, scope lookups, AST accessors |
| `src/subst.ves` | `infer.rs` `Subst`, `unify`, `settle` | association-list substitution with mark/undo instead of clone |
| `src/infer.ves` | `infer.rs` signatures, bodies, tensor and pool ops, `analyze` | the same arms in the same order |
| `src/lib.ves` | | input decoder and report writer |

## Tests

`tests/self_host_check_tensor_types.rs` compares the port with Rust over the
fixtures, the checker's own test sources, seeded mutants (set
`TENSOR_MUTANTS` per fixture, default 40) and the repository corpus.
`abba_against_rust` (ignored; `CHECK_HARNESS_SPEED=1`) times both.

**Function budget.** The native JIT refuses a package with more than 1,024
monomorphized functions. This package plus `@vesper/ast` lowering sits near
that limit, so small one-line helpers are written inline at their call
sites. Check the count before adding helpers.

Elaboration (`elaborate.rs`) is not ported yet: its map is private to the
Rust module, so it needs a swap point in `src/tensor_types` first.
