# Mechanized metatheory for a larger λ_Vesper fragment (#258)

Wave six, lane 16. Base: `origin/main` at
`07322e3ba47890e939a48967a1ea8f62fdd7aac1` (a descendant of the required
baseline `cda092f`). Branch `claude/project-thread-mr79yb`.

## What was built

* `lean/MetatheoryFull.lean` (new, about 2,300 lines, Lean 4 core only, no
  imports, no Mathlib). It defines a calculus, a type system, a small-step
  semantics over configurations with a resource store, and proves progress,
  preservation, type safety, one-shot resumption and Theorem 2 (total linear
  exhaustion) for it. `lean/MetatheoryAppendix.lean` is not imported or changed.
* `tests/metatheory_full.rs` (new root integration test, so Bazel target
  `//:test/metatheory_full`). It always checks source hygiene: no `sorry`,
  `admit`, `axiom`, `unsafe`, `partial`, `native_decide`, `opaque`,
  `implemented_by`/`extern`, no imports, at least 100 theorems, no duplicate
  names, every `theorem` has a `#print axioms` line, and the headline theorems
  exist. With `VESPER_LEAN` set it compiles a private regular-file copy of the
  model (Bazel runfiles are symlinks), requires a clean compile with no
  warnings, and parses the kernel axiom report: every declared theorem is
  audited exactly once and depends only on `propext`, `Classical.choice` and
  `Quot.sound`. It needs no BUILD change: the root package's test data already
  contains `lean/`.
* This report.

No other paths were touched, and there is no root patch.

## The modelled fragment (Appendix A, §4-§6)

The calculus is in A-normal form: primitives take atoms (variables or values),
and only `let` and `handle` sequence computations. Variables are de Bruijn
indices; contexts are functions `Nat → Option (Ty × Mode)`.

| Spec item | In the model |
| --- | --- |
| Mode lattice copy ⊑ affine ⊑ linear, join/meet | `Mode`, `Mode.Le`, `join`, `meet`, lattice lemmas |
| [K-Prim], [K-Adt] `m_decl ⊔ ⊔ (k_i ⊓ m_i)`, [K-Fn] | `Ty.kind` for `unit`, `nat`, `hdl`, two-field structs with field overrides, continuation types |
| Field modality premise `k_i ⊑ m_i` | a premise of struct construction (`HasType.pair`, `VT.pair`); `vt_rleM` uses it to show a value's resources are bounded by its kind; `hidden_ticket_kind_is_copy` + `hidden_ticket_rejected` show the `Ticket @@ copy` hole it closes |
| Context split ⊛ (copy contracts, affine/linear go to one side), discard(Γ) | `Split`/`SplitE`, `Discard`; resources split without contraction (`SplitR`) |
| [T-Var], [T-SubMode], [T-Cross] | folded into the leaf rules: a variable or value is usable at any mode above its type's kind |
| [T-Let] with the Resumption Frame Condition | `HasType.letE` with `FrameOK Γ2 R2 (res ε1)`, over both the residual bindings and their runtime resources |
| [T-Match], uniform arm consumption | `letPair` (struct unpacking, fields bound at their crossed kinds) and `ifz` (both arms get the same residual context) |
| [T-CallProj] place projections | field-tree paths: `get` (`in` read of a copy leaf, root restored), `swap` (`inout` replacement), `swap2` (two `inout` projections of one root, which must be disjoint, `PathDisj`) |
| [T-Perform], [T-Handle], one-shot continuations of mode q | deep, lexically scoped handlers; `k` has mode q; the ambient clause context is copy-only and clauses own no resources; the continuation is a first-class value `kontV r body` with a store id |
| Checked arithmetic aborts | `arith` (add/sub/mul on u64, `vesper.arith.checked/1` style: exact result in range or abort); its row contains the affine `abort` label |
| Linear obligations (`WriteTxn`, ...) | `mint`/`consume` of linear handles |

Runtime configurations are `⟨e, live, next⟩`, where `live` maps every live
linear handle and every live continuation to its mode. `consume` and `resume`
require the id to be live and remove it, so the store is the dynamic
exact-once/at-most-once check. The theorems show that well-typed programs
never fail that check.

## Theorems

All statements are about the Lean model above, not about the Rust compiler
or its checker.

* `subst_ty`: substituting a closed value (with its exact resources) for a
  variable preserves typing, including under copy contraction, discard and
  the frame condition. `weakenR`/`weakenRow` add discardable resources or a
  larger row.
* `plug_inv`: decomposition of a closed term in an evaluation context, with
  re-plugging and the frame condition along the context.
