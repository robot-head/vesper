# Wave five lane 5: Vesper Crucible (§29, excluding §29.3)

Design coverage: §29.1 items 1–3, §29.2 (ChoiceTape property suites and
concolic SimPulse fuzzing), §29.4 theorem 1 (`choice_tape_shrink_terminates`),
§29.5 shrinker. §29.3 / §29.5 lifter (`vesper migrate`) is lane 11. Issue #125.

- Base: `af03122ea74d403f203170926945f9d35786b289` (origin/main).
- Branch: `claude/project-thread-vfxjh1`.

## What was built

New crate `crates/vesper-crucible` (`#![forbid(unsafe_code)]`, std only; its
one internal dependency is `vesper-simpulse`, read-only), the Lean model
`lean/CrucibleShrink.lean` and its executable fixture
`lean/CrucibleShrink/Fixture.lean`.

### Public API

- `ChoiceTape { values: Vec<u64> }` with a stable text format
  (`vesper-crucible-tape/1`, one lowercase hex value per line;
  `to_text` / `from_text`, typed `TapeError` for a missing or unknown
  header, a bad value or more than `MAX_TAPE_LEN` = 2^20 choices).
- `ShrinkKey { len, sum: u128 }`, `ShrinkKey::of`, `smaller_than`: the design's
  `(len, sum)` "shortlex" order. `u128` sums cannot overflow for 2^20 entries.
