# A How to read the Lean models

Before applying a Lean theorem to a Vesper program, read its statement and assumptions. Check which operations the model admits and how it defines the behavior you want to rely on.

<!-- technical-detail -->

Appendix A documents the mechanized Lean 4 models that formalize individual type, resource, place, effect, and code-generation rules of `lambda_Vesper`. Each development proves properties over its own stated syntax and operational or algebraic semantics rather than establishing a single unified soundness theorem for the full surface language and native runtime. Reading a theorem requires checking which terms, heap shapes, and invariants its module admits before transferring its conclusion to compiler behavior.

All mechanized artifacts reside under `lean/*.lean` and are tracked against the remediation boundaries in `docs/manuscript/REVIEW-STATUS.md` (lines `29-47`). Proof-admission and source-hygiene gates restrict kernel dependencies to Lean's three standard axioms (`propext`, `Classical.choice`, and `Quot.sound`) while rejecting `sorryAx`, compiler-trust axioms, Lean 4.35 generated `_native` axioms, and stale extraction caches. Nightly runs and pull requests replay all `158` Lean modules and extraction fixtures from scratch through the standalone checker rooted at `tools/logos42` (`tools/logos-sweep`) with zero rejections; `tools/logos42` declines the RV64 (`lean/CodegenRv64Encoding.lean`) and AArch64 (`lean/CodegenA64Encoding.lean`) `decode_encode` bit-encoding theorems on its fixed recursion-depth ceiling, leaving those two proofs and their callers verified by Lean's kernel alone. Neither zero-`sorryAx` kernel verification nor `tools/logos42` replay proves correctness of the production Rust typechecker, lowering passes, or hardware execution.

## A.1 Match a theorem to the claim you need

The correspondence matrix maps topics such as mode ordering, resumption safety, disjoint projections, and grade arithmetic to declarations. For a statement about a projected call, ask which paths count as disjoint and what appears in the conclusion after return. A proof that counts resources and a proof that preserves each resource’s identity establish different properties.

The code-generation models relate selected abstract instructions to source operations. A validation certificate supplies facts such as operand ranges at each control-flow edge. Kernel replay checks a proof artifact, and an axiom audit inspects its dependencies. Establishing that the deployed compiler produced the modeled rows and machine bytes needs additional evidence.

<!-- technical-detail -->

When matching a language guarantee to a Lean declaration, check both how the model represents resources or places and which bridge hypotheses connect neighboring calculi. Counting minted versus consumed list lengths is weaker than preserving each `ResId` count individually, filtering a static binding list is weaker than proving trace-preserving ghost erasure, and shifting relative frame indices in a stack model does not prove relocation safety for native stacks holding raw pointers or active DMA buffers.

    Lean 4 Repository Mechanization Map (Separate Calculi & Explicit Bridges — Not One Unified Language Proof)
    ==========================================================================================================
    [Core & Ghost Erasure]                          [Operational Calculi (Joined Field Floors: TYPE-01)]
    +----------------------------+                  +----------------------------+    +----------------------------+
    | MetatheoryAppendix.lean    |                  | ResourceModel42/Proofs.lean|--->| MetatheoryFull.lean        |
    | 1-let, canonical ValTyped  |                  | finite checker/eval bridge |    | 1-shot handlers, u64, PCG  |
    +-------------+--------------+                  +----------------------------+    +----------------------------+
                  | big-step erase                                                    +----------------------------+
                  v                                                                   | Metatheory/Closures.lean   |
    +----------------------------+     step-levelCheckedGhost instrumentation         | closures + deep state hdlr |
    | GhostErasure.lean          |---> +-----------------------------------+ -------> +----------------------------+
    | closed terminating ghosts  |     | OperationalGhostBridge.lean       |          | Metatheory/Polymorphism    |
    +----------------------------+     | trace eq, no infinite stuttering  |          | recursion, tvar/row poly   |
                                       +-----------------------------------+          +-------------+--------------+
    [Refinement, Lens & Mutable-Place Bridges (TYPE-03, TYPE-09)]                                   ^
    +--------------------+   lens    +-----------------------+   frames   +-------------------------+--------------+
    | StrongUpdate.lean  |<--------->| RefinementPlaces.lean |<---------->| TypedPlaceRefinements   |              |
    | Hoare, Stabilize,  |  patch    | together_lawful,      |  snapshots | stack_snapshot & swap   |              |
    | ForgetMutable      |           | heap_post_is_lens_post|            +------------+------------+              |
    +--------------------+           +-----------------------+                         |                           |
                                                                                       v                           |
    [Algebraic, Modal & Grade Models]                                     +-------------------------+   exchange   |
    +-------------------------------+  +-------------------------------+  | MetatheoryPlaces.lean   |<-------------+
    | VesperUnifiedTypeTheory.lean  |  | HandlerGrades / OrderedCtx /  |  | borrows, fibers, reloc  |  PlacePoly-
    | 4-axis ModeVec, Capsule, Quot |  | ResourceGrades / ClockQuotient|  | (relative slot indices) |  morphismBridge
    +-------------------------------+  +-------------------------------+  +-------------------------+
    ----
    [Slot-IR Selection & Range Certificate Stack]                         [Verification & Audit Gates]
    VerifiedCodegen -> CodegenComposition -> CodegenControlFlow           * Lean 4 Kernel: {propext, Classical.choice,
                              ^                      ^                      Quot.sound}; rejects sorryAx & _native
                              |                      |                    * tools/logos42: replays 158 Lean modules
                      CodegenValidation ---> CodegenFlowValidation          (declines RV64 & A64 decode_encode depth)
                                                     :                    * Open: unified language/runtime soundness,
                         differential #eval tests    :                      AST elaboration, Rust lowering, bytes
                      src/verified_codegen/certificate.rs

The correspondence matrix below maps all `18` core metatheory, place/refinement bridge, modal/grade, and slot-IR codegen modules under `lean/` to their primary verified declarations and scope boundaries.

