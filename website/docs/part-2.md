# 8 How the compiler sees a program

The parser identifies the tokens in a field assignment. The type checker resolves the field and its type, and the place checker verifies that the caller may write there.

<!-- technical-detail -->

Vesper compiles `.ves` source modules through a five-stage pipeline that resolves syntax, ownership modes, disjoint field projections, and lexical effects before emitting native machine code or pure Lean 4 verification models. Each stage narrows what later passes must trust by turning surface notation into explicit capability, control-flow, and rewrite representations.

In the repository checkout, the active batch compiler lives primarily in the root `vesper` crate under `src/` (`src/check.rs`, `src/ir.rs`, `src/ir_check.rs`, `src/optimize.rs`, `src/native.rs`, `src/lean.rs`, and `src/verified_codegen/`), supported by extracted workspace crates under `crates/` and 40 self-hosted Vesper compiler packages under `packages/vesper-*`. As documented in `REVIEW-STATUS.md` (`REPO-03`, `COMP-01`, and `TYPE-03` through `TYPE-10`), manuscript crate names such as `vesper-hir` and `vesper-mir-pcg` describe architectural boundaries rather than standalone Cargo crates, and the separate Lean metatheory developments do not yet establish end-to-end soundness of the combined compiler.

## 8.1 Follow the five stages

The proposed pipeline parses a lossless syntax tree, expands notation and resolves types, then checks places and refinements over control flow. Optimization searches for equivalent expressions while preserving effects. The last stage can lower supported operations to native instructions or translate them into a Lean model. A theorem about the Lean model covers the operations the translation represents.

<!-- technical-detail -->

Compilation lowers `.ves` files and `Loom.toml` packages through five deterministic stages targeting native execution and formal verification from a shared front end. Stage 1 runs zero-copy SIMD tokenization (`logos`) into an import-free LL(2) recursive-descent parser (`crates/vesper-syntax` and `crates/vesper-span`), producing a lossless `rowan` Red-Green Concrete Syntax Tree with open `PhraseSkeleton` clause, operator, and tagged-literal nodes memoized in `salsa` queries (`src/database/`) for `ariadne` diagnostics (`crates/vesper-diagnostics`). Stage 2 enforests stratified `sugar` rules under `[WF-SugarPass]`, expands hygienic `macro` declarations and fuel-metered `wasm32-wasip2` `#[wasm_macro]` components under `[T-MacroConfinement]` (`crates/vesper-forge` and `src/forge_frontend/`), resolves tabled trait and named `model` goals (`crates/vesper-pact` and `src/pact_frontend.rs`), and performs bidirectional modal elaboration (`src/check.rs`) with mode crossing (`[T-Cross]`) and lexical effect-row unification.

Stage 3 lowers typed AST/HIR into basic-block `PlaceIr` (`src/ir.rs`) and builds a `petgraph` Place Capability Graph (`src/ir_check.rs`) over field trees and slice intervals to enforce alias-XOR-mutability (`[T-CallProj]`, `[T-CallSliceProj]`) alongside exact-once `linear` consumption, complete `out`-slot initialization, and stack-ordered `lifo` release across every branch and `Fail` unwind edge (`[T-Let]`, `[T-Match]`). Per `REVIEW-STATUS.md` (`COMP-01` and `TYPE-09`), Bitwuzla integration is confined to supported `QF_UFBV` bit-vector obligations rather than an unconstrained source-level Horn or general refinement solver. Stage 4 runs effectful equality saturation (`egg` and `egglog` in `src/optimize.rs` and `src/ir_opt.rs`), threading state and capability tokens to fuse tail-resumptive handlers (`Fail`, lexical `Region` bumps), rewrite tail-modulo-cons (TMC) recursion into `out`-slot loops, honor `@fip` in-place constructor reuse (`src/fip.rs`), hoist bounds checks (`src/bounds_hoist.rs`), and emit checkable `EgglogRewriteWitness` certificates. Stage 5 splits into Stage 5A, which erases `@0` ghost bindings, clocks, and refinements to emit Cranelift IR (`src/native.rs`), `wasm32-wasip2` (`src/wasm_backend.rs`), Linux `eBPF` (`src/ebpf_backend.rs`), or direct scalar machine code (`src/verified_codegen/`), and Stage 5B, which translates checked `PlaceIr` with `@0` ghost witnesses intact into pure state-passing Lean 4 definitions (`src/lean.rs`).

    ┌────────────────────────────────────────────────────────────────────────────┐
    │ .ves Source  ──▶  Stage 1: logos SIMD + LL(2) rowan PhraseSkeleton CST     │
    │                   (memoized in salsa incremental queries + ariadne spans)  │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Stage 2: Stratified sugar + WASI #[wasm_macro] + vesper-pact + HIR         │
    │          Bidirectional modal inference, [T-Cross], lexical eff [...] rows  │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Stage 3: Basic-block PlaceIr + petgraph PCG + QF_UFBV Bitwuzla checks      │
    │          Alias-XOR-Mutability ([T-CallProj]), linear / out / lifo checks   │
    └──────────────────┬──────────────────────────────────────┬──────────────────┘
                       ▼                                      ▼
    ┌─────────────────────────────────────────┐  ┌───────────────────────────────┐
    │ Stage 4: egglog Equality Saturation     │  │ Stage 5B: Lean 4 Extractor    │
    │ Handler fusion, @fip, TMC, witnesses    │  │ Retains @0 ghost + refinements│
    └──────────────────┬──────────────────────┘  │ Pure lenses, $sync, EffM      │
                       ▼                         │ Emits Types/Funs/Specs.lean   │
    ┌─────────────────────────────────────────┐  └───────────────────────────────┘
    │ Stage 5A: Erase @0 ghost + refinements  │
    │ Cranelift (x64/A64/RV64/Wasm/eBPF)      │
    │ or --codegen verified scalar emitter    │
    └─────────────────────────────────────────┘

    $ vesper emit file.ves --emit cst
    $ vesper emit file.ves --emit hir
    $ vesper emit file.ves --emit ir
    $ vesper emit file.ves --emit clif
    $ vesper emit file.ves --emit lean
    $ vesper emit file.ves --emit object -o file.o
    $ vesper emit file.ves --emit interface --package @acme/demo -o demo.vhir

## 8.2 Find the implementation of a stage

Several compiler stages can share a Rust crate. The checkout implements the checker, Place IR, optimizer and code generation under `src/` and uses self-hosted code in `packages/vesper-*` through `crates/vesper-hybrid`. Names like `vesper-hir`, `vesper-mir-pcg`, `vesper-codegen-clif` and `vesper-verify-lean` describe roles in the proposed decomposition. Run `cargo metadata --no-deps --format-version 1` in the repository to find the actual workspace members before looking for an illustrative crate name.

<!-- technical-detail -->

Manuscript chapters use illustrative subsystem crate names (`vesper-hir`, `vesper-refine`, `vesper-mir-pcg`, `vesper-opt-egglog`, `vesper-codegen-clif`, `vesper-verify-lean`) to separate conceptual responsibilities, whereas the repository (`REPO-03`) keeps tightly coupled passes inside the root `src/` facade until their shared `World` tables are decoupled. Always run `cargo metadata --no-deps --format-version 1` to inspect live Rust workspace members. Alongside the Rust host compiler, `docs/maintenance/self-hosting-plan.md` tracks 40 self-hosted Vesper compiler packages in `packages/vesper-*` (such as `packages/vesper-front`, `packages/vesper-ast`, `packages/vesper-check-*`, `packages/vesper-place-ir`, `packages/vesper-ir-check`, `packages/vesper-ir-opt`, `packages/vesper-optimize`, `packages/vesper-egraph`, `packages/vesper-codegen-*`, `packages/vesper-wasm-backend`, `packages/vesper-ebpf-backend`, and `packages/vesper-lean-extract`) that swap incrementally into the differential hybrid driver via `crates/vesper-hybrid`, `src/ir_swap.rs`, and `src/codegen_swap.rs`. Because the Rust compiler cannot directly link a Vesper library artifact that requires the compiler itself to build, the workspace also carries 18 frozen `*/seed` Rust crates (`crates/vesper-*/seed`, such as `vesper-pact/seed`, `vesper-loom-calculus/seed`, and `vesper-semver/seed`) until Stage 2 self-hosting retires the Rust seed path.

    Stage / Subsystem   Manuscript Label          Active Rust Path                        Self-Hosted packages/vesper-*            Frozen */seed Crates
    ----                ----                      ----                                    ----                                     ----
    1. Lex / CST / IDE  vesper-span, -syntax      crates/vesper-{span,syntax,lsp}         vesper-front, -ast, -memo, -lsp*         span/seed, diagnostics/seed
    2. Sugar / Elab     vesper-hir, -pact, -forge src/{check,packages,pact_frontend}.rs   vesper-check-{data,core-typing,pact,*}   forge/seed, pact/seed
    3. Place IR / PCG   vesper-mir-pcg, -refine   src/{ir,ir_check}.rs, crates/vesper-core vesper-place-ir, -place-ir-lower, -ir-check alloc/seed, tensor/seed
    4. E-Graph Opt      vesper-opt-egglog         src/{optimize,ir_opt,bounds_hoist}.rs   vesper-egraph, -optimize, -ir-opt, -bounds-hoist (none)
    5A. Native / eBPF   vesper-codegen-clif       src/{native,wasm_backend,ebpf_backend}.rs vesper-codegen-*, -wasm-backend, -ebpf-backend ebpf/seed, silicon/seed
    5A. Direct Verified vesper-codegen-verified   src/verified_codegen*, crates/vesper-codegen-verified (stays Rust as diverse oracle) (none)
    5B. Lean 4 Extract  vesper-verify-lean        src/{lean,verify_commands,proof_commands}.rs vesper-lean-extract                 lean-syntax/seed
    Hybrid Swap Bridge  @vesper-lang/compiler     crates/vesper-hybrid, src/{ir,codegen}_swap.rs vesper-compiler (Stage 1 seed)    18 crates/vesper-*/seed

## 8.3 Represent an operation in high-level IR

High-level intermediate representation (HIR) retains resolved names, ownership modes, and effect rows. A field tree groups projections by their parent. Two leaves under the same broker can denote two writable fields. A lexical effect label identifies the handler scope an operation depends on.

