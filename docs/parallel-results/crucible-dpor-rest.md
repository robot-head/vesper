# Wave seven lane 19: Crucible preemption bounding, state caching and a Lean DPOR model (§29)

Issue #342. Follows the wave-six report
([crucible-depth.md](crucible-depth.md)) and closes the gaps it listed for
systematic exploration.

- Base: `283c8115d2203232d0e9b473cc4bb0312c3a8509` (origin/main, a descendant
  of the required baseline `c025b9a`).
- Branch: `claude/project-thread-rm1nyv`.

## What was built

All Rust is in `crates/vesper-crucible` (still `#![forbid(unsafe_code)]`, no
new dependencies). The Lean model is new: `lean/CrucibleDpor.lean` and
`lean/CrucibleDpor/Fixture.lean`.

### 1. Preemption bounding

`ExploreConfig::preemption_bound: Option<u32>` limits `explore_exhaustive`,
`explore_dpor` and `explore_cached` to schedules with at most `k`
preemptions (iterative context bounding, Musuvathi and Qadeer). A preemption
is switching to another fiber while the fiber that ran last is still ready
(its last step returned `Yield`). Deliveries and timer firings are free, and
so is switching away from a fiber that parked or finished. Choices the bound
forbids are counted in `Exploration::bound_pruned`.

Bounded DPOR follows bounded POR (Coons, Musuvathi and McKinley, OOPSLA
2013), with two additions the differential tests forced:

- Every race is also reversed at the start of the scheduling block of its
  first event (the latest event that switched the running fiber), where the
  switch costs no extra preemption. Every awake member of FG05's candidate
  set is added, not only the first.
- **Sleep sets are off under a bound.** With them, the lost-update program
  lost an outcome: the explored member of a trace class needed two
  preemptions, while a pruned, equivalent one needed one.
- **A node that takes a preemption also explores letting the running fiber
  continue.** Without it, a minimized three-fiber program missed
  `step(1) step(1) step(0) step(2) step(0)`. The only explored equivalent
  schedule spent its one preemption early.

### 2. State caching

`explore_cached(Search::{Exhaustive, Dpor}, cfg, setup, observe, key)` takes
a caller key over `(&W, &[F])`. The explorer adds the machine's own state,
keyed independently of the schedule: per-fiber task state, cancellation flag,
next wake reason, I/O and timer sequence counters, pending timer and its
scheduled result, pending external completions with their declared results,
and virtual time. It also adds the sorted failures so far and, when bounded,
the running fiber and the preemptions used. A state whose subtree has been
fully explored is pruned when it is reached again (`Exploration::cache_hits`).

- SimPulse does not expose a ready task's wake reason. The machine tracks it
  itself and checks it against SimPulse's on every dispatch. The first
  caching bug was a key without the reason.
- Under DPOR, a cached state is pruned only if it was explored with a sleep
  set contained in the current one. Every next transition seen in its
  subtree, executed or not, is race-checked against the current prefix. That
  check covers every racing event, not only the latest, because the clocks
  of summarized transitions are only lower bounds.
- **Bounded DPOR does not prune on cache hits.** A 1,500-program stress run
  found one bounded program where it lost an outcome (seed 483, minimized to
  two fibers that both spawn). Caching therefore applies to exhaustive
  search (bounded or not) and to unbounded DPOR.

### 3. Spawning, cancellation and chosen completion values

- `StepCx::spawn()` returns the child's fiber index. After the step, the
  machine takes the child from the new `SimFiber::take_spawned` (default
  `None`), once per call. Spawning needs `Footprint::spawn()`, and spawns are
  mutually dependent, because indices come from one counter. A spawned fiber
  is a new process that happens after its spawning step.
  `ExploreConfig::max_fibers` (default 64) bounds spawning.
