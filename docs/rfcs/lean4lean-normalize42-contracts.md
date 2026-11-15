# Actual recursive level construction (#424)

Frozen parent #423 is `7e426817ab783b034847f6c4bd9c14de639dd117`.
Claims #425–#428 precede source. All actual helper/vendor/provenance and
parent proof, auditor/projector/profile, runtime/compiler, Cargo and workflow
sources stay unchanged. The narrow #429 BUILD setting selects the existing
regular-input launcher for these tests; validator bytes remain unchanged.
This packet concerns actual `Normalize.normalizeAux`
before the later `.subsumption` call.

Ownership under `lean/Lean4LeanLevel42/`: #425 NEW `LevelSemantics.lean`;
#426 NEW `NormalizeInvariants.lean`; #427 NEW `NormalizeValue.lean`.
Each owns its NEW `docs/rfcs/lean4lean-normalize42-{semantics,invariants,value}.md`.
#428 owns NEW `tests/lean4lean_normalize42.rs`, acceptance report and fifth
static Packet arm in `tests/common/lean4lean_proof_packet.rs` only.
Coordinator owns this contract, NEW `lean/Lean4LeanNormalize42.lean`,
integrated evidence and final all-five execution.

Isolated worktrees, immutable versioned dependency/artifact roots with exact
source/compiled-parent correspondence, exclusive Cargo and at most two active
Lean compiler leases. #425 starts lease1; #426 lease2 once dependencies ready.
#427 may draft but needs explicit coordinator grant after a lease is freed.
No Cargo outside coordinator. Public ASCII `theorem` commands for all named
proof roots; no sorry/admit/project axiom/unsafe/partial/native_decide. Fixed
handwritten Lean allowed; any generated Lean uses vesper-lean-syntax AST and
one render, including audit scripts.

## Structural domain and algebra (#425)

Namespace `Vesper.Lean4LeanLevel42`, import frozen Semantics and actual Lean.
Define over ACTUAL Lean.Level:

```lean
def levelValue (ρ : Valuation) : Level → Nat
  | .zero => 0
  | .succ u => levelValue ρ u + 1
  | .max u v => max (levelValue ρ u) (levelValue ρ v)
  | .imax u v => Nat.imax (levelValue ρ u) (levelValue ρ v)
  | .param v => ρ v
  | .mvar _ => 0

def NoMVars : Level → Prop
  | .zero | .param _ => True
  | .mvar _ => False
  | .succ u => NoMVars u
  | .max u v | .imax u v => NoMVars u ∧ NoMVars v

def levelSize : Level → Nat
  | .zero | .param _ | .mvar _ => 1
  | .succ u => levelSize u + 1
  | .max u v | .imax u v => levelSize u + levelSize v + 1
```

The mvar interpretation merely totalizes levelValue. Correctness excludes
metavariables structurally. Do not use cached `Level.hasMVar` as the domain,
read opaque mkData properties, or import old Verify/Axioms bridges.
levelSize is only a proof measure, independent of names and cached data.

Required public APIs:

- Six actual evaluator equations `levelValue_zero`, `levelValue_succ`,
  `levelValue_max`, `levelValue_imax`, `levelValue_param`, `levelValue_mvar`.
- `levelSize_pos (l) : 0 < levelSize l`.
- `levelSize_imax_max_left`: size (.imax u v) < size (.imax u (.max v w)).
- `levelSize_imax_max_right`: size (.imax u w) < the same right size.
- `levelSize_imax_imax_left`: size (.imax u w) < size (.imax u (.imax v w)).
- `levelSize_imax_imax_right`: size (.imax v w) < the same right size.
- `nat_imax_zero`: Nat.imax a 0 = 0.
- `nat_imax_succ`: Nat.imax a (b+1) = max a (b+1).
- `nat_imax_max`: Nat.imax a (max b c) = max (Nat.imax a b) (Nat.imax a c).
- `nat_imax_imax`: Nat.imax a (Nat.imax b c) =
  max (Nat.imax a c) (Nat.imax b c).
- `max_add_offset`: max a b + k = max (a+k) (b+k).
- `nat_imax_add_offset`: Nat.imax a b + k =
  max k (if 0 < b then max (a+k) (b+k) else 0).

Additional genuine public helpers allowed. Publish exact signatures early.
Do not change actual normalization or copy a replacement algorithm.

## Actual recursive invariants (#426)

Import LevelSemantics, frozen PathPriming and required helpers. Required:

```lean
theorem normalizeAux_preserves_pathPrimed
    (l : Level) (p q : List Name) (k : Nat) (acc : NormLevel)
    (h : PathPrimed q acc) :
    PathPrimed q (Normalize.normalizeAux l p k acc)

theorem normalizeAux_preserves_WF
    (l : Level) (p : List Name) (k : Nat) (acc : NormLevel)
    (h : MapWF acc) : MapWF (Normalize.normalizeAux l p k acc)

theorem normalizeAux_empty_WF (l : Level) :
    MapWF (Normalize.normalizeAux l [] 0 {})
```

Preserve ARBITRARY q, including when normalizing at a different path p.
Both preservation facts hold for ALL levels: no NoMVars, current-path
priming, sortedness or additional WF premise on the first theorem.
WF insertion uses actual orderedInsert some/none membership lemmas.

Strong induction/recursive proof on levelSize must handle fresh imax
expressions, not merely structural subterms. The actual well-founded
definition needs its equation theorem to unfold; do not rely on rfl for
arbitrary normalizeAux equations. No assumption of an invariant result.

## Exact actual construction value (#427)

Import NormalizeInvariants, LevelSemantics and frozen map/path proofs.
Required main API:

```lean
theorem normalizeAux_value
    (ρ : Valuation) (l : Level) (p : List Name)
    (k : Nat) (acc : NormLevel)
    (hl : NoMVars l) (hp : PathPrimed p acc) :
    mapValue ρ (Normalize.normalizeAux l p k acc) =
      max (mapValue ρ acc)
        (if enabled ρ p then levelValue ρ l + k else 0)

theorem normalizeAux_empty_value (ρ : Valuation) (l : Level)
    (hl : NoMVars l) :
    mapValue ρ (Normalize.normalizeAux l [] 0 {}) = levelValue ρ l
```

Arbitrary valuation/map/offset, with ONLY the stated domain and priming
premises. No sorting, MapWF, semantic preservation flag or desired-conclusion
premise. Sequential calls need arbitrary-path priming preservation. Parameter
some needs addConst priming then actual path extension and enabled equality;
none needs membership/nonempty/current-row presence for positive-offset
modify, and existing variable coverage for offset zero.

Required public witness roots (agree names with #428): both fresh imax
rewrite cases; repeated/member variable at zero and positive offset;
disabled activation; arbitrary existing-map retention; genuinely primed
duplicate/unsorted current key; and the necessary metavariable-domain
counterexample `.succ (.mvar m)` at empty path/map/zero offset (actual
construction value zero but totalized evaluator one). Witnesses must refer
to actual normalizeAux; use the general proved theorem where useful.
No cached-domain equivalence claim or full Normalize.normalize result.

## Fixed exact acceptance (#428)

Minimal fixed closure in dependency order: actual `Lean4Lean.List`,
`Lean4Lean.Level`, then `Lean4LeanLevel42.Semantics`, `ListProofs`,
`OrderedInsert`, `VarInsert`, `NameOrder`, `MapValue`, `MapHelpers`,
`PathPriming`, `LevelSemantics`, `NormalizeInvariants`, `NormalizeValue`,
and aggregate `Lean4LeanNormalize42`. Full names for all helper modules use
the `Lean4LeanLevel42.` prefix. SortedKeys/Subsumption remain outside this
new imported closure, with their existing packet specs unchanged.

Closed `Packet::Normalize`, label `lean4lean_normalize42`, module
`Lean4LeanNormalize42`, projection `Lean4LeanNormalize42Kernel`, scope
`audited_pre_subsumption_normalization_proofs`, marker
`LEAN4LEAN_NORMALIZE42_EVIDENCE`. Required fixed guards cover all contracted
APIs and actual chosen witnesses. Unchanged source-command extraction audits
EVERY actual authored public theorem in the closure plus ALL actual aliases,
including helpers absent from the guard catalog; existing 128-root batching
continues automatically.

All FOUR old required arrays/specifications and complete POLICY-through-EOF
validator/control/policy/budget bytes stay fixed. Preserve actual source/
provenance, original/projection, occurrence/partition/alias, standard-three
axiom, bounded capture and every real missing/corrupt/changed-source/
forbidden-alias control. Same finite 300-second per-matrix budget; no guessed
cap/scheduling increase. No arbitrary-module/expected-answer/weaker-policy
input. Run ALL FIVE real matrices after extension, targeted strict Clippy,
format/Bazel/whitespace and source associations. No implementation-proof
audit-status or compiler/native equivalence claim follows from this packet.
