# RFC: Vesper Anvil — Unified Test Harness (`crates/vesper-test`, `vesper test`)

> **Status**: Draft — Codebase Audit & Phased Implementation Plan  
> **Baseline**: `5898582b` (`main`)  
> **Specification note**: The [manuscript](../manuscript/README.md) remains the sole language specification. This RFC audits the Vesper Anvil test-harness proposal against the live repository and defines a phased, workspace-compliant implementation plan.

---

## 1. Summary & Goals

**Vesper Anvil** (`crates/vesper-test`, surfaced via `vesper test`) unifies five assurance tiers behind one `@test` attribute whose **function signature and `eff [...]` clause determine the synthesized harness**:

1. **Deterministic Unit Tests** (`fn t() eff [Fail[E]]`) — shared Cranelift JIT module, resettable guard-paged arena, fuel cap, and dynamic leak auditing.
2. **Parameterized Property / Concolic Tests** (`fn t(x: T, ...)`) — `ChoiceTape`-driven generation, coverage-guided/operand-solver exploration, shortlex shrinking, and `@test_case(...)` regression pinning (later extended with refinement-seeded SMT solving).
3. **Deterministic `SimPulse` Chaos & DPOR Suites** (`fn t() eff [Scope, Net, Storage, Clock]`) — virtual time, DPOR schedule exploration, packet/WAL/crash fault injection, and `.vchrono` failure capsules.
4. **Hardware & Budget Gates** (`fn t() eff [ConstTime, Realtime]` / stack & PMU budgets) — Stage-5 `WCSD`, SSA secret-taint, zero-`Alloc`, and PMU cycle checks integrated into test pass/fail.
5. **In-Memory Substructural Negative Tests** (`@test(expect_error = E0302, primary = "...", related = "...")`) — forked checker state asserting exact diagnostic codes and primary/related spans without brittle `.stderr` snapshots.

Across all tiers, Anvil aims to replace `src/test_runner.rs`'s current narrow JIT subset + evaluator fallback + string-splicing mutation loop with:
- **Per-function transitive `fn_blake3` verdict caching** in `~/.vesper/store`,
- **A broadened shared Cranelift JIT session** with an indirect function-pointer dispatch table for hot-swapped `--mutate` runs, and
- **First-class `.vchrono` replay and *Vesper Lens* regression pinning**.

---

## 2. Codebase Audit: Proposal vs. Current Reality

Every subsystem referenced by the Anvil proposal was audited against `/home/antigravity/robot-head/vesper` at commit `5898582b`.