| Module Path | Primary Mechanized Declarations & Exact Scope Boundary |
| --- | --- |
| lean/MetatheoryAppendix.lean | Theorems: mode_submode_allows_count, adt_kind_preserves_field_floor, |
                                      adt_kind_never_relaxes_linear_component, abort_soundness_preserves_linear,
                                      call_proj_axm_exclusivity, pcg_join_preserves_linear_agreement,
                                      vesper_resource_identity_conservation, typed_evaluation_exists, closed_exact_once.
                                      Scope: 1-binding (gamma2 = none), recursion-free core; no closures/heap/borrows.
    ----                              ----
    lean/VesperUnifiedTypeTheory.lean Theorems: modevec_le_trans, capsule_seal_produces_portable_uncontended,
                                      exclusive_place_never_duplicated, shared_linear_view_can_split,
                                      lifo_rejects_out_of_order_irq_restore, ghost_linear_token_zero_runtime_footprint,
                                      out_split_slice_disjoint_and_exhaustive, semver_refinement_variance_sound,
                                      graded_region_loop_elides_limit_checks, uncompacted_zset_eq_compacted,
                                      sugar_shared_hole_forbids_hidden_mutation.
                                      Scope: Algebraic rules; CapsuleEvidence assumed; eraseGhostBindings is list filter.
    ----                              ----
    lean/MetatheoryFull.lean          Theorems: preservation, progress, type_safety, total_linear_exhaustion,
                                      scalar_linear_floor_kind.
                                      Scope: Small-step 1-shot handlers, checked u64, field-tree paths, joined floors.
    ----                              ----
    lean/Metatheory/Closures.lean     Theorems: type_safety, total_linear_exhaustion_per_kind, row_poly_safety,
                                      linear_state_across_abort_rejected, linear_capture_copy_closure_rejected.
                                      Scope: Closures, stateful deep handlers, row substitution; no mutable frames.
    ----                              ----
    lean/Metatheory/Polymorphism.lean Theorems: type_safety, eval_sound, eval_exhaustion, tvar_safety,
                                      escaped_closure_never_performs, escape_rejected.
                                      Scope: Recursive closures, bounded type/row polymorphism; safety != termination.
    ----                              ----
    lean/MetatheoryPlaces.lean        Theorems: type_safety, total_linear_exhaustion, mut_borrows_exclusive,
                                      shr_never_aliases_mut, zero_pin_relocation.
                                      Scope: Second-class borrows & fibers; relative frame indices (no raw ptrs/DMA).
    ----                              ----
    lean/ResourceModel42/Proofs.lean  Theorems: checked_body_hasType, evalSteps_preservation, evalSteps_noInvariant,
                                      evalSteps_Full_progress, evalSteps_completed_copy_exact_once.
                                      Scope: Finite checker/evaluator into MetatheoryFull; not production Rust checker.
    ----                              ----
    lean/GhostErasure.lean            Theorems: erasure_preserves_typing, erasure_equivalence, ghost_noninterference,
                                      ghost_proof_terminates, annotated_closed_exact_once, ghost_resource_leak_rejected.
                                      Scope: Big-step erasure on MetatheoryAppendix; closed terminating ghost proofs.
    ----                              ----
    lean/OperationalGhostBridge.lean  Theorems: erasure_trace_equivalence, annotated_type_safety,
                                      no_infinite_ghost_stuttering, annotated_exhaustion_per_kind.
                                      Scope: Step-level ghosts over Polymorphism.lean; no captured ghosts/AST elab.
    ----                              ----
    lean/StrongUpdate.lean            Theorems: hoare_sound, post_iff_reachable, total_correctness,
                                      stale_alias_fact_rejected, stabilize_greatest, forget_mutable_least.
                                      Scope: Scalar stores & aliased int heap fields (Stabilize/ForgetMutable); no alloc.
    ----                              ----
    lean/RefinementPlaces.lean        Theorems: patch_post, post_iff_patch, together_lawful,
                                      aliased_pair_is_not_lawful, heap_post_is_lens_post, heap_patch_preserves_refinements.
                                      Scope: Equates StrongUpdate postconditions with lawful lens patch & disjoint pairs.
    ----                              ----
    lean/TypedPlaceRefinements.lean   Theorems: stack_snapshot_and_borrow_soundness, swap_refinement_and_ownership,
                                      frame_swap_matches_lens, own_swap_operational_step, linear_assignment_rejected.
                                      Scope: Snapshots & linear swaps on MetatheoryPlaces; requires initialized roots.
    ----                              ----
    lean/PlacePolymorphismBridge.lean Theorems: encoded_configuration_typed, exchange_typing,
                                      exchange_preserves_polymorphic_typing, frame_write_correspondence.
                                      Scope: Initialized place exchange in Polymorphism.lean; no heap closures/borrows.
    ----                              ----
    lean/HandlerGrades.lean           Theorems: evaluator_matches_cps, handler_budget_sound, extracted_handler_budget,
                                      outer_abort_skips_post_resumption_work.
                                      Scope: Additive 1-shot handler budgets & CPS match (TYPE-06/07); no linear state.
    ----                              ----
    lean/OrderedContexts.lean         Theorems: check_iff_typed, typed_prefix, ordered_conservation,
                                      closed_trace_exhausts, out_of_order_release_rejected.
                                      Scope: LIFO guard-trace checker equivalence (TYPE-09); source emission open.
    ----                              ----
    lean/ResourceGrades.lean          Theorems: stack_bound_sound, exact_time_sound, exact_time_noninterference,
                                      sequential_frames_reuse_space, nested_frames_add_space, equality_cannot_weaken_to_bound.
                                      Scope: Finite call-tree peak stack reuse vs nested addition & SemVer exact costs.
    ----                              ----
    lean/ClockQuotientSafety.lean     Theorems: force_clock_closed, guarded_fix, observation_independent_of_representative,
                                      step_independent_of_representative, state_only_congruence_insufficient.
                                      Scope: Clock-closed Later forcing & quotient congruence (TYPE-08); elab open.
    ----                              ----
    lean/CodegenComposition.lean      Theorems: compiled_program_simulates, prefixes_simulate, divergence_iff,
    + ControlFlow / Validation /      validation_sound, validated_closed_program, validation_invariant,
      FlowValidation.lean             validated_fault_exclusion, widened_validation_sound.
                                      Scope: Slot-IR selection & flow range certificates; no calls/addresses/bytes.

In the compiler pipeline, `src/verified_codegen/certificate.rs` bridges toward `lean/CodegenFlowValidation.lean` by running an interprocedural range analysis and checking emitted certificates with a Rust mirror of `validateProgram`. That Rust validator is differentially tested against Lean `#eval` across accepted and mutated certificates, and `Options::require_certificate` can reject uncertified builds, but the flag remains off by default until certificate facts track variant-sensitive enum payloads, `Fail` status words, and non-returning call continuations; its `Enter`, `Call`, `Return`, and trap-scope handler summary rules are conservatively tested in Rust rather than proved in Lean (`REVIEW-STATUS.md:47`).

## A.2 Read the small resource calculus

A small program can mint a fresh handle, pass it through a binding, and consume it. The core calculus can track the identity through those steps and show exhaustion for its stated closed programs. A place capability graph separately records overlapping access paths. A branch join must agree on which linear bindings remain before execution continues.

<!-- technical-detail -->

`lean/MetatheoryAppendix.lean` formalizes the core usage-mode lattice (`Mode.copy`, `Mode.affine`, `Mode.linear`), aggregate field-floor kinding, resumption frame safety, `[T-CallProj]` leaf disjointness, place-capability-graph (`PCG`) branch joins, and deterministic big-step resource conservation. To remediate `TYPE-01`, `AdtFieldSpec` pairs each field's intrinsic `compKind` with an effective `overrideMode` floor (`overrideMode := compKind` when omitted) constrained by `FieldModalityWf`. Computing `adtKind` via `Mode.join` over `overrideMode` proves `adt_kind_preserves_field_floor` and `adt_kind_never_relaxes_linear_component`: wrapping a `copy` scalar `i32` with an explicit `linear` floor yields a `linear` aggregate (`owned_fd_is_linear`), whereas annotating a `linear` component `copy` fails well-formedness.

