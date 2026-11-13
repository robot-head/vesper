# Frozen kernel-port slice for external model replay (#297)

This contract authorizes only the kernel-port and raw-declaration-control
slice after its child issue is created and this revision is pushed. The
machine protocol, Rust runner and setup receipt remain proposed in the
[contract plan](lean4lean42-contract-plan.md); their writers must wait for
their own frozen contract and claims. No complete self-verification claim is
authorized by this component.

## Ownership and immutable inputs

One writer owns new `tools/lean4lean42/patches/kernel.patch`,
`tools/lean4lean42/patches/toolchain.patch`,
`tools/lean4lean42/controls/**`,
`tools/lean4lean42/known-fixes.json`, and
`docs/rfcs/lean4lean42-kernel-report.md`.

The upstream base is `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa`, Lean is
`leanprover/lean4:v4.24.0`, and Batteries is
`8da40b72fece29b7d3fe3d768bac4c8910ce9bee`. Both dependency manifests must pin
that exact commit. Preserve upstream licensing and identify every selected
upstream correction and adaptation.

The kernel patch may modify only these upstream implementation files:

- `Lean4Lean/Environment/Basic.lean` and `Lean4Lean/Environment.lean`;
- `Lean4Lean/Level.lean`, `Lean4Lean/TypeChecker.lean`, `Lean4Lean/Primitive.lean`;
- `Lean4Lean/Inductive/Add.lean`, `Lean4Lean/Inductive/Reduce.lean`;
- `Lean4Lean/Quot.lean`.

The toolchain patch owns upstream `lean-toolchain`, `lakefile.toml` and
`lake-manifest.json`. It may add a `Main` Lean-library target for the separate
machine entrypoint writer, preserving the existing executable target. No
other dependency or toolchain change is allowed by this slice.

Preserve the 4.24-compatible public checker entrypoint signatures. Add local
helpers when needed rather than importing newer fuel/API conventions. Do not
edit upstream `Main.lean` or add a replay implementation: a separate writer
owns that slice. Do not edit Vesper Cargo, main/lib/native/checker, existing
proof audit, arithmetic oracle, mathematical models, metatheory or external
lane files.

Use a new isolated upstream worktree for experiments. The original reference
and coordinator port6 snapshots remain unchanged. The writer may compile the
runtime library and run the focused controls in that isolated tree. Coordinate
the expensive complete model replays with the root's build lease.

## Required corrections and controls

The [proof-plan disposition table](lean4lean42-proof-plan.md#disposition-of-later-fixes-against-the-actual-424-prototype)
provides the detailed evidence. Apply narrowly reviewed, 4.24-compatible
semantics rather than entire newer-toolchain commits.

1. Retain all three pre-inference declaration-value closure checks from
   `779c51fdea49c57c60f8039703605faae126b17f`. The actual cache-primed theorem,
   safe-definition and opaque controls must reject for the closure check,
   while their three closed counterparts pass. Add explicit MVar coverage.
2. Retain the private `mainModule?` compatibility access, exact primitive
   reflection correction `d7a52a83bea3e93968672b89ba88af4372b08f23`, and level
   ordering correction `bc9a736be9c318209992e91ef6a218864b817dcf`.
3. Port the coupled dropped-nested-parameter repair
   `d7e70a5f09d514db2a2b3a052f3a9d2dde153da6`. Preserve valid nested declarations;
   reject the actual ill-typed discarded argument and invalid double projection.
4. Check original inductive and constructor types for MVars/FVars before
   nested elimination, and reject constructor references/projections naming an
   actually generated reserved `_nested` auxiliary. Use the reachable controls
   identified in `5518bf83860bab48e8d53f1f447cdd6c50c30c3f`, with valid counterparts.
5. Port the coordinated normalized-Prop, projection/eta and recursor-level
   corrections from `997e2985e513793c3dcc2e5ffead983f6b7d1cba` and `5518bf8`.
   Distinguish `isAlwaysZero` from `isNeverZero`. Accept the normalized-Prop
   declaration, inspect its elimination universes, and reject the Boolean
   projection leak from a proof. Preserve ordinary valid data projections.
6. Enforce equal universe-parameter lists and unique names in a mutual block
   before insertion. Reject both unequal-header and actual overwrite examples;
   accept a distinct, equally parameterized block.
7. Check all four quotient names before first initialization. Exercise each
   collision, a clean initialization and the idempotent already-initialized call.
8. Retain logical primitive safety using
   `cbb70bc3500060f22098c838f92e56bd363834e6`. The real partial Nat.add clone must
   not be admitted as a primitive; the genuine safe definition must pass.
   Review the body-typing/recognition ordering obligation from `80d3dbef8b8130e49cedbdc4a22fdf0217222ee3`.
   Do not run defeq recognition on an untyped candidate body or temporarily
   trust an unverified definition to make checking succeed. Test malformed
   primitive types/bodies/evidence and changed arithmetic equations, retaining
   the genuine definitions as positives.
9. Correct the reachable level incompleteness described by
   `4172c0fadafa54a8cb39787843f262883bb20e5b`, without replacing the implementation
   with newer Std/native level APIs. Include the valid `imax 2 v <= max 2 v`
   datatype, the documented canonicity example, and bounded independent level
   interpretation controls. Retain the valid/undersized-universe controls.

Keep positivity, universe, primitive equation, nested-parameter and header
checks. Do not change declaration safety flags to bypass a failure. Do not
add `sorry`, axioms, native decision escapes or proof-state resets that hide
an unvalidated declaration. A compile or fresh-replay failure is evidence to
diagnose, not permission for a weaker successful path.

Later reduction, projection/restoration and primitive-recognition differences
must receive the explicit dispositions described by the proof plan. An
unrelated missing constant, parser rejection or elaborator failure cannot
discharge a raw-declaration control. Any reachable successful default/assertion
fallback must become a checked failure or be repaired with a discriminating
control. The untouched incomplete upstream implementation proofs remain
separate from these runtime corrections.

## Evidence and handoff

Controls are fixed handwritten Lean sources calling the actual candidate
checker on actual `Declaration`/`Expr` values. Do not substitute Boolean
models, canned checker outputs or a native elaboration refusal. Assert the
intended rejection class/reason and retain closed positives. All dynamically
generated Lean, if later needed, uses `vesper-lean-syntax` once; no source
template interpolation is introduced here.

`known-fixes.json` is a review/evidence manifest, not a runtime success
receipt. Use schema1, full source commit IDs and stable case IDs. For every
reviewed correction record its disposition, exact changed files, fixed control
source, intended positive/negative result, actual commands/exits/log identities
when executed, and any remaining obligation. Mark unexecuted cases explicitly.
Do not invent successful results to complete the table.

The report separates authored code, source review and actual execution. After
integration, the coordinator must run the focused controls and both complete
fresh model closures on the assembled final patch. The old port6 successes
remain evidence only for that old snapshot. Final patch identities and the
production release decision follow the reviewed, exercised candidate; this
slice alone cannot produce a completed external-audit report.
