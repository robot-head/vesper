# Mechanized metatheory: functions, handler state and effect rows (#338)

Wave seven, lane 15 (Appendix A, §4-§6). Base: `origin/main` at
`283c8115d2203232d0e9b473cc4bb0312c3a8509` (a descendant of the required
baseline `c025b9a`). Branch `claude/project-thread-2jk39f`.

## What was built

* `lean/Metatheory/Closures.lean` (new, about 3,300 lines, Lean 4 core only,
  no imports, no Mathlib). It extends the wave-six fragment of
  `lean/MetatheoryFull.lean` (which is not imported or changed) with
  functions and closures, stateful handlers, several operations per effect,
  effect-row variables and a kinded resource store, and re-proves the whole
  metatheory for the extended calculus: 194 theorems, every one audited with
  `#print axioms`.
* `tests/metatheory_closures.rs` (new root integration test, so Bazel target
  `//:test/metatheory_closures`; the root package's test data already contains
  `lean/`, so no BUILD change). It always checks source hygiene (no `sorry`,
  `admit`, `axiom`, `unsafe`, `partial`, `native_decide`, `opaque`,
  `implemented_by`/`extern`, no imports, at least 150 theorems, no duplicate
  names, every theorem has a `#print axioms` line, the 26 headline theorems
  exist). With `VESPER_LEAN` set it compiles a private regular-file copy
  (Bazel runfiles are symlinks), requires a clean compile with no warnings,
  and parses the kernel axiom report: every declared theorem is audited
  exactly once and depends only on `propext`, `Classical.choice` and
  `Quot.sound`.
* This report.

No other paths were touched, and there is no root patch.

## The extended fragment

The calculus stays in A-normal form with de Bruijn variables, a
substitution-based small-step semantics over configurations
`⟨e, live, next⟩`, and the wave-six features (modes and kinds with
`k_i ⊑ m_i`, context splitting, [T-Let] with the Resumption Frame Condition,
struct unpacking, `ifz`, field-tree places with [T-CallProj] disjointness,
checked arithmetic aborts, `mint`/`consume` of linear handles). New:

| Spec item | In the model |
| --- | --- |
| Function types `τ1 -[ε]@m_fn-> τ2`, [K-Fn] | `Ty.arrow σ τ am ε mf`, kind `mf` |
| [T-Abs] with the capture premise `m_z ⊑ m_fn` | `clos env body`: the captured environment is one atom `env : τ_z @ m_z` (several captures are a struct), `HasType.clos` requires `m_z ⊑ m_fn`; the body is typed in its own two-binding context (environment, argument), so closures are closed values |
| Function application | `app f a` ([T-App]): `f` is used at its mode `m_fn`, so a linear closure is called at most once; β substitutes the argument and then the environment |
| [T-Handle] with `state s = e_init` | `handle l s body ret cls`: the state is an atom (`e_init` is let-bound in ANF) typed at `τ_s @ m_s`; `return(x, s)` and `op_i(y, s, k)` bind the state; `k : (σ_i, τ_s) -[ε]@q-> τ_out` (`Ty.kont` with a state component) and `resume(k, v, s')` passes the next state, so linear state threads through the clauses while the ambient context Γ_h stays copy-only |
| Several operations per effect | `EffSig` has `nops`, `arg o`, `res o`; `perform l.op_o(a)`; one clause per operation in a clause chain (`ccons`/`cnil`), selected by `nthC` at capture |
| Effect-row variables ρ (Δ) | `Label.rvar ρ` in rows; `res` counts ρ as affine (any row may replace it); `Row.inst`/`Ty.inst`/`Ctx.inst` instantiate them |
| Theorem 2 per kind | the store records each live id's kind: `Ent.hdl` (a linear handle) or `Ent.kont q` (a continuation of mode q); `consume` needs a live handle, `resume` a live continuation |

Arguments, operation results and clause parameters `y` are bound at their
type's kind (the [T-Cross] result), so a copy-kind argument can be dropped.

## Theorems

All statements are about the Lean model above, not about the Rust compiler
or its checker.

