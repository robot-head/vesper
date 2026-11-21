# Self-hosting SH3 lane 10: the AST optimizer and its certificates

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1249. Base: `main` at `c8ac321`. Lane prompt:
[`docs/agent-prompts/self-host-sh3/10-optimize.md`](../agent-prompts/self-host-sh3/10-optimize.md).
Rust stays the default optimizer, and no Rust code was deleted.

The lane lands in several PRs. This report grows with each one.

| Part | PR | What |
| --- | --- | --- |
| 1 | this PR | the certificate checker and candidate builder (`optimize/witness.rs`), the swap point |
| 2 | | the search on `@vesper/egraph` (after SH3 lane 8's first PR): language, rules and guards, islands, refusals, the proof forest of `optimize/certificate.rs` |
| 3 | | the swap into the hybrid build (`optimize` module), the full suite on it, the scoreboard row |

## Part 1: the certificate checker

### What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-optimize/{Loom.toml,README.md,src/*.ves}` |
| Swap point | `src/optimize.rs`: `OptimizeTable`, `install_optimize`; `install_proposal` calls an installed table's `verify` and otherwise runs the Rust gate exactly as before |
| Budget hand-over | `src/optimize/witness.rs`: `Spent`, `WitnessBudget::spent`, `verify_spent` (additive) |
| Test | `tests/self_host_optimize.rs` |
| Bazel data and timeout | `BUILD.bazel` |

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

CALLGRIND

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

### What is left

- Part 2: the search on `@vesper/egraph` once SH3 lane 8's first PR lands:
  the rule language and guards, island admission (`Barrier`, `Unchanged`,
  refusals), the proof forest (`Builder`: union with proof edges,
  congruence, prefix folding, record explanation), and the optimized-module
  differential.
- Part 3: the hybrid `optimize` module through `install_optimize`, the full
  suite on the hybrid build, and the SH3 scoreboard row (whole corpus and
  the pass alone, ABBA and callgrind).
