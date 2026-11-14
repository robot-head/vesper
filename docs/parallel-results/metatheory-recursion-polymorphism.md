# Mechanized metatheory: recursion, type variables, first-class row polymorphism and label escape (#454)

Wave eight, lane 5 (Appendix A, §4-§6). Base: `origin/main` at
`1094e649c95e4d81eef4e705d13882ab1339542d` (a descendant of the required
baseline `ea1ed29`). Branch `claude/project-thread-nxkbq8`.

## What was built

* `lean/Metatheory/Polymorphism.lean` (new, about 4,300 lines, Lean 4 core
  only, no imports). It extends the wave-seven calculus of
  `lean/Metatheory/Closures.lean` (not imported or changed) with recursion,
  generic type variables with modal bounds, first-class `∀ρ` row
  polymorphism and lexical discharge of handler labels, and re-proves the
  whole metatheory for the extended calculus: 272 theorems, every one
  audited with `#print axioms`.
* `tests/metatheory_poly.rs` (new root integration test, Bazel target
  `//:test/metatheory_poly`; the root package's test data already contains
  `lean/`, so no BUILD change). It always checks source hygiene (no `sorry`,
  `admit`, `axiom`, `unsafe`, `partial`, `native_decide`, `opaque`,
  `implemented_by`/`extern`, no imports, at least 250 theorems, no duplicate
  names, every theorem audited, the 40 headline theorems exist). With
  `VESPER_LEAN` set it compiles a private copy, requires a clean compile, and
  parses the kernel axiom report (joining Lean's wrapped lines): every
  theorem is audited exactly once and depends only on `propext`,
  `Classical.choice` and `Quot.sound`.
* This report.

No other paths were touched and there is no root patch. `MetatheoryFull.lean`,
`Closures.lean` and their tests are unchanged.

## The extension

| Spec item | In the model |
| --- | --- |
| Recursion (`fix`) | `fixc env body`, a recursive closure value. [T-Fix]: the body is typed in `(f : τ_fn @ m_fn, env : τ_z @ copy, x : σ @ m_x)`. The environment must be copy because every unfolding duplicates it. β-rule `app (fixc v b) w → b[w/x][v/env][fixc v b/f]`. |
| Fuel / step-indexed semantics | `Eval Sig n c acts o`: at most `n` steps, ending in a value, an abort or `outOfFuel`. It is sound with respect to the small-step semantics (`eval_sound`) and total for well-typed programs at every fuel (`eval_total`). |
| [K-Var], `α : mod m` in Δ | `Ty.tvar α m` with `kind = m`. The bound is part of the variable's name, so Δ is implicit and every occurrence agrees with it. |
| Substitution for type variables | `Ty.tsub θ` with `TSubOK θ : ∀ α m, kind (θ α m) ⊑ m`. Kinds can only go down (`kind_tsub_le`). |
| First-class `∀ρ` | `Ty.rall H τ` binds de Bruijn row variable 0 in `τ`. Values are `rlam v` ([T-RAbs], `v` a closed value typed with the fresh `ρ`), and every use site is `rapp a ρ` ([T-RApp], type `τ[ρ/0]`). At run time the abstraction is erased: `rapp (rlam v) ρ → v`. |
| Lexical escape prevention | [T-Handle] gets two premises, `user l ∉ ε_c` and `¬ τ_out.Lab l`, so the handler's label is discharged from the clause row and the result type. A closure that may perform `l` has `l` in its arrow row ([T-Abs]/[T-Perform]), so it cannot leave the handler. |

Binders are generalised the way [T-Match] (`m0 ⊔ k`) and [T-Abs] (`x :_(m_x)`)
already read: a binder (closure and continuation arguments, struct fields,
`fixc` arguments) may be bound at any mode at or above its type's kind, and
struct fields may be typed at any mode. Without this, kinds would not be
stable under type substitution, because an instance's kind can be smaller
than its variable's bound. Binding above the kind is only more restrictive,
and every wave-seven proof goes through with this change.

## Theorems

All statements are about the Lean model above, not about the Rust compiler
or its checker.

* **Type safety for the extended calculus:** `subst_ty`, `weakenR`,
  `weakenRow`, `plug_inv`, `head_pres`, **`preservation`**, **`progress`**,
  **`type_safety`**. New cases: the `fixc` unfolding, `rapp` instantiation,
  canonical forms for arrows (`clos` or `fixc`) and for `∀ρ` (`rlam`).
* **Linearity and one-shot, re-proved:** `release_at_most_once`,
  **`one_shot_resumption`**, **`total_linear_exhaustion`**, and Theorem 2 per
  kind (`minted_never_resumed`, `captured_never_consumed`,
  **`total_linear_exhaustion_per_kind`**).
* **Recursion with fuel:** `eval_sound`, `eval_length`, `eval_out_of_fuel`,
  `eval_terminal`, **`eval_total`** (a well-typed configuration with no
  unhandled user effect evaluates at every fuel to a well-typed value, abort
  or out-of-fuel configuration, so it is never stuck), **`eval_exhaustion`**
  (Theorem 2 for every evaluation that finishes within its fuel), and
  `steps_eval`.
* **Type variables:** `kind_tsub_le`, `at_tsub`, `tsub_inst` (type and row
  substitution commute), `lab_tsub`, the context lemmas, and **`tsub_ty`**:
  a derivation with `α : mod m` stays valid under every bound-respecting
  instantiation. **`tvar_safety`** combines it with type safety.
* **Row variables and `∀ρ`:** composition and identity laws for row
  substitutions under binders (`row_inst_inst`, `ty_inst_inst`, `liftθ_comp`,
  `ty_inst_id`), **`inst_open`** (instantiating after opening is opening after
  instantiating under the binder), **`inst_ty`** (prenex instantiation, now
  also through `∀ρ` binders), `row_poly_safety`, and **`rapp_inst`** (a use
  site of a row abstraction at a closed row that avoids `H` is typed at
  `τ[ρ/0]`).
* **Label escape:** `handle_discharges`, `handler_result_label_free`,
  `unhandled_perform_in_row`, **`run_performs_in_row`** (along any run of a
  well-typed configuration, every operation pending without a handler is in
  the configuration's row), and **`escaped_closure_never_performs`**: if a
  handler for `l` returns a closure, then no run of applying that closure,
  to any argument and in any store, ever leaves a `perform l` pending outside
  a handler the closure installs itself. So no other handler for `l` can ever
  catch an operation of the escaped closure. In the wave-seven rule this was
  possible.
* `typed_below` and `captured_kont_self_contained` (the syntactic core of
  Zero-Pin), re-proved.
* **Worked examples:**
  * `loop_fn_typed`, `loop_typed`, `loop_step_inv`, **`loop_diverges`**,
    `loop_out_of_fuel`: `fix f. λx. f x` is well typed and never reaches a
    value or an abort, and the evaluator runs out of fuel on it at every fuel.
  * `count_fn_typed`, `count_typed`, `count_runs`, `count_evaluates`: a
    recursive countdown using checked subtraction, which has `abort` in its
    row, unfolds twice and returns 0 within 6 steps.
  * `dup_typed`, `dup_at_nat`, `dup_hdl_out_of_bound`,
    **`dup_linear_rejected`**: `λx. (x, x)` is typable at `α : mod copy` and
    instantiates at `nat`. `hdl` is outside the bound. At `α : mod linear`
    the term is untypable, because a linear argument does not contract.
  * `poly_use_typed`, `poly_use_runs`: one first-class value
    `Λρ. λg. g ()` is bound once and instantiated at two use sites, at the
    Fail row and at the empty row, and the program runs.
  * **`escape_rejected`**: a handler returning a closure that performs the
    handler's own label is untypable.
  * The wave-seven examples (linear closures, a stateful two-operation
    handler with Theorem 2 per kind on its run, abort across linear state,
    and row instantiation of `call_it`) still hold.

### Axiom footprints (all 272 theorems)

Each theorem has a `#print axioms` line, so a normal compile prints the
whole audit. There is no `sorryAx` and no new axiom. `Classical.choice` comes
from `grind` in the context-algebra lemmas and from `Classical.byContradiction`.

| Footprint | Count | Theorems |
| --- | ---: | --- |
| none | 95 | `Mode.le_refl`, `Mode.le_trans`, `Mode.copy_le`, `Mode.le_linear`, `Mode.le_join_left`, `Mode.le_join_right`, `Mode.join_le`, `Mode.meet_le_left`, `Mode.meet_of_le`, `Mode.le_copy`, `RMode.le_refl`, `RMode.le_trans`, `RMode.not_le_copy`, `RMode.le_affine_iff`, `RMode.affine_le_mode`, `Ty.at_nil`, `nthC_val`, `nthC_subst`, `splitR_empty_left`, `splitR_empty_right`, `splitR_congr`, `discardR_empty`, `rleM_split`, `rleM_mono`, `rleM_copy`, `rleM_affine_discard`, `discardR_rleM`, `empty_rleM`, `empty_discardR`, `split_left_all`, `split_right_all`, `discard_empty`, `allCopy_empty`, `split_del`, `discard_del`, `allCopy_del`, `splitR_congr_left`, `splitR_self_empty`, `del_eq`, `vt_isVal`, `vt_rleM`, `rowSub_trans`, `rowSub_refl`, `weakenRow`, `row_inst_nil`, `inst_empty`, `splitE_inst`, `split_inst`, `discard_inst`, `allCopy_inst`, `substOK_ext`, `substOK_discard`, `substOK_frame`, `nthC_subst_some`, `nthC_subst_isSome`, `frame_rleM`, `splitR_left_of_empty`, `substOK_val`, `split_two`, `step_parts`, `head_consume_hdl`, `head_resume_kont`, `eval_sound`, `eval_terminal`, `eval_out_of_fuel`, `Mode.join_mono`, `Mode.meet_mono_left`, `kind_tsub_le`, `lab_tsub`, `tsub_empty`, `splitE_tsub`, `split_tsub`, `discard_tsub`, `allCopy_tsub`, `frameOK_tsub`, `handle_discharges`, `handler_result_label_free`, `ctxBelow_two`, `ctxBelow_three`, `exSig_inv`, `frame_empty`, `linear_capture_copy_closure_rejected`, `st_clause_split0`, `st_clause_split1`, `st_rest_split`, `split_self_copy`, `allCopy_discard`, `var_copy`, `frame_copy`, `loop_step`, `loop_out_of_fuel`, `count_runs`, `exSig_tinv`, `dup_hdl_out_of_bound`, `poly_use_runs` |
| `propext` | 54 | `Mode.le_antisymm`, `RMode.affine_le`, `splitE_left`, `splitE_right`, `discard_split`, `splitE_none`, `splitE_some`, `subst_val`, `row_inst_cons`, `row_inst_rvar`, `row_inst_append`, `row_inst_inst`, `row_inst_id`, `row_inst_closed`, `kind_inst`, `at_inst`, `inst_cons_user`, `nthC_HL`, `isAtom_subst`, `bind_map_l`, `bind_map_r`, `vSet_comm_disjoint`, `vSet_overlap_not_comm`, `overlap_not_disj`, `atom_closed`, `var_only0`, `var_only1`, `vt_set_exists`, `head_alloc`, `at_tsub`, `tsub_inst`, `ctxBelow_split_left`, `ctxBelow_split_right`, `var_at`, `discard_two`, `discard_one_copy`, `lin_closure_typed`, `lin_closure_runs`, `st_handler_typed`, `st_handler_runs`, `call_it_typed`, `allCopy_ext`, `allCopy_two`, `allCopy_three`, `loop_fn_typed`, `loop_typed`, `loop_step_inv`, `loop_diverges`, `count_fn_typed`, `count_typed`, `dup_typed`, `dup_linear_rejected`, `poly_use_typed`, `escape_rejected` |
| `Quot.sound` | 3 | `rempty_eq`, `inst_ext`, `tsub_ext` |
| `propext`, `Quot.sound` | 45 | `res_le`, `res_mono`, `split_empty`, `ext_del`, `split3_empty`, `liftθ_id`, `liftθ_comp`, `ty_inst_id`, `ty_inst_inst`, `inst_open`, `mem_inst_of`, `mem_inst_user`, `noCap_lift`, `notmem_inst`, `rowSub_inst`, `res_inst`, `inst_two`, `inst_three`, `frameOK_inst`, `clauseCtx_inst`, `substOK_handle`, `clauseCtx_del`, `ext_del_zero`, `two_del_zero`, `one_del_zero`, `head_next`, `head_fresh`, `head_dead`, `head_release`, `step_fresh`, `step_dead`, `steps_fresh`, `steps_dead`, `count_resume_le_rel`, `steps_alloc_ge`, `count_consume_le_rel`, `relCount_eq`, `eval_length`, `tsub_two`, `tsub_three`, `clauseCtx_tsub`, `tsub_ty`, `lin_closure_twice_rejected`, `linear_state_across_abort_rejected`, `dup_at_nat` |
| `propext`, `Classical.choice`, `Quot.sound` | 75 | `splitR_comm`, `splitR_of_empty_left`, `splitR_of_empty_right`, `splitR_assoc`, `splitR_assoc'`, `splitR_swap`, `discardR_split`, `discardR_join`, `rleM_part`, `split4`, `splitR_of_empty`, `val_inv`, `val_ty`, `weakenR`, `lab_inst`, `inst_ty`, `substOK_split`, `discard_rest`, `subst_ty`, `splitR_rot`, `vt_at`, `vt_xchg`, `vt_swap2`, `state_rleM`, `plug_inv`, `regroup`, `nat_discard`, `capture_splits`, `split_remove`, `resume_splits`, `head_pres`, `preservation`, `sub_left`, `sub_right`, `canon_nat`, `canon_hdl`, `canon_kont`, `canon_arrow`, `canon_rall`, `canon_pair`, `progress_gen`, `progress`, `type_safety`, `head_keep`, `release_at_most_once`, `one_shot_resumption`, `steps_keep`, `alloc_live`, `terminal_no_linear`, `total_linear_exhaustion`, `dead_no_release`, `live_hdl_never_resumed`, `live_kont_never_consumed`, `minted_never_resumed`, `captured_never_consumed`, `total_linear_exhaustion_per_kind`, `steps_pres`, `eval_total`, `eval_exhaustion`, `steps_eval`, `row_poly_safety`, `tvar_safety`, `rapp_inst`, `unhandled_perform_in_row`, `run_performs_in_row`, `escaped_closure_never_performs`, `ctxBelow_ext`, `typed_below`, `captured_kont_typed`, `captured_kont_self_contained`, `lin_closure_exhausts`, `st_handler_exhausts`, `call_it_at_fail`, `call_it_pure`, `count_evaluates` |

## Appendix A rules: what is mechanized now

| Rule | Status in `Polymorphism.lean` |
| --- | --- |
| Mode lattice, [T-SubMode], [T-Cross] | Mechanized; both are folded into the variable rule and the binder modes. |
| [K-Prim], [K-Adt] with `k_i ⊑ m_i`, [K-Fn] | Mechanized for two-field structs. |
| [K-Var] | **New**: `tvar α m` with kind `m`, plus the type substitution lemma. |
| [T-Var], context splitting `⊛`, `discard` | Mechanized. |
| [T-Abs] (capture premise `m_z ⊑ m_fn`) | Mechanized with closure conversion; binder mode `m_x ⊒ kind`. |
| Recursion / [T-Fix] | **New**: `fixc`, with a copy environment. |
| [T-App], [T-Let] with the Resumption Frame Condition, [T-Match] | Mechanized. |
| [T-CallProj] `p_i ⊥ p_j` | Functional places only (as in wave seven); lane 6 covers mutable places. |
| [T-Perform], [T-Handle] with state threading, several operations | Mechanized, now with the discharge premises. |
| Effect-row variables ρ in Δ | Prenex instantiation (`inst_ty`) and, **new**, first-class `∀ρ` with use-site instantiation. |
| Lexical escape prevention (§6) | **New**: label discharge and the escape theorems. Labels stay global names (see gaps). |
| Theorem 1 | Small-step progress and preservation, plus fuel-indexed totality. |
| Theorem 2 | Re-proved, including the per-kind statement. |
| Zero-Pin corollary | Syntactic core only (unchanged from wave seven). |
| [T-RegionAlloc], [T-RegionScope] (§15) | Not modelled. |

## Checks

Lean 4.24.0 (x86_64 Linux release, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`), from the GitHub release zip
because elan downloads are blocked in the cloud:

```sh
cd lean/Metatheory && lean Polymorphism.lean   # exit 0, no warnings, 272 axiom lines, ~18 s
```

Rust, with `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`
and a private target directory:

```sh
cargo fmt --all -- --check
VESPER_LEAN=<lean 4.24.0> cargo test --locked --test metatheory_poly
cargo +1.100.0 clippy --locked --all-targets -- -D warnings   # root package (the only package changed)
```

The results are recorded in the PR description.

## Decisions

* **A new file.** Every proof is touched (new expressions, generalised
  binders, new [T-Handle] premises), so keeping the wave-six and wave-seven
  models and their audited theorems intact is simpler for review.
* **Fuel as a relation, beside the small-step semantics.** `Eval` is
  indexed by fuel and proved sound and total, rather than replacing the
  semantics with a step-indexed logical relation. Progress and preservation
  stay small-step, which is what makes non-termination harmless: a diverging
  program simply always steps.
* **Recursive closures have copy environments.** Unfolding copies the
  environment into the body and into the self reference. A linear
  environment would be duplicated, so [T-Fix] requires `m_z = copy`. The
  closure itself may still be affine or linear.
* **Bounds in variable names.** `tvar α m` carries its Δ bound, so
  substitutions are maps `α ↦ m ↦ τ` with `kind τ ⊑ m`. This avoids threading
  Δ through every judgment and every lemma.
* **Binders at any mode at or above the kind** (see above), which [T-Match]
  and [T-Abs] already allow. This is what keeps kinds stable under type
  substitution.
* **Discharge is a typing premise, and instantiations must not capture.**
  Labels are still global effect names, so a row variable instantiated with
  a row that contains a label handled inside the code would be captured by
  that handler, which is dynamic scoping. `inst_ty` and `tsub_ty` therefore
  require that the instantiation does not introduce a label handled in the
  term (`NoCap`). A `∀ρ#H` type records the labels handled inside the
  polymorphic value (`HL v ⊆ H`), and each use site's row must avoid `H`.
* **First-class use sites take closed rows.** `rapp a ρ` requires `ρ` to
  contain no row variable. Otherwise, a generic function passing its own `ρ`
  on to a first-class polymorphic value would need lacks constraints on `ρ`
  (Δ entries `ρ#H`) to stay sound. Prenex row polymorphism (`inst_ty`)
  still covers code that is generic over `ρ`.

## Remaining gaps compared with §4-§6

* **Labels are global, not generative.** Discharge and the escape theorem
  give the static half of lexical scoping. A faithful dynamic model would
  allocate a fresh runtime label per handler instance, which needs a label
  context in the judgment (Σ). Because of that, instantiations must not
  capture handled labels (above), and passing a capability-performing
  closure to row-polymorphic code that handles the same label is rejected
  rather than given lexical behaviour.
* **No lacks constraints in Δ.** A use site of a first-class `∀ρ` value is
  instantiated at a closed row, and a `∀ρ` instance of a type variable must
  have no row variables (`TRowClosed`) and must not mention a handled label.
* **Type variables are not first-class.** There is no `∀α` type. A generic
  definition is typed once with `α : mod m` and instantiated by `tsub_ty`
  (prenex), which matches how generic functions are checked.
* **Recursive closures cannot capture linear or affine values** (copy
  environment).
* **Unchanged from wave seven:** places are functional (lane 6 does mutable
  places and the Place Capability Graph), aborts are untrappable, only the
  syntactic core of Zero-Pin is proved, and there is no link to the Rust
  compiler or checker. Nothing here proves that `vesper check` implements
  these rules, and Rust ownership in the host libraries is not evidence for
  them.