| Area | Status | Current Codebase Evidence | Key Gaps & Corrections |
| :--- | :--- | :--- | :--- |
| **`@test` / `@test_case` attributes** | **Missing** | `crates/vesper-syntax/src/parser.rs:743` only parses `@fip`; `src/test_runner.rs:197-254` discovers `test_*` functions and `// @test` comments. | Must add `@test(...)` and `@test_case(...)` item attributes to `vesper-syntax` and `vesper-core::ast`, while keeping `// @test` comments supported for migration. |
| **`sugar` & `std/test` (`expect`)** | **Missing (Spec-only)** | `docs/manuscript/content.typ` §7.5–7.6 explicitly notes `sugar` is a design proposal with no implementation. `packages/vesper-std/` has `core`, `collections`, `fmt`, `bytes`, `sort`, `tests` (only internal `packages/vesper-std/tests/src/support.ves`). | Do not block Anvil on §7.5 `sugar`. Ship `@vesper-std/test` with standard assertion helpers / Tier-1 macros (`crates/vesper-syntax/src/macro_source.rs`) first, then upgrade `expect` once `sugar` enforestation lands. |
| **Shared Cranelift JIT (`NativeBatch`)** | **Partial (Very narrow)** | `src/native_batch.rs` compiles 0-arg entries into one `JITModule` (`cranelift-jit = "0.125"`). `src/native.rs` supports aggregates, `inout`, recursion, handlers, SIMD, `dyn` vtables (`call_indirect`), regions, and fiber sessions. | `NativeBatch::preflight_ir` (`src/native_batch.rs:400-580`) restricts batching to pure, monomorphic, scalar-only, loop-free, recursion-free, non-`inout` functions. Must widen `NativeBatch` (or build an Anvil JIT session over `src/native.rs`) before all tiers can share one JIT module. |
| **Mutation Testing (`--mutate`)** | **Partial (Evaluator-only)** | `src/test_runner.rs:556-946` (`MutantReport`, `MutantStatus::{Killed, Survived, IllTyped}`) collects source-span mutations, splices source text, re-parses/re-checks the whole file, and runs all tests on the tree-walk `Runtime` (`prefer_jit = false`). | No HIR/PCG-filtered mutant generator, no reverse call-graph test slicing, and no indirect function-pointer table hot-swap in `JITModule`. |
| **Per-function `fn_blake3` & CAS** | **Partial** | `src/database.rs` uses Salsa 0.24 with file-level `SourceIdentity::blake3` and module-level `SignatureSnapshot::digest`. `crates/vesper-loom-cas` implements `~/.vesper/store` (`Store`, `DerivationKey`, `Domain::{Source, Interface, Artifact}`). `src/publish_commands.rs:459` hashes `.vhir` MIF interfaces. | No per-function body/closure `fn_blake3` hash or reverse call-graph impact DAG. `vesper-loom-cas` is not yet wired into `src/test_runner.rs`. |
| **Refinement Types & Bitwuzla SMT** | **Missing** | `crates/vesper-core/src/ast.rs::Type` has no refinement variant; `Cargo.toml` has no SMT crate. Formal verification uses Lean 4 (`src/lean.rs`, `src/verify_commands.rs`). | Refinement syntax `{ v: u64 \| phi }` and Bitwuzla seeding are major compiler additions and should be Phase 5 rather than blocking parameterized property tests. |
| **`ChoiceTape`, Property & Concolic Testing** | **Implemented** | `crates/vesper-crucible` provides `ChoiceTape`, `Source`, `Arbitrary`, shortlex `shrink` (proved in `lean/CrucibleShrink.lean`), coverage-guided `fuzz` (bounded operand-replacement solver), `refute`, and `VirtualArena`. Wired to `vesper fuzz` / `vesper verify --refute` (`src/crucible_commands.rs`, `src/native_coverage.rs`) for scalar carriers. | Not yet wired into `vesper test` for parameterized `@test` functions, and currently limited to scalar carriers (`i64`, `u64`, `u32`, `u8`, `bool`). |
| **`SimPulse`, Faults & DPOR** | **Implemented (Split across crates)** | `crates/vesper-simpulse` (`Simulator`, `RunToken<T>`, `Journal`, virtual clock); `crates/vesper-sim-session` (evaluator-based `CheckedProgram`, `model::wal::{Wal, TailDamage}`, `model::net::Network`, `model::warp::WarpSpec`, `Fault`, `CrashChoice`); `crates/vesper-quarry/src/device.rs` (`SimDevice`); `crates/vesper-chorus/src/simnet.rs` (`SimNet`). Crucibly, **`crates/vesper-crucible` already implements `explore_dpor` / `explore_cached`** (sleep-set DPOR proved in `lean/CrucibleDpor.lean`). | `vesper-simpulse` and `vesper-sim-session` do not have DPOR directly; Anvil should bridge `vesper-crucible::explore_dpor` with `vesper-sim-session` rather than reinventing DPOR. `SimStorageCtl` and `TornWalSector` do not exist yet (map them onto `vesper_sim_session::model::wal::TailDamage` + `vesper_quarry::device::SimDevice`). |
| **Hardware & Budget Gates** | **Partial (Backend implemented, syntax missing)** | `crates/vesper-silicon/src/stack.rs` + `src/stack_bound.rs` (WCSD); `src/capability_effects.rs` + `crates/vesper-vault::verify_constant_time` (`ConstTime`); `src/capability_effects.rs` + `crates/vesper-resonance::confine::verify_realtime` + `src/fip.rs` (zero-`Alloc`/`Realtime`); `src/native_verifiers.rs` (`verify_native_with_capabilities`); `crates/vesper-bench-pmu` (`PerfGroup`, `RdpmcCounters`, `KpcCounters`, `AllocationObserver`). | `Stack @ <= N` source syntax does not exist (`src/native_verifiers.rs:19-21` takes JSON policy). PMU cycle/alloc assertions exist in `vesper bench` (`src/bench_commands.rs`) but are not wired into `vesper test`. |
| **Negative Checker Tests & Spans** | **Partial (Diagnostics ready, API private)** | `crates/vesper-core/src/ast.rs::Diagnostic` has `code`, `span`, `related: Vec<DiagnosticLabel>`, `help`. `src/check.rs` emits `E0300`–`E0313` with `Checker::related`. `World`, `Env`, and `Checker<'a>` all derive `Clone`. `src/doctest.rs` already supports `compile_fail(E...)`. | **Correction**: in `src/check.rs`, `E0301` is overlapping projections / alias-XOR-mutability (lines 1173, 1380, 3100), while **`E0302`** is unconsumed linear obligation (lines 1306, 1723, 1874, 3164). `Checker` and `Env` are private to `src/check.rs`, so per-item speculative forking needs a thin public API. |
| **`VirtualArena` & Guard Pages** | **Partial** | `crates/vesper-alloc/src/virtual_arena.rs` (`VirtualArena`) and `crates/vesper-crucible/src/arena.rs` (64 KiB resettable arena). `crates/vesper-fiber-abi/src/stack.rs` implements `PROT_NONE` guard pages (`FiberStack`). | `vesper-alloc::VirtualArena` does not yet map `PROT_NONE` guard pages; must add an opt-in guarded reservation mode reusing `vesper-fiber-abi`'s platform pattern. |
| **Dynamic Obligation & Secret Ledger** | **Partial (Misattributed oracle)** | `crates/vesper-vault/src/obligation.rs` (`LeakAuditor`, `LeakReport`, `SecretSlot::shred`); `crates/vesper-chorus/src/session.rs` (`LeakAuditor`); `crates/vesper-crucible` (`ARENA_LINEAR_LEAK_KIND`); `crates/vesper-pulse/src/uring.rs` (`IoDriver`, `Ledger<T>`). | **Correction**: `ResourceOracle42` (`lean/ResourceOracle42.lean`, `src/bin/resource_oracle42/`) is a fixed Lean logical model endpoint, *not* a runtime leak auditor. `io_uring` `EscrowCell` is spec-only (`docs/manuscript/supplements/continuation-fission.typ`). Build Anvil's `ObligationLedger` from `vesper-vault`, `vesper-chorus`, `vesper-crucible`, and `vesper-pulse::uring::Ledger`. |
| **`.vchrono` Capsules & *Vesper Lens*** | **Partial** | `crates/vesper-chrono` implements `FlightRecorder`, `ChronoTimelineIndex`, `ObligationLineage`, keyed-BLAKE3 `Bundle` (`.vchrono`), `ReplaySession`, and `SimPulseTap`. `src/replay_commands.rs` wires `vesper replay`. `crates/vesper-lsp` + `src/lsp/{lens,actions}.rs` implement proof lenses and `vesper.code-action/1`. | `vesper test` does not emit `.vchrono` capsules on failure, and `src/lsp/actions.rs` has no test-run, replay, or `@test_case(...)` pinning code actions yet. |