* `subst_ty`, `weakenR`, `weakenRow`, `plug_inv` (substitution, weakening and
  decomposition, now including closures, three-way handler/resume splits and
  clause chains), `vt_rleM` (a value's resources are bounded by its kind; this
  is where `k_i ⊑ m_i` and the capture premise `m_z ⊑ m_fn` are used).
* **`preservation`**, **`progress`**, **`type_safety`** (Theorem 1) for the
  extended fragment: well-typed closed configurations keep their type, mode and
  row, and are a value, an abort or step. In particular `consume` always finds
  a live handle and `resume` a live continuation.
* `release_at_most_once`, **`one_shot_resumption`**,
  **`total_linear_exhaustion`** (Theorem 2, as in wave six, re-proved).
* **Theorem 2 per kind**: `minted_never_resumed` (along any run from a fresh
  store, a minted handle is never released by `resume`),
  `captured_never_consumed` (a captured continuation is never released by
  `consume`), and **`total_linear_exhaustion_per_kind`**: for a closed program
  typed at mode copy with no unhandled user effect, every run ending in a value
  or an abort consumes each minted handle exactly once and never resumes it,
  and resumes each captured linear continuation exactly once and never
  consumes it.
* **`inst_ty`** (row instantiation): if `Γ ⊢ e : τ @ m | ε` holds with row
  variables, then `θΓ ⊢ e : θτ @ m | θε` for every instantiation `θ` (for an
  effect signature that does not mention the variables, `SigInv`). The key
  lemma is `res_inst`: instantiation can only raise `res(ε)`, so frame
  conditions survive. `row_poly_safety` combines it with type safety.
* `typed_below`, **`captured_kont_self_contained`**: see the Zero-Pin item
  below.
* Worked examples:
  * `linear_capture_copy_closure_rejected`: a copy closure cannot capture a
    linear handle (the capture premise).
  * `lin_closure_typed`, `lin_closure_runs`, `lin_closure_exhausts`: a closure
    capturing a handle is linear, runs once, and Theorem 2 per kind holds on
    the run.
  * `lin_closure_twice_rejected`: calling that closure twice is untypable,
    because the linear binding `f` is not contracted by ⊛.
  * `st_handler_typed`, `st_handler_runs`, `st_handler_exhausts`: a linear
    Storage-like effect with two operations and a linear handle as handler
    state; both clauses thread the state through `resume`, the return clause
    consumes it, and on the run the handle is consumed once and each of the
    two continuations is resumed once.
  * `linear_state_across_abort_rejected`: a linear state under a body that may
    abort is rejected by the state premise (see Decisions).
  * `call_it_typed`, `call_it_at_fail`, `call_it_pure`: `λg. g ()` typed once
    at `(unit -[ρ]-> nat) -[ρ]-> nat` and instantiated with the affine Fail
    effect and with the empty row.

### Axiom footprints (all 194 theorems)

Each theorem has a `#print axioms` line, so a normal compile prints the
whole audit. There is no `sorryAx` and no new axiom. `Classical.choice` comes
from `grind` in the pointwise context-algebra lemmas and from the proofs built
on them.

