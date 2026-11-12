# Appendix A metatheory compilation and axiom audit (#54)

Implemented [issue #54](https://github.com/robot-head/vesper/issues/54) in the new
`continuation37/metatheory` worktree from `d121e03`. Only the new
`lean/MetatheoryAppendix.lean` and this report are changed. Existing Lean models,
compiler sources, root wiring and the Google Doc are read-only.

## Exact source and preserved model

The input is the Appendix A Lean extraction at
`/tmp/VesperMetatheory-41.lean`, SHA-256
`9db4f6b8e378cbb75d729f37e624b1913ad6538107cc1c5f36a0298efea4c847`,
from the refreshed 41-section design in `docs/specification.txt`. The source
metadata records revision
`AHj4eMRg35hVwN07Ww4ah8QktN80KS_xgnyZypmazCIeUl-0B2N9N-ucp4P_nKYdwQMnGLpzGw3DjIaQE3liPaE18kxN9vW5Y-9PERTVdhw`,
modified `2026-11-12T09:18:30.204Z`, and complete snapshot SHA-256
`8e0f05ae32a6c87a8ce25fb339f66c760fe5d5f3fd3cb05e58236d291fa62207`.
The original failed-compilation diagnostics were supplied in
`/tmp/vesper-spec41-appendix-lean.log`.

The implementation preserves all 42 original data declarations and all 22
original theorem statement headers. An independent source-text comparison
checked these declarations/headers byte-for-byte after stripping surrounding
whitespace. Original definitions, constructors, premises and conclusions were
not weakened. In particular, `FieldModalityWf` remains exactly
`Mode.Le f.compKind f.overrideMode`, the user-approved `k_i ≤ m_i` premise.
The file's opening comment replaces the source's broad compiler-mechanization
claim with the actual boundary below.

## Repaired Lean 4.24 elaboration/proofs

- `List.mem_cons_self` is a proof term, not a two-argument function on this Lean
  version; removed the invalid applications.
- Induction tails inferred as `List ResumptionMode` cannot resolve the abbreviated
  `EffectRow.res` through `.res` field notation; named that function explicitly.
- Completed the PCG join proof's live/consumed and consumed/live mode cases.
- Eliminated outcome equality with `cases`, preserving the dependent value
  correspondence in the variable case.
- Split conjunction goals and rewrote the empty-resource environment before
  simplification, avoiding premature unfolding that hid the rewrite target.
- Made the empty-frame mode-split equality precise and supplied the intermediate
  evaluation environment explicitly in the dependent let induction.
- Specified `env := none` when applying the general conservation theorem to the
  closed exhaustion theorem. Removed an unused simp argument without suppressing
  any linter.

These are proof/elaboration repairs. There are no admissions, added axioms,
opaque trusted executors, `native_decide`, `bv_decide` or external solver trust.
`decide`, `simp`, case analysis and `omega` construct terms checked by Lean's
kernel. No additional library dependencies or toolchain changes were needed.

## Non-vacuous applicability examples

Seven additional named theorems check concrete uses of the unchanged rules:

- `mint_consume_has_type` derives a closed let-bound mint followed by consumption
  of its variable, yielding copy mode with two linear effects.
- `mint_consume_evaluates n` derives the actual evaluation: mint `[n]`, consume
  `[n]`, return unit, and advance the counter to `n + 1`.
- `mint_consume_exhaustion n` instantiates the general closed exhaustion result
  using those concrete typing and evaluation derivations.
- `mint_then_leak_evaluates` and `mint_then_abort_evaluates` exhibit operational
  derivations that mint `[n]` and consume nothing.
- The matching `mint_then_leak_rejected` and `mint_then_abort_rejected` theorems
  prove that those programs have no closed copy typing derivation for any
  effect row. They do not pretend that untyped evaluation itself forbids leaks.

## Actual proof command and axiom footprints

Run from the repository root with its pinned `lean-toolchain`:

```sh
/home/agent/.elan/bin/lean --version
/home/agent/.elan/bin/lean lean/MetatheoryAppendix.lean
```

Actual result: Lean **4.24.0**, x86_64 Linux release, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`; the file exits zero with no warnings or
errors. Every one of its 29 named theorems has a corresponding `#print axioms`
command, so the normal compilation prints the entire audit.

| Actual footprint | Count | Theorems (names relative to `Vesper.Metatheory`) |
| --- | ---: | --- |
| Empty | 13 | `Mode.le_refl`, `Mode.le_trans`, `Mode.le_join_left`, `Mode.meet_le_left`, `Mode.meet_le_right`, `Mode.linear_not_discardable`, `Mode.discardable_iff_le_affine`, `foldl_join_ge_init`, `semver_narrowing_preserves_disjoint`, `mint_consume_has_type`, `mint_consume_evaluates`, `mint_then_leak_evaluates`, `mint_then_abort_evaluates` |
| `propext` | 12 | `Mode.le_antisymm`, `wf_field_meet_eq_comp`, `adt_kind_never_relaxes_linear_component`, `res_le_affine_of_mem_affine`, `abort_soundness_preserves_linear`, `uncancellable_io_allows_live_linear`, `unconsumed_txn_across_question_rejected`, `call_proj_axm_exclusivity`, `pcg_join_preserves_linear_agreement`, `discardable_env_has_no_linear_resources`, `mint_then_leak_rejected`, `mint_then_abort_rejected` |
| `propext`, `Quot.sound` | 4 | `Mode.mode_submode_allows_count`, `vesper_linear_conservation_soundness`, `vesper_total_linear_exhaustion`, `mint_consume_exhaustion` |

The only nonempty dependencies are Lean's existing propositional extensionality
and quotient soundness axioms. The audit contains no `sorryAx`, `Classical.choice`
or newly declared axiom. “No admissions” does not mean every proof has an empty
axiom footprint.

## Exact scope and remaining work

The environment contains **one optional binding**, not a map of source variables.
Expressions are variable, natural literal, mint, consume, abort and sequencing;
there are no source functions, stores, allocation regions, CFGs, fibers or
compiler transformations. `HasType.tLet` explicitly requires `gamma2 = none`:
its residual-frame premise is therefore vacuous for actual let derivations.
The separate `FrameSafe` lemmas describe resumption restrictions, but do not
prove a production continuation/stack implementation.

`vesper_linear_conservation_soundness` assumes both a typing derivation and a
completed evaluation derivation. It establishes a mode condition on successful
results and conservation of **list lengths**: initial-resource count plus minted
count equals consumed count plus returned count. The closed copy theorem derives
only equality of minted and consumed counts. It does not establish identity or
multiset equality, uniqueness/freshness of resource IDs, progress, termination,
absence of stuck terms, or production compiler correctness. `ValTyped` at linear
mode is deliberately `True`; e.g. a linear-mode value need not be a handle, so
consumption requires an applicable evaluation derivation rather than a proved
progress theorem.

Projection lemmas are semantic set implications over the stated selectors; PCG
agreement assumes both input states exclude poisoned recovery. These are not
complete checker/CFG correctness proofs. Extending the calculus to multiple live
bindings and nonempty residual frames, then proving identity-sensitive ownership,
progress/preservation for richer values and a connection to checked compiler IR,
remains separate formalization work. The repaired Appendix is an executable,
audited small model with its original statements intact.