---

## 3. Architectural Contracts & Design Adjustments

### 3.1 Workspace Leaf-Crate Boundary (`crates/vesper-test`)
Per Vesper's workspace contract (see [`Cargo.toml`](file:///home/antigravity/robot-head/vesper/Cargo.toml#L1-L2) and [`tooling41-contracts.md`](file:///home/antigravity/robot-head/vesper/docs/rfcs/tooling41-contracts.md#L40-L43)), new leaf crates **must remain root-free** and never introduce a `vesper -> vesper-test -> vesper` cycle.

- **`crates/vesper-test` (leaf crate, with `BUILD.bazel` via `vesper_crate()`)** owns:
  - Test metadata & attribute AST/DTO models (`TestAttr`, `TestCaseAttr`, `ExpectErrorSpec`, `FaultProfile`, `BudgetGate`).
  - Signature-to-tier classifier (`TestMode::{Unit, Property, SimPulse, BudgetGate, ExpectError}`).
  - Verdict cache key & record schema (`VerdictKey { fn_blake3, sim_seed, target_isa, engine_abi }`, `VerdictRecord`).
  - Unified `ObligationLedger` trait + built-in adapters for `vesper-vault::LeakAuditor` and arena/handle leak counters.
  - Mutant plan/slicing data structures (`MutantPlan`, `MutantDispatchSlot`, `MutantReport`) and JSON/human reporters.
- **Root facade adapter (`src/test_runner.rs` + `src/anvil/`)** owns:
  - Wiring `vesper-syntax`, `src/check.rs` (`World`/`Checker`), `src/database.rs` (Salsa), `src/native_batch.rs` / `src/native.rs`, `src/native_verifiers.rs`, `vesper-crucible`, `vesper-sim-session`, `vesper-chrono`, `vesper-bench-pmu`, and `vesper-loom-cas`.
  - Dispatch from `src/main.rs` -> `src/doctest_commands.rs` -> `src/test_runner.rs` (keeping `vesper test --doc` intact).