<!-- technical-detail -->

High-level IR (HIR) pairs resolved declarations with substructural usage modes, continuation resumption guarantees, parameter passing projections, and open lexical effect rows (`crates/vesper-core/src/field_tree.rs`, `modal_kind.rs`, and `src/check.rs`). The primary usage axis orders `Mode::Copy < Mode::Affine < Mode::Linear`, where `join` takes the maximum (most restrictive obligation) and `meet` takes the minimum. Each effect declaration carries a `ResumptionMode` (`Affine` for abortable handlers such as `Fail[E]`, `Linear` for exact-once resumption such as `Storage`), and `EffectRow::resumption_guarantee` folds `meet` across active effect instances so a single abortable capability lowers the whole row's guarantee to `Mode::Affine`. Field projections use a prefix tree `FieldTree` where an empty `children` map marks a leaf covering the entire sub-place, allowing `is_disjoint` to certify parallel loans (`broker.{index, metrics}`) and `is_subset_of` to check SemVer footprint narrowing.

    // Representative HIR modal, field-tree, and lexical effect types
    #[repr(u8)]
    pub enum Mode { Copy = 0, Affine = 1, Linear = 2 }
    impl Mode {
        pub fn join(self, other: Self) -> Self { core::cmp::max(self, other) }
        pub fn meet(self, other: Self) -> Self { core::cmp::min(self, other) }
        pub fn is_discardable(self) -> bool { self <= Self::Affine }
    }
    // Continuation contract q in {affine, linear} and second-class passing modes
    #[repr(u8)]
    pub enum ResumptionMode { Affine = 1, Linear = 2 }
    pub enum PassMode { In, InOut, Out, Sink }
    pub struct FieldIdx(pub u16);
    pub struct FieldTree { pub children: FxHashMap<FieldIdx, FieldTree> }
    impl FieldTree {
        pub fn whole_place() -> Self { Self { children: FxHashMap::default() } }
        pub fn is_disjoint(&self, other: &Self) -> bool {
            if self.children.is_empty() || other.children.is_empty() { return false; }
            self.children.iter().all(|(f, lhs)| {
                other.children.get(f).is_none_or(|rhs| lhs.is_disjoint(rhs))
            })
        }
        pub fn is_subset_of(&self, older: &Self) -> bool {
            if older.children.is_empty() { return true; }
            if self.children.is_empty() { return false; }
            self.children.iter().all(|(f, sub)| {
                older.children.get(f).is_some_and(|old_sub| sub.is_subset_of(old_sub))
            })
        }
    }
    pub struct EffectInst {
        pub cap_label: Option<SmolStr>,
        pub effect_id: u32,
        pub resumption: ResumptionMode,
    }
    pub struct EffectRow { pub instances: Vec<EffectInst>, pub tail_var: Option<u32> }
    impl EffectRow {
        pub fn resumption_guarantee(&self, tail_bound: Mode) -> Mode {
            self.instances.iter().fold(tail_bound, |acc, i| acc.meet(i.resumption.as_mode()))
        }
    }

## 8.4 Check modes, ordered obligations, and costs

One helper combines field modes. Another tracks the stack of ordered guards. Others track initialization and resource intervals. For example, exit from both arms of a branch must agree on which guard is topmost. A bound on stack cost uses different composition rules from a bound on allocated bytes.

<!-- technical-detail -->

Rather than a monolithic `vesper-refine` Horn solver crate, the repository currently provides audited standalone domain helpers inside `crates/vesper-core`. Specifically, `mode_vec::{ModeVec, CapsuleEvidence}` models the 24-element four-axis product lattice (`usage`, `portability`, `contention`, `release_order`) and checks unique, unordered, alias-free evidence for `capsule` isolation (`TYPE-04`); `ghost::{Binding, Context, Presence}` demotes runtime bindings to shared ghost observations while blocking ghost side effects and premature erasure with live ghost tokens (`TYPE-05`); `lifo::LifoState` enforces strict stack-ordered guard release (`TYPE-09`); `out_slice::OutSlice` tracks constant disjoint output-slice initialization intervals; and `resource_grade::{GradeKind, IntervalBound, RefineTerm, ResourceGradeError, verify_graded_resource_ceiling}` distinguishes nested `GradeKind::caller_stack_depth` frame addition from sequential peak budgets (`TYPE-09`). These modules are standalone checked building blocks, not an integrated source refinement pass, and a verified interval bound in `vesper-core` does not automatically authorize native bounds-check erasure.

    // Standalone checked analysis helpers in crates/vesper-core
    pub use vesper_core::mode_vec::{CapsuleEvidence, ModeVec};
    pub use vesper_core::ghost::{Binding, Context, Presence};
    pub use vesper_core::lifo::LifoState;
    pub use vesper_core::out_slice::OutSlice;
    pub use vesper_core::resource_grade::{
        GradeKind, IntervalBound, RefineTerm, ResourceGradeError,
        verify_graded_resource_ceiling,
    };
    // Nested calls sum live caller frame bytes with the peak callee frame:
    fn nested_stack(frame: u64, callee: u64) -> Result<u64, ResourceGradeError> {
        GradeKind::caller_stack_depth(frame, &[callee])
    }
    // Interval ceiling check; does not by itself erase runtime bounds checks:
    fn arena_bound(
        term: &RefineTerm,
        ceiling: u64,
        env: &std::collections::BTreeMap<String, IntervalBound>,
    ) -> Result<bool, ResourceGradeError> {
        verify_graded_resource_ceiling("Region", term, ceiling, env)
    }

## 8.5 Follow place permissions across control flow

Place IR divides execution into basic blocks joined by branches. A place capability graph records which locations are owned, lent, initialized, or moved at each point. Joining branches exposes a ticket consumed on only one path. At a possible suspension, check which live values the continuation would carry.

<!-- technical-detail -->

After elaboration, `PlaceIr` organizes each function into basic blocks of `Statement` nodes (`Assign`, `UnpackLinear`, `CallProj`) terminated by `Goto`, `SwitchInt`, `PerformEffect`, or `Return`. The Place Capability Graph solver (`src/ir_check.rs` and `crates/vesper-core/src/pcg.rs`) maps every `LocalId` in `PcgState` to `LocalCapability::LiveOwned`, `LocalCapability::UninitializedOutSlot`, or `LocalCapability::Consumed`. At `CallProj`, `check_call_projections` rejects any pair of projections on the same root local that overlap in `FieldTree` unless both are `PassMode::In`, marks `Sink` arguments `Consumed`, promotes `Out` slots to initialized `LiveOwned`, and checks the Resumption Frame Condition: if `effects.resumption_guarantee(Mode::Linear)` is `Mode::Affine`, no live `Mode::Linear` binding may remain suspended in the caller frame. Crucially, `UninitializedOutSlot` bindings safely ignore abortable unwinds (`abort_unwind` on `PerformEffect` or fallible calls): an unfilled `out` slot denotes caller-allocated backing storage carrying no live linear resource yet, whereas `check_return_exhaustion` still demands complete initialization before any normal `Return`, and `join_at_block` rejects branches that consume a `Mode::Linear` value on one path while leaving it live on another.

    pub enum Statement {
        Assign { dst: LocalId, ty_mode: Mode },
        UnpackLinear { src: LocalId, bound_locals: Vec<(LocalId, Mode)> },
        CallProj { args: Vec<(PassMode, PlaceProjection)>, ret_dst: LocalId, ret_mode: Mode, effects: EffectRow },
    }
    pub enum Terminator {
        Goto(BlockId),
        SwitchInt { discr: LocalId, targets: Vec<BlockId> },
        PerformEffect { effects: EffectRow, resume_target: BlockId, abort_unwind: Option<BlockId> },
        Return,
    }
    pub enum LocalCapability {
        LiveOwned { mode: Mode, moved_subtrees: FieldTree },
        UninitializedOutSlot { initialized_subtrees: FieldTree },
        Consumed,
    }
    impl PcgState {
        pub fn check_call_projections(&mut self, args: &[(PassMode, PlaceProjection)], effects: &EffectRow) -> Result<(), PcgError> {
            for (i, (m_i, p_i)) in args.iter().enumerate() {
                for (m_j, p_j) in &args[i + 1..] {
                    if !matches!((m_i, m_j), (PassMode::In, PassMode::In))
                        && p_i.local == p_j.local && !p_i.tree.is_disjoint(&p_j.tree)
                    {
                        return Err(PcgError::OverlappingProjection { local: p_i.local });
                    }
                }
            }
            for (pass_mode, proj) in args {
                match *pass_mode {
                    PassMode::Sink => { self.locals.insert(proj.local, LocalCapability::Consumed); }
                    PassMode::Out => {
                        self.locals.insert(proj.local, LocalCapability::LiveOwned { mode: Mode::Linear, moved_subtrees: FieldTree::default() });
                    }
                    PassMode::In | PassMode::InOut => {}
                }
            }
            // UninitializedOutSlot carries no live linear obligation across abort unwinds;
            // only LiveOwned bindings with mode > res_guarantee violate frame safety:
            let res_guarantee = effects.resumption_guarantee(Mode::Linear);
            for (&local, cap) in &self.locals {
                if let LocalCapability::LiveOwned { mode, .. } = cap && *mode > res_guarantee {
                    return Err(PcgError::ResumptionFrameViolation { live_linear: local });
                }
            }
            Ok(())
        }
        pub fn join_at_block(&self, other: &Self, block: BlockId) -> Result<Self, PcgError> { /* rejects asymmetric Mode::Linear consumption */ }
        pub fn check_return_exhaustion(&self) -> Result<(), PcgError> { /* rejects LiveOwned(Linear) and UninitializedOutSlot */ }
    }

# 9 What a Lean proof verifies

Extraction translates supported Vesper operations into definitions in the Lean proof assistant. A theorem establishes a property of those definitions. To apply it to a native binary, you also need evidence that the compiler preserved the modeled behavior.

<!-- technical-detail -->

Proof extraction translates a checked sequential subset of `PlaceIr` into pure, prophecy-free Lean 4 definitions so engineers can prove functional invariants over storage transactions, protocol state machines, and arithmetic kernels. Because Vesper restricts mutable loans to second-class call projections and enforces linear cleanup statically, extracted models avoid backward prophecy variables and heap-separation framing while remaining subject to explicit kernel replay and axiom-hygiene gates.

