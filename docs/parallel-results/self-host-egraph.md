# Self-hosting SH3 lane 8: `@vesper/egraph`

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1247. Base: `main` at `c8ac321`. Lane prompt:
[`docs/agent-prompts/self-host-sh3/08-egraph.md`](../agent-prompts/self-host-sh3/08-egraph.md).
Rust stays the default everywhere, and no Rust code was changed or deleted.
SH3 lanes 9 (`ir_opt`) and 10 (`optimize`'s search) build on this package.

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-egraph/{Loom.toml,README.md,src/*.ves}` |
| Test | `tests/self_host_egraph.rs` |
| Bazel data and timeout | `BUILD.bazel` (one data glob, one `long` timeout) |

`@vesper/egraph` is the part of egg 0.10 and egglog 3 the compiler uses, in
flat tables (see the [package README](../../packages/vesper-egraph/README.md)):

- `graph.ves`: hash-consed e-nodes (operator, one `i64` of data, up to two
  children), union-find with path halving, `add`/`add_uncanonical`,
  `union`, `lookup`, and `rebuild` (egg's `process_unions` then
  `rebuild_classes`).
- `rules.ves`: patterns as egg `PatternAst`s with data variables (for
  egglog's `(Read (Write s p v) q)`), egg's e-matching compiler (`Bind`,
  `Compare`), guards (`total(?x)`, `rel(p, q)`) and the relation table.
- `run.ves`: search and apply; `run_egg` is egg's `Runner` as
  `optimize/certificate.rs` configures it (iteration limit 8, node limit
  2048, match cap 128 per rule, the recording applier's id and record caps,
  the root-leaf hook, records of every successful union), and `run_rounds`
  is egglog's `(run n)`.
- `extract.ves`: `AstSize` extraction and `build_recexpr`'s node order.
- `lib.ves`: `optimize`'s language and its 18 rules, `ir_opt`'s terms and
  its 2 rules, and the two differential entries.

The Stage 1 guard holds by construction: the lane touches no file under
`src/`, `bootstrap/`, `lean/` or `packages/vesper-compiler/`, and installs
nothing in any compiler. The `bootstrap-stage1` workflow on the branch
checks the lock.

## Differential

`tests/self_host_egraph.rs` (numbers from a local release run on x86-64
Linux):

| Test | Inputs | Result |
| --- | --- | --- |
| `memory_matches_egglog` | ir_opt's egglog program, built as `ir_opt::optimize` builds it, on every block with a scalar read of every corpus program's IR and one mutant of each: 90,820 blocks, 162,350 reads, 20,477 forwarded | **0 differences** (also 0 on 204,332 blocks with all five mutants). CI compares every 20th block with egglog (`EGRAPH_MEMORY_STRIDE`) |
| `math_matches_egg` | optimize's rules as certificate.rs runs them (no witness budget) on 4,969 pure integer islands of the corpus, 3,000 generated expressions, 7 long wrapping chains | **0 differences in outcome or cost**; see below |
| `unoptimized_agrees` | 200 generated expressions, 5,000 blocks | the JIT with the optimizer off gives the same words |

For `optimize`'s rules each case compares the stop reason, the iterations,
the graph sizes, a digest of every id's original e-node, every application
record and the extracted term:

| Inputs | Exact | Both refused | Same term, other search order | Equal-cost tie | Other |
| --- | --: | --: | --: | --: | --: |
| corpus islands (4,969) | 4,885 | 0 | 26 | 58 (3 with other graph sizes) | 0 |
| generated (3,000) | 2,287 | 224 | 307 | 182 (98 with other graph sizes) | 0 |
| chains (7) | 1 | 3 | 0 | 3 (2 with other graph sizes) | 0 |

Every non-exact case has the same stop reason and the same root cost as
egg, and every tie's term evaluates like egg's on 64 assignments (checked
`+ - *` as failures, wrapping ops modulo 2^64). "Both refused" is a run
that hit a cap in both (certificate.rs installs nothing); only the records
before the failure differ in order.

### Why ties differ (owner decision)

egg (built without its `deterministic` feature) keeps classes in hashbrown
maps keyed by FxHash. It visits them in that map's order when it searches a
rule (`classes_by_op`) and when the extractor passes over classes. That
order is what decides which match is applied first (and so which class
becomes a union root, and sometimes how many duplicate e-nodes a stale
hash-cons makes in one iteration) and which of two equal-cost e-nodes is
kept. It depends on hashbrown's table layout, which also differs between
x86-64 (16-byte SSE2 groups) and AArch64 (8-byte groups), so egg's own
choices are not the same on every platform. The port visits classes in
ascending id order. Every other order egg has is reproduced (union roots by
parent count, parent lists into the pending stack, stale hash-cons keys,
sorted class e-nodes, `build_recexpr`'s order), which is why 98% of the
corpus matches byte for byte.

The plan's spec call for this lane asks the owner to decide whether
equal-cost results are acceptable. The alternative is to emulate
hashbrown's layout (insertion slots, tombstones, rehashing, group width per
platform) inside the port, which would make the self-hosted compiler's
output depend on a Rust library's internals. Lane 10 runs the certificate
checker on the port's terms when it swaps its search onto this package.

## Numbers

ABBA, best of six per side, release build, x86-64 Linux cloud host (wall
clock moves by up to 30% between runs):

| Task | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| `optimize`'s search + extraction, 7,976 expressions (`speed_math`) | egg 373 ms | 134 ms | 0.36x |
| `ir_opt`'s saturation, all 90,820 corpus blocks | egglog 410 s | 0.99 s | 0.002x |

The egg side builds the 18 rules per expression, as `run_graph` does; the
Vesper side includes decoding its input words. egglog's time is mostly
building an `EGraph` and parsing one program text per block, which is how
`ir_opt` uses it. The committed `speed_memory` times every 20th block so one
ABBA run stays under ten minutes. Callgrind runs are left for lanes 9 and 10, which measure
the passes themselves.

## Compiler gaps

- Constants cannot be imported yet (#1239), so `NONE`, the pattern kinds
  and the guard kinds are repeated in `rules.ves`, `run.ves` and `lib.ves`.
- Per-class lists would read best as `ChunkSeq[ChunkSeq[u32]]`; the port
  uses linked cells in two flat tables instead (egg's `concat_vecs` then
  costs O(1)). Element views (#1221) may make the direct form usable.

## What is left

- Lane 9 ports `ir_opt` onto `run_rounds`. `ir_opt`'s second egglog program,
  `FIP_RULES` (a join of slot and allocation tags), is a relational join
  with no rewriting; it is not covered here.
- Lane 10 ports `optimize`'s search onto `run_egg`, with the witness budget
  charges around search and apply, and translates the records and the raw
  e-nodes into proofs.
- If the owner wants egg's exact choices, a hash-order emulation would go
  behind the class visit order (`live` in `graph.ves`).