### 3.2 Signature-Driven Classification Rules
Given a checked `@test` item `f`, `vesper_test::classify(&TestSignature)` resolves its execution mode deterministically:
1. **`@test(expect_error = EXXXX, ...)`** -> `TestMode::ExpectError` (never compiled or executed; checked via forked `Checker`).
2. **`eff [...]` contains any of `Scope`, `Net`, `Storage`, `Clock`** (or `@test(sim_schedules = ..., fault_profile = ...)`) -> `TestMode::SimPulse`.
3. **1+ non-capability value parameters** (or `@test(cases = ...)` / `@test_case(...)`) -> `TestMode::Property` (may compose with `SimPulse` or `BudgetGate`).
4. **`eff [...]` contains `ConstTime` or `Realtime`, or `@test(max_stack = ..., max_cycles = ..., zero_alloc = true)`** -> `TestMode::BudgetGate` (runs Stage-5 verifiers + execution).
5. **Otherwise (0 params, pure or `Fail[E]`)** -> `TestMode::Unit`.

### 3.3 Diagnostic Code Correction for Negative Substructural Tests
The proposal's example 4 (`linear_txn_cannot_cross_unhandled_question_mark`) expects `E0301`. In `src/check.rs`, `E0301` is reserved for overlapping projections / alias-XOR-mutability, whereas unconsumed linear obligations emit **`E0302`**. Negative test specs must assert `expect_error = E0302` for linear leaks and `E0301` for aliasing conflicts, matching `crates/vesper-diagnostics`.

---

## 4. Phased Implementation Plan

### Phase 0 — Crate Scaffold, `@test` / `@test_case` Syntax, and Mode Classifier
**Goal**: Establish `crates/vesper-test` and real attribute syntax without breaking existing `// @test` or `test_*` suites.

- **Deliverables**:
  - Add `crates/vesper-test` to root `Cargo.toml` (`members` & `default-members`) and create `crates/vesper-test/BUILD.bazel` using `vesper_crate()`.
  - Extend `crates/vesper-syntax/src/parser.rs` (alongside `fip_attr`) and `crates/vesper-core/src/ast.rs` to parse `@test`, `@test(...)`, and repeatable `@test_case(...)` attributes on `Item::Function`.
  - Implement `vesper_test::classify` mapping parsed attributes + parameter lists + `eff [...]` rows to `TestMode` + `HarnessConfig`.
  - Keep `src/test_runner.rs::discover_tests` accepting legacy `// @test` comments and `test_*` names with a soft migration path.
- **Exit Criteria**: `cargo test -p vesper-test` and existing `vesper test` CLI integration tests pass; parser round-trips all 5 attribute shapes.

### Phase 1 — In-Memory Negative Tests (`expect_error`) & `fn_blake3` Verdict Cache
**Goal**: Deliver `< 1 ms` negative substructural tests and incremental verdict caching in `~/.vesper/store`.

- **Deliverables**:
  - **Forked Checker API (`src/check.rs`)**: Expose a public `World::check_isolated_item(&self, func: &Function) -> Vec<Diagnostic>` that clones `World`/`Checker` state (already `#[derive(Clone)]`) and checks a single `@test(expect_error = ...)` body without re-checking the entire module or tainting sibling functions.
  - **Span & Code Matcher (`crates/vesper-test`)**: Match `Diagnostic.code == expected`, primary span text/substring (`primary = "..."`), and related label text (`related = "..."`) against `Diagnostic.related`. Reject unrelated parse errors as test failures (matching `src/doctest.rs` discipline).
  - **Per-function Merkle Fingerprints (`fn_blake3`) & Reverse Call-Graph**: Add a deterministic canonical HIR/AST function hasher + transitive call-graph closure (`TransitiveFnBlake3`) in `src/database.rs` (backed by Salsa tracked queries).
  - **CAS Verdict Store**: Store deterministic pass/fail verdicts in `~/.vesper/store` via `vesper-loom-cas` (`Domain::Artifact` or a dedicated verdict namespace) keyed by `(TransitiveFnBlake3, SimSeed, TargetIsa, HarnessVersion)`. Add `--no-cache` flag to `vesper test`.