- `StepCx::request_cancel(target)` (declared with `Footprint::cancel(target)`)
  calls SimPulse's `request_cancel` after the step. A request for a finished
  fiber is ignored. The target sees it through `StepCx::cancel_requested()`
  (SimPulse's `RunToken::cancel_requested`). A cancellation is dependent with
  every step of its target.
- `IoRequest::ExternalChoice(IoChoices)` declares 1 to 8 distinct results
  (`IoChoices::new` returns `IoChoicesError::{Empty, TooMany, Duplicate}`).
  Each result is its own delivery process, `Proc::Io { task, seq, choice }`,
  shown as `deliver(t#s=k)`. DPOR adds the sibling results as backtracking
  points whenever it takes one. `explore_random` draws the choice from the
  tape like any other decision.
- Typed errors: `ExploreError::UnknownFiber`, `SpawnMismatch`, `FiberLimit`,
  and `Footprint` for an undeclared spawn or cancellation.

Breaking change inside the crate: `Proc::Io` gained the `choice` field.
Nothing outside the crate matched on it.

### 4. Lean model: `lean/CrucibleDpor.lean`

An abstract deterministic transition system (`step : σ → α → Option σ`, a
list `acts` holding every action that is ever enabled, `indep : α → α →
Bool`). Its one semantic hypothesis, `Commutes`, is the diamond property of
independent actions. `Equiv` is Mazurkiewicz equivalence: the
reflexive-transitive closure of swapping adjacent independent actions.
`explore fuel s sleep` is sleep-set depth-first search (Godefroid), the
pruning core of Crucible's DPOR, with `fuel` playing `max_depth`.

Proved, about the model only, with no `sorry`, axioms limited to `propext`,
`Classical.choice` and `Quot.sound`, and 7 theorems audited with `#print
axioms`:

- `equiv_symm`, `equiv_trans`, `equiv_length`;
- `run_equiv`: reordering within a class keeps an execution an execution
  that reaches the same state;
- `explore_sound`: every explored word is an execution to a maximal state;
- `explore_complete`: if no sleeping action can be moved to the front of a
  maximal execution `w` (no longer than the fuel), some explored word is
  equivalent to `w`;
- `every_trace_class_visited`: from the initial state with an empty sleep
  set, every Mazurkiewicz trace class of maximal executions within the depth
  bound has an explored member, which reaches the same final state.

Not modelled: FG05 persistent (backtrack) sets, preemption bounding, state
caching, timers disabled by deliveries (Crucible makes timers dependent with
everything, so `Commutes` is only needed for pairs it calls independent),
and whether declared footprints match what steps do. **This is not a theorem
about the Rust code.**

## Acceptance evidence

Numbers come from `cargo test -p vesper-crucible --locked -- --nocapture` on
the cloud container (x86_64 Linux, 4 cores), unless a run says otherwise. The
runs are deterministic.

**Differential tests against exhaustive search (`tests/dpor_rest.rs`).** In
each sweep, the two explorers find the same set of distinct outcomes (final
worlds and failure sets) on every program. Programs whose exhaustive baseline
exceeds its budget (4,000 executions in CI, 20,000 in stress runs) are
skipped and counted. The random programs mix reads, writes, updates, local
steps, failing checks, sleeps and I/O. The extended generator adds spawns,
cancellations and three-way chosen results.

| Sweep (default CI run, 100 seeds) | Programs | Baseline executions | Reduced executions |
| --- | --- | --- | --- |
| Spawn/cancel/chosen results: DPOR vs exhaustive | 72 | 34,001 | 415 |
| Bound 0: bounded DPOR vs bounded exhaustive | 88 | 15,503 | 2,334 |
| Bound 1 | 80 | 17,919 | 2,275 |
| Bound 2 | 77 | 29,195 | 2,723 |
| Cached exhaustive vs exhaustive (1/3 of programs bounded at 1) | 78 | 21,444 (204,722 transitions) | 542 (29,587 transitions) |
| Cached DPOR vs plain DPOR, both checked against exhaustive | 78 | 804 | 798 |

Stress runs in release with `VESPER_CRUCIBLE_DPOR_SEEDS=1500` (up to 4 ops
per fiber, 20,000-execution budget):

- Bounded, on the final bounded-DPOR algorithm. Bound 0: 1,279 programs,
  957,860 vs 108,633 executions. Bound 1: 1,186 programs, 1,505,496 vs
  169,402. Bound 2: 1,110 programs, 1,764,669 vs 166,898. All equal.
- Spawn/cancel/choice: 987 programs, 1,823,997 vs 11,563 executions, all
  equal.
- Caching (after the two fixes): 1,127 programs. Exhaustive went from
  1,284,720 to 13,719 executions with caching (15,508,935 to 1,147,856
  transitions); DPOR went from 51,213 to 50,343. All equal.
- The wave-six sweep (`tests/dpor.rs`, unchanged): 1,474 programs,
  758,740 vs 4,599 executions, all equal.

The default-run sweeps also check that their schedules replay to their
outcomes (DPOR in the spawn/cancel/choice sweep, cached DPOR in the caching
sweep, first 20 seeds each).

**Targeted tests.**

- *Lost update needs one preemption.* Two fibers each read and then update
  the same cell. Unbounded search finds 4 outcomes in 6 executions. Bound 0
  finds only the 2 serial outcomes. Bounded DPOR at bound 1 finds all 4.
- *Bound is monotone and converges.* Outcome sets grow with the bound from 0
  to 4. At bound 8, bounded search equals unbounded search exactly, in
  outcomes and executions.
- *Caching collapses independent interleavings.* For 3 fibers × 3 steps on
  disjoint cells, exhaustive search takes 1,680 executions and 15,120
  transitions. Cached, it takes 1 execution, 441 transitions and 81 cache
  hits, with the same outcome.
- *Chosen completion values.* Two readers park on an external read with
  results {success, error, cancelled}, and an error fails the run. DPOR (54
  executions, against exhaustive's 180) finds the failure, with the schedule
  `step(0) step(1) deliver(0#0) step(0) deliver(1#0=1) step(1)`. It also
  finds the 4 success/cancelled outcomes.
- *Spawned workers and cancellation.* A supervisor spawns two workers and
  cancels the first. Exactly 3 outcomes: cancelled before any work, after
  one unit, or finished first, so the request is ignored. DPOR takes 15
  executions, exhaustive 41. Every schedule replays.
- *Misuse is typed:* cancelling a fiber that does not exist, a spawn without
  a child, an undeclared spawn, and spawning past `max_fibers`. `IoChoices`
  validation is tested too.

**Lean (`tests/lean_model.rs`, with `VESPER_LEAN` set to Lean 4.24.0 from
the GitHub release zip).**

- `CrucibleDpor.lean` compiles, and its axiom report is audited (7 lines).
- A source audit (always on) rejects `sorry`, `admit`, `native_decide`,
  `axiom` and `partial` in the model.
- Differential: the fixture runs the model's `explore` on 300 random
  read/write programs (2 or 3 processes, 1 to 3 ops, 2 cells). On every
  program, its explored final states equal the outcome sets of Crucible's
  `explore_dpor` and `explore_exhaustive` over an equivalent `SimFiber`.
  Total executions: Lean sleep-set model 3,684, Rust DPOR 3,684, exhaustive
  32,999. On these programs, which have no disabling, Rust DPOR ran exactly
  as many executions as sleep sets alone.
- The existing shrink-model checks still pass.

## Checks

Rust 1.100.0 for clippy, 1.97 for tests, isolated target directories.

- `cargo fmt --all -- --check`: clean.
- `cargo test -p vesper-crucible --locked` with `VESPER_LEAN`: 58 tests pass
  (13 api, 5 arena, 1 arena_alloc, 7 dpor, 10 dpor_rest, 6 explore,
  6 fuzzing, 4 lean_model, 6 shrinking). `dpor_rest` takes about 42 s
  in a debug build.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D
  warnings`: clean (one pre-existing `unused dependency` manifest warning in
  `vesper-pact`).
- `cargo test --workspace --locked --no-fail-fast` (with
  `VLB_CARGO_LOCK=$PWD/Cargo.lock`): 4,023 passed, 4 failed. The 4 are the
  §42 resource-model tests (`lean4lean42_acceptance`, `oracle42_acceptance`,
  `oracle42_runner`, `replay42_kernel`). They need a `lean` binary on `PATH`
  ("`lean --print-prefix`: No such file or directory") and are unrelated to
  this lane.
- `python3 tools/bazel/sync_build_files.py`: no BUILD changes. The new test
  file is picked up automatically, and the Lean files fall under `//:lean`.

## Root patch

None. No new dependencies, no `Cargo.lock` change, and no edits outside the
lane's paths.

## Decisions

- **Sleep sets off under a bound, plus a continuation backtrack.** Both are
  conservative. They were adopted because the differential tests produced
  concrete missed outcomes without them. Bounded DPOR still ran about 7 to
  11 times fewer executions than bounded exhaustive search in the sweeps.
- **No cache pruning in bounded DPOR.** The bounded case that caching missed
  was not explained in the time available. Turning pruning off there keeps
  the guarantee, and exhaustive bounded search, where caching pays most,
  keeps it.
- **Cancellation does not wake a parked fiber.** That is SimPulse's
  semantics: the target sees the request on its next step.
- **Spawns share one dependent counter** instead of hierarchical child
  names. This keeps `Proc::Task(usize)` and serializes concurrent spawners.
  That is sound, at the cost of some reduction.
- `explore_random` ignores `preemption_bound`. Bounding is a systematic
  search tool, and random schedules are shrunk instead.

## Remaining gaps

- The Lean theorem covers sleep-set exploration over a commuting transition
  system. It does not cover the persistent-set backtracking, the bounding
  rules, caching, or the Rust code. Those are argued from the literature and
  tested differentially, not proved.
- Bounded DPOR's guarantee (every outcome of a schedule within the bound is
  found) is checked, not proved. The same holds for caching under DPOR.
- Cache keys are the caller's responsibility. A key that misses part of a
  fiber's or the world's state makes caching unsound, and there is no way to
  check this.
- Vesper source fibers still do not run on the step machine. Lowering
  `vesper test --sim`, `spawn` and `cancel` onto it is compiler work. Rust
  ownership here is not evidence that Vesper's source typing rules are
  implemented.