In the repository, model extraction and proof auditing are implemented in `src/lean.rs`, `src/verify_commands.rs`, `src/proof_commands.rs`, and `tools/proof-audit/Audit.lean`, with mechanized place and ghost bridges in `lean/StrongUpdate.lean`, `lean/RefinementPlaces.lean`, `lean/GhostErasure.lean`, and `lean/OperationalGhostBridge.lean`. Per `REVIEW-STATUS.md` (`COMP-01` through `COMP-05`, `TYPE-03`, and `REPO-02`), these artifacts verify theorems about the extracted functional representation only: full extraction soundness, general loop termination, macro-provenance completeness, and semantic preservation down to Cranelift or hardware execution remain separate obligations.

## 9.1 Choose a verification boundary

Specify inputs, assumptions, and an observable result. For a buffer update you might require an in-bounds index and promise that other elements retain their values. Ask which allocation, scheduling, and failure behavior the extracted model includes before applying that result to a running service.

<!-- technical-detail -->

Before applying a Lean theorem to a production service, inspect what the extracted model represents and how Vesper's call-scoped loans simplify verification relative to first-class reference extractors such as Charon/Aeneas (`LLBC`) or Hax. When mutable borrows cannot outlive a call, a callee's state change resolves completely at return without future-value prophecies, and `PlaceIr` disjointness proofs replace separation-logic frame rules on immutable value aggregates.

| Dimension | First-Class Reference Extraction (LLBC / Separation Logic) | Vesper Prophecy-Free Extraction (src/lean.rs) |
| --- | --- | --- |
| Mutable Borrowing | Region reconstruction and backward prophecy variables | Call-scoped inout x.pi and out y lower to pure tuple returns + lawful lenses |
| Aliasing & Framing | Separation-logic heap framing or Tree-Borrows tracking | PCG proves p_i perp p_j statically; Lean models aggregates as pure Type values |
| Protocol Finalization | Cannot assume Drop ran or succeeded on early error paths | Mode::Linear and [T-Let] enforce exact-once discharge across Ok and Fail edges |
| Async & I/O Effects | Opaque Pin coroutine state machines re-modeled by hand | One-shot eff [...] rows lower to Freer Interaction-Tree monads (EffM) |
| Modular Proof Caching | Ad-hoc subproject rebuilds exposed to axiom/toolchain drift | Kernel-replayed .olean certificates (OLEAN_CERTS) with mif:blake3 early cutoff |

## 9.2 Turn mutation into a returned value

A lens pairs a getter with an operation that rebuilds a larger value after changing one part. Extraction can represent a write to a record field by returning the updated record. Mutable parameters become additional outputs. Effects need an explicit model, and a recursive translation of a loop needs a termination argument for a total function. The manuscript sketches a `Notations.lean` layer to preserve the shape of source expressions. That layer is a proposal.

Suppose a pair starts as (4, 9) and the callee writes 7 to its first field. The functional result is (7, 9). Reading back the changed field gives 7, and the sibling stays 9. The bounded loan ends at the call, so the caller receives the actual new pair. The chapter calls this translation “prophecy-free” because the model can compute the final value at the call boundary.

<!-- technical-detail -->

The functional translation targets a sequential, deterministic verification profile: concurrent shared-memory interleavings, asynchronous completion ordering, hardware floating-point corner cases, unmodeled allocator exhaustion, and wire endianness lie outside its soundness scope. Machine scalars (`u8`, `u16`, `u32`, `u64`, `i64`) lower to `BitVec`-backed Lean 4 fixed-width types (`UInt8`, `UInt16`, `UInt32`, `UInt64`, `Int64`) amenable to `bv_decide` and `omega`, with checked `+`, `-`, and `*` raising `Fail[ArithOverflow]` while `wrapping_add` lowers to total `UInt64.add`. Surface refinements `{ v: T | phi }` become Lean subtypes `{ v : T // phi v }`, `@0` `ghost` bindings remain explicit Lean parameters, `quotient Q = repr R modulo eq` maps to `Quot eq`, structs map to `structure`, and enums map to `inductive` (`Never` to `Empty`, `Unit` to `Unit`). For unconstrained record projections `inout x.pi`, the extractor synthesizes `get_pi` and `set_pi` satisfying the three lens laws (`set_get_pi`, `get_set_pi`, and disjoint sibling preservation `get_pi2 (set_pi1 s v) = get_pi2 s`) by `rfl`; however, projections across enum variants, coupled multi-field refinements, bounded array slices, or quotients do not reduce by `rfl` and require partial prisms, bounds proofs, or quotient-lift obligations (`TYPE-03`, `TYPE-08`).

Every call takes initialized `in`, `inout`, and `sink` inputs by value and returns `RetTuple(f)` bundling the nominal return with updated `inout` and `out` values across three adaptive monad tiers. Tier 0 (`eff []`) emits a bare total function returning `RetTuple(f)`. Tier 1 (`eff [Fail[E]]`) emits `Except E RetTuple(f)` when no stationary `inout` parameter can observe an abort, or generalizes over a monad `m` with a trailing synchronization callback `$sync : Sigma_io -> m Unit` invoked with current `inout` values prior to every potentially aborting step (`PostCall_abort`); instantiating `m := ExceptT E (StateM Sigma_io)` with `$sync := set` preserves exact pre-abort mutations on `.error`, whereas indirect calls through function values combining `inout` parameters with abortable effect rows are refused rather than mis-modeled with rollback. Tier 2 lowers general one-shot lexical effect rows to the universe-polymorphic Freer Interaction-Tree monad `EffM`. Natural CFG loops undergo functional SSA conversion into tail-recursive `f.loop` definitions whose `decreases` clauses emit `termination_by` obligations discharged by `omega`. Finally, while `@lean_notation` `sugar` rules are designed to emit companion `Notations.lean` macros and `@[app_unexpander]` pretty-printers (`content.typ:2768`), `Notations.lean` generation is not yet implemented in `src/lean.rs`.

    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Validated PlaceIr (inout x.pi, out y, @0 ghost, {v: T | phi}, eff [...])   │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Lawful Disjoint Lenses (get_pi / set_pi) + RetTuple(f) = T_ret × T_mut_out │
    ├───────────────────────┬──────────────────────────┬─────────────────────────┤
    │ Tier 0: eff []        │ Tier 1: eff [Fail[E]]    │ Tier 2: eff [E1, ...]   │
    │ Total RetTuple(f)     │ Except E + $sync :       │ Freer Interaction-Tree  │
    │                       │ Sigma_io -> m Unit       │ EffM sig (RetTuple f)   │
    └───────────────────────┴─────────────┬────────────┴─────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Lean 4.35.0 Bundle (Types/Funs/Specs.lean; Notations.lean unimplemented)   │
    │ Fresh .olean byte check ──▶ Audit.lean Replay (0 sorryAx, no _native)      │
    │                         ──▶ Vesper.f$contract + Vesper.f$admissible        │
    │                         ──▶ Nightly logos42 replay (158 Lean modules)      │
    └────────────────────────────────────────────────────────────────────────────┘

    -- Lawful record lens obligations (rfl on plain structs; prisms/obligations on enums, slices, quotients):
    --   set_get_pi : set_pi s (get_pi s) = s
    --   get_set_pi : get_pi (set_pi s v) = v
    --   frame_pi   : get_pi2 (set_pi1 s v) = get_pi2 s   (for pi_1 perp pi_2)
    -- Tier 1 stationary inout abort synchronization:
    --   f_sync (inputs...) ($sync : Sigma_io -> m Unit := fun _ => pure ()) : m (RetTuple f)
    -- Tier 2 Freer Interaction-Tree monad (VesperStd.EffM):
    inductive EffRowSig where
      | nil  : EffRowSig
      | cons : (Op : Type) -> (Ret : Op -> Type) -> ResumptionMode -> EffRowSig -> EffRowSig
    inductive EffM : EffRowSig -> Type -> Type 1 where
      | pure : a -> EffM sig a
      | vis  : MemberOp Op Ret q sig -> (op : Op) -> (Ret op -> EffM sig a) -> EffM sig a

## 9.3 Specify what a batch append changes

The `KrabkaWal.appendBatch` illustration models `walSlot`, `dirtyPages` and `batchesWritten`. Its `appendBatch_crc_err_aborts_cleanly` theorem states that a failed CRC takes the abort path and preserves the original modeled state. Device durability still depends on the storage implementation.

<!-- technical-detail -->