* `head_pres`, **`preservation`** (Theorem 1, first half): a step of a
  well-typed configuration gives a well-typed configuration with the same
  type, mode and effect row, and the store stays fresh above `next`.
* `progress_gen`, **`progress`** (Theorem 1, second half): a closed,
  well-typed configuration whose row has no unhandled user effect is a value,
  an abort, or steps; in particular `consume` and `resume` never find their
  id dead.
* **`type_safety`**: along any run, configurations stay well typed and are
  never stuck.
* `release_at_most_once`, **`one_shot_resumption`** (Theorem 2, continuation
  half): along any run, each store id is released at most once; each
  continuation is resumed at most once.
* `alloc_live`, `terminal_no_linear`, **`total_linear_exhaustion`**
  (Theorem 2): for a closed program typed at mode copy with no unhandled user
  effects (aborts are allowed), every run that ends in a value or in a
  checked-arithmetic abort releases each minted handle and each captured
  linear continuation exactly once. So nothing leaks on an early error edge
  and nothing is duplicated across a continuation.
* `vSet_comm_disjoint`: writes through disjoint places commute;
  `vSet_overlap_not_comm` and `overlap_not_disj` show overlapping places
  don't commute and are rejected.
* `ArithOp.eval_some`, `add_overflow_aborts`: checked results are exact and in
  range; overflow aborts.
* Worked examples:
  * `txn_across_abort_rejected` is the §6 derivation: a live `WriteTxn`
    across an abortable step is untypable, because `linear ⊑ affine` fails.
  * `txn_then_arith_typed`, `txn_then_arith_runs` and
    `txn_then_arith_exhausts` cover the accepted order, its run, and
    Theorem 2 on that run.
  * `lin_handler_typed`, `lin_handler_runs` and `lin_handler_resumes_once`
    cover a linear handler that resumes exactly once.
  * `aff_handler_typed` and `aff_handler_runs` cover an affine handler that
    drops `k`, leaving only an affine id live.
  * `linear_drop_rejected` shows that a linear handler dropping `k` is
    untypable.
  * `checked_overflow_runs` shows that overflow aborts the program.

### Axiom footprints (all 134 theorems)

Each theorem has a `#print axioms` line, so a normal compile prints the whole
audit. There is no `sorryAx` and no new axiom. `Classical.choice` comes from
the `grind` tactic in the pointwise context-algebra lemmas, and from the
proofs built on them.

| Footprint | Count | Theorems |
| --- | ---: | --- |
| none | 44 | `Mode.le_refl`, `Mode.le_trans`, `Mode.copy_le`, `Mode.le_linear`, `Mode.le_join_left`, `Mode.le_join_right`, `Mode.join_le`, `Mode.meet_le_left`, `Mode.meet_of_le`, `Mode.le_copy`, `RMode.le_refl`, `RMode.not_le_copy`, `RMode.le_affine_iff`, `splitR_empty_left`, `splitR_empty_right`, `splitR_congr`, `discardR_empty`, `rleM_split`, `rleM_mono`, `rleM_copy`, `empty_rleM`, `empty_discardR`, `split_left_all`, `discard_empty`, `allCopy_empty`, `split_del`, `discard_del`, `allCopy_del`, `splitR_congr_left`, `splitR_self_empty`, `del_eq`, `vt_isVal`, `vt_rleM`, `substOK_ext`, `substOK_frame`, `rowSub_trans`, `rowSub_refl`, `weakenRow`, `frame_rleM`, `splitR_left_of_empty`, `step_parts`, `hidden_ticket_kind_is_copy`, `linear_field_kind`, `one_none_split` |
| `propext` | 29 | `Mode.le_antisymm`, `discardR_split`, `discardR_join`, `rleM_affine_discard`, `splitE_left`, `splitE_right`, `discard_split`, `splitE_none`, `splitE_some`, `subst_val`, `substOK_discard`, `isAtom_subst`, `discard_rest`, `bind_map_l`, `bind_map_r`, `vSet_comm_disjoint`, `vSet_overlap_not_comm`, `overlap_not_disj`, `atom_closed`, `vt_set_exists`, `head_alloc`, `ArithOp.eval_some`, `add_overflow_aborts`, `txn_then_arith_typed`, `txn_then_arith_runs`, `lin_handler_typed`, `lin_handler_runs`, `aff_handler_runs`, `checked_overflow_runs` |
| `Quot.sound` | 1 | `rempty_eq` |
| `propext`, `Quot.sound` | 17 | `res_le`, `res_mono`, `split_empty`, `ext_del`, `substOK_handle`, `ext_del_zero`, `head_next`, `head_fresh`, `head_dead`, `head_release`, `step_fresh`, `step_dead`, `steps_fresh`, `steps_dead`, `count_resume_le_rel`, `txn_across_abort_rejected`, `linear_drop_rejected` |
| `propext`, `Classical.choice`, `Quot.sound` | 43 | `splitR_comm`, `splitR_of_empty_left`, `splitR_of_empty_right`, `splitR_assoc`, `splitR_assoc'`, `splitR_swap`, `rleM_part`, `split4`, `splitR_of_empty`, `val_inv`, `val_ty`, `substOK_split`, `subst_ty`, `weakenR`, `splitR_rot`, `vt_at`, `vt_xchg`, `vt_swap2`, `plug_inv`, `regroup`, `nat_discard`, `head_pres`, `preservation`, `sub_left`, `sub_right`, `canon_nat`, `canon_hdl`, `canon_kont`, `canon_pair`, `progress_gen`, `progress`, `type_safety`, `head_keep`, `release_at_most_once`, `one_shot_resumption`, `steps_keep`, `alloc_live`, `terminal_no_linear`, `total_linear_exhaustion`, `hidden_ticket_rejected`, `aff_handler_typed`, `txn_then_arith_exhausts`, `lin_handler_resumes_once` |

