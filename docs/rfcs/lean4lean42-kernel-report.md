# Lean4Lean 4.24 kernel slice (#298)

Implemented under the published kernel contract `262b5e9`, before writing. The
coherent source/control checkpoint is `1364376`. This lane owns the two patches,
fixed controls, `known-fixes.json` and this report only. No Vesper compiler,
Cargo, model, existing proof-audit, upstream Main or replay source was changed.
The coordinator's reference checkout and port6 experiment remain unchanged.

## Delivered behavior

`kernel.patch` changes only the eight permitted upstream implementation files.
It retains the three declaration-value closure guards, the private 4.24
`mainModule?` access adaptation, the Church-encoded primitive reflection, and
corrected level ordering. It adds the coupled nested-parameter repair (open
parameter context, abstraction during restoration, checking discarded
applications against the resulting environment), original type/constructor
closure checks, reserved nested-auxiliary checks, normalized-Prop recognition
and projection/eta restrictions, corresponding recursor/K classification,
mutual-header/name guards, and the four quotient-name checks.

Primitive admission now checks its type and closed body before defeq recognition,
without adding the candidate to the environment first. Reserved primitive-name
permission is decided after recognition; duplicate names remain checked before
body admission. Non-safe primitive definitions are not recognized. Reflection
proposition, boolean and evidence arguments are typechecked together. This is a
small adaptation of the later recognizer's preconditions, not an unreviewed
transplant of its full later recursion implementation.

The custom canonical level implementation is ported through exact upstream
`4172c0fadafa54a8cb39787843f262883bb20e5b`, including the earlier ordering fix and
its supporting normalization changes. The older `isEquivList` behavior using
`isEquiv'` is retained. No replacement with newer Std/native level operations
was introduced. The obsolete free-variable filter on primitive numeric
reduction is removed using the exact `d41b637` compatibility correction;
arguments still pass through actual weak-head reduction.

`toolchain.patch` changes only the three permitted metadata files, pinning Lean
4.24.0 and Batteries commit `8da40b72fece29b7d3fe3d768bac4c8910ce9bee` consistently
in both dependency metadata files. It adds no Main library target. All selected
full upstream SHAs, changed files, dispositions and remaining obligations are
recorded in `known-fixes.json`.

## Fixed runtime controls

Setup copies `controls/VesperControls.lean` to upstream
`Lean4Lean/VesperControls.lean`, under its existing library prefix. It exposes:

```lean
Vesper.Lean4Lean42Controls.ControlResult
Vesper.Lean4Lean42Controls.runControls
  (baseline : Lean.Kernel.Environment) : IO (Array ControlResult)
```

The result fields are `id`, `expectedAccept`, `actualAccept`, `expectedReason`,
`actualReason` and `matched`. Reasons are bounded stable classifications, not
unbounded prettyprinted exceptions. Actual error classification is independent
of a case's expected reason. The production adapter must independently compare
expected fields to the pinned inventory and recompute `matched`; returned
expected fields are not authority. `suite.complete` means the suite reached its
end, while individual results still determine success.

`controls/Check.lean` loads only the fixed compiled umbrella with `importAll`,
then invokes this same runtime suite. It does not maintain a parallel elaborator
checker. Invalid declarations and expressions are supplied directly to actual
`Lean4Lean.addDecl`, `TypeChecker` or primitive-check functions, so native
elaboration refusal cannot satisfy a negative control. The control fixtures use
genuine imported constants; primitive candidates are removed from that baseline
before admission, and unverified candidates are never preinserted. Quotient
collisions use temporary raw axiom-declaration *data* to occupy names; the
control module introduces no logical axiom declarations.

Actual final result: **89 controls passed**, including 34 intended refusals.
The inventory distinguishes 52 raw-declaration controls, 15 checker-expression
controls and 22 condition/invariant controls; those are not 89 independently
proved theorems. Coverage includes:

- Cache-primed FVar rejection for theorem/safe-definition/opaque values, explicit
  MVars and all three closed positives.
- Valid nested parameters and restored constructor/recursor types; discarded
  ill-typed arguments and double projections; original type/constructor closure;
  the actually generated reserved auxiliary and projection-name scan.
- Normalized-Prop admission, recursor universes/K flag, rejected Boolean
  projection from a proof, proof eta restriction, and valid data eta/projection.
- Unequal mutual universe headers, duplicate overwrite, and valid distinct
  blocks with empty and shared nonempty universe lists.
- Candidate-checked Eq bootstrap, each of four quotient collisions, clean
  initialization and idempotent initialization.