On the operational side, `ValTyped` enforces canonical linear forms (`∃ r, v = .linHandle r`), ruling out scalar values masquerading as linear handles (`natural_is_not_a_linear_handle`) so `consumeLin` cannot get stuck in a typed environment. Evaluation existence (`typed_evaluation_exists`) is proved unconditionally rather than assumed, and `vesper_resource_identity_conservation` tracks each `ResId` via `List.count` rather than comparing aggregate list lengths alone. Combining fresh monotone allocation (`evaluation_mints_fresh`) with conservation yields `closed_exact_once` for closed `copy`-result programs. Keep the fragment's structural restriction in view: `HasType.tLet` requires `h_z_none : gamma2 = none`, restricting sequential bindings to a single active variable with no recursion, closures, heap locations, or effect handlers.

    -- lean/MetatheoryAppendix.lean (core lattice, TYPE-01 field floors, PCG join, and exact-once proof)
    namespace Vesper.Metatheory
    inductive Mode : Type where | copy | affine | linear deriving DecidableEq, Repr
    --
    structure AdtFieldSpec where
      compKind     : Mode
      overrideMode : Mode := compKind
      deriving DecidableEq, Repr
    @[simp] def FieldModalityWf (f : AdtFieldSpec) : Prop := Mode.Le f.compKind f.overrideMode
    @[simp] def adtKind (mDecl : Mode) (fields : List AdtFieldSpec) : Mode :=
      fields.foldl (fun acc f => Mode.join acc f.overrideMode) mDecl
    --
    theorem adt_kind_preserves_field_floor (mDecl : Mode) (fields : List AdtFieldSpec)
        (f : AdtFieldSpec) (h_mem : f ∈ fields) : Mode.Le f.overrideMode (adtKind mDecl fields)
    theorem adt_kind_never_relaxes_linear_component (mDecl : Mode) (fields : List AdtFieldSpec)
        (h_wf : ∀ f, f ∈ fields → FieldModalityWf f)
        (fLin : AdtFieldSpec) (h_mem : fLin ∈ fields) (h_lin : fLin.compKind = .linear) :
        adtKind mDecl fields = .linear
    theorem owned_fd_is_linear :
        adtKind .copy [{ compKind := .copy, overrideMode := .linear }] = .linear := rfl
    --
    theorem abort_soundness_preserves_linear {liveModes : List Mode} {eff1 : EffectRow}
        (h_frame : FrameSafe liveModes eff1) (h_abortable : .affine ∈ eff1) :
        forall m, m ∈ liveModes -> m.isDiscardable = true
    theorem call_proj_axm_exclusivity {args : List CallArg} (h_valid : CallProjValid args)
        (i j : Fin args.length) (h_neq : i != j) (h_mut_or_sink : (args.get i).mode != .sharedIn)
        {loc : LocalId} {leaf : LeafPath} (h_cov_i : (args.get i).proj.covers loc leaf) :
        ¬ (args.get j).proj.covers loc leaf
    theorem pcg_join_preserves_linear_agreement {lhs rhs merged : CapState}
        (h_no_poison_l : lhs != .poisonedRecovery) (h_no_poison_r : rhs != .poisonedRecovery)
        (h_join : pcgJoinLocal lhs rhs = some merged) :
        (lhs = .liveOwned .linear <-> rhs = .liveOwned .linear)
    --
    def ValTyped (v : Val) : Mode -> Prop
      | .copy | .affine => v.linResources = []
      | .linear         => ∃ r, v = .linHandle r
    theorem natural_is_not_a_linear_handle (n : Nat) : ¬ ValTyped (.nat n) .linear := by simp [ValTyped]
    --
    -- In HasType.tLet, h_z_none : gamma2 = none enforces the 1-binding frame restriction.
    theorem vesper_resource_identity_conservation (resource : ResId)
        {env : Option Val} {gamma : Option Mode} {e : Expr} {m : Mode} {eff : EffectRow}
        {n n' : Nat} {minted consumed : List ResId} {out : Outcome}
        (h_type : HasType gamma e m eff) (h_env : EnvTyped env gamma)
        (h_eval : EvalDet env e n minted consumed out n') :
        (forall v, out = .val v -> ValTyped v m) /\
        (EnvResources env).count resource + minted.count resource =
          consumed.count resource + (OutcomeResources out).count resource
    --
    theorem typed_evaluation_exists {gamma : Option Mode} {e : Expr} {m : Mode} {eff : EffectRow}
        (typed : HasType gamma e m eff) : ∀ (env : Option Val) (n : Nat), EnvTyped env gamma →
          ∃ minted consumed out next, EvalDet env e n minted consumed out next
    --
    theorem closed_exact_once {e : Expr} {eff : EffectRow} (typed : HasType none e .copy eff) (n : Nat) :
        ∃ minted consumed out next,
          EvalDet none e n minted consumed out next ∧
          (∀ v, out = .val v → ValTyped v .copy) ∧ minted.Nodup ∧
          (∀ resource, minted.count resource = consumed.count resource) ∧
          (∀ resource, resource ∈ minted → consumed.count resource = 1)
    end Vesper.Metatheory

## A.3 Connect modes, destinations, and refinements

The four-axis model combines field requirements and checks restrictions on ordered guards and capsules. Destination coverage accounts for which pieces an output requires. A refinement snapshot relates an old field value to its updated value. When exchanging linear payloads, the displaced owner must still appear exactly once. Two names for the same place cannot justify a disjoint swap.

Additional models address clock-indexed observations, quotient congruence, handler state, resource bounds, closures, polymorphism, and mutable places. A bridge relates specific operations in two models. For example, the bridge for exchanging typed fields assumes both fields already contain values. Extending it to uninitialized storage would need a separate proof.

<!-- technical-detail -->

`lean/VesperUnifiedTypeTheory.lean` collects self-contained algebraic proofs for the four-axis `ModeVec` lattice (`modevec_le_trans`), capsule sealing (`TYPE-04`), permission-aware place splitting (`TYPE-02`), `OrderAxis.lifo` guard discipline, `out` slice partitioning, refinement variance, arena bump-check elision, Z-set quotients, and `sugar` pass-mode visibility. Capsule admission (`capsuleSealSafe`) requires `CapsuleEvidence` with unique ownership, zero external aliases, zero outer capabilities, zero thread-affine handles, and an uncontended unordered mode (`capsule_seal_produces_portable_uncontended`), paired with five negative theorems rejecting each violation (`capsule_rejects_thread_affinity`, `capsule_rejects_outer_capability`, `capsule_rejects_external_alias`, `capsule_rejects_shared_owner`, and `capsule_rejects_ordered_owner`). Similarly, `PlaceSplit` separates access permission (`Access.own`, `.shr`, `.mut`, `.out`) from value linearity: `exclusive_place_never_duplicated` forbids duplicating `.mut` or `.out` bindings and `ordered_owner_never_duplicated` blocks `.lifo` owner contraction, while `shared_linear_view_can_split` permits sharing read-only `.shr` views of a linear resource.

    -- lean/VesperUnifiedTypeTheory.lean (4-axis modes, TYPE-04 capsule gate, TYPE-02 place split, quotients)
    namespace Vesper.UnifiedTypeTheory
    structure ModeVec where
      usage : UsageMode
      port  : Portability
      cont  : Contention
      ord   : OrderAxis
      deriving DecidableEq, Repr
    theorem modevec_le_trans {a b c : ModeVec} (h1 : ModeVec.Le a b) (h2 : ModeVec.Le b c) : ModeVec.Le a c
    --
    structure CapsuleEvidence where
      uniquelyOwned : Bool
      externalAliases : Nat
      freeOuterCaps : Nat
      threadAffineHandles : Nat
      deriving DecidableEq, Repr
    theorem capsule_seal_produces_portable_uncontended
        {e : CapsuleEvidence} {m sealed : ModeVec} (h : capsuleSealSafe e m = some sealed) :
        e.uniquelyOwned = true ∧ e.externalAliases = 0 ∧ e.freeOuterCaps = 0 ∧
          e.threadAffineHandles = 0 ∧ sealed.port = .portable ∧
          sealed.cont = .uncontended ∧ sealed.ord = .unordered ∧ sealed.usage = m.usage
    -- 5 rejection theorems: capsule_rejects_{thread_affinity,outer_capability,external_alias,shared_owner,ordered_owner}
    --
    def Contractible (b : PlaceBinding) : Prop :=
      b.access = .shr ∨ (b.access = .own ∧ b.usage = .copy ∧ b.order = .unordered)
    theorem exclusive_place_never_duplicated {b left right : PlaceBinding}
        (exclusive : b.access = .mut ∨ b.access = .out) : ¬ PlaceSplit (some b) (some left) (some right)
    theorem shared_linear_view_can_split :
        PlaceSplit (some ⟨.shr, .linear, .lifo⟩) (some ⟨.shr, .linear, .lifo⟩) (some ⟨.shr, .linear, .lifo⟩)
    --
    theorem lifo_rejects_out_of_order_irq_restore (outer inner : GuardId) (rest : List GuardId)
        (h_neq : inner ≠ outer) : popLifo (inner :: outer :: rest) outer = none
    theorem out_split_slice_disjoint_and_exhaustive (mid len idx : Nat) (_h_mid : mid ≤ len) :
        (¬ (idx < mid ∧ (mid ≤ idx ∧ idx < len))) ∧ (idx < len → (idx < mid ∨ (mid ≤ idx ∧ idx < len)))
    theorem semver_refinement_variance_sound {Val : Type} (P_old P_new Q_old Q_new : Val → Prop)
        (h_pre : ∀ x, P_old x → P_new x) (h_post : ∀ y, Q_new y → Q_old y)
        (h_new : ∀ x, P_new x → ∃ y, Q_new y) : ∀ x, P_old x → ∃ y, Q_old y
    theorem graded_region_loop_elides_limit_checks (cursor0 initB iters stepB i limit : Nat)
        (h_iter : i ≤ iters) (h_budget : cursor0 + initB + iters * stepB ≤ limit) :
        cursor0 + initB + i * stepB ≤ limit
    theorem uncompacted_zset_eq_compacted :
        (Quot.mk ZWeightEquiv [10, -10, 42] : ZSetQuot) = Quot.mk ZWeightEquiv [42]
    theorem sugar_shared_hole_forbids_hidden_mutation (u : SugarPass)
        (h_wf : passPermitsUse .sharedIn u = true) : isMutatingOrConsuming u = false
    end Vesper.UnifiedTypeTheory

Four dedicated bridge modules connect logical refinements and mutable places beyond those standalone combinators (`TYPE-03`, `TYPE-09`). `lean/StrongUpdate.lean` proves `hoare_sound`, `post_iff_reachable`, and `total_correctness` over scalar stores and aliased integer heap fields, replacing syntactic conjunct stripping with universal contention interference via maximal retention (`Heap.Stabilize`, `stabilize_greatest`) and minimal sound weakening (`Heap.ForgetMutable`, `forget_mutable_least`). `lean/RefinementPlaces.lean` proves that heap strong updates coincide with lawful lens patching (`heap_post_is_lens_post`, `heap_patch_preserves_refinements`) and that simultaneous paired updates require independent getters and commuting setters (`together_lawful`, `aliased_pair_is_not_lawful`). `lean/TypedPlaceRefinements.lean` lifts those lens contracts onto the concrete frames and borrow slots of `lean/MetatheoryPlaces.lean` (`stack_snapshot_and_borrow_soundness`, `swap_refinement_and_ownership`, `own_swap_operational_step`), proving that overwriting a live linear handle loses identity unless exchanged (`linear_assignment_rejected`) and that uninitialized leaves violate lens laws (`uninitialized_path_is_not_a_lawful_lens`). Finally, `lean/PlacePolymorphismBridge.lean` embeds initialized place values into `lean/Metatheory/Polymorphism.lean`, proving `exchange_typing` and `frame_write_correspondence` while leaving uninitialized storage, heap-allocated closures, captured borrows, dynamic allocation, and source elaboration outside the bridge.

    -- Strong-update, lens, mutable-place, and polymorphic exchange bridge theorems
    StrongUpdate.Heap.hoare_sound          : Hoare P cmd Q → P names h0 → Exec names cmd h0 h1 → Q names h1
    StrongUpdate.Heap.stabilize_greatest   : Stable mut Q → (Q ==> P) → (Q ==> Stabilize mut P)
    StrongUpdate.Heap.forget_mutable_least : Stable mut Q → (P ==> Q) → (ForgetMutable mut P ==> Q)
    RefinementPlaces.heap_post_is_lens_post: Heap.Post P r f rhs names h' ↔ Post (heapField loc) evalRhs (P names) h'
    RefinementPlaces.together_lawful       : Lawful L → Lawful R → DisjointLenses L R → Lawful (together L R)
    TypedPlaceRefinements.stack_snapshot_and_borrow_soundness : snapshot post ∧ CfgOK borrows ∧ ResId balance
    TypedPlaceRefinements.swap_refinement_and_ownership       : ¬ Ovl p q → paired snapshot ∧ cnt id & allLt preserved
    TypedPlaceRefinements.own_swap_operational_step           : ¬ Ovl p q → Step prog before after == swapValues lens
    PlacePolymorphismBridge.exchange_typing                   : Unique v → Places.VT v root → Polymorphism.HasType ...
    PlacePolymorphismBridge.frame_write_correspondence        : encodeValue (frame.set idx p v') == exchange result

## A.4 Compare execution before and after ghost erasure

Start with a computation that acquires and consumes a proof-only token alongside runtime work. Remove the ghost steps and compare the observable runtime result and resource trace. The operational erasure results require the ghost work to terminate and discharge its obligations. The bridge admits closed instrumentation in the runtime calculus. Captured ghost resources require a further argument.

<!-- technical-detail -->

In `lean/VesperUnifiedTypeTheory.lean`, `eraseGhostBindings` (`ghost_linear_token_zero_runtime_footprint`) merely filters `.ghost0` entries out of a static `List GradedBinding`; that list identity alone says nothing about evaluation, divergence, or linear handle lifetimes. Operational erasure is instead established in two layered modules. First, `lean/GhostErasure.lean` extends `lean/MetatheoryAppendix.lean` with `Annotated` expressions covering pure ghost bindings and closed ghost resource proofs (`GhostProofWellTyped`). Its `erasure_equivalence` theorem proves bidirectional big-step correspondence between `Annotated` evaluation and `erase source` in the core calculus, preserving the runtime `Outcome`, exact `minted` and `consumed` `ResId` traces, and `next` counter, while `ghost_noninterference` proves independence from the ghost environment. Closed ghost proofs must terminate and discharge every ghost-minted token (`ghost_proof_terminates`), rejecting leaks (`ghost_resource_leak_rejected`), aborts (`ghost_failure_rejected`), and runtime reads of ghost state (`runtime_observation_rejected`), and composing into `annotated_closed_exact_once`.

Second, `lean/OperationalGhostBridge.lean` attaches finite lists of `CheckedGhost` obligations to small-step configurations of `lean/Metatheory/Polymorphism.lean` (covering closures, recursion, row polymorphism, and stateful handlers) using a disjoint ghost resource counter. It proves bidirectional runtime trace equivalence after draining pending annotations (`erasure_trace_equivalence`), transfers safety (`annotated_type_safety`), rules out infinite ghost-only reduction loops (`no_infinite_ghost_stuttering`), and establishes per-kind linear handle and continuation exhaustion (`annotated_exhaustion_per_kind`) across recursive and stateful-handler workloads. As documented under `TYPE-05` in `docs/manuscript/REVIEW-STATUS.md`, this bridge models closed step-level instrumentation rather than compiler AST elaboration: captured ghost tokens inside runtime closures, observation borrow lifetimes, dependent heap predicates, and clocked ghost recursion remain open.

    Operational Ghost Erasure Commuting Diagram (GhostErasure.lean + OperationalGhostBridge.lean)
    ---------------------------------------------------------------------------------------------
    Annotated State:  ⟨conf_0, pending_ghosts, g_0⟩  ===[ DrainSteps (finite, no stuttering) ]===>  ⟨conf_0, [], g_1⟩
                             |                                                                             |
       erase (drops @0)      |  ghost store g_i isolated from runtime ResId supply                         | AnnotatedSteps
                             v                                                                             v
    Erased Runtime:       conf_0                     ===[ Polymorphism.Steps (identical acts) ]===>     conf_k
    --
    theorem GhostErasure.ghost_proof_terminates (well : GhostProofWellTyped proof) :
        ∀ start, ∃ minted consumed next,
          EvalDet none proof start minted consumed (.val .unit) next ∧ (∀ id, minted.count id = consumed.count id)
    theorem GhostErasure.erasure_equivalence (typed : RuntimeTyped gamma source m eff) :
        (Eval ghost env source n m c out next → EvalDet env (erase source) n m c out next) ∧
        (EnvTyped env gamma → EvalDet env (erase source) n m c out next → ∀ ghost, Eval ghost env source n m c out next)
    theorem GhostErasure.ghost_noninterference (typed : RuntimeTyped gamma source m eff)
        (ev1 : Eval g1 env source n m1 c1 out1 n1) (ev2 : Eval g2 env source n m2 c2 out2 n2) :
        m1 = m2 ∧ c1 = c2 ∧ out1 = out2 ∧ n1 = n2
    theorem GhostErasure.annotated_closed_exact_once (typed : RuntimeTyped none source .copy eff)
    theorem GhostErasure.ghost_resource_leak_rejected : ¬ GhostProofWellTyped (.letSeq .mintLinear (.litNat 0))
    theorem OperationalGhostBridge.erasure_trace_equivalence (before after : Conf) (pending : List CheckedGhost) :
        (∃ actions, AnnotatedSteps Sig instrument ⟨before, pending, g⟩ actions ⟨after, [], g'⟩) ↔
        ∃ actions, Polymorphism.Steps Sig before actions after
    theorem OperationalGhostBridge.no_infinite_ghost_stuttering : ∀ s, ∃ d, DrainSteps s d ∧ Ready d
    theorem OperationalGhostBridge.annotated_exhaustion_per_kind -- exact-once handles & 1-shot resumptions preserved

# B The Vesper mark and its visual meaning

The Vesper mark shows an intact record above three separate permissions. The open channels between the blades preserve that meaning in monochrome.

<!-- technical-detail -->

Appendix B specifies Vesper's official visual identity, The Disjoint Field-Tree Prism, as a deterministic four-polygon construction on a `256 x 256` coordinate grid bounded by a colinear `2:1` V-cone. An intact upper chevron sits above three separated lower blades representing `Mode::Linear` ownership transfer, exclusive `inout` field projections, and shared `in` views or `eff [...]` capabilities. Encoding field disjointness as open negative-space channels keeps all three permissions legible in 1-bit monochrome, etched hardware stencils, and small UI chrome without decorative curves or raster blobs.

Normative synthesis lives in `crates/vesper-highlight/src/brand.rs` and `crates/vesper-highlight/src/palette.rs`, checked-in web vectors reside at `website/brand/*.svg` (`vesper-mark-dusk.svg`, `vesper-mark-dawn.svg`, `vesper-favicon-16.svg`, and `vesper-wordmark-dusk.svg`), and coordinate proofs live in `lean/BrandGeometrySafety.lean`. Per `docs/manuscript/REVIEW-STATUS.md` and `website/VALIDATION.md` (lines `40-44`), `lean/BrandGeometrySafety.lean` verifies polygon coordinate equations independently of language soundness claims, and contrast claims are scoped to concrete surface pairings: Dusk code tokens clear `8.84:1` on card surfaces, the Dawn prism mark hexes clear WCAG AA (`>= 4.67:1` on `#FAF8F5`), and `ChromaPalette::vesper_dawn()` darkens foreground code-token swatches (`#8B3C00`, `#015C6F`, `#6627D0`, `#016137`) to satisfy its `>= 7.0:1` integer gate.

## B.1 Read the field-tree prism

The upper chevron represents an aggregate such as a broker. The gap beneath it represents splitting a set of field projections for a call. The left blade denotes consumption or linear ownership, the center blade exclusive access, and the right blade shared access or an effect capability. Their common outline recalls the caller’s record after the loans end.

<!-- technical-detail -->

Every polygon and separating channel in The Disjoint Field-Tree Prism maps directly to a structural rule of `lambda_Vesper`. When `website/scripts/docs.mjs` builds this documentation chapter (`docs.mjs:122`), it automatically appends the theme-reactive Dusk/Dawn prism `<figure>` immediately below this section so readers can inspect the live SVG channels alongside the correspondence table.

| Geometric Element | Visual Form on 256x256 Grid | lambda_Vesper Construct | Soundness Invariant Represented |
| --- | --- | --- | --- |
| Outer 2:1 V-Cone | Colinear rays (30,34) & (226,34) -> (128,230) | Monogram V & [T-CallProj] restoration | Projected sub-places re-converge into caller root (0 'a). |
| Upper Root Chevron | Solid chevron (w_root = 38 px, 45 deg valley) | Aggregate root place (broker: Broker) | Intact parent struct entering call or Pulse::scope boundary. |
| Prism Refraction Split | Diagonal gap (g_split = 14 px, slope +-1/2) | Field-tree selector .{index, metrics} | Splits parent aggregate into leaf projections without boxing. |
| Port Blade | Left blade (#FFB454 Dusk / #B85200 Dawn) | sink & Mode::Linear (copy<=affine<=lin) | Exact-once consumption across normal and Fail unwind paths. |
| Center Blade | Central 30 px blade ending at apex (128,230) | inout x.{a, b} (PassMode::InOut) | Exclusive mutable leaf projection isolated on both sides (AXM). |
| Disjoint Negative Channels | Dual 12 px vertical air gaps flanking center | FieldTree::is_disjoint (p_i _\|_ p_j) | Zero overlapping leaf paths, enabling parallel fiber mutation. |
| Starboard Blade | Right blade (#D2A6FF Dusk / #6B2FD6 Dawn) | in & eff [...] (EvidenceVector r15/x28) | One-shot lexical effect handlers and shared views on fibers. |

## B.2 Construct the silhouette and its cuts

The master uses a 256-by-256 coordinate grid. From the top-left outer corner (30, 34) to the apex (128, 230), a 98-unit horizontal run accompanies a 196-unit fall. That two-to-one slope fixes the outer wall. A matching ray defines the other wall. Place the blade vertices on those rays so the silhouette has no step.

The canonical polygon construction sets the channel width, center blade width, chevron width, and split gap. The vertical channels are 12 units wide in the master. An optical cut is a version of the mark sized for a particular use. At small sizes it adjusts the proportions so the gaps remain visible.

<!-- technical-detail -->

A four-parameter optical tuple `Theta = (g_chan, w_center, w_root, g_split)` controls stroke weight and gap width across display sizes while locking every outer vertex onto the same `2:1` V-cone rays. From `(30, 34)` to the bottom apex `(128, 230)`, the vertical drop `Delta y = 196` is exactly twice the horizontal run `Delta x = 98`, fixing slope `+2` on the left wall and `-2` on the right wall. Both the lower chevron splitter rays from `(128, 113)` and the shifted refraction boundary from `(128, 127)` run at slope `+-0.5`, meeting the `2:1` outer walls orthogonally (`2 * (-0.5) = -1`) so silhouette corners never step or kink.

    Annotated 256x256 Master CAD Diagram — Theta_master = (g_chan=12, w_center=30, w_root=38, g_split=14)
    -----------------------------------------------------------------------------------------------------
      (30,34)        (87,34)            (169,34)        (226,34)    y = 34  (top rim, 45 deg valley)
         \──────────────\                  /──────────────/
          \   Upper Root \   (128,75)     /  Aggregate     /
           \   Chevron    \      v       /   w_root = 38  /
      (50,74)\.._          \____/ \_____/          _..(206,74)      slope +-0.5 orthogonal splitters
                 ``--.._        (128,113)   _..--''
       (59.33,92.67)    ``--.._     |  _..--''   (196.67,92.67)     g_split = 14 px refraction gap
             \``--.._          `v' (128,127)        _..--''/        y_refract(x) = 127 - 0.5*|128 - x|
              \ Port \  (101,  /   \  (155,        / Star /
               \ #FFB \ 113.5)|(113,| 113.5)      / #D2A /
                \ 454  \  |   |119.5|   |        /  6FF /
                 \ sink \ |12 |     |12 |       /  eff /            g_chan = 12 px vertical air gaps:
                  \  &   \|px |Center 30|px    /  & in/               left:  x in (101, 113)
                   \linear(101|inout|px |(155,/ views/                right: x in (143, 155)
                    \    ,176)| #5CCFE6 |176)/      /
      y_outer(x) =   `        |(113,200)|(143,200) '                colinear 2:1 V-cone walls:
      34 + 2*|x - 30|          \       /                              left:  y = 34 + 2*(x - 30)
                                \     /                               right: y = 34 + 2*(226 - x)
                                 \   /
                               (128,230)                            bottom apex (x = 128, y = 230)
    ----
    Closed-Form Master Polygon Vertices (256x256)
    * Upper Root Chevron (8v): (30,34)->(87,34)->(128,75)->(169,34)->(226,34)->(206,74)->(128,113)->(50,74)
    * Center Apex Blade  (6v): (128,127)->(143,119.5)->(143,200)->(128,230)->(113,200)->(113,119.5)
    * Port Blade         (3v): (59.33,92.67)->(101,113.5)->(101,176)
    * Starboard Blade    (3v): (196.67,92.67)->(155,113.5)->(155,176)
    ----
    Pixel-Snapped 16x16 Micro Grid (vesper-favicon-16.svg: 1 px channels at x in [6,7] and x in [9,10])
      x: 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15
      y=1: . R R R R . . . . .  .  R  R  R  R  .   R = Root Chevron (#E2E8F5 / #1B2030)
      y=4: . . R R R R R R R R  R  R  R  R  .  .   L = Port Blade   (x in [3.1, 6], #FFB454)
      y=7: . . . L L L | C C |  S  S  S  .  .  .   | = 1 px Negative Channels (x in [6,7], [9,10])
     y=11: . . . . . L | C C |  S  .  .  .  .  .   C = Center Blade (x in [7, 9], y to 15.5, #5CCFE6)
     y=15: . . . . . . . . C .  .  .  .  .  .  .   S = Starboard    (x in [10, 12.9], #D2A6FF)
    ----
    Optical Cut           Viewport      Parameters Theta = (g_chan, w_center, w_root, g_split)   Deployment Surfaces
    --------------------  ------------  ------------------------------------------------------   --------------------------------------------------------
    Canonical Master      >= 32x32 px   (12, 30, 38, 14), evening_star_glint = false             vesper-lang.org hero, README.md, PDF, 1200x630 OG cards
    Bold App Tile         24..64 px     (16, 34, 42, 16), evening_star_glint = false             Desktop app tiles (Vesper Lens, Vesper Aura)
    Release Star Seal     >= 48x48 px   (12, 30, 38, 14) + StarGlint diamond at (128,38)         Verified 0-sorryAx .olean badges and release emblems
    Micro Pixel-Snapped   16..20 px     Dedicated 16x16 grid (1 px channels x in [6,7], [9,10])  Browser favicons, .ves IDE file icons, vesper top bar

## B.3 Use color and typography consistently

Dusk and Dawn use different foreground and background pairings. The palette uses OKLCH to control lightness, chroma, and hue. Check contrast against the surface where the mark or text actually appears. The wordmark uses Space Grotesk Bold, prose uses Inter, and code uses JetBrains Mono. Scale the mark uniformly to keep the channels and slopes intact.

<!-- technical-detail -->

Brand marks share the perceptual OKLCH palette slots of Vesper Chroma (`--vc-*`) so vector emblems and syntax-highlighted code blocks align without hue drift across Dusk and Dawn themes.

| Semantic Role | CSS Token | Dusk Hex | Dusk OKLCH (L, C, h) | Dawn Mark Hex | Dawn Spec OKLCH (L, C, h) | Spec Ratio & Audited Gate |
| --- | --- | --- | --- | --- | --- | --- |
| Canvas Surface | --vc-bg-canvas | #0B0E17 | (14.5%, 0.022, 265 deg) | #F2EFE9 | (95.2%, 0.010, 85 deg) | Page/canvas backdrop |
| Card Surface | --vc-bg-card | #111625 | (18.5%, 0.026, 265 deg) | #FAF8F5 | (98.0%, 0.006, 85 deg) | Code & card contrast reference |
| Root & Wordmark | --vc-fg-primary | #E2E8F5 | (92.0%, 0.018, 255 deg) | #1B2030 | (23.5%, 0.028, 265 deg) | 12.7:1 (>= 7.0:1 primary gate) |
| Port (linear / sink) | --vc-linear | #FFB454 | (83.5%, 0.158, 72 deg) | #B85200 | (52.0%, 0.145, 52 deg) | Spec >=7.1:1; Dawn mark 4.67:1 AA |
| Center (inout) | --vc-inout | #5CCFE6 | (84.5%, 0.132, 205 deg) | #006B80 | (48.5%, 0.102, 212 deg) | Spec >=7.0:1; Dawn text #015C6F |
| Starboard (in / eff) | --vc-effect | #D2A6FF | (81.0%, 0.149, 298 deg) | #6B2FD6 | (49.0%, 0.215, 292 deg) | Spec >=7.0:1; Dawn text #6627D0 |
| Proof (0 sorryAx) | --vc-wire-proof | #6EE7B7 | (85.0%, 0.145, 158 deg) | #087443 | (48.0%, 0.118, 156 deg) | Spec >=7.4:1; Dawn text #016137 |

In `crates/vesper-highlight/src/brand.rs`, `BrandColorMode` selects among three approved renderings: `TriPillarOklch` (default four-role assignment with `--vc-fg-primary` root, `--vc-linear` Port, `--vc-inout` Center, and `--vc-effect` Starboard), `SolarAmberAccent` (`--vc-fg-primary` root, `--vc-linear` Port and Center, and `--vc-comment` slate Starboard), and `MonochromeStencil` (all four polygons in `#E2E8F5`, `#1B2030`, or `#000000` for 1-bit print, e-ink, and etched silicon). Note the measured contrast split recorded in `website/VALIDATION.md` (lines `40-44`) and `crates/vesper-highlight/src/palette.rs`: Dusk foreground tokens clear `8.84:1` on `#111625`, while `website/brand/vesper-mark-dawn.svg` retains the manuscript's `#B85200`, `#006B80`, and `#6B2FD6` polygon fills (meeting WCAG AA down to `4.67:1` on `#FAF8F5`), and `ChromaPalette::vesper_dawn()` darkens foreground text swatches (`#8B3C00`, `#015C6F`, `#6627D0`, `#016137` on `#FAF8F5` / `#F2EFEA`) so `verify_contrast_gates` enforces `>= 7.0:1` in integer luminance arithmetic. On the documentation website, `website/site.css` pairs warm neutral chrome variables (`--bg: #101111` / `#f7f5f0`, `--surface: #161717` / `#eeede7`, `--ink: #f3f1ed` / `#212320`, `--accent: #ffad65` / `#914209`) for page layout with `--vc-*` variables inside highlighted code surfaces.

Typography locks lowercase `vesper` in `Space Grotesk` Bold (`700`) with `-0.035em` letter-spacing (`letter-spacing="-3"` at `84 px` in SVG), paired with `Inter` for body copy and `JetBrains Mono` for source code, diagnostics, and mode badges. Preserve a minimum clear-space margin of `0.25 * W` around the mark on all four sides, scale strictly uniformly so the `2:1` outer cone and `+-0.5` refraction slopes never warp, keep negative-space channels unfilled, and never apply outer glow or blur filters that bleed into the `12 px` air gaps.

## B.4 Choose the artifact for the surface

Use the master vector mark when there is room for its geometry. At 16 pixels the favicon uses its own pixel-snapped cut. The horizontal lockup adds the lowercase wordmark. Use the pixel-snapped artwork for a file icon. In a terminal, the character banner fits the text grid.

<!-- technical-detail -->

Deploy `website/brand/vesper-mark-dusk.svg` (or `vesper-mark-dawn.svg` on light surfaces) at `32 x 32 px` and above, switch to the dedicated pixel-snapped `website/brand/vesper-favicon-16.svg` at `16 x 16 px` to `20 x 20 px` so vertical channels land cleanly on integer pixel columns `x in [6, 7]` and `[9, 10]`, and use `website/brand/vesper-wordmark-dusk.svg` (`520 x 140`) for wide headers and footers. Terminal tools (`vesper --version` and `vesper top`) emit the four-line ASCII V-cone banner; the version/hash metadata line below illustrates the formatting layout rather than a published release digest.

    <!-- website/brand/vesper-mark-dusk.svg (256x256 Canonical Master) -->
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" width="256" height="256" fill="none">
      <!-- Upper Aggregate Root Struct Chevron -->
      <polygon points="30,34 87,34 128,75 169,34 226,34 206,74 128,113 50,74" fill="#E2E8F5"/>
      <!-- Port Blade: Mode::Linear & sink Ownership Transfer -->
      <polygon points="59.33,92.67 101,113.5 101,176" fill="#FFB454"/>
      <!-- Center Apex Blade: Disjoint inout FieldTree Projection -->
      <polygon points="128,127 143,119.5 143,200 128,230 113,200 113,119.5" fill="#5CCFE6"/>
      <!-- Starboard Blade: Shared in Views & One-Shot eff [...] Capabilities -->
      <polygon points="196.67,92.67 155,113.5 155,176" fill="#D2A6FF"/>
    </svg>
    <!-- -->
    <!-- website/brand/vesper-favicon-16.svg (16x16 Pixel-Snapped Cut) -->
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16" fill="none">
      <polygon points="1,1.5 5,1.5 8,4.5 11,1.5 15,1.5 13.6,4.3 8,7.1 2.4,4.3" fill="#E2E8F5"/>
      <polygon points="3.1,5.7 6,7.1 6,11.5" fill="#FFB454"/>
      <polygon points="8,8.1 9,7.6 9,13.5 8,15.5 7,13.5 7,7.6" fill="#5CCFE6"/>
      <polygon points="12.9,5.7 10,7.1 10,11.5" fill="#D2A6FF"/>
    </svg>
    <!-- -->
    <!-- website/brand/vesper-wordmark-dusk.svg (520x140 Horizontal Lockup) -->
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 520 140" width="520" height="140" fill="none">
      <g transform="translate(12, 12) scale(0.453125)">
        <polygon points="30,34 87,34 128,75 169,34 226,34 206,74 128,113 50,74" fill="#E2E8F5"/>
        <polygon points="59.33,92.67 101,113.5 101,176" fill="#FFB454"/>
        <polygon points="128,127 143,119.5 143,200 128,230 113,200 113,119.5" fill="#5CCFE6"/>
        <polygon points="196.67,92.67 155,113.5 155,176" fill="#D2A6FF"/>
      </g>
      <text x="148" y="95" fill="#E2E8F5" font-family="'Space Grotesk', 'Inter', system-ui, sans-serif" font-size="84" font-weight="700" letter-spacing="-3">vesper</text>
    </svg>
    <!-- -->
    -- Canonical 4-line CLI ASCII banner (vesper --version / vesper top specimen)
      \\    //   vesper 1.0.0 (stage3-ddc blake3:e9f01b4c)
       \\  //    target: x86_64-unknown-linux-gnu (cranelift + lean4)
        \\||/    modes: copy <= affine <= linear | 0 sorryAx
         \/

## B.5 Interpret the geometric proof

In this construction, collinearity means that the chosen vertices lie on one straight ray. Disjointness means the designated polygons have a separating channel. Lean can check those facts about coordinates. Check the rasterized mark at its intended size and measure contrast against its actual background.

<!-- technical-detail -->

`lean/BrandGeometrySafety.lean` checks two arithmetic invariants of the `256 x 256` coordinate design using `omega` with zero extra axioms. First, `OnLeftOuterVWall` defines the left `2:1` boundary ray `y - 34 = 2 * (x - 30)` over `30 <= x <= 128`, and `canonical_prism_outer_vertices_colinear` verifies that the outer Root Chevron vertices `(30, 34)` and `(50, 74)` and outer Center Blade vertices `(113, 200)` and `(128, 230)` are strictly colinear. Second, `prism_three_blades_pairwise_disjoint` proves that for any positive channel width `gap > 0` fitting inside the cone (`halfC + gap < 98`), no horizontal coordinate `x` belongs to more than one lower blade. These theorems validate exact polygon coordinates; subpixel antialiasing at small DPIs and perceptual contrast against actual page surfaces must still be checked empirically.

    -- lean/BrandGeometrySafety.lean
    namespace Vesper.BrandGeometry
    def OnLeftOuterVWall (x y : Nat) : Prop :=
      y - 34 = 2 * (x - 30) ∧ 30 ≤ x ∧ x ≤ 128
    --
    theorem canonical_prism_outer_vertices_colinear :
        OnLeftOuterVWall 30 34 ∧
        OnLeftOuterVWall 50 74 ∧
        OnLeftOuterVWall 113 200 ∧
        OnLeftOuterVWall 128 230 := by
      unfold OnLeftOuterVWall
      omega
    --
    theorem prism_three_blades_pairwise_disjoint
        (halfC gap x : Nat) (h_gap : gap > 0) (h_fit : halfC + gap < 98) :
        let portMax   := 128 - halfC - gap
        let centerMin := 128 - halfC
        let centerMax := 128 + halfC
        let starMin   := 128 + halfC + gap
        ¬(x ≤ portMax ∧ centerMin ≤ x ∧ x ≤ centerMax) ∧
        ¬(centerMin ≤ x ∧ x ≤ centerMax ∧ starMin ≤ x) ∧
        ¬(x ≤ portMax ∧ starMin ≤ x) := by
      dsimp only
      refine ⟨?_, ?_, ?_⟩ <;> omega
    end Vesper.BrandGeometry

## B.6 Reproduce the artwork from parameters

A deterministic emitter takes the approved dimensions and palette and writes the asset. Two runs with identical inputs should reproduce the same geometry. If the favicon needs adjustment, change its optical cut while preserving the master used by documentation and larger surfaces.

<!-- technical-detail -->

`crates/vesper-highlight/src/brand.rs` synthesizes the master mark, `16 x 16` favicon, and `520 x 140` wordmark deterministically from `ChromaPalette` swatches without external SVG templates. Before emitting XML, `verify_prism_geometry` rejects `channel_gap_px == 0` (`ZeroChannelGapBreaksDisjointness`), chevron-shoulder collisions (`g_split * 4 <= w_center`), and parameters whose blades cross `[30, 226]` or `y = 230` (`CenterBladeExceedsVConeEnvelope`). Unit tests in `brand.rs` assert that `emit_prism_mark_svg(&ChromaPalette::vesper_dusk(), BrandColorMode::TriPillarOklch, PrismProportions::CANONICAL_MASTER)` reproduces `website/brand/vesper-mark-dusk.svg` byte-for-byte.

    // crates/vesper-highlight/src/brand.rs
    use crate::ChromaPalette;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BrandColorMode { TriPillarOklch, SolarAmberAccent, MonochromeStencil }
    //
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PrismProportions {
        pub channel_gap_px: u16,
        pub center_width_px: u16,
        pub root_weight_px: u16,
        pub split_gap_px: u16,
        pub evening_star_glint: bool,
    }
    impl PrismProportions {
        pub const CANONICAL_MASTER: Self = Self {
            channel_gap_px: 12, center_width_px: 30, root_weight_px: 38, split_gap_px: 14, evening_star_glint: false,
        };
        pub const BOLD_APP_TILE: Self = Self {
            channel_gap_px: 16, center_width_px: 34, root_weight_px: 42, split_gap_px: 16, evening_star_glint: false,
        };
        pub const RELEASE_STAR_SEAL: Self = Self { evening_star_glint: true, ..Self::CANONICAL_MASTER };
    }
    //
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BrandGeometryError { ZeroChannelGapBreaksDisjointness, CenterBladeExceedsVConeEnvelope }
    //
    pub fn verify_prism_geometry(p: PrismProportions) -> Result<(), BrandGeometryError> {
        if p.channel_gap_px == 0 { return Err(BrandGeometryError::ZeroChannelGapBreaksDisjointness); }
        let (w_c, g_ch, w_r, g_sp) = (
            u32::from(p.center_width_px), u32::from(p.channel_gap_px),
            u32::from(p.root_weight_px), u32::from(p.split_gap_px),
        );
        let y_refract = 75 + w_r + g_sp;
        if w_c == 0 || g_sp == 0 || w_r <= 8 || g_sp * 4 <= w_c || w_c + g_ch * 2 >= 196
            || y_refract >= 230 || (y_refract - 38) * 4 >= (256 - w_c - g_ch * 2) * 3
        {
            return Err(BrandGeometryError::CenterBladeExceedsVConeEnvelope);
        }
        Ok(())
    }
    //
    pub fn emit_prism_mark_svg(
        palette: &ChromaPalette, mode: BrandColorMode, prop: PrismProportions,
    ) -> Result<String, BrandGeometryError>;
    pub fn emit_favicon_16_svg(palette: &ChromaPalette) -> String;
    pub fn emit_wordmark_svg(
        palette: &ChromaPalette, mode: BrandColorMode, prop: PrismProportions,
    ) -> Result<String, BrandGeometryError>;