In the `KrabkaWal` write-ahead log showcase (`content.typ:2775–2855`), extracting `src/wal_txn.ves` emits `WriteTxn` (`walSlot : UInt64`, `dirtyPages : BufferPoolSlice`), `CommitReceipt`, and the projected record `BrokerIndexMetricsProj` (`index : SegmentedIndex`, `batchesWritten : UInt64`) paired with `Broker.getIndexMetrics` and `Broker.setIndexMetrics` so `(b.setIndexMetrics p).config = b.config` holds by `rfl`. Inside `appendBatch`, a `Storage.beginTxn` handle `tx` is either consumed by `WriteTxn.commit` after `validateCrc` and `checkedAddU64` succeed or unwound cleanly via `Storage.recycleBatch` and `WriteTxn.abort` on a CRC failure. Proving `appendBatch_crc_err_aborts_cleanly` over `RefStorageModel.run` verifies that any CRC error returns `.error crcErr` with `activeWalSlots` and `pinnedSlices` restored to their pre-call values, though this theorem covers only the modeled `StorageOp` interpreter and not physical disk durability or DMA completion.

    namespace KrabkaWal
    structure WriteTxn where
      walSlot    : UInt64
      dirtyPages : BufferPoolSlice
    structure CommitReceipt where
      walSlot : UInt64
    structure BrokerIndexMetricsProj where
      index          : SegmentedIndex
      batchesWritten : UInt64
    def appendBatch
        (brokerProj : BrokerIndexMetricsProj) (config : BrokerConfig) (batch : RecordBatch)
        : EffM [StorageEff, FailEff IoError] (CommitReceipt × BrokerIndexMetricsProj) := do
      let tx <- liftLinear (Storage.beginTxn config.maxBatchBytes)
      match validateCrc batch with
      | .ok () =>
        let index' := SegmentedIndex.insertInPlace brokerProj.index batch
        let written' <- checkedAddU64 brokerProj.batchesWritten 1
        let receipt <- WriteTxn.commit tx
        pure (receipt, { index := index', batchesWritten := written' })
      | .error err =>
        liftLinear (Storage.recycleBatch batch)
        liftLinear (WriteTxn.abort tx)
        Fail.raise err
    theorem appendBatch_crc_err_aborts_cleanly
        (st : RefStorageModel) (proj : BrokerIndexMetricsProj)
        (cfg : BrokerConfig) (batch : RecordBatch) (crcErr : IoError)
        (h_crc : validateCrc batch = .error crcErr) :
        let (st', res) := RefStorageModel.run st (appendBatch proj cfg batch)
        res = .error crcErr ∧
        st'.activeWalSlots = st.activeWalSlots ∧
        st'.pinnedSlices   = st.pinnedSlices := by
      simp [appendBatch, h_crc, WriteTxn.abort, RefStorageModel.run]
      omega
    end KrabkaWal

## 9.4 Admit a compiled proof artifact

A compiled Lean module has the extension `.olean`. The gate replays artifacts and inspects the axioms their declarations depend on, including indirect dependencies. For a cached extraction, it compares against a fresh compilation. Checking a contract also requires the expected theorem signature and an admissible input. Checking the expected signature rules out a valid proof about an unrelated function.

<!-- technical-detail -->

A textual `0 sorryAx` scan cannot secure proof admission against user `axiom` declarations, `opaque`/`unsafe` substitutions, `Lean.ofReduceBool`, `Lean.trustCompiler`, or cached `.olean` swapping. In `src/proof_commands.rs` and `tools/proof-audit/Audit.lean`, Vesper snapshots bounded `.olean` inputs (up to 128 MiB / 4,096 files), imports package modules with environment extensions and initializers disabled, and replays every declaration in a fresh kernel environment via `Lean.Replay`. The default axiom allowlist is empty, and `forbidden_axiom` unconditionally rejects `sorryAx`, `Lean.ofReduceBool`, `Lean.trustCompiler`, and any Lean 4.35 generated axiom containing a `_native` namespace component (`Meta.nativeEqTrue`), even if explicitly passed to `--allow-axiom`; theorems touching `$macro_provenance` definitions additionally require `--macro-provenance`. On extraction cache hits, `vesper verify --extract` compiles the current bundle freshly and demands byte-for-byte `.olean` equality before replaying the fresh artifact closure, blocking cache-key substitution attacks.

When binding proofs to source contracts via `--contract-proof Function=Theorem`, the gate requires a companion `--contract-witness Function=Theorem` for every emitted contract. Each proof wrapper must match the generated `Vesper.<Function>$contract` constant with zero extra hypotheses or implicit section instances (`checked_contracts`), and each witness must prove the closed existential precondition `Vesper.<Function>$admissible` (`checked_preconditions`) using at most standard mathematical axioms (`propext`, `Classical.choice`, `Quot.sound`), ruling out vacuous contracts with contradictory `where` clauses. Beyond Lean's own kernel, CI runs the independent pinned Logos kernel checker (`tools/logos42` via `tools/logos-sweep`) nightly and on Lean/extractor PRs across all 158 repository Lean modules and extraction fixtures with zero rejections; Logos currently declines the RV64 and AArch64 `decode_encode` encoding theorems due to its fixed recursion depth ceiling, leaving those two theorems and their dependents verified by Lean's kernel alone (`REVIEW-STATUS.md:43`).

## 9.5 Distinguish extraction, audit, and publication

Extraction starts from a checked source module and produces a Lean bundle. An artifact audit starts from compiled proof evidence. Publication can apply an admission policy to evidence attached to a release. Follow the artifact and identify which gate checked it before treating a proof report as evidence for a particular source contract.

For an existing module called `Model`, an explicit `vesper proof-audit` request names the theorem, such as `Example.correct`, and the directory holding its artifacts. In contrast, `vesper verify --extract` starts with the Vesper file. Both paths need the pinned Lean toolchain. The default axiom policy is empty. Explicitly allowed mathematical axioms remain assumptions. Compiler and proof escape axioms are rejected.

~~~sh
vesper proof-audit Model --root ./artifacts \
  --theorem Example.correct --lean /path/to/lean --json
vesper verify program.ves --extract --proofs proofs.lean \
  --theorem Example.correct --lean /path/to/lean --json
~~~

<!-- technical-detail -->

Vesper separates three verification entry points across `src/verify_commands.rs`, `src/proof_commands.rs`, and `crates/vesper-tools/src/publish.rs`. Running `vesper verify --extract` translates a checked `.ves` module into a Lean bundle, compiles optional user proofs, and verifies explicit contract and admissibility bindings; `vesper proof-audit` replays precompiled `.olean` artifacts in a local snapshot root against a named theorem list and axiom policy; and `vesper publish` can apply `.olean` replay as an offline release gate, though package-wide contract-signature enforcement and SemVer proof transport across upgrades remain open (`content.typ:2864`). Both commands require the pinned Lean 4.35.0 executable, validate structured `VESPER_PROOF_AUDIT_V1:` output while rejecting any subprocess `PANIC` output even on exit code zero, and carry an operational security boundary: disabling `.olean` initializers during replay does not make Lean source or tactic elaboration an operating-system sandbox, so untrusted `.lean` proof scripts must only be compiled inside external OS containment.

    $ vesper proof-audit Model --root ./artifacts \
        --theorem Example.correct --lean /path/to/lean --json
    $ vesper verify program.ves --extract --proofs proofs.lean \
        --theorem Example.correct \
        --contract-proof append_batch=KrabkaWal.appendBatch_contract \
        --contract-witness append_batch=KrabkaWal.appendBatch_admissible \
        --lean /path/to/lean --json

# 10 Bootstrapping a compiler

Building a compiler with itself tests whether successive builds agree. Independent builds help check where the binary came from. Rewrite witnesses record how to check that an optimization preserved behavior.

<!-- technical-detail -->

Self-hosting and diverse double-compiling (DDC) test whether a compiler can reproduce its own binary deterministically and agree with an independently implemented translation path. Combined with proof-producing optimization witnesses and a small non-Cranelift backend, multi-stage bootstrap narrows where miscompilation or seed-binary trust can hide.

In the repository, the Stage 0 Rust compiler (`src/`), the direct scalar verified backend (`src/verified_codegen.rs` and `src/verified_codegen/`), the Stage 1 Vesper front-end seed (`packages/vesper-compiler`), the committed lockfiles (`bootstrap/stage0_seed.lock` and `bootstrap/stage1.lock`), and the offline quorum auditor (`crates/vesper-codegen-verified`) are active and tested. As recorded in `REVIEW-STATUS.md` (`COMP-01` through `COMP-05`) and `docs/maintenance/self-hosting-plan.md`, full Stage 2/3 self-hosting is still progressing across hybrid waves SH0–SH6, rewrite witnesses are not yet wired into `vesper-codegen-verified`, and neither the Lean codegen models nor the DDC lock schema eliminate host OS, linker, hardware, or C-runtime trust.

## 10.1 Identify the trusted components

The design combines an executable reference model, replayable rewrite evidence, backend validation, a confined self-hosted build, and diverse double-compiling. A reference run can catch a disagreement on a particular input. A simulation theorem relates steps in two models under stated assumptions. For a simulation result, record the assumptions. For a reference run, record the inputs.

<!-- technical-detail -->

Toolchain assurance rests on five complementary mechanisms with explicit scope limits. First, Appendix A's computable Lean 4 reference typechecker (`checkExpr`) and fuel-stepped evaluator (`evalSteps`) pair with Crucible (`vesper fuzz`) to run differential diagnostic and resource-trace lockstep tests. Second, Stage 4 equality saturation targets linear-time `EgglogRewriteWitness` checking (`src/optimize.rs` and `lean/RewriteWitness.lean`) rather than trusting e-graph search. Third, Stage 5 contrasts the high-throughput Cranelift backend (`src/native.rs`, paired with `--verify=const-time,stack,realtime,detfloat` Stage-5 verifiers) against `vesper build --codegen verified` (`src/verified_codegen.rs` and `src/verified_codegen/`), which lowers a scalar and flat-aggregate subset (`u8`, `u32`, `u64`, `i64`, `f64`, `bool`, `Unit`, monomorphized structs/tuples/non-recursive enums, checked and wrapping arithmetic, abort-only `Fail[E]`, direct/tail recursion, and copy-in/copy-out `inout` parameters) directly to `x86_64`, `AArch64`, and `RV64` machine code without Cranelift, explicitly refusing heap allocation, closures, strings, sequences, atomics, and resuming handlers via `VerifiedError::Unsupported`.

Inside the verified backend's proof stack (`REVIEW-STATUS.md:45–47`), `lean/VerifiedCodegen.lean` proves single-row forward simulation, `lean/CodegenComposition.lean` composes straight-line row streams and arithmetic aborts under reachable operand bounds, `lean/CodegenControlFlow.lean` extends simulation to abstract branches, jumps, halts, and loop divergence under an inductive invariant, and `lean/CodegenValidation.lean` plus `lean/CodegenFlowValidation.lean` provide executable validators (`validateProgram` and `widened_validation_sound`) establishing those operand and control invariants. At compile time, `src/verified_codegen/certificate.rs` synthesizes per-row range certificates and checks them with a differentially tested Rust mirror of `validateProgram`; `Options::require_certificate` can reject uncertified builds (currently off by default until variant-sensitive enum payload and `Fail` status word facts land), while rules for `Enter`, `Call`, `Return`, trap-scope handler edges, physical branch offsets, and binary encodings remain conservative Rust checks rather than Lean theorems. Fourth, self-hosted compiler packages run inside bounded regions under `capability_ceiling = ["Fail"]`. Fifth, prospective CompCert DDC (`content.typ:2896`) faces an open runtime boundary: Lean-emitted C relies on Lean's C runtime headers and extensions, which do not yet have an independently validated CompCert compilation path.

## 10.2 Compare compiler generations

Stage 0 is a seed compiler. It builds a compiler from source to produce stage 1. Stage 1 compiles the same source to produce stage 2. Diverse double-compiling repeats the chain from a sufficiently independent compiler and compares the resulting artifacts under reproducible build conditions. Two matching builds from one seed establish a narrower result.

    Seed A + source → compiler A1      Independent seed B + source → compiler B1
    A1 + same source → compiler A2     B1 + same source → compiler B2

Compare A2 with B2 under the same build recipe. The first-stage binaries can differ because the two seeds generated them differently. A second-stage mismatch points to a difference worth tracing through the inputs, compilers, and environment.

<!-- technical-detail -->

Bootstrap progresses from Stage 0 (`vesperc-rs` host Rust seed differentially tested against Lean semantics) to Stage 1 (`packages/vesper-compiler` compiled by Stage 0), Stage 2 (self-compiled inside `vesper-loom-sandbox` with epoch-zero clocks and no network), and Stage 3 (fixed-point verification where Stage 2 rebuilds an identical Stage 3 binary). Today, `bootstrap/stage0_seed.lock` records deterministic Stage 0 Rust seed builds (`seed_binary_a == seed_binary_b`) and semantic agreement across 10 corpus programs against Lean-extracted GCC binaries (`ddc_semantic`), while `bootstrap/stage1.lock` freezes `packages/vesper-compiler` (an 8.4k-line byte-scanning lexer, parser, and partial checker) across 10 corpus digests (`ackermann`, `collatz`, `divide_by_zero`, `factorial_overflow`, `fib`, `gcd`, `lets_negative`, `lexer-byte-classes`, `max3`, `pow_mod`, `sum_squares`) built by both Cranelift and `--codegen verified`. Because two matching builds from a single seed cannot rule out a self-propagating seed trojan, diverse double-compiling requires an independent compiler `compileRef` to build `stage1_B = compileRef(compilerSrc)` and `stage2_B = exec(stage1_B, compilerSrc)` so `stage2_B` can be compared byte-for-byte against `stage2_A`; the `bootstrap/stage3_ddc.lock` manifest below is an illustrative target schema (`content.typ:2904–2921`) audited by `crates/vesper-codegen-verified` rather than a live attestation.

    ┌──────────────────────────────┐        ┌──────────────────────────────────┐
    │ Path A: Stage 0 Seed         │        │ Path B: Diverse Reference Seed   │
    │ vesperc-rs (stage0_seed.lock)│        │ Lean 4 + non-Rust C compiler     │
    └──────────────┬───────────────┘        └────────────────┬─────────────────┘
                   │ compiles compilerSrc                    │ compiles compilerSrc
                   ▼                                         ▼
    ┌──────────────────────────────┐        ┌──────────────────────────────────┐
    │ Stage 1 (A1: stage1.lock)    │        │ Stage 1_B = compileRef(src)      │
    └──────────────┬───────────────┘        └────────────────┬─────────────────┘
                   │ builds in WASI sandbox                  │ builds in WASI sandbox
                   ▼                                         ▼
    ┌──────────────────────────────┐        ┌──────────────────────────────────┐
    │ Stage 2_A (Self-Hosted Bin)  │        │ Stage 2_B = exec(Stage 1_B, src) │
    └──────────────┬───────────────┘        └────────────────┬─────────────────┘
                   │ rebuilds compilerSrc                    │
                   ▼                                         │
    ┌──────────────────────────────┐                         │
    │ Stage 3_A (Fixed-Point Bin)  │                         │
    └──────────────┬───────────────┘                         │
                   └────────────────────┬────────────────────┘
                                        ▼
    ┌──────────────────────────────────────────────────────────────────────────┐
    │ audit_ddc_bootstrap_quorum (crates/vesper-codegen-verified):             │
    │ BLAKE3(Stage 2_A) == BLAKE3(Stage 3_A) == BLAKE3(Stage 2_B) + Ed25519    │
    └──────────────────────────────────────────────────────────────────────────┘

    [bootstrap]
    # Illustrative target schema: bootstrap/stage3_ddc.lock (records no live attestation yet)
    compiler_package     = "@vesper-lang/compiler"
    version              = "1.0.0"
    source_merkle_blake3 = "blake3:7a19e4c0f82b31d69a04e581c3b72f109e84d2a16c5b30f9e1d2a4c8b7f60123"
    lean_spec_mif        = "mif:blake3:3c8e91a204f7b5d1e6c28094a7b3d5f18290e4c6b1a3f5d790e2c4a6b8d0f1e2"
    allowed_lean_axioms  = ["propext", "Quot.sound", "Classical.choice"]
    [fixed_point_quorum]
    path_a_stage2_blake3      = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
    path_a_stage3_blake3      = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
    path_b_lean_compcert_ddc  = "blake3:e9f01b4c8d2a76543210fedcba9876543210fedcba9876543210fedcba987654"
    lila_builder_signatures   = 3
    aegis_tdx_snp_attested    = true

## 10.3 Make the driver’s inputs explicit

A hermetic compiler driver reads only the source, tools, and capabilities supplied to the build. Pinning inputs makes a mismatch between generations easier to investigate. For example, embedding the current time can make two builds of the same program produce different bytes.

<!-- technical-detail -->

To keep cross-generation builds bit-reproducible, the target self-hosted compiler driver (`@vesper-lang/compiler`, `capability_ceiling = ["Fail"]`) takes source bytes, a `CapabilityCeiling`, and a caller-provided `ByteSliceMut` output buffer inside a deterministic `Region::scoped` arena with no ambient clock, filesystem, or network effects (`eff [Isa[P], Fail[CompileDiag]]`). Per `REVIEW-STATUS.md` (`COMP-03`) and `content.typ:2928–2930`, the compact `RewriteStepWire` header in the manuscript listing is illustrative only: a `rule_id`, `target_node_idx`, and `subst_arity` tuple cannot by itself validate an optimization step, so a sound production witness must record explicit e-class variable substitutions, congruence paths from the root expression, and side-condition certificates for arithmetic non-overflow or ownership preservation.

~~~vesper
// Illustrative hermetic driver (packages/vesper-compiler/src/driver.ves)
// COMP-03 note: production witnesses additionally require substitutions,
// congruence paths, and side-condition certificates beyond RewriteStepWire.
copy struct RewriteStepWire {
    rule_id: u16,
    target_node_idx: u32,
    subst_arity: u16,
}
// Purely arena-scoped compilation unit under capability_ceiling = ["Fail"]:
pub fn compile_unit_verified[P: IsaProfile](
    in src_bytes: ByteSlice,
    in ceiling: CapabilityCeiling,
    inout out_elf: ByteSliceMut,
) -> Result[UnitArtifactDigests, CompileDiag] eff [Isa[P], Fail[CompileDiag]] {
    Region::scoped(16.mib(), |cap pass_arena| {
        let cst = Syntax::parse_lossless_ll2[P](in pass_arena, in src_bytes)?;
        let typed_hir = HirChecker::infer_and_constrain(in pass_arena, in cst, in ceiling)?;
        let place_ir = PcgSolver::lower_and_verify_exhaustion(in pass_arena, in typed_hir)?;
        let (opt_ir, witness_steps) = EgglogOpt::saturate_with_witness(in pass_arena, in place_ir);
        WitnessChecker::verify_equational_chain(in place_ir, in opt_ir, in witness_steps)?;
        let code_len = VerifiedCodegen::emit_elf_deterministic(
            in pass_arena,
            in opt_ir,
            inout out_elf,
        )?;
        Ok(UnitArtifactDigests::compute_blake3[P](in place_ir, in out_elf.prefix(code_len)))
    })
}
~~~

## 10.4 Relate source steps to target steps

Forward simulation asks whether the target can match an observable source transition. An instruction-selection proof may assume particular operand ranges or a well-formed control-flow edge. A bootstrap fixed point is a statement about repeated compilation under given assumptions. It does not by itself establish instruction correctness.

<!-- technical-detail -->

Connecting source semantics across optimization and code generation relies on transitive forward simulation: if accepted rewrite witnesses preserve `PlaceIr` outcomes and backend emission preserves `PlaceIr` outcomes in target machine execution, then emitted machine code matches the unoptimized `PlaceIr` outcome. As emphasized in `content.typ:2963`, `end_to_end_verified_codegen_simulation` is an abstract composition schema—it proves only that the two hypotheses `h_witness_sound` and `h_backend_sim` compose by `rw`, whereas concrete row-level simulation lives separately in `lean/CodegenComposition.lean` and `lean/CodegenControlFlow.lean` (`lean/BootstrapDdc.lean` models finite bootstrap equality policies).

    -- illustrative/BootstrapSimulationAndDdc.lean: abstract composition schema only
    theorem end_to_end_verified_codegen_simulation
        {Ir Mach Outcome : Type}
        (evalIr   : Ir -> Outcome)
        (evalMach : Mach -> Outcome)
        (verifyWitness : Ir -> Ir -> Bool)
        (emitMach      : Ir -> Option Mach)
        (h_witness_sound : forall ir1 ir2, verifyWitness ir1 ir2 = true -> evalIr ir2 = evalIr ir1)
        (h_backend_sim   : forall opt m, emitMach opt = some m -> evalMach m = evalIr opt)
        (srcIr optIr : Ir) (machBin : Mach)
        (hw : verifyWitness srcIr optIr = true)
        (hc : emitMach optIr = some machBin) :
        evalMach machBin = evalIr srcIr := by
      rw [h_backend_sim optIr machBin hc, h_witness_sound srcIr optIr hw]

## 10.5 Replay an optimization witness

A rewrite witness identifies the rule, the matched expression, and evidence for its side conditions. A validator can reject a claimed rewrite that changes overflow behavior or loses an effect. Keep that validation result alongside the output comparison when evaluating a bootstrap claim.

<!-- technical-detail -->

`crates/vesper-codegen-verified` (`#![forbid(unsafe_code)]`) implements the standalone bootstrap lock parser, Stage 0/Stage 1 lock verifiers (`seed` and `stage1` modules), toy algebraic `verify_rewrite_certificate_chain` step checker, and `audit_ddc_bootstrap_quorum` gate. An important workspace boundary applies (`crates/vesper-codegen-verified/src/lib.rs:9–16`): because library crates cannot depend on the root `vesper` package where the real optimizer certificate checker lives (`src/optimize*` and `lean/RewriteWitness.lean`), supplying non-empty `RewriteEvidence` to `audit_ddc_bootstrap_quorum` returns `BootstrapVerifyError::RewriteCheckerNotWired`, and TEE quote cryptographic verification must be performed upstream by `vesper-aegis` before this crate checks report-data digest binding.

    // crates/vesper-codegen-verified/src/lib.rs: actual error variants and quorum gate
    pub enum BootstrapVerifyError {
        LockTooLarge { len: usize, max: usize },
        LockSyntax { message: String },
        InvalidLockField { field: &'static str, reason: &'static str },
        LockNotAtFixedPoint,
        SourceMerkleMismatch { locked: blake3::Hash, actual: blake3::Hash },
        LeanSpecMismatch { locked: blake3::Hash, actual: blake3::Hash },
        DisallowedLeanAxiom { axiom: String },
        TamperedDigest { field: &'static str, locked: blake3::Hash, actual: blake3::Hash },
        StageFixedPointDivergence { stage2: blake3::Hash, stage3: blake3::Hash },
        DdcCrossCompilerMismatch { stage3_path_a: blake3::Hash, stage3_path_b_ddc: blake3::Hash },
        InsufficientLilaBuilderQuorum { actual: usize, min_required: usize },
        BuilderSignatureRejected { codes: Vec<String> },
        MissingTeeAttestation,
        TeeReportDataMismatch,
        RewriteCheckerNotWired, // Real PlaceIr/AST rewrite checking lives in src/optimize* & lean/RewriteWitness.lean
    }
    // Enforces Stage 2 == Stage 3, Path A == Path B, Lean axiom allowlist, and Ed25519 builder quorum:
    pub fn audit_ddc_bootstrap_quorum(
        lock_toml: &[u8],
        evidence: &BootstrapEvidence<'_>,
    ) -> Result<AuditReport, BootstrapVerifyError>;

# 11 Packages, authority, and reproducible builds

A package dependency may need permission to allocate, call foreign code, or run a build step. Loom’s package contract records allowed effects alongside version constraints, build inputs, and the identity of the resulting artifacts.

<!-- technical-detail -->

Vesper Loom integrates version solving, static effect-capability ceilings, deterministic WebAssembly build sandboxes, Merkle interface early cutoff, and content-addressed storage into a single package contract. Rather than granting dependencies or build scripts ambient host authority, `Loom.toml` and `Loom.lock` bind versions alongside permitted lexical effects, foreign library manifests, and artifact digests.

In the repository, local multi-module and path-dependency compilation is implemented in `src/packages.rs` and `src/package_graph.rs`, backed by nine specialized `crates/vesper-loom-*` workspace crates and CLI subcommands in `crates/vesper-tools`. Per `REVIEW-STATUS.md` (`REPO-04` and `COMP-04`), offline catalog resolution, capability-ceiling auditing, Wasmtime `wasm32-wasip2` build execution, filesystem `BLAKE3` CAS storage, offline TUF target checks, and configured-key Ed25519 builder quorums are implemented today, whereas live Sigstore OIDC registry publishing, `vesper add`, remote registry fetching, and `redb` index tables remain unimplemented design proposals.

## 11.1 Solve for versions and policies

A resolver chooses versions satisfying all dependency constraints. A policy can add constraints on authority or provenance. If one package needs a newer interface and another pins an incompatible version, a useful explanation identifies the two requirements that conflict.

<!-- technical-detail -->

Loom lowers package manifests, strictly monotonic additive `[features]`, and declarative `.vsys` C-library bindings into a unified Package Calculus IR (`crates/vesper-loom-calculus`, mechanized for root inclusion, dependency closure, and version uniqueness). Everyday dependency solving uses a fast CDCL engine (`resolvo` / PubGrub incompatibility derivation trees in `crates/vesper-loom-solver`, completing typical graphs in under 50 ms) that explains version, feature, and capability-ceiling conflicts in human-readable derivation chains. For policy-guided upgrades (`vesper resolve --optimize` / `vesper_loom_solver::optimize`), Loom encodes candidate catalogs alongside OSV vulnerability counts, capability weights, duplicate major versions, and package staleness as Max-SMT soft objectives (`Bitwuzla` / `Z3` design target; bounded offline optimizer in `vesper-loom-solver`) while enforcing hard No-Authority-Amplification constraints across every edge.

## 11.2 Follow authority through the build

Capability ceilings limit what a package may request. Sandboxed generators get explicit inputs. An interface fingerprint can spare consumers a rebuild when a private implementation changes. A fast solver handles ordinary dependency constraints. A policy solver can compare several valid choices. Loom's design reserves `std/*` for the standard platform and gives third-party packages names such as `@acme/parser`. A publisher identity answers who controls `@acme`. A package URL would instead tell a client where to fetch the code. They answer different questions.

The proposed name check can quarantine a name close to one already in use. It considers how similar the names are, how long the namespace has existed, and what the trust policy requires. A close match is a reason to review a package before installing it.

Cached binaries have a separate check. Independent builders sign statements naming the digest of the bytes they produced, and the policy can require more than one builder to agree. The manuscript names this part of the design after Lila, a reproducible-builds project. The current offline verifier checks signatures and the configured quorum.

<!-- technical-detail -->

Six mechanisms govern authority and reproducibility across a Loom build. First, every `Loom.toml` declares a `capability_ceiling` (`effects = []` for pure codecs) and `forbid_unsafe_ffi`; the compiler checks that no module instantiates an OS handler or invokes FFI outside its ceiling, that every package ceiling covers its dependencies' grants, and that minor dependency upgrades escalating `effects` fail immediately with a Capability Escalation Error. Second, code generators (`build.ves`) run as `wasm32-wasip2` components inside Wasmtime (`crates/vesper-loom-sandbox` and `crates/vesper-loom-build`) with deterministic instruction-fuel limits, epoch-zero clocks, zero network sockets, read-only preopened input mounts, and an isolated `/out` directory, while native libraries bind exclusively through declarative `.vsys` files rather than arbitrary host shell scripts. Third, downstream units depend on a package's Merkle Interface Fingerprint (`mif:blake3` over `.vhir` exports), giving automatic early cutoff whenever private body edits leave the public interface unchanged.

Fourth, namespace governance reserves unscoped `std/*` identifiers for the standard platform and scopes third-party packages as `@org/pkg`. In the full registry design, `@org/pkg` ownership binds via keyless Sigstore OIDC to Rekor v2 transparency logs and TUF metadata (`crates/vesper-loom-tuf` implements bounded offline TUF target verification), while slopsquatting quarantine (`crates/vesper-loom-trust/src/slopsquat.rs`, strict under `VESPER_AGENT_MODE=1`) screens Levenshtein/confusable edit distance, namespace age, and trust cost before admitting unfamiliar names. Fifth, binary cache admission (`crates/vesper-loom-trust` and `crates/vesper-loom-admission`) verifies Ed25519 signatures from independent builder operators over identical `BLAKE3` derivation outputs—inspired by Lila reproducible-builds attestations, though Loom replaces Lila's centralized service with a multi-operator offline quorum. Sixth, verified blobs and source trees materialize into build workspaces via OS copy-on-write reflinks (`clonefile` on APFS, `FICLONE` on XFS/Btrfs) with verified copy fallback.

    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Loom.toml (capability_ceiling, forbid_unsafe_ffi, .vsys, path deps)        │
    │   │  CapabilityCeiling::verify_edge + slopsquat gate (VESPER_AGENT_MODE=1) │
    │   ▼                                                                        │
    │ Resolvo CDCL (<50 ms) / Max-SMT Optimizer ──▶ Loom.lock (4 BLAKE3 anchors) │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Wasmtime wasm32-wasip2 Sandbox (build.ves): fuel-metered, epoch 0, no net  │
    │ Read-only preopened inputs (/in/*)  ──▶  Isolated generated output (/out)  │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ Compiler emits .vhir (mif:blake3 Early Cutoff) + .o + .olean certificates  │
    │   ├──▶ vesper-loom-cas: Domain::{Source,Interface,Artifact}, rehash-read   │
    │   ├──▶ vesper-loom-trust / -admission: Ed25519 multi-builder quorum        │
    │   └──▶ Store::materialize: APFS clonefile / Btrfs-XFS FICLONE reflinks     │
    └────────────────────────────────────────────────────────────────────────────┘

## 11.3 Read a manifest and trace an import

`Loom.toml` describes the package, dependencies, and declared foreign bindings. A path dependency resolves relative to that manifest. A lockfile records resolved decisions so a later build can use the same inputs. Inspect both when a build works locally but selects different code elsewhere.

Source modules expose public items. `use` brings a name into scope, a qualified path identifies the item explicitly, and `pub use` exposes it through another module. Types and values have distinct namespaces. This is how a library can reorganize implementation modules while keeping a stable public entry point.

<!-- technical-detail -->

`Loom.toml` is a purely declarative manifest parsed in under a millisecond, paired with a `Loom.lock` file whose resolved nodes record `source_blake3`, `interface_mif` (`mif:blake3:...`), `effective_capabilities`, and `provenance`. In `src/packages.rs`, `[dependencies]` entries currently resolve local `path` directories relative to `Loom.toml` (such as `"@demo/geometry" = { version = "^0.1", path = "../geometry" }`): the target `Loom.toml` must match the declared `@scope/name` and satisfy the SemVer range, package cycles and loading the same package from two directories are rejected, `vesper check`, `run`, `jit`, `build`, and `emit` accept a package directory directly, and omitting `path` is an error because remote registry fetching and `vesper add` are not yet implemented.

    [package]
    # Declarative Loom.toml combining implemented local path deps with governance sections
    name = "@robot-head/krabka-wal"
    version = "0.4.0"
    edition = "2026"
    license = "Apache-2.0"
    capability_ceiling = ["Storage", "Net", "Clock", "Fail"]
    forbid_unsafe_ffi = true
    [features]
    io_uring = { enables_effects = ["Storage"], sys_deps = ["liburing ^2.6.0"] }
    [dependencies]
    "@demo/geometry" = { version = "^0.1.0", path = "../geometry", effects = [] }
    "@vesper-io/uring-reactor" = { version = "^0.9.2", path = "../uring-reactor", effects = ["Storage", "Fail"] }
    [build-sandbox]
    script = "build.ves"
    wasi_read_paths = ["proto/"]
    fuel_limit = 50_000_000
    allow_network = false
    [resolver.policy]
    optimize = ["zero_cve", "min_capability_surface", "min_dup_majors", "min_staleness"]
    min_repro_attestations = 2
    block_unvetted_namespaces = true
    [verification]
    export_lean4_specs = ["src/wal_txn.ves"]

Within a package (`src/packages.rs`), `src/lib.ves` or `src/main.ves` forms the root module and `src/a/b.ves` maps to module `a::b`. A `use` declaration starts at `@scope/name::` (a declared dependency) or `self::` (the current package), names a module path, and imports a single item or braced `{Item, other as alias}` list; whole-module imports (`use self::m;`) are forbidden. Only `pub` functions, types, effects, constants, and value enums (`E::M`) can be imported, while enum variants, effect operations, `trait` declarations, canonical `impl Trait for Type` blocks, and named `model` definitions occupy global program-wide namespaces (traits and models are never imported, and inherent `impl` blocks may span modules that import the type). Types and values inhabit separate namespaces—functions and constants live in the value namespace, effects in the type namespace, and structs/enums in both—so importing a function named `u64` binds `u64(x)` as a call without shadowing the scalar type `: u64`. Qualified paths (`self::`, repeatable `super::`, or `@scope/name::`) resolve the longest module prefix without requiring `use`, and `.vsys` foreign libraries (`Lib::f(..)`, `eff [Ffi[Lib]]`) are visible in every module of the declaring package with no import. In `src/lib.ves`, `pub use` re-exports submodule or dependency items as C-ABI library symbols (`vesper_<library>_<root_name>`), resolving re-export chains to the original declaration. Violations emit diagnostics `E0110` (`use` or qualified path in a standalone `.ves` file outside a package), `E0111` (unknown module/item or `super::` above the package root), `E0112` (referencing a non-`pub` item across modules), and `E0113` (re-exporting the same item twice under ambiguous root names).

~~~vesper
// src/lib.ves inside package @robot-head/krabka-wal
// Imports from a path dependency and package submodules (whole-module `use self::util;` is illegal):
use @demo/geometry::shapes::{Shape, area as shape_area};
use self::reader::u64;
use self::util::clamp;
// Root pub use exports vesper_krabka_wal_advance and vesper_krabka_wal_frontier_merge (duplicate root export -> E0113):
pub use self::frontier::{advance, merge as frontier_merge};
// Separate value/type namespaces: `u64(raw)` calls self::reader::u64 while `: u64` stays the scalar type;
// qualified `self::` / `super::` paths need no `use`, and `.vsys` library `Liburing` needs no import:
pub fn decode_clamped(in raw: u64) -> u64 eff [Ffi[Liburing]] {
    let parsed: u64 = u64(raw);
    let _ring_rc: i32 = Liburing::probe_version();
    let _pt = self::codec::Point::origin();
    clamp(parsed, 0, 4096)
}
~~~

## 11.4 Find the command for a package operation

Use the checkout’s `vesper --help` and the relevant subcommand help for available flags. Distinguish resolving source dependencies, building artifacts, auditing evidence, and publishing a release. For a failed release, identify whether the resolver rejected a dependency, the build failed, or the publication gate rejected the evidence.

<!-- technical-detail -->

`src/main.rs` exposes 33 subcommands in `vesper --help` (the 32 commands listed in `content.typ:3188` plus `publish` from `crates/vesper-tools/src/publish.rs`), split between 15 in-process compiler commands (`COMPILER_COMMANDS`) and 18 standalone `vesper-<tool>` binaries (`tool_commands::TOOLS`) so tool dependencies stay out of the core compiler closure. Lifecycle commands assumed by online ecosystems—such as `vesper add`, `vesper init`, or remote registry login—are unimplemented proposals; always check `vesper <subcommand> --help` to distinguish offline local catalog commands from proposed network workflows.

    Category              Implemented Subcommands (33 in src/main.rs; 32 in content.typ:3188 + publish)
    ----                  ----
    Core Compile & Exec   check, run, jit, build (--codegen verified, --lib, --tee), emit (cst|hir|ir|clif|object|lean|interface)
    Loom Graph & Supply   resolve (--catalog, --locked), graph, sbom, audit, cas, attest, semver, publish (--initial|--baseline)
    Proofs & Testing      verify (--extract, --format, --refute), proof-audit, test (--doc), fuzz, bench, replay
    Syntax, Macros & IDE  forge, grammar, fmt, highlight, shot, lint, doc, lsp
    Systems & Telemetry   ebpf (ebpf check), migrate, top, json, traceparent, simd
    ----
    $ vesper resolve Loom.toml --catalog catalog.json -o Loom.lock
    $ vesper resolve Loom.toml --catalog catalog.json --locked --lockfile Loom.lock
    $ vesper publish src/lib.ves --baseline old.vhir --old-version 0.3.0

## 11.5 Retrieve an artifact by content

The checkout’s `vesper-loom-cas` stores bytes on the filesystem under domain-separated BLAKE3 digests. Loom splits this work from package trust and resolution across the `vesper-loom-*` crates. A sandbox controls what a generator can read and produce, while a store index helps locate the resulting artifact. A matching digest establishes identity with the requested bytes. Check who published the artifact and whether your policy permits the capabilities it requests.

<!-- technical-detail -->

While `content.typ:3193` sketches five Loom crates backed by a `redb` database (`~/.vesper/store/index.redb` with tables `DERIVATIONS`, `MIF_CACHE`, `OLEAN_CERTS`, and `LILA_QUORUM`), the repository actually partitions Loom across nine Rust crates (plus three frozen `*/seed` crates for `calculus`, `solver`, and `audit`): `vesper-loom-calculus` (manifests, `Solution`, `CapabilityCeiling`), `vesper-loom-solver` (offline CDCL + `optimize`), `vesper-loom-sandbox` (fuel-metered Wasmtime `wasm32-wasip2` host), `vesper-loom-build` (two-pass input snapshotting and WASI build-step runner), `vesper-loom-cas` (immutable filesystem CAS), `vesper-loom-trust` (Ed25519 builder quorum and `slopsquat` checks), `vesper-loom-admission` (bounded 64 MiB blob quorum admission), `vesper-loom-audit` (offline license/policy/CVE graph audits), and `vesper-loom-tuf` (offline `tough`-based TUF target verification). Specifically, `crates/vesper-loom-cas` does not use `redb`: it implements a traversal-free filesystem store with explicit hash-domain separation (`Domain::Source`, `Domain::Interface`, `Domain::Artifact`), canonical Merkle `Tree` objects, atomic concurrent-safe `DerivationKey` publication, mandatory rehash-on-read with corruption quarantine, and `Store::materialize` workspace creation using OS reflinks (`CopyStrategy`). Content digests prove byte identity only; authority confinement (`CapabilityCeiling::verify_edge`) and provenance (`vesper-loom-trust`) are checked independently.

    // 1. crates/vesper-loom-calculus: No Authority Amplification across dependency edges
    impl CapabilityCeiling {
        pub fn verify_edge(&self, pkg: &SmolStr, dep: &SmolStr, dep_eff: &Self) -> Result<(), ConfinementViolation> {
            if dep_eff.allow_unsafe_ffi && !self.allow_unsafe_ffi {
                return Err(ConfinementViolation::ForbiddenUnsafeFfi { pkg: dep.clone() });
            }
            for eff in &dep_eff.allowed_effects {
                if !self.allowed_effects.contains(eff) {
                    return Err(ConfinementViolation::AuthorityAmplification {
                        pkg: pkg.clone(), dep: dep.clone(), escalated_effect: eff.clone(),
                    });
                }
            }
            Ok(())
        }
    }
    // 2. crates/vesper-loom-cas: domain-separated filesystem BLAKE3 store (rehash-on-read + reflinks, no redb)
    let store = vesper_loom_cas::Store::open("~/.vesper/store")?;
    let src_tree = store.ingest_dir(Domain::Source, Path::new("./pkg"))?;
    store.publish_derivation(&derivation_inputs, artifact_blob.id)?;
    store.materialize(&src_tree.id, Path::new("./workspace"), MaterializeOptions::default())?;
    // 3. crates/vesper-loom-sandbox: zero-network, fuel-metered wasm32-wasip2 build.ves execution
    pub fn run_hermetic_build_step(cfg: &HermeticBuildConfig<'_>) -> anyhow::Result<()>;

# 12 Will a library change break its callers?

An upgrade is source-compatible if an existing caller can still supply the same inputs, rely on the same results, and stay within its permissions and resource budgets.

<!-- technical-detail -->

Mathematical SemVer in Vesper reduces library upgrade compatibility to decidable subtyping checks across ownership modes, lexical effect rows, disjoint field-tree footprints, value refinements, and resource budgets. An interface evolution from `I_old` to `I_new` is Minor-compatible only when every well-typed caller compiled against `I_old` remains well-typed against `I_new` without source edits or broken resource invariants.

In the repository, interface export and comparison are implemented in `src/interface.rs` (`vesper emit --emit interface`), `crates/vesper-semver` (with a self-hosted Vesper port alongside `crates/vesper-semver/seed`), and `crates/vesper-tools/src/semver.rs` (`vesper semver`). Per `REVIEW-STATUS.md` (`TYPE-09` and `TYPE-10`), `crates/vesper-semver` (`SCHEMA_VERSION = 1`, `SEMANTICS_VERSION = 2` for checked integer arithmetic) conservatively compares resolved JSON interface DTOs for source-level compatibility only: it does not certify C/native ABI layout stability (struct sizes, alignments, field offsets, or calling conventions), caller-side layout-sensitive stack/arena bounds, dynamic/wire `model` brand transport, or automatic `.olean` proof transport.

## 12.1 Compare modes, effects, and field footprints

Narrowing a helper’s writes from the whole broker to its index can free other fields for the caller. Introducing a new effect can make an existing caller lack a handler. Changing a consuming parameter into a borrow also changes who must finish a linear resource. Compare passing modes as well as the types printed next to them.

<!-- technical-detail -->

Source signature subtyping (`I_new <=_pub I_old`) checks four substructural dimensions simultaneously. First, `ModeVec` variance strengthens covariantly on return values and closed ADTs (`m_new <= m_old`, so relaxing `linear -> affine -> copy`, `pinned -> portable`, or `lifo -> unordered` is Minor-compatible via `[T-SubMode]`, whereas tightening `affine -> linear` or `unordered -> lifo` breaks caller `discard(Gamma)` drops, `[T-Let]` frames, or release order) and contravariantly on generic type bounds (`m_bound_old <= m_bound_new`). Second, `[SV-EffRow]` and `[T-SubEff]` govern graded lexical effect rows: `caps(eps_new) <= caps(eps_old)`, `row_vars(eps_new) <= row_vars(eps_old)`, `res(eps_old) <= res(eps_new)` (removing abortable `Fail[E]` raises `res` from `affine` to `linear` and preserves every caller's live `linear` `[T-Let]` frame, whereas adding `Fail[E]` lowers `res` to `affine` and is Major-breaking), `exactCaps(eps_old) == exactCaps(eps_new)`, and `gradeCompatible(g_new(E), g_old(E))` for each retained capability. Third, `FieldTree` projections narrow covariantly (`pi_new <= pi_old`, such as `inout self.{index, metrics}` to `inout self.index`), shrinking `leaves(pi)` so every caller's `[T-CallProj]` disjointness proof stays valid. Fourth, parameter passing modes must remain identical (`mu_new == mu_old`): changing `sink` to `in` or `inout` on a potentially `linear` parameter leaves the caller's obligation unconsumed, and changing `in` to `inout`, `out`, or `sink` demands exclusive or consuming access callers never granted.

    ┌────────────────────────────────────────────────────────────────────────────┐
    │ SemVer Variance Axes (Minor-Compatible Direction: I_new <=_pub I_old)      │
    ├────────────────────────────────────────────────────────────────────────────┤
    │ Return / Closed ADT Mode (covariant):    linear ──▶ affine ──▶ copy        │
    │ Generic Bound Mode (contravariant):      copy   ──▶ affine ──▶ linear      │
    │ Borrowed FieldTree (narrowing):          self.{index, metrics} ──▶ .index  │
    │ Lexical Effect Row [SV-EffRow]:          caps shrink; res: affine ─▶ linear│
    │ Parameter Passing Mode:                  mu_new == mu_old (strictly equal) │
    │ Precondition / Postcondition:            phi_old => phi_new; psi_new => old│
    │ Resource Grades [SV-Grade]:              upper (<=) shrinks; exact (==) eq │
    └─────────────────────────────────────┬──────────────────────────────────────┘
                                          ▼
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ old.vhir + new.vhir ──▶ vesper semver ──▶ Patch (mif:blake3 equal)         │
    │                                       ├─▶ Minor (all subtyping rules hold) │
    │                                       └─▶ Major + JSON caller witness DTO  │
    └────────────────────────────────────────────────────────────────────────────┘

    [SV-EffRow]  caps(eps_new) <= caps(eps_old)    row_vars(eps_new) <= row_vars(eps_old)
                 res(eps_old) <= res(eps_new)      exactCaps(eps_old) == exactCaps(eps_new)
                 forall E in caps(eps_new), gradeCompatible(g_new(E), g_old(E))
                 --------------------------------------------------------------------------
                 eps_new <: eps_old

## 12.2 Check promises in the right direction

A compatible precondition can admit more inputs. A compatible postcondition must still imply what earlier callers relied on. Lowering an upper memory bound can satisfy the old budget. Changing an exact cost can invalidate the contract in either direction. Extending an open enum also requires checking whether the new payload changes the ownership of the whole value.

<!-- technical-detail -->

Declaration, refinement, and resource-grade rules orient each contract check according to whether caller or library bears the obligation. Function subtyping `[SV-Fn]` requires contravariant generic modal bounds (`m_bound_old <= m_bound_new`), covariant closure/function mode (`m_fn_new <= m_fn_old`), identical passing modes (`mu_i_new == mu_i_old`), narrowed field projections (`pi_i_new <= pi_i_old`), contravariant input types on `in`/`sink` (`tau_old <: tau_new`), invariant-split `inout` types (`tau_in_old <: tau_in_new` and `tau_out_new <: tau_out_old`), covariant return type and mode (`tau_ret_new <: tau_ret_old`, `m_ret_new <= m_ret_old`), and `eps_new <: eps_old`. For closed-field structs (`[SV-Adt-Closed]`), strengthening kind `k_new <= k_old` is source-compatible only (`TYPE-10`): private field edits can still change byte size, alignment, calling convention, or layout-dependent resource grades, and dynamic/wire `model` brands may never be erased under this rule. For `open enum` declarations (`[SV-Enum-Open]`), newly added variants must satisfy `k_payload <= k_enum_old` so existing wildcard `_ =>` arms can still discard unknown variants; for effects (`[SV-Effect-Decl]`), operation signatures must match exactly, `sealed` effects allow `q_old <= q_new`, and `open` effects require strict invariance `q_new == q_old` because third-party `handle` blocks that drop continuation `k` would fail `[T-Handle]` if `affine` became `linear`.

Refinements and resource grades follow the same variance discipline (`REVIEW-STATUS.md:19`). Under `[SV-Refine-Pre]`, weakening an input refinement or `where` clause (`phi_old => phi_new`, e.g., widening `I64Range` `[8, 64]` to `[4, 64]`) is Minor-compatible while strengthening it is Major-breaking; under `[SV-Refine-Post]`, tightening a return refinement, `ensures` clause, or `~>` post-state (`psi_new => psi_old`) is Minor-compatible. Under `[SV-Grade]`, upper-bound resource ceilings (`<=`) may decrease (`Stack @ <= 384` to `Stack @ <= 256` is Minor-compatible, raising it is Major-breaking), whereas exact resource grades (`==`, used for constant-time or deterministic-cost contracts) require strict equality: changing `== 100` to `== 99`, weakening `== 100` to `<= 100`, or removing an exact grade is rejected as Major-breaking.

    [SV-Fn]           m_bound_old <= m_bound_new, m_fn_new <= m_fn_old, mu_new == mu_old, pi_new <= pi_old,
                      in/sink: tau_old <: tau_new, inout: tau_in_old <: tau_in_new & tau_out_new <: tau_out_old,
                      tau_ret_new <: tau_ret_old, m_ret_new <= m_ret_old, eps_new <: eps_old
    [SV-Adt-Closed]   vis(T) == closed_fields & k_T_new <= k_T_old  (SOURCE ONLY; excludes ABI layout & model brands)
    [SV-Enum-Open]    variants(E_old) <= variants(E_new) & forall new V(tau_v): k(tau_v) <= k(E_old)
    [SV-Effect-Decl]  ops(E_new) == ops(E_old) & (sealed => q_old <= q_new) & (open => q_new == q_old)
    [SV-Grade]        upper bound (<=): g_new <= g_old  |  exact grade (==): g_new == g_old (removal/weakening Major)

## 12.3 Find a witness for a breaking change

A witness is a caller or input admitted by the old interface that the new one rejects or handles outside its promise. For example, changing a minimum buffer size from eight to sixteen breaks a caller passing eight bytes. Source substitutability, binary layout compatibility, and transport of proofs each require their own comparison.

<!-- technical-detail -->

When comparing baseline and candidate `.vhir` exports, `vesper semver` classifies every public change as `Patch` (identical `mif:blake3` interface digest, enabling build early cutoff), `Minor` (backward-compatible weakening of inputs or strengthening of outputs/bounds), or `Major` (any obligation that breaks a previously valid caller). While `content.typ:3372` describes synthesizing a standalone 5–10 line Vesper counterexample module from a Bitwuzla `SAT` model of `phi_old && !phi_new`, the actual implementation in `crates/vesper-semver` (`src/lib.rs:5–7`) evaluates resolved JSON interface DTOs (primitives, `I64Range`/`U64Range` intervals, modes, `Projection` path sets, effect rows, and upper-vs-exact grades) and emits structured diagnostic witness descriptions explaining which caller contract (`[T-Let]`, `[T-CallProj]`, `[T-Handle]`, range bound, or grade) fails, without generating or typechecking synthetic `.ves` source files.

    Interface Evolution (I_old -> I_new)                                  SemVer Class         Governing Rule and Soundness Invariant
    ----                                                                  ----                 ----
    1. Refactor body or private item with unchanged .vhir                 Patch (mif equal)    Early cutoff; downstream packages skip recompilation
    2. Relax generic modal bound (T: mod copy -> T: mod linear)           Minor                Contravariant m_bound_old <= m_bound_new in [SV-Fn]
    3. Strengthen return or closed-struct mode (linear -> affine -> copy) Minor                Covariant [T-SubMode] (source-only on closed structs; TYPE-10)
    4. Tighten ADT mode (affine -> linear or unordered -> lifo)           Major                Breaks caller discard(Gamma), [T-Let] frames, or release order
    5. Remove Fail[E] or other capability from eff [...]                  Minor                caps shrinks and res(eps) rises (affine <= linear)
    6. Add Fail[E] or Storage to eff [...]                                Major                Exceeds caller capability_ceiling and breaks live linear frames
    7. Narrow inout projection (self.{index, metrics} -> self.index)      Minor                pi_new <= pi_old preserves caller [T-CallProj] disjointness
    8. Widen inout projection (self.index -> self.{index, config})        Major                Collides with concurrent in self.config borrows at call sites
    9. Change sink x: T to in x: T on potentially linear T                Major                Leaves caller linear argument unconsumed at call boundary
    10. Weaken input refinement or lower <= Stack/Region upper bound      Minor                [SV-Refine-Pre], [SV-Refine-Post], and [SV-Grade] (<=)
    11. Strengthen input refinement, raise <= bound, or alter == grade    Major                Counterexample input / budget / exact-cost violation witness
    ----
    $ vesper emit old_pkg/ --emit interface -o old.vhir
    $ vesper emit new_pkg/ --emit interface -o new.vhir
    $ vesper semver old.vhir new.vhir --old-version 0.4.0 --new-version 0.5.0 --json