- Genuine Nat.add/mod/div/gcd/bitwise admission, changed arithmetic equations,
  partial primitive refusal, malformed type/body and reflection evidence.
- Valid/undersized datatype universes, the reachable `imax 2 v <= max 2 v`
  datatype, canonical scaffolding/drained-key examples and bounded independent
  level interpretation. The deterministic grid checks 852 levels of size≤5
  over three parameters at 512 valuations each, comparing original/normalized
  values and canonical representatives. A second grid compares all 44 size≤3
  levels pairwise at 125 valuations. This is bounded evidence, not a global
  normalizer correctness/completeness proof.
- Erased-local arithmetic reduction, shallow typing, and deep normalization
  refusal under the existing fixed Methods recursion budget.

Two exploratory expectations were corrected before the final run. A deeply
nested Nat expression can typecheck without normalizing, so its accepted
`checkType` result was not a fuel-bound defect. The final refusal control uses
actual `whnf` depth. The old numeric-reduction FVar filter left erased-local
arithmetic constructor-shaped; the narrow upstream compatibility correction now
permits the direct numeric reduction. Neither observation is described as a
proof-of-False exploit.

## Actual commands and identities

The exclusive experiment was `/tmp/vesper-lean4lean42-kernel-worker`, based at
`4faf51403bc4cb35cf6666d91fc9c938ae46d3aa`. Existing exact-pinned Batteries source
and generated dependency artifacts were copied into this private tree; no new
dependency/toolchain was installed. Actual package HEAD was verified as
`8da40b72fece29b7d3fe3d768bac4c8910ce9bee`. Actual compiler output was Lean4.24.0,
x86_64 Linux, commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`.

```sh
# In the exclusive upstream experiment:
/home/agent/.local/share/vesper-lean/bin/lake build Lean4Lean
/home/agent/.local/share/vesper-lean/bin/lake build Lean4Lean.VesperControls
/home/agent/.local/share/vesper-lean/bin/lake env lean --run /workspace/vesper-worktrees/lean4lean42-kernel/tools/lean4lean42/controls/Check.lean

# Read-only apply checks against the clean reference base:
git apply --check /workspace/vesper-worktrees/lean4lean42-kernel/tools/lean4lean42/patches/kernel.patch
git apply --check /workspace/vesper-worktrees/lean4lean42-kernel/tools/lean4lean42/patches/toolchain.patch

# In the owned Vesper worktree:
git diff --cached --check
```

The focused runtime-library build passed (28 jobs). The final umbrella build
passed, and the exact final source's IO control wrapper exited0 with
`CONTROLS PASSED 89`. Retained logs are `/tmp/lean4lean42-kernel-build.log`,
`/tmp/lean4lean42-control-build.log` and `/tmp/lean4lean42-runtime-controls.log`.
The final two logs' hashes and commands are recorded in the manifest.
Both standard `git apply --check` commands passed, and the reference checkout
remained clean. Blank unified-diff context lines are represented without a
trailing space; Git accepts these patches and the owned staged diff check passes.

Source SHA256 identities at checkpoint:

| Input | SHA256 |
|---|---|
| kernel.patch | `f244667d85e91656feb91ab0e4d36238efdad0aaebbf901a5e2ed342336cc388` |
| toolchain.patch | `720457143a4b66be681fce1b0a1386188ca0be1aa04a185b9150dc6022691f50` |
| controls/VesperControls.lean | `3a197b3fa68e105a9b5fc20e2776aac0b37baec0c7ad25d0f443f6235cf3b05d` |

`known-fixes.json` is review evidence and a fixed expected-control inventory,
not an executable setup receipt or proof of a later invocation's success.

## Remaining integration and proof limits

The final assembled machine replay, stored-constructor/recursor metadata-mutation
rejections and both complete fresh model closures remain coordinator/separate
machine-lane checks. This lane did not invoke the unchanged historical Main or
run expensive full replays. Port6's old successes do not stand in for validating
this changed patch. No Cargo build, platform matrix, native model execution,
whole-process memory bound or compiler/refinement proof was performed.

The old upstream Theory/Verify proof sources are untouched, unported and unbuilt;
the new normalizer's internal shape can require proof-file adaptation. Runtime
build success and controls do not establish those proofs compile or close their
existing sorry/project-axiom obligations. The manifest explicitly retains these
limits, distinguishes unapplied native-level/domain-metadata/phase changes, and
keeps restoration invariants separate from any full verified-eliminator claim.
There is no new complete self-verification or §42-completion claim.