| Footprint | Count | Theorems |
| --- | ---: | --- |
| none | 67 | `Mode.le_refl`, `Mode.le_trans`, `Mode.copy_le`, `Mode.le_linear`, `Mode.le_join_left`, `Mode.le_join_right`, `Mode.join_le`, `Mode.meet_le_left`, `Mode.meet_of_le`, `Mode.le_copy`, `RMode.le_refl`, `RMode.le_trans`, `RMode.not_le_copy`, `RMode.le_affine_iff`, `RMode.affine_le_mode`, `nthC_val`, `nthC_subst`, `splitR_empty_left`, `splitR_empty_right`, `splitR_congr`, `discardR_empty`, `rleM_split`, `rleM_mono`, `rleM_copy`, `rleM_affine_discard`, `discardR_rleM`, `empty_rleM`, `empty_discardR`, `split_left_all`, `split_right_all`, `discard_empty`, `allCopy_empty`, `split_del`, `discard_del`, `allCopy_del`, `splitR_congr_left`, `splitR_self_empty`, `del_eq`, `vt_isVal`, `vt_rleM`, `substOK_ext`, `substOK_discard`, `substOK_frame`, `nthC_subst_some`, `nthC_subst_isSome`, `rowSub_trans`, `rowSub_refl`, `weakenRow`, `frame_rleM`, `splitR_left_of_empty`, `substOK_val`, `split_two`, `step_parts`, `head_consume_hdl`, `head_resume_kont`, `inst_empty`, `splitE_inst`, `split_inst`, `discard_inst`, `allCopy_inst`, `ctxBelow_two`, `exSig_inv`, `frame_empty`, `linear_capture_copy_closure_rejected`, `st_clause_split0`, `st_clause_split1`, `st_rest_split` |
| `propext` | 32 | `Mode.le_antisymm`, `RMode.affine_le`, `splitE_left`, `splitE_right`, `discard_split`, `splitE_none`, `splitE_some`, `subst_val`, `isAtom_subst`, `bind_map_l`, `bind_map_r`, `vSet_comm_disjoint`, `vSet_overlap_not_comm`, `overlap_not_disj`, `atom_closed`, `var_only0`, `var_only1`, `vt_set_exists`, `head_alloc`, `kind_inst`, `at_inst`, `inst_cons_user`, `ctxBelow_split_left`, `ctxBelow_split_right`, `var_at`, `discard_two`, `discard_one_copy`, `lin_closure_typed`, `lin_closure_runs`, `st_handler_typed`, `st_handler_runs`, `call_it_typed` |
| `Quot.sound` | 2 | `rempty_eq`, `inst_ext` |
| `propext`, `Quot.sound` | 33 | `res_le`, `res_mono`, `split_empty`, `ext_del`, `split3_empty`, `substOK_handle`, `clauseCtx_del`, `ext_del_zero`, `two_del_zero`, `one_del_zero`, `head_next`, `head_fresh`, `head_dead`, `head_release`, `step_fresh`, `step_dead`, `steps_fresh`, `steps_dead`, `count_resume_le_rel`, `steps_alloc_ge`, `count_consume_le_rel`, `relCount_eq`, `mem_inst_of`, `rowSub_inst`, `res_inst`, `inst_two`, `frameOK_inst`, `clauseCtx_inst`, `inst_ty`, `lin_closure_twice_rejected`, `linear_state_across_abort_rejected`, `call_it_at_fail`, `call_it_pure` |
| `propext`, `Classical.choice`, `Quot.sound` | 60 | `splitR_comm`, `splitR_of_empty_left`, `splitR_of_empty_right`, `splitR_assoc`, `splitR_assoc'`, `splitR_swap`, `discardR_split`, `discardR_join`, `rleM_part`, `split4`, `splitR_of_empty`, `val_inv`, `val_ty`, `substOK_split`, `discard_rest`, `subst_ty`, `weakenR`, `splitR_rot`, `vt_at`, `vt_xchg`, `vt_swap2`, `state_rleM`, `plug_inv`, `regroup`, `nat_discard`, `capture_splits`, `split_remove`, `resume_splits`, `head_pres`, `preservation`, `sub_left`, `sub_right`, `canon_nat`, `canon_hdl`, `canon_kont`, `canon_arrow`, `canon_pair`, `progress_gen`, `progress`, `type_safety`, `head_keep`, `release_at_most_once`, `one_shot_resumption`, `steps_keep`, `alloc_live`, `terminal_no_linear`, `total_linear_exhaustion`, `dead_no_release`, `live_hdl_never_resumed`, `live_kont_never_consumed`, `minted_never_resumed`, `captured_never_consumed`, `total_linear_exhaustion_per_kind`, `row_poly_safety`, `ctxBelow_ext`, `typed_below`, `captured_kont_typed`, `captured_kont_self_contained`, `lin_closure_exhausts`, `st_handler_exhausts` |

## Checks