- **Exit Criteria**: Negative `E0301`/`E0302` tests run in `< 1 ms` per case; unchanged `@test`s hit the CAS verdict store on incremental edits with zero JIT recompilation.

### Phase 2 — Broadened Shared Cranelift JIT, Guarded `VirtualArena`, and Dynamic Leak Ledger
**Goal**: Move real Vesper unit tests off the tree-walk evaluator onto one shared JIT module with arena isolation and leak auditing.

- **Deliverables**:
  - **Widen `NativeBatch` (`src/native_batch.rs` / `src/native.rs`)**: Lift the strictest `preflight_ir` restrictions incrementally:
    1. Loops & bounded recursion with injected fuel counter check,
    2. `inout` parameters and local aggregates/tuples/ADTs (`native_aggregates.rs`),
    3. Fallible `Fail[E]` returns / `Result`-shaped outcomes.
  - **Guard-Paged `VirtualArena` (`crates/vesper-alloc`)**: Add `VirtualArenaConfig::guard_pages: bool` mapping trailing `PROT_NONE` guard pages (reusing the Unix/Windows pattern from `crates/vesper-fiber-abi/src/stack.rs`), and expose 1-instruction cursor reset per test frame on worker threads.
  - **Runtime `ObligationLedger` (`crates/vesper-test`)**: Wrap every JIT/evaluator test invocation in a ledger snapshot/diff covering:
    - `vesper_vault::LeakAuditor::this_thread()` (un-shredded `SecretSlot`, `NonceTicket`, `AeadKey`, etc.),
    - `vesper_chorus::session::LeakAuditor` (unclosed session channels),
    - `vesper_crucible` arena linear leak counter (`ARENA_LINEAR_LEAK_KIND`),
    - `vesper_pulse::uring::Ledger` (undrained I/O requests when active).
    Any non-zero delta fails the test with the obligation kind and minting context.
- **Exit Criteria**: Standard unit tests using loops, `inout`, ADTs, and `Fail[E]` execute on `TestEngine::Jit` with `VirtualArena` reset and fail deterministically when a `SecretSlot` is dropped without `shred`.

### Phase 3 — Indirect-Table Hot-Swapped JIT Mutation Testing (`vesper test --mutate`)
**Goal**: Replace source-string splicing + full re-parse + evaluator execution in `src/test_runner.rs` with PCG-valid HIR mutants hot-swapped in one `JITModule`.

- **Deliverables**:
  - **PCG-Filtered Mutant Generator**: Generate structured AST/HIR mutants (boundary flips `<`/`<=`, arithmetic/boolean swaps, dropped `inout` field stores, swapped same-type `in` args, forced `Fail::raise`) and filter `IllTyped` mutants via Phase 1's `World::check_isolated_item` before JIT compilation.
  - **Reverse Call-Graph Slicing**: Using Phase 1's call graph, map each target function `f` to only the `@test`s that transitively call `f`.
  - **Indirect Dispatch Table in `JITModule`**:
    - When `--mutate` is active, compile each mutable function `f` and its valid mutants `f$mut_0 .. f$mut_k` into the **same** `JITModule`.
    - Lower internal calls to mutable functions through a thread-local or module-owned `AtomicPtr<u8>` / function-pointer dispatch table (`call_indirect`, reusing existing Cranelift lowering in `src/native.rs:6496`).
    - For each mutant `k`, swap `table[f_id] = ptr(f$mut_k)`, run only `reverse_callers(f)`, restore `table[f_id] = ptr(f)`, and record `MutantReport`.
- **Exit Criteria**: `vesper test <path> --mutate` runs mutated tests on `TestEngine::Jit` with a single `JITModule` compilation per batch and produces identical `Killed`/`Survived`/`IllTyped` semantics to the baseline evaluator mode.

### Phase 4 — Property/Concolic Testing, `SimPulse` Chaos/DPOR, Hardware Gates, and `.vchrono` Replay
**Goal**: Unify the remaining three assurance tiers and failure reproduction into `vesper test`.

