# @vesper/egraph

An e-graph in Vesper for the compiler's two equality saturation passes
(self-hosting wave SH3, lane 8, #1247; gap S7 of the
[self-hosting plan](../../docs/maintenance/self-hosting-plan.md)).
`src/optimize.rs` runs egg 0.10 on pure integer expressions and
`src/ir_opt.rs` runs egglog 3 on memory observations. Both rule sets are
small, so this package is the part of egg and egglog they use, not a general
egglog. Rust stays the default: nothing here replaces Rust code. SH3 lanes 9
(`ir_opt`) and 10 (`optimize`) port those passes onto it.

| File | What | Rust it follows |
|---|---|---|
| `src/graph.ves` | hash-consed e-nodes in flat tables, union-find, `add`, `union`, `rebuild` | egg `EGraph` (`add_uncanonical`, `perform_union`, `process_unions`, `rebuild_classes`) |
| `src/rules.ves` | patterns, the e-matching compiler, guards, the relation table | egg `Pattern`, `machine::Compiler`, `ConditionalApplier`; egglog `(relation disjoint ..)` |
| `src/run.ves` | search, apply, egg's runner, egglog's `(run n)` | egg `Runner::run_one`, certificate.rs's `BoundedScheduler`/`RecordingApplier`/hook |
| `src/extract.ves` | `AstSize` extraction and the best expression | egg `Extractor`, `build_recexpr` |
| `src/lib.ves` | `optimize`'s language and 18 rules, `ir_opt`'s terms and 2 rules, the differential's entries | `optimize::rules()`, `ir_opt::RULES` |

## Data

An e-node is an operator, one `i64` of data (a constant, a variable number,
a place) and up to two children. An operator's arity is fixed by the
language table a graph is made with. egglog's `(Write S i64 V)`,
`(Barrier S i64)` and `(Read S i64)` keep their `i64` as data, so one
e-node shape covers both passes. Ids are `u32`; every table is a
`ChunkSeq` of scalars indexed by id.

## What matches egg, and what does not

The port keeps every egg detail that decides a result:

- An id is made only for a new class, and `r_*` (egg's `nodes`) holds each
  id's original e-node, append-only. certificate.rs replays these.
- A union makes the class with more parents the root (ties keep the first
  argument); parent lists keep egg's order (`concat_vecs` appends the
  shorter list) and a union pushes the absorbed class's parents onto the
  pending stack.
- The hash-cons keeps stale keys, so its size is egg's `total_size()`, which
  the node limit reads.
- A rebuild sorts and deduplicates each class's canonical e-nodes by egg's
  derived `Ord` (operator in variant order, data, children), so e-matching
  yields substitutions in egg's order.
- The runner checks limits, runs the hook, searches every rule, applies
  rule by rule and rebuilds exactly as `Runner::run_one` does, with the
  match cap and the record/id caps certificate.rs adds.
- Extraction takes the first least-cost e-node in sorted order and only
  replaces it with a strictly cheaper one; `find_best` emits nodes in
  `build_recexpr`'s order.

One thing is not kept: egg (without its `deterministic` feature) stores
classes in a hashbrown map with FxHash, and visits them in that map's order
when it searches a rule and when the extractor passes over classes. That
order depends on hashbrown's table layout, which also differs between x86-64
(16-byte SSE2 groups) and AArch64 (8-byte groups), so egg's own results are
not the same on every platform. The port visits classes in ascending id
order. The order changes which equal-cost term is extracted, and, rarely,
the order of unions. The differential reports both (see the lane report).

## Tests

`tests/self_host_egraph.rs`:

- `math_matches_egg`: optimize's rules as certificate.rs runs them on every
  pure integer island of the corpus, generated expressions and long
  wrapping chains, against egg.
- `memory_matches_egglog`: ir_opt's program on every block of every corpus
  program's IR and its mutants, against egglog.
- `unoptimized_agrees`: the JIT with the optimizer off.
- `speed_math`, `speed_memory` (ignored; `CHECK_HARNESS_SPEED=1`): ABBA.
