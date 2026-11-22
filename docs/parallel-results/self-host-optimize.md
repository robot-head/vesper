# Self-hosting SH3 lane 10: the AST optimizer and its certificates

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1249. Base: `main` at `c8ac321`. Lane prompt:
[`docs/agent-prompts/self-host-sh3/10-optimize.md`](../agent-prompts/self-host-sh3/10-optimize.md).
Rust stays the default optimizer, and no Rust code was deleted.

The lane lands in several PRs. This report grows with each one.

| Part | PR | What |
| --- | --- | --- |
| 1 | #1300 | the certificate checker and candidate builder (`optimize/witness.rs`), the swap point |
| 2 | #1375 | the search on `@vesper/egraph`: language, rules and guards, islands, refusals, the proof forest of `optimize/certificate.rs` |
| 3 | this PR | the swap into the hybrid build (`optimize` module), the full suite on it, the scoreboard row |

## Part 1: the certificate checker

### What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-optimize/{Loom.toml,README.md,src/*.ves}` |
| Swap point | `src/optimize.rs`: `OptimizeTable`, `install_optimize`; `install_proposal` calls an installed table's `verify` and otherwise runs the Rust gate exactly as before |
| Budget hand-over | `src/optimize/witness.rs`: `Spent`, `WitnessBudget::spent`, `verify_spent` (additive) |
| Test | `tests/self_host_optimize.rs` |
| Bazel data and timeout | `BUILD.bazel` |
| 128-bit kinds in the checker ports | `packages/vesper-check-data/src/world.ves`, `packages/vesper-check-core-typing/src/ints.ves` |

`src/optimize/witness.rs` is outside the lane's exclusive paths. The swap
point has to continue the producer's budget across the language boundary,
and the budget's counters are private to that module, so this adds a
read accessor and `verify_spent`, which rebuilds a budget from its counters
and runs the unchanged `verify_with_budget`. Nothing existing changed. The
default compiler installs no table, so its optimizer runs the same code as
before.

Ported (`src/optimize/witness.rs` to `@vesper/optimize`):

- `budget.ves`: `WitnessBudget` (`work`, `metadata`, `names`, `reserve`,
  the limit validation of `new`).
- `witness.ves`: `bind_source`, `validate_terms`, `compare_input`,
  `compare_output`, `build_candidate`, `check_rule` (all 24 rules, their
  substitutions and totality guards) and `verify_with_budget`.
- `input.ves`: the expressions as `@vesper/ast` nodes (an `Ast` with two
  capacity columns), the type map entries the checker asks for, and the
  certificate.

### Same answer, same budget

The checker is a resource-bounded gate, so the differential compares more
than accept or refuse. Each line it compares is the `WitnessError` (or
`Ok`) and the three counters the budget spent (`work`, `metadata`,
`names`). A refusal by `Limit` at a different charge site, or one byte of
`metadata` more, is a difference.

Rust charges `metadata` for every vector growth by the capacity
`Vec::try_reserve_exact` asks for, and by `size_of` of the element types.
So the Vesper checker keeps a capacity counter beside each sequence that
plays a Rust vector's part, and the `S_*` constants are Rust's sizes on
64-bit targets. `rust_sizes_match_the_vesper_constants` checks the five
public ones (`Term` 48, `Equality` 36, `Expr` 240, `Arg` 256, `Type` 96
bytes). The private frame types (`SourceFrame` 16, `Origin` 16, `Shape` 32,
...) are checked by the counters matching on every case. The input also
carries the capacities of the candidate's strings and vectors and of the
certificate's two vectors, which an AST does not keep.

### Differential

The test installs a recording table at the swap point. Every proposal the
Rust optimizer makes reaches it with the producer's remaining budget, and
the table records it and answers with Rust's `verify_spent`. Proposals come
from:

| Source | Proposals |
| --- | --: |
| every checked corpus world (lane 4's harness) | 95 |
| the fixtures of `optimize/certificate.rs` and `adversarial_tests.rs` | 23 |
| 400 generated integer islands (four kinds, checked and wrapping, depth 1-5) | 148 |

All 266 are accepted by Rust. For each one:

- The Vesper checker gives the recorded answer and counters: 0 differences.
- The Vesper `build_candidate`, run on the certificate's output, rebuilds
  the Rust candidate exactly (spans, operator and intrinsic names, callee
  origins, argument modes): 0 differences.

Then 59,366 mutants of those proposals change one thing each: versions,
conclusion, input and output indices, each term's kind, value, operator,
tag and children, each proof's reason, endpoints, rule number,
substitution and guard, the candidate's names, literals, operators, callee
spans, argument modes and type arguments, the source's callee and
literals, the type map, every limit at tight values, and the work, metadata
and name budgets cut at twelve points through the check. Rust's
`verify_spent` and the Vesper checker give the same line on every one:
0 differences. Every `WitnessError` but `Allocation` (which only a failed
allocation raises) occurs:

| Code | Error | Mutants |
| --: | --- | --: |
| 0 | Ok | 10,366 |
| 1 | InvalidLimits | 532 |
| 2 | Limit | 13,236 |
| 4 | UnsupportedAst | 312 |
| 5 | MissingType | 532 |
| 6 | TypeMismatch | 2,302 |
| 7 | ConstantOutOfRange | 3,418 |
| 8 | UnknownVariable | 147 |
| 9 | InvalidIntrinsicOrigin | 354 |
| 10 | UnsupportedVersion | 532 |
| 11 | InvalidTermIndex | 3,332 |
| 12 | NonTopologicalTerm | 930 |
| 13 | InvalidProofIndex | 776 |
| 14 | NonTopologicalProof | 2,723 |
| 15 | UnknownRule | 1,630 |
| 16 | InvalidSubstitution | 1,644 |
| 17 | InvalidRuleInstance | 5,905 |
| 18 | UnprovenTotality | 29 |
| 19 | InvalidEquality | 6,947 |
| 20 | InputMismatch | 2,431 |
| 21 | OutputMismatch | 1,288 |

The JIT with the optimizer off gives the same lines as with it on
(`unoptimized_agrees`, every fifth proposal and its mutants).

The debug test binary runs in about 12 minutes: 8 of them load and check
the corpus and run the Rust optimizer over it, and 4 check and compile the
package for the JIT.

### Numbers

Release build, ABBA (best of six per side, 20 rounds each) over the 266
genuine proposals. The Rust side runs `verify_spent` on prepared copies.
The Vesper side decodes its input words into an `Ast` and checks; the
decode-only entry `optimize_decode` is timed separately and subtracted.

| | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| checker with its input decode | 13.2 ms | 44.4 ms | 3.36x |
| checker alone (decode 18.8 ms subtracted) | 13.2 ms | 25.6 ms | 1.94x |

Callgrind instruction counts, one round over the 171 proposals of the
fixtures and the generated islands (`OPTIMIZE_SKIP_CORPUS=1`; each side
collected alone with `--toggle-collect='*measured_side*'`):

| | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| checker with its input decode | 3.69 M | 16.21 M | 4.39x |
| checker alone (decode 8.61 M subtracted) | 3.69 M | 7.60 M | 2.06x |

The decode costs as much as the check because each case builds a fresh
`Ast` (29 arenas and an interner). Part 3's swap will pass the module once
per island set, not once per proposal, and the search of part 2 builds the
proposals in Vesper, so no decode remains on that path.

### Stage 1 guard

No guarded path is touched (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`,
`packages/vesper-compiler/**`). The bootstrap-stage1 workflow result is
linked from the PR.

### Compiler gaps

Minimal repros, each refused by `vesper check` on `main` at `c8ac321`:

1. **No `saturating_add_k` builtin.** `checked_add_k` exists, so
   `witness.ves` writes its own `saturating_add` over it.

   ```text
   fn f(a: u64) -> u64 { saturating_add_u64(a, 1) }   // E0201 unknown name
   ```

2. **A tuple cannot be a type argument.** `ChunkSeq::new[(u32, u32)]()`
   does not parse as a call (E0201 `ChunkSeq::new`, then "indexing requires
   a tuple"), so the two work stacks use the copy structs `Pair` and
   `OutFrame`.

   ```text
   fn main() -> u64 { let mut s = ChunkSeq::new[(u32, u32)](); s.push((1, 2)); s.len() }
   ```

3. **`if` is not an operand.** `a && if b { .. } else { .. }` is E0002
   "expected expression"; Rust's `check_rule` writes its rule checks this
   way, and the port binds the `if` to a `let` first.

   ```text
   fn f(a: bool, b: bool) -> bool { a && if b { true } else { false } }
   ```

4. **Constants cannot be imported** (known; #1239 is open). `budget.ves`
   and `witness.ves` each declare the error codes they use.

### Gaps in other ports

The package is the corpus's first world with `i128` in its types, and the
checker ports did not know the 128-bit kinds yet. `@vesper/check-data`'s
`Known::is_integer` now names `i128` and `u128` (so their mode is copy, as
in Rust), and `@vesper/check-core-typing`'s `int_bits`, `int_unsigned` and
`fits` give them 128 bits. Before, `self_host_check_data` (a struct holding
`ChunkSeq[TermRow]` came out linear, Rust says affine) and
`self_host_check_core_typing_exprs` (`value: 0` into an `i128` field gave
E0200) each failed on this package alone.

## Part 2: the search

### What changed

| Area | Files |
| --- | --- |
| Producer | `packages/vesper-optimize/src/search.ves` (new), `lib.ves` (`optimize_propose` and two speed entries), `Loom.toml` (depends on `@vesper/egraph`) |
| E-graph driver hooks | `packages/vesper-egraph/src/run.ves`: `matches_new`, `search_rule`, `apply_one`, `guard_holds` made `pub`; `Matches::{mark, range, val}` (additive) |
| Swap point | `src/optimize.rs`: `OptimizeTable::propose` (`Option<ProposeFn>`), `Proposal`, `Produced`, `propose_rust`; `visit` uses an installed producer with the installed gate, else runs exactly as before |
| Candidate rebuild | `src/optimize/witness.rs`: `rebuild_candidate` (additive) |
| Test | `tests/self_host_optimize_search.rs` (new); `tests/common/check_harness.rs` adds `map_checked_corpus`, which `tests/self_host_optimize.rs` uses too |
| Bazel data and timeout | `BUILD.bazel` |

`@vesper/egraph` is SH3 lane 8's package. The Rust producer drives egg's
`Runner` one rule at a time through a budgeted scheduler and a recording
applier, so the port needs the same per-rule entry points. The edit only
makes four functions public and adds three read accessors; the lane 8
differential is unchanged.

Ported (`src/optimize/certificate.rs` to `search.ves`):

- Island admission: `Barrier`, `Unchanged`, and every refusal
  `WitnessError`.
- Constant folding (rules 19-24) with prefix folding of operand chains.
- The budgeted search: egg 0.10's `Runner` order (limit checks, the
  root-leaf hook, search then apply per rule, rebuild, saturation), the
  `BoundedScheduler` charges and its 128-match limit, the
  `RecordingApplier` charges and caps, and egg's compacted pattern lengths.
- Extraction by AST size, the candidate and its term table.
- The proof forest (`Builder`): union with proof edges, reflexive,
  reverse and transitive steps, congruence, guards, and explanation of the
  recorded applications.
- Budget parity: the same Rust `Vec` capacity model as part 1, so
  `work`, `metadata`, `names` and the certificate vectors' capacities match.

### Differential

The test installs a recording producer at the swap point. Every expression
the Rust optimizer visits reaches it; it records Rust's `propose_rust`
answer and returns it, so the optimizer runs unchanged. The Vesper
producer then answers each recorded visit, and the two are compared on the
outcome (barrier, unchanged, error code, candidate), and for a candidate on
the candidate's dump, every term and proof, and the spent counters and
capacities.

| Source | Visits |
| --- | --: |
| every checked corpus world: every candidate and error, every 64th barrier and unchanged visit (`OPTIMIZE_SAMPLE_STEP`) | 15,998 |
| the fixtures of `optimize/certificate.rs` and `adversarial_tests.rs` | 48 |
| 600 generated islands (depth 5 and depth 8) | 1,200 |

| Outcome | Visits |
| --- | --: |
| barrier | 13,474 |
| unchanged | 3,339 |
| error | 70 |
| candidate | 363 |

Result: 17,146 identical, 100 different only in search order, 0
differences. The JIT with the optimizer off gives the same answers
(`unoptimized_agrees`).

The search-order cases are the difference the lane prompt allows.
`@vesper/egraph` visits classes in id order and egg in hash order, so
extraction can pick another tree of the same cost. A tie passes when the
Vesper certificate passes Rust's `verify_spent`, Rust's
`rebuild_candidate` of it equals the Vesper candidate, and both candidates
have the same size. The other kind of tie is Vesper answering `Unchanged`
where egg picks a same-size reordering, for example
`wrapping_add(wrapping_mul(y, 100), wrapping_add(wrapping_mul(x, 1000), r))`
with its operands swapped. That tie passes when Rust's candidate has the
source's size. There is no egg hash-order emulation, as agreed.

The whole corpus makes about 900,000 visits, nearly all barriers and
unchanged leaves. The sample keeps the debug test at about 6 minutes on 4
cores: 4.5 recording the corpus (`check_harness::map_checked_corpus`
parses and checks each world once, on worker threads) and 1.5 compiling
the package and answering in Vesper. Part 1's test now records its
proposals the same way: 12 minutes down to under 6.

The speed numbers below were taken over the 15,838 visits of the corpus
before main grew to these 17,246.

### Numbers

Release build, ABBA (best of six per side, five rounds each) over all
15,838 visits. The Rust side runs `propose_rust` on the recorded sources.
The Vesper side decodes its input words and proposes, writing one line per
case and not the proposal. The decode-only entry is timed separately and
subtracted.

| | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| producer with its input decode | 807 ms | 638 ms | 0.79x |
| producer alone (decode 90 ms subtracted) | 807 ms | 548 ms | 0.68x |

Callgrind instruction counts, one round over the 1,248 visits of the
fixtures and the generated islands (`OPTIMIZE_SKIP_CORPUS=1`; each side
collected alone with `--toggle-collect='*measured_side*'`):

| | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| producer with its input decode | 277.8 M | 296.0 M | 1.07x |
| producer alone (decode 25.2 M subtracted) | 277.8 M | 270.7 M | 0.97x |

The generated islands are the deep ones (up to depth 8, all wrapping), so
the search does most of its work there, and the two sides come out about
even. The corpus sample is mostly barriers and small islands, where the
Vesper side is ahead on wall clock.

No profile went into why the Vesper side is ahead. A likely reason is
that egg hashes every e-node into its memo and class maps, while
`@vesper/egraph` keeps flat `ChunkSeq` columns indexed by class id.

### Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed.

## Part 3: the hybrid swap

### What changed

- `crates/vesper-hybrid/modules/optimize` (`@vesper/hybrid-optimize`) wraps
  `@vesper/optimize` in two export ABI v4 entries, `propose_words` and
  `verify_words`. The hybrid installs both through `install_optimize`, so
  the Vesper search proposes each rewrite and the Vesper checker checks it.
  Rust stays the default compiler's optimizer.
- The input is split. The case words carry each name's length only, and
  the names' text crosses as one `Str` (`read_names_text` in `input.ves`,
  entries `optimize_propose_split` and `optimize_verify_split`). The old
  entries, which pack names into the words, stay for the part 1 and 2
  tests.
- The Rust side of the swap builds the candidate from the certificate
  Vesper returns (`witness::rebuild_candidate`), with the certificate's
  term and proof capacities reserved as Vesper reported them, so the
  checker's metadata budget is spent the same way.
- The word encoders of the part 1 and 2 tests moved to
  `tests/common/optimize_words.rs`, which the hybrid reuses.
- A `divergent-optimize` feature makes the module propose `B` for every
  case. `hybrid.yml` checks that the differential catches it.

### Differential

`crates/vesper-hybrid/tests/differential.rs` runs 538 files, 1,614 runs per
compiler (`check`, `emit --emit ir`, object). 0 runs differ.

6 runs differed before the tie rule: `emit --emit ir` and the object for
`examples/traits/shapes.ves`, `tests/fixtures/bounded_generics/nested.ves`
and `tests/fixtures/native_handlers/shared_locals.ves`. These are the
equal-cost extraction ties of part 2 (spans 1049:1135, 532:611,
1201:1320), which the owner accepted. Main later added
`tests/fixtures/wide_literals.ves` (#1264), whose
`wrapping_add(score, wrapping_add(wrapping_mul(c, 10), ...))` is the same
kind of tie (span 1522:1598), so the list has four files. On that main the harness runs 553 files,
1,659 runs per compiler: 0 differ, 8 are accepted ties. The harness lists these files in
`OPTIMIZE_EQUAL_COST_TIES`. For a mismatch on one of them, it reruns the
hybrid with `VESPER_HYBRID_RUST_PRODUCER=1` (Rust search, Vesper checker).
If that run equals the default, it counts the run as a tie. A mismatch on
any other file, or one the Rust search does not explain, still fails. A
run is eligible only when both compilers succeed with the same
diagnostics and only the emitted IR or object differs.

The export glue test (`crates/vesper-hybrid/tests/codegen_export.rs`)
meets one more tie: `semver` built with the optimizer, where the Vesper
search keeps `wrapping_add(wrapping_add(start, acc), seg)` and egg picks
the same-size `acc, start` order. That build is compared again with the
Rust search, so its glue is still checked byte for byte.

The split entries take a batch of several cases. Each case's names follow
the previous case's in the one `Str`, and a cursor carries the offset
across cases. A hybrid unit test (`split_batches_keep_the_names_cursor`)
checks that a two-case batch answers as the two cases do one by one.

The divergent build is caught: 10 runs differ, the first on
`emit --emit ir tests/fixtures/bounded_generics/hash.ves`. The rest of the
hybrid tests pass. The pilot test checks that the module proposed for at
least one expression (21 on its program).

### Numbers

Whole corpus, wall clock of the differential's 1,614 runs per compiler
(debug builds, 4 cores, both compilers run file by file), three runs:

| Run | Default | Hybrid | Ratio |
| --- | --: | --: | --: |
| 1 | 30.11 s | 29.32 s | 0.97x |
| 2 | 28.29 s | 28.22 s | 1.00x |
| 3 | 27.73 s | 26.89 s | 0.97x |
| 4, on main with #1371 (553 files) | 29.31 s | 25.40 s | 0.87x |

The optimizer is a small part of a whole compile, so this mostly says the
swap costs nothing measurable end to end. The pass alone is part 2's
producer (release, ABBA 0.68x; callgrind 0.97x) plus part 1's checker
(ABBA 1.94x; callgrind 2.06x).

### Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed.

## What is left

- Nothing for this lane. Deleting the Rust optimizer is a later call under
  the 1.5x rule: the producer is within it, and the checker is not yet
  (1.94x alone).