- **Deliverables**:
  - **Parameterized Property Tests (`vesper-crucible`)**: For `@test(cases = N)` with value parameters, drive `ChoiceTape` + `Arbitrary` + `NativeCoverage` + shortlex `shrink` from `crates/vesper-crucible` (extending `src/crucible_commands.rs` logic into `vesper test`), replay explicit `@test_case(...)` pins first, and support aggregates/slices beyond the current scalar carriers.
  - **`SimPulse` + DPOR + Fault Profiles**:
    - Bridge `vesper_crucible::explore_dpor` with `vesper_sim_session` (`CheckedProgram`, `model::wal::{Wal, TailDamage}`, `model::net::Network`) and `vesper_quarry::device::SimDevice`.
    - Introduce `SimStorageCtl` capability handle and `fault_profile = TornWalSector | NetPartition | ClockSkew` mapped onto `vesper-sim-session`'s fault models.
  - **Hardware & Budget Gates**:
    - Wire `src/native_verifiers.rs` (`verify_native_with_capabilities`) into `vesper test` so `eff [ConstTime]` runs `vesper_vault::verify_constant_time`, `eff [Realtime]` runs `vesper_resonance::confine::verify_realtime`, and `@test(max_stack = N)` runs `src/stack_bound.rs` (`WCSD`).
    - Wire `vesper_bench_pmu::AllocationObserver` (`zero_alloc = true`) and optional PMU counters (`max_cycles = N`, reporting unavailable cleanly when hardware PMU access is absent, per `tooling41-contracts.md`).
  - **`.vchrono` Capsules & *Vesper Lens* Pinning**:
    - On any failing `Property` or `SimPulse` run, seal a `.vchrono` bundle at `target/anvil/<test>.vchrono` via `crates/vesper-chrono` (`Bundle::seal` + `SimPulseTap` + shrunk `ChoiceTape`), replayable via `vesper replay`.
    - Add a `vesper.code-action/1` action in `src/lsp/actions.rs` and test code lenses in `src/lsp/session/lens_requests.rs` to run individual `@test`s and insert `@test_case(...)` regression locks.
  - **`@vesper-std/test` Package**: Add `packages/vesper-std/test/` with ergonomic assertion helpers (`expect_eq`, `expect_ne`, `expect_ok`, `expect_err`, `expect`) usable immediately without waiting for `sugar`.
- **Exit Criteria**: All 5 `@test` signature shapes work end-to-end via `vesper test`, failing `SimPulse`/property tests emit valid `.vchrono` bundles that replay via `vesper replay`, and LSP offers `@test_case(...)` pinning.

### Phase 5 — Language-Level Upgrades (Decoupled Compiler Milestones)
**Goal**: Plug future language features cleanly into Anvil without redesigning `crates/vesper-test`.

- **Refinement Types & SMT Boundary Seeding**: Once `{ v: T | phi }` lands in `vesper-core`/`src/check.rs`, add an optional SMT boundary-seed provider (e.g. Bitwuzla behind a Cargo feature) feeding initial `ChoiceTape` / concrete seeds ahead of Crucible's PRNG/concolic exploration.
- **Effect-Row Stack Budget Syntax (`eff [Stack @ <= N]`)**: Replace `@test(max_stack = N)` with first-class `Stack @ <= N` effect annotations once supported by `vesper-syntax` and `src/native_verifiers.rs`.
- **`sugar` Power-Assert `expect` (§7.5)**: Upgrade `expect <expr>;` in `@vesper-std/test` to AST-decomposing power-asserts once §7.5 enforestation is implemented.
- **Continuation Fission `EscrowCell`**: Register `EscrowCell` drainage checks into `ObligationLedger` once continuation fission is implemented in `vesper-pulse` and the native backend.

---

## 5. Risks, Invariants, and Verification Plan

1. **Soundness of `fn_blake3` Verdict Caching**:
   - Only `Unit`, `ExpectError`, and seeded `Property`/`SimPulse` tests with **zero ambient host I/O** are cacheable. Any test exercising hardware PMU counters (`max_cycles`) or nondeterministic FFI must be marked non-cacheable.
   - Cache keys must include the compiler/harness ABI hash, target ISA, and transitive closure of called functions, types, and `@test` attributes.
2. **No Silent Passes on Unsupported Subsets**:
   - Following `tooling41-contracts.md`, if a `BudgetGate` requests PMU cycles on a host without PMU permissions, or a `SimPulse` test invokes an unsupported native feature, Anvil must report `Unsupported` / explicit fallback reason rather than silently passing.
3. **Verification Commands (per phase)**:
   - `cargo test -p vesper-test`
   - `cargo test --test '*' test_runner` (and new `tests/anvil_cli.rs` covering all 5 signature modes, `--mutate`, `--no-cache`, and `.vchrono` emission)
   - `cargo clippy --workspace --all-targets`
   - Bazel parity: `bazel test //crates/vesper-test/...`
