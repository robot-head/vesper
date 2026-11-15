# Audited Lean4Lean foundations (#402)

This is an independent proof packet. It changes no existing kernel, external
profile, resource model, metatheory, auditor/projector, runtime, Cargo or CI
source. Parent #402 preceded all edits. Source work starts after the actual
helper-package pilot and this pushed contract; children #403–#406 own disjoint
files. All new named proofs use public ASCII `theorem` commands, not private,
local or generated-source theorem declarations. No sorry/admit, project axiom,
unsafe/partial definition or native_decide escape is permitted.

## Source and ownership

Root owns `tools/lean4lean-foundations42/upstream/**`, its provenance manifest,
this contract, aggregates `lean/Lean4LeanLevel42.lean` and
`lean/Lean4LeanFoundations42.lean`, integration evidence and issue coordination.
The copied Level/List files must byte-match the actual pinned patched checker
and their existing source-manifest entries. Never modify the sealed checker
tree or pretend these package declarations are trusted toolchain inputs.

- #403: `lean/Lean4LeanLevel42/{Semantics,ListProofs}.lean` and semantics report.
- #404: `lean/Lean4LeanLevel42/Subsumption.lean` and subsumption report.
- #405: `lean/Lean4LeanNames42.lean`, its optional directory and names report.
- #406: `tests/lean4lean_foundations42.rs`, fixed fixtures under
  `tools/lean4lean-foundations42/fixtures/**`, and acceptance report.
- Pilot lane owns only its new pilot report.

Each writer uses an isolated worktree. Cargo has one exclusive coordinator
lease; at most two Lean compiler leases may be active. Publish immutable,
versioned compiled dependency roots and source/object hashes for consumers.

## Level semantic API

Use namespace `Vesper.Lean4LeanLevel42`, importing the actual
`Lean4Lean.Level` and opening `Lean` and `Lean.Level.Normalize`.

```lean
abbrev Valuation := Name → Nat
def enabled (ρ : Valuation) (p : List Name) : Bool :=
  p.all (fun v => decide (0 < ρ v))
def varsValue (ρ : Valuation) (vs : List VarNode) : Nat :=
  vs.foldr (fun v acc => max (ρ v.var + v.offset) acc) 0
def nodeValue (ρ : Valuation) (p : List Name) (n : Node) : Nat :=
  if enabled ρ p then max n.const (varsValue ρ n.var) else 0
def NodeWF (p : List Name) (n : Node) : Prop :=
  ∀ v ∈ n.var, v.var ∈ p
```

Semantics provides `enabled_eq_true_iff`, `enabled_of_subset`,
`varsValue_nil`, `varsValue_cons`, and useful elementary value bounds.
ListProofs provides actual `Name.cmp` equality reflection, `subset_sound`,
`subsumeVars_union` and `offset_bound` with these statements:

- `Normalize.subset Name.cmp p₂ p₁ = true → p₂ ⊆ p₁`.
- `max (varsValue ρ (Normalize.subsumeVars a b)) (varsValue ρ b) =
  max (varsValue ρ a) (varsValue ρ b)`.
- If `vs ≠ []` and `∀ v ∈ vs, 0 < ρ v.var`, then
  `vs.foldl (fun acc v => max acc v.offset) 0 + 1 ≤ varsValue ρ vs`.

State explicit `Name.cmp`, not unqualified `compare`: the implementation's
Ord instance was file-local. Soundness needs equality reflection, not sorted
keys/variables or uniqueness assumptions.

## Actual node helper proofs

Subsumption imports the two fixed semantic modules. For any valuation and
`same : Bool`, with precisely `p₂ ⊆ p₁` and `NodeWF p₂ n₂`, prove:

```text
max (nodeValue ρ p₁ (n₁.subsumeBy same n₂)) (nodeValue ρ p₂ n₂)
  = max (nodeValue ρ p₁ n₁) (nodeValue ρ p₂ n₂)
```

Public required names: `node_subsumeBy_union`, `node_subsume_union`,
`node_subsumeBy_self` and `node_subsume_self`. Prove the stronger self equality
separately: `NodeWF p n → nodeValue ρ p (n.subsumeBy true n) = nodeValue ρ p n`.
No NodeWF assumption on the left node, sorting requirement, or premise that
merely assumes the desired semantic conclusion is needed.

Public computational theorems must show the invalid empty-key/zero-valuation
counterexample, enabled valid variable domination with actual constant removal,
larger-constant domination, disabled larger key and valid self-same removal.
The first is a genuine failure without condition well-formedness, not a claimed
unconditional theorem or a changed implementation.

## Actual Core names

Use namespace `Vesper.Lean4LeanNames42` and `import Lean` only. Do not import
the unsafe Expr utilities or old Theory/Verify assumptions. Define:

- `Reserves g fv := ∀ i, fv = ⟨Name.num g.namePrefix i⟩ → i < g.idx`.
- `Extends g h := g.namePrefix = h.namePrefix ∧ g.idx ≤ h.idx`.
- `advance 0 g = g`; `advance (n+1) g = advance n g.next`.
- `nameAt g i := ⟨(advance i g).curr⟩`;
  `emitted g n := (List.range n).map (nameAt g)`.

Prove the six useful reservation/order leaf facts, prefix/index equations,
advance extension and reservation monotonicity, injective nameAt, non-aliasing
with prior reserved IDs, new-ID reservation, emitted length and Nodup.
Reserves is intentionally vacuous for other prefixes. These are conditional
facts about actual curr/next calls, not proof that caller/cache priming meets
the invariant or a replacement allocator/refinement.

## Genuine acceptance and scope

Freshly compile exact vendored helpers and all new modules, then invoke the
unchanged exact kernel snapshot into a disjoint private root. Preserve every
canonical ConstantInfo and every actual alternate-proof alias; verify original,
projection, occurrence/partition and source/tool associations before/after.
Strictly audit the full nonempty authored theorem catalog plus ALL aliases in
batches of at most 128. Definition roots are deliberately not accepted by
`--theorem`; private numeric names are not public roots. Replay still checks
all canonical declarations.

Exercise real missing/corrupt original and projected objects, forbidden-axiom/
alias controls and source/provenance gates. Reuse existing fixed adversarial
fixtures when sufficient. Dynamic Lean uses the syntax AST once; handwritten
fixed modules/fixtures are allowed. Emit bounded actual evidence before later
refusal controls, and measure finite budgets before changing scheduling.

These lemmas do not complete normalize/map/checker correctness, whole-checker
self-verification, native equivalence or compiler refinement. Existing external
report audit fields remain not_run. The old Verify library's project assumptions
are not accepted as proof obligations discharged by this packet.