Lean 4.24.0 (x86_64 Linux release, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`), from the GitHub release zip
because elan downloads are blocked in the cloud:

```sh
cd lean/Metatheory && lean Closures.lean    # exit 0, no warnings, 194 axiom lines, ~12 s
```

Rust, with `CARGO_BUILD_JOBS=3 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`
and a private target directory:

```sh
cargo fmt --all -- --check
VESPER_LEAN=<lean 4.24.0> cargo test --locked --test metatheory_closures
cargo +1.100.0 clippy --locked --all-targets -- -D warnings   # root package (the only package changed)
```

The results are recorded in the PR description.

## Decisions

* **A new file instead of growing `MetatheoryFull.lean`.** The extension
  changes the store, the handler rule, the continuation type and the effect
  signature, so every proof is touched. Keeping the wave-six model intact
  keeps its 134 audited theorems and its checker fixture test stable; the new
  model lives under `lean/Metatheory/` (this lane's path).
* **Closure conversion in the calculus.** A closure carries its environment
  as one atom and its body is typed in a context of exactly (environment,
  argument). Values stay closed, so substitution never enters closure or
  continuation bodies and no shifting is needed. This is the [T-Abs] premise
  read literally: the captured Γ is the environment, each of its bindings at
  most `m_fn`. Several captures are a struct, whose kind is the join of its
  fields'.
* **The handler state is part of the body's frame: `m_s ⊑ res(ε)`.** [T-Handle]
  in §6 has no premise relating `m_s` to the clause row. Without one, Theorem 2
  fails: in `handle l state s = mint() in abort with ...` the abort (or an
  outer affine handler dropping the captured continuation) discards the live
  handle. The model therefore adds `m_s ⊑ res(ε_c)`, the Resumption Frame
  Condition applied to the state binding; `linear_state_across_abort_rejected`
  shows the case it rejects. This looks like a gap in the spec's rule worth
  raising upstream.
* **Clauses have no resources and a copy-only Γ_h**, exactly as in §6; linear
  resources reach clauses only through the state.
* **Row variables as abstract labels plus an instantiation theorem**, rather
  than first-class `∀ρ` types. A row-polymorphic definition is typed once with
  ρ free in Δ and used at any instantiation (`inst_ty`), which is how
  generic effectful functions are checked and used. Counting ρ as affine in
  `res` is what makes this sound: an unknown row may abort.
* **A kinded store** (`Ent.hdl` / `Ent.kont q`) so the per-kind statement is a
  property of runs, checked dynamically by the semantics and shown never to fail
  for well-typed programs, instead of a syntactic side argument.
* **Clause chains** are `ccons`/`cnil` constructors of `Expr` rather than a
  nested `List Expr`, which keeps `Expr`, `subst` and the inductions
  structural. The typing rule checks clauses through `nthC`.

## Remaining gaps compared with §4-§6

* **Zero-Pin corollary: only its syntactic core.** The corollary is about
  machine stacks (no first-class pointer into a suspended stack, so it can be
  moved without `Pin`). This calculus has no addresses, no stack frames and
  no second-class `shr`/`mut` borrows in Γ, so the statement has nothing to
  quantify over and is not stated. What is proved is the part the calculus can
  express: every continuation a well-typed run captures has a body whose only
  free variables are its two parameters (`captured_kont_self_contained`), so it
  refers to the rest of the program only through location-independent store
  ids. A real statement needs a stack/relocation model with borrows.
* **No recursion** (`fix`) and no generic type variables (Δ's `α : mod m`,
  [K-Var]); row variables are the only part of Δ modelled.
* **No first-class `∀ρ` types**: a row-polymorphic value is instantiated once,
  for the whole program, not separately at each use site of the same value.
* **Lexical escape prevention** is not modelled: effect labels are global
  signature indices, so a closure capturing a handler capability label `l`
  cannot be expressed or rejected.
* **Places are still functional** (`get`/`swap`/`swap2` on values); there are
  no mutable locals and no Place Capability Graph. Closures therefore cannot
  capture borrows, so the "no shr/mut borrows in Γ" half of [T-Abs] holds
  trivially.
* **Aborts are untrappable**, as in wave six.
* **No link to the Rust compiler or checker.** Nothing here proves that
  `vesper check` implements these rules, and Rust ownership in the host
  libraries is not evidence for these source typing rules.