- `Source`, the host form of the `FuzzSource` effect: `replay(&tape)`
  (strict), `with_prefix(prefix, seed)`, `random(seed)`;
  `draw_u64_bounded` (the design's `draw_u64_bounded`), `draw_u64/u32/u16/u8`,
  `draw_bool`, `draw_range_u64`, `draw_range_i64`, `draw_index`, `draw_enum`,
  `draw_vec`, `draw_bytes` (the design's `draw_bytes`), `assume` (the
  design's `assume_guard`), `draw_run_now` (a SimPulse scheduling decision),
  `probe()`, `realized()`. Replayed values above a draw's bound are clamped;
  strict replay past the tape, a draw budget overrun, a rejected `assume`, an
  empty choice or an empty range are `Invalid`, never success.
  `Arbitrary` for `bool`, `u8..u64`, `i64` (zig-zag), `Option`, `Vec`, pairs
  and triples.
- `Probe`: `edge(id)`, `cmp(site, CmpKind, lhs, rhs)`, `eq`, `eq_bytes`,
  `edges()`, `comparisons()`. This is the small API integration wires to
  Cranelift instrumentation.
- `PropError::{Fail(Failure), Invalid(Invalid)}`, `Failure { kind, detail }`,
  `check`, `Verdict::{Pass, Fail, Invalid}`, `PANIC_KIND`.
- `shrink(&mut prop, &tape, &ShrinkConfig) -> Result<Shrunk, DoesNotFail>`,
  `ShrinkStats`, `replay(&mut prop, &tape) -> Verdict`.
- `fuzz(&FuzzConfig, prop) -> FuzzReport` with `Strategy::{Guided, Random}`,
  `Counterexample { tape, failure, original, found_after_execs, shrink }`.
- `run_fibers(src, world, fibers, &SimConfig) -> Result<SimRun, PropError>`,
  `replay_fibers(src, world, fibers, journal, cfg)`, trait `Fiber<W>`,
  `FiberStep`, `SimRun { seed, journal, dispatches, deferrals, steps }`.
- `refute(&FuzzConfig, generate, predicate) -> Refutation<T>` with
  `Refuted { input, counterexample }` (only after regenerating the input
  from the shrunk tape and re-checking that the predicate is false on it),
  `Survived { execs, witnesses }`, `Crashed { counterexample }` (the
  generator or predicate panicked) and `Unreproducible { counterexample,
  error }`; `REFUTED_KIND`, `NONDETERMINISTIC_KIND`.

### Design decisions

- **One tape for data and schedules.** SimPulse's own round-robin dispatch
  order comes from its seed, which is the first draw of a SimPulse run; each
  dispatch then draws run (0) or defer (1). Defer yields the token back to
  SimPulse without stepping. Shrinking toward 0 therefore means seed 0 and the
  fewest deferrals, and deleting spans deletes data (for example noise fibers
  whose count and lengths were drawn). The crate does not modify SimPulse.
- **Validity without type-specific shrinkers.** The shrinker never sees
  generated values. It accepts a candidate only if the re-run property fails
  with the *same failure kind*, consumed only tape choices (strict replay, no
  overrun), passed every `assume`, and its realized tape is strictly smaller.
  So the only values ever executed are ones the generator itself builds. A
  generator that only offers legal next operations keeps producing legal
  sequences while shrinking. Shrink quality depends on generator locality:
  collections use a continuation flag right before each element, so an
  element is one contiguous tape span. (An earlier test generator drew all
  flags first and the ops afterwards; its counterexamples shrank to seven ops
  instead of six because one op was no longer one span.)
- **Termination is enforced, not assumed.** `attempt` compares the candidate
  key and the realized key with the current best before accepting; a run
  budget (`max_runs`) bounds wall-clock as well. Passes: span deletion
  (halving chunk sizes, as in §29.5), zeroing spans of 8/4/2, value lowering
  (values 0..8 first, then binary search), and "lower a count by one and
  delete up to eight following choices".
- **Solver.** Neither `z3` nor a Bitwuzla binding builds from crates.io on
  Linux, macOS and Windows CI without a system library or a long bundled C++
  build, so the solver step is bounded input-to-state replacement: for every
  comparison a corpus entry executed, search its tape for either operand as a
  whole drawn value or as 1/2/4/8 little/big-endian bytes in the byte lane,
  substitute the other operand (±1 for ordering comparisons) and run. Each
  new corpus entry is solved once, at most 256 candidates. Coverage features
  are edges and comparison outcomes with AFL hit-count buckets, plus per-site
  operand closeness (equal low bits / leading bytes).
- **Panics** in a property are failures of kind `"panic"` (configurable).
- **Determinism.** No library code reads a clock or OS entropy; campaigns are a
  function of `FuzzConfig::seed` (tested: two runs give equal reports).

### Corrections to the design text

- §29.5 phase 2 binary-searches each value assuming failure is monotone in it;
  the reducer first tries values 0..8 because properties need not be monotone
  (a parity property otherwise stuck at `[1, 5]`).
- §29.1.2 says every shrunk candidate "remains 100 percent
  linear-obligation-valid under Theorem 2". What is implemented is weaker and
  host-side: candidates are exactly the generator's own outputs for some tape,
  and invalid ones are discarded. Whether that implies source-level
  obligation validity depends on the source checker, which this lane does not
  touch.
- `expansion-37-review.md`: a decreasing shortlex order proves termination,
  not a globally minimal failing tape. No minimality is claimed; results are
  local minima for these passes.
- The `(len, sum)` order is what the design's theorem states. It is coarser
  than lexicographic shortlex: distinct tapes with equal length and sum share
  a key.

## Lean

`lean/CrucibleShrink.lean` (Lean 4.24.0, no Mathlib). Model: a tape is
`List Nat`; `key t = (t.length, tapeSum t)`; `Smaller a b` is
`Prod.Lex (<) (<)` on keys. Proved, about this model only:

- `smaller_wf`: `Smaller` is well-founded.
- `choice_tape_shrink_terminates`: the design's statement verbatim.
- `smallerB_correct`: the executable `smallerB` decides `Smaller`.
- `delete_span_smaller`: deleting a non-empty span decreases the order.
- `lower_pointwise_smaller`: pointwise `≤` with at least one strict decrease
  (zeroing a span, lowering a value) decreases the order;
  `set_lower_smaller`: replacing one value by a smaller one does.
- `shrink` (a definition, accepted by well-founded recursion on `Smaller`,
  so total): a loop that accepts an oracle's candidate only if `smallerB`.
  `shrink_preserves`: if the oracle only proposes tapes satisfying an
  invariant (Crucible: "fails the same way and is generator-valid") and the
  start satisfies it, so does the result. `shrink_le`: the result is the
  start or `Smaller` than it.

No `sorry`, `admit`, `native_decide` or axioms; the axiom report lists only
`propext`, `Quot.sound` and (for `shrink_le`) `Classical.choice`. This is not
a theorem about the Rust code. `tests/lean_model.rs` audits the source,
compiles the model under `VESPER_LEAN`, checks the axiom report, and compares
`smallerB` with `ShrinkKey::smaller_than` on 2000 tape pairs (including
values near `u64::MAX`), which is a differential test only.

## Acceptance evidence

All numbers are from `cargo test -p vesper-crucible -- --nocapture` on the
cloud container (x86_64 Linux, test profile, one thread per test), with
`FuzzConfig::default()` seeds noted in the tests; they are deterministic.

| Planted bug (`tests/fuzzing.rs`) | Guided (20,000-exec budget) | Pure random (same budget) | Shrunk tape |
| --- | --- | --- | --- |
| 8-byte magic `"!VESPER!"` at offset 0 | found after 65 execs (1 solver run) | not found in 20,000 | 17 choices: the 8 magic bytes, nothing else |
| 4-byte LE length prefix == body length, body ≥ 4 | found after 65 execs (1 solver run) | not found in 20,000 | prefix `04 00 00 00`, four zero body bytes |
| `"VC"` tag, command byte `'!'`, CRC-32 equality | found after 67 execs (3 solver runs) | not found in 20,000 | `"VC"`, valid CRC, payload `"!"` |

- **SimPulse interleaving** (`tests/shrinking.rs`): a lost-update race
  between two read-then-write incrementers plus 0–4 drawn noise fibers.
  Found on the first execution (the race is not hard to hit); shrunk from a
  26-dispatch schedule (8 deferred, 32-choice tape, seed
  7715421508079507883) to 7 dispatches with 1 deferral, no noise fibers,
  seed 0, 9 choices: the shortest losing schedule for these fibers. Replaying
  the shrunk tape three times gives the identical SimPulse journal, and
  `Simulator::replay` (via `replay_fibers`) validates every transition of it;
  a truncated journal is rejected.
- **WriteTxn linear protocol**: a generator that only offers legal next
  operations (`Read`/`Begin` when idle, `Write`/`Commit`/`Abort` inside a
  transaction, abort at the end) and a store that drops the last write of
  any commit with ≥ 3 writes. The 18-op counterexample shrank to 6 ops
  `[Begin, Write(0,0), Write(0,0), Write(1,0), Commit, Read(1)]`. Across all
  360 executed sequences (fuzzing and 413 shrinker runs, including discarded
  invalid candidates) an independent protocol checker found 0 illegal
  sequences, and the drop auditor on the non-Clone `#[must_use]` `WriteTxn`
  counted 0 leaked transactions and opened == closed. Contrast: deleting a
  single op from the decoded 18-op list (what a type-directed `Vec`
  shrinker does) gives an illegal sequence in 10 of 18 cases.
- **refute**: the claim "wrapping midpoint of two bytes ≥ the smaller byte"
  is refuted with a locally minimal pair summing to exactly 256; a probe-
  reported 64-bit secret equality is pierced within 5,000 execs; a true
  predicate survives with its corpus returned as witnesses.
- **Shrinker contract**: 200 random tapes against a non-monotone parity
  property: the result never grows, never exceeds the run budget, always
  replays as a failure, and with an adequate budget is `[1, 1]`.
- **Throughput (measured, not a design claim)**: 50,000 guided executions of
  a trivial target in 0.11 s, about 430,000–450,000 execs/s in the
  unoptimized test profile on one core. This says nothing about Vesper code
  under Cranelift instrumentation.

## Checks

Run on the cloud container, Rust 1.100, with an isolated target directory
and `VESPER_LEAN` pointing at Lean 4.24.0:

- `cargo fmt --all -- --check`: clean.
- `cargo test -p vesper-crucible --locked`: 27 tests pass (13 api, 6
  fuzzing, 2 lean_model, 6 shrinking); the Lean model compiles and the
  2000-pair differential agrees.
- `lean CrucibleShrink.lean` from `lean/`: 8 axiom-report lines, standard
  axioms only.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: clean.
- `cargo test --workspace --locked`: 2275 passed, 0 failed.

## Review fixes (Codex review on 764e5f8)

- The solver now stops building candidates once `max_solver_candidates` is
  reached, instead of cloning the tape for every match and discarding the
  extras (quadratic copying on long tapes).
- `refute` returns `Refuted` only for a reproduced `"refuted"` failure whose
  predicate is false on the regenerated input; panics are `Crashed`,
  nondeterministic results `Unreproducible`.
- `Source::replay` (strict) is budgeted by `MAX_TAPE_LEN`, so tapes found
  with a raised `max_draws` replay.

## Root patch

One commit: `crates/vesper-crucible` added to root `members` and
`default-members` before `experiments/native-pulse`, and the regenerated
`Cargo.lock` entry. No new workspace dependencies, no `src/**`, CI or global
doc edits.

## Remaining gaps

- Cranelift edge-counter and comparison-operand instrumentation of Vesper
  source (Stage 5) is integration B; today targets call `Probe` by hand.
  No nightly-free instrumentation claim is made.
- The `vesper fuzz` and `vesper verify --refute` CLIs, emitting `@test`
  regression cases into source modules, and seeding `bv_decide`/`aesop` from
  survivors are integration B.
- MPK/PKRU differential shadowing and `vesper migrate` are lane 11.
- The source-level `linear effect FuzzSource`, `@fuzz_prop`, `Arbitrary[r]`
  over `Region` arenas and the 64 KiB `VirtualArena` reset are not
  implemented; the host `Source` allocates on the Rust heap. No "0 alloc" or
  "500,000 execs/s" claim is made.
- No SMT engine; comparisons whose operand is a non-trivial function of the
  compared input field are out of reach of the solver step.
- SimPulse exploration covers Rust step-machine fibers with yield/finish; it
  does not use SimPulse timers or I/O completions, and it is not DPOR.
- Rust ownership (`#[must_use]` WriteTxn owners in tests) is not evidence that
  Vesper's source typing rules are implemented.
