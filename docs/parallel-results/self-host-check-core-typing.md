# Self-hosting SH2 lane 7: core typing and modes

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1179. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/07-core-typing.md`](../agent-prompts/self-host-sh2/07-core-typing.md).
Rust stays the default checker everywhere, and no Rust code was deleted.

The lane lands in several PRs. This report grows with each one.

| Part | PR | What |
| --- | --- | --- |
| 1 | #1222 | type relations and exhaustiveness |
| 2 | (this PR) | expressions and inference (`expr_expected`, `call`, `apply_args`, `construct`) |
| 3 | | modes and places (`place`, `bind_mode`, moves, loans) |
| 4 | | literal typing, integer-width and float checks (after SH2 lane 2, #1174) |
| 5 | | the swap into the hybrid build (`core_typing` module) and the scoreboard row |

## Part 2: expressions and inference

### What changed

| Area | Files |
| --- | --- |
| Package | `src/exprs.ves` (the expression checker) and the `core_typing_exprs` entry in `src/lib.ves` |
| Test | `tests/self_host_check_core_typing_exprs.rs` |
| Bazel data and timeout | `BUILD.bazel` |

No Rust source changed in this part.

Ported (`src/check.rs` to `exprs.ves`): `expr_expected` and `expr` for
literals, names (locals, parameters, unit constructors, functions),
tuples, struct and constructor literals (`construct`, with generic ADT
inference and bound checks), field access (`field_path`), blocks with
`let` (annotations, destructuring and `bind_mode`), `if`, `match` (arm
joins and coverage from part 1), unary and binary operators, and calls
(`call`, `apply`, `apply_args`, generic inference through `infer`,
explicit type arguments and inferred results). Each recorded type goes
through the same `types.insert` order as Rust, so a span written twice
keeps its last type.

The harness encodes each world's function table and every checked
function's body as a pre-order expression table. A function whose body
uses a construct the port does not have yet (loops, closures, methods,
handlers, regions, sequences, strings and the other foreign types,
effectful callees, intrinsics) is reported as skipped, not compared.

### The differential

Corpus: 283 worlds on this base. For every function the port checks, the
test compares every expression type Rust recorded in `world.types` inside
the function's span with the port's, and any diagnostic the port reports
(the clean corpus has none).

| Worlds differing | Functions checked | Functions skipped | Expression types compared |
| --: | --: | --: | --: |
| 0 | 4,818 | 7,435 | 28,034 |

The test also asserts a coverage floor, so a port that skipped everything
cannot pass.

The comparison covers types, not diagnostics on bad programs: the corpus
is clean, and mutants that fail type checking are part 4's job (literal
typing only reruns failing functions).

### Numbers

None in this part. Rust's expression checking runs only inside the whole
`Checker`, so there is no fair Rust side to time against a subset of
functions. The timing and callgrind counts move to the swap PR, where the
hybrid build runs the same functions both ways.

### Compiler gaps

No new gaps. A local named like an imported function (`infer`) shadows it
and the call fails with E0206 ("u32 is not callable"), which is Rust's
rule too, so it is not a gap.

### What is left

Parts 3 to 5. Literal typing (part 4) is unblocked now that #1219 has
merged. Coverage grows as part 3 adds places and modes and later parts add
the skipped constructs.

## Part 1: type relations and exhaustiveness

### What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-core-typing/{Loom.toml,README.md,src/*.ves}` |
| Rust probe | `src/check.rs`: `join_types` is now a free function that `Checker::join_types` calls; `#[doc(hidden)] pub mod core_typing_probe` exposes `matrix_covered`, `is_uninhabited`, `infer`, `refine_annotation`, `unresolved` and `join_types` to the test, unchanged |
| Test | `tests/self_host_check_core_typing.rs` |
| Bazel data and timeout | `BUILD.bazel` |

`src/check.rs` is outside this lane's exclusive paths. The differential
needs the private functions it ports, and this is the smallest change that
reaches them. Behaviour is unchanged: the whole suite runs through the same
code.

Ported (`src/check.rs` to `@vesper/check-core-typing`):

- `relations.ves`: `compatible`, `effects_include`, `refine_annotation`,
  `unresolved`, `union_effects` (`union_rows`), `join_types` (with its
  E0200) and `infer` (integer literal merging and row-tail remainders).
- `patterns.ves`: `is_uninhabited`, `matrix_covered` (constructor
  specialization of `bool`, ADTs with substituted payloads and tuples; the
  wildcard filter for other types; the depth bound), `irrefutable` and
  match coverage (E0212). Patterns go in one flat table, and pattern 0 is
  the wildcard that specialization fills missing payloads with.
- `ints.ves`: `IntegerKind::fits` and `default_for` over the arena's
  literal form (a sign and a u64 magnitude).

### The differential

Corpus: lane 4's, 282 worlds on this base (Cargo, Linux).

| Task | Compared | Lines | Worlds differing |
| --- | --- | --: | --: |
| `relations` | type pairs: `compatible` both ways, `unresolved`, `join_types` (Debug of the type and the E0200 count), `refine_annotation`; literals around every kind's bounds against literals, integer kinds and world types; `infer` over every generic function's signature with substituted, unrelated and literal actuals | 910,842 | 0 |
| `patterns` | `is_uninhabited` of every world type; coverage of every `match`, irrefutability of every arm and destructuring `let` | 31,834 | 0 |

The pairs are each type against itself and its next four types in `Debug`
order, so most pairs share a constructor and the relations recurse. The
pairs also include each type against its mirror in that order.

**Mutants.** The clean corpus has only exhaustive matches. So each `match`
also runs with each of its first six arms dropped, and with a constructor
pattern missing its first field (the branch where Rust's closure returns
`false`). Of 7,112 cases, 4,684 are not covered (E0212 would fire), and
17,371 arms are refutable.

The JIT with the optimizer off gives the same texts
(`unoptimized_agrees`, every 23rd world).

### Numbers

Release build, ABBA (best of six per side) over the whole corpus, with each
side's cases built up front. The Vesper side also decodes its input; the
Rust side does not. Both produce the same text.

| Task | Rust | Vesper | Ratio |
| --- | --: | --: | --: |
| relations | 105 ms | 124 ms | 1.18x |
| patterns | 21.0 ms | 25.5 ms | 1.21x |

A first measurement charged building the case lists and type `Debug`
strings to the Rust side and showed 0.38x and 0.21x. Those numbers measured
the harness, not the checker, and are not the result.

Callgrind instruction counts come with part 2, where there is enough
checker on both sides for them to mean something.

### Stage 1 guard

No guarded path is touched (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`,
`packages/vesper-compiler/**`). The bootstrap-stage1 workflow was dispatched
on the branch, and its result is linked from the PR.

### Compiler gaps

No new gaps. The known gaps hit here:

- **Constants cannot be imported across modules** (check-data report,
  gap 1). `relations.ves`, `patterns.ves` and `lib.ves` each repeat
  `K_*` and the pattern kinds.
- **No `i128`** (census C2, SH2 lane 2). Literals are a sign and a u64
  magnitude, so the harness cannot encode a literal above `u64::MAX` (it
  asserts), and `ints.ves` will need the 128-bit kinds once #1219 adds
  `i128`/`u128` to `IntegerKind`.

### What is left

Parts 2 to 5 above. The hybrid module and the scoreboard row need the
checker spine (`expr_expected` and its environment) before there is
anything to swap.