## Checks

Lean 4.24.0 (x86_64 Linux release, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`), from the GitHub release zip
because elan downloads are blocked in the cloud:

```sh
cd lean && lean MetatheoryFull.lean    # exit 0, no warnings, 134 axiom lines, ~6 s
```

Rust, with `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`
and a private target directory:

```sh
cargo fmt --all -- --check
VESPER_LEAN=<lean 4.24.0> cargo test --locked --test metatheory_full
cargo clippy --locked --all-targets -- -D warnings      # root package (the only package changed)
```

The results are recorded in the PR description.

## Decisions

* **Syntactic progress and preservation** over a substitution-based
  small-step semantics, instead of a step-indexed (Iris-style) model. §6's
  Theorem 1 is stated for an Iris model, which Lean core cannot host in this
  scope. The syntactic route proves the same two properties for the fragment.
* **A-normal form.** Only `let` and `handle` frames are evaluation
  contexts. This keeps the frame condition in one place, [T-Let], as in the
  spec's desugaring of `?`. Surface programs elaborate into λ_Vesper anyway.
* **Store ids for continuations.** A captured continuation is a value
  carrying an id in the same store as linear handles, so one-shot resumption
  is a property of runs (`one_shot_resumption`), not only of the types.
* **Values are closed.** Substitution does not enter continuation bodies,
  which are typed in their own one-hole context, so no shifting is needed.
* **Binders of struct fields** use the fields' kinds (the [T-Cross] result)
  rather than `m0 ⊔ k_i`. This is admissible and lets scalar fields of a linear
  struct be dropped.
* **Rows** are lists of labels with membership-based sub-rows. The handler
  rule takes an explicit clause row `εc ⊆ ε`, so row weakening is admissible
  even though the continuation type mentions the row.
* `grind` is used for pointwise reasoning on resource splits. It brings in
  `Classical.choice`, which is a standard Lean axiom and is reported above.

## Remaining gaps compared with §4-§6

* **No functions or closures.** There is no [T-Abs] and no arrow types other
  than continuations, so the `m_z ⊑ m_fn` capture premise isn't modelled.
  Continuations are the only first-class computations.
* **No handler state threading** (`state s = e_init`). Linear state across
  multiple operations isn't covered. Ambient clause contexts are copy-only, as
  in [T-Handle].
* **One operation per effect**, effects given by a fixed global signature.
  There are no effect-row variables (ρ), no type variables (Δ, [K-Var]) and
  no generics.
* **Places are functional.** `get`/`swap`/`swap2` consume the root and return
  the restored or updated root. There are no second-class `shr`/`mut` borrows
  in Γ, no store of mutable locals and no Place Capability Graph. `swap2` is
  the two-argument case of the [T-CallProj] disjointness premise; n-ary
  calls with mixed `in`/`inout`/`sink` modes aren't modelled.
* **Theorem 2 is proved for releases, not per kind.** It counts releases of
  store ids. A linear handle and a linear continuation are both released
  exactly once. The theorem doesn't separately prove that a handle is never
  released by `resume`. The `Head` rules make that impossible syntactically,
  but no theorem states it.
* **Aborts are untrappable**, like a process-level trap. A `Fail` handler
  that catches arithmetic aborts isn't modelled. User affine effects play
  that role.
* **The Zero-Pin corollary isn't stated**: there is no stack or relocation
  model.
* **No link to the Rust compiler or checker.** Nothing here proves that
  `vesper check` implements these rules. Rust ownership in the host libraries
  is not evidence for these source typing rules either.
