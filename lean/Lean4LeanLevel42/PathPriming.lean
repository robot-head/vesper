import Lean4LeanLevel42.MapHelpers
import Lean4LeanLevel42.OrderedInsert

/-! Syntactic coverage of the current path by actual stored variable names.
Witness keys may be strict subsets of the current path; current-key presence
is an independent requirement. These facts support, but do not complete, a
future induction for the actual normalizer. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

/-- Every current-path variable occurs in a stored row whose key is a subset
of the current path. A nonempty current path also has an actual current row. -/
def PathPrimed (p : List Name) (s : NormLevel) : Prop :=
  (p ≠ [] → ∃ n, s.get? p = some n) ∧
  ∀ v ∈ p, ∃ q n w, s.get? q = some n ∧ q ⊆ p ∧
    w ∈ n.var ∧ w.var = v

theorem pathPrimed_empty (s : NormLevel) : PathPrimed [] s := by
  constructor
  · intro h
    exact False.elim (h rfl)
  · intro v hv
    simp at hv

theorem pathPrimed_variable_bound {p : List Name} {s : NormLevel}
    {ρ : Valuation} {v : Name} (h : PathPrimed p s) (hv : v ∈ p)
    (hp : enabled ρ p = true) : ρ v ≤ mapValue ρ s := by
  rcases h.2 v hv with ⟨q, n, w, hn, hq, hw, he⟩
  have hqe : enabled ρ q = true := enabled_of_subset hq hp
  have hvar := varsValue_mem_bound (ρ := ρ) hw
  have hnode := nodeValue_le_mapValue (ρ := ρ) hn
  rw [nodeValue_of_enabled hqe] at hnode
  rw [he] at hvar
  omega

theorem pathPrimed_one_bound {p : List Name} {s : NormLevel}
    {ρ : Valuation} (h : PathPrimed p s) (hne : p ≠ [])
    (hp : enabled ρ p = true) : 1 ≤ mapValue ρ s := by
  cases p with
  | nil => exact False.elim (hne rfl)
  | cons v ps =>
    have hv : v ∈ v :: ps := by simp
    have hpos := enabled_eq_true_iff.mp hp v hv
    have hb := pathPrimed_variable_bound h hv hp
    omega

/-- Preservation uses actual row and represented-name preservation, including
when the update and witness keys coincide and offsets are merged. -/
theorem pathPrimed_addConst {p : List Name} {s : NormLevel}
    (h : PathPrimed p s) (k : Nat) (q : List Name) :
    PathPrimed p (s.addConst k q) := by
  constructor
  · intro hne
    rcases h.1 hne with ⟨n, hn⟩
    exact map_addConst_preserves_lookup k q hn
  · intro v hv
    rcases h.2 v hv with ⟨r, n, w, hn, hr, hw, he⟩
    rcases map_addConst_preserves_var k q hn hw with ⟨n', w', hn', hw', he'⟩
    exact ⟨r, n', w', hn', hr, hw', he'.trans he⟩

theorem pathPrimed_addNode {p : List Name} {s : NormLevel}
    (h : PathPrimed p s) (v : Name) (k : Nat) (q : List Name) :
    PathPrimed p (s.addNode v k q) := by
  constructor
  · intro hne
    rcases h.1 hne with ⟨n, hn⟩
    exact map_addNode_preserves_lookup v k q hn
  · intro x hx
    rcases h.2 x hx with ⟨r, n, w, hn, hr, hw, he⟩
    rcases map_addNode_preserves_var v k q hn hw with ⟨n', w', hn', hw', he'⟩
    exact ⟨r, n', w', hn', hr, hw', he'.trans he⟩

theorem pathPrimed_addVar {p : List Name} {s : NormLevel}
    (h : PathPrimed p s) (v : Name) (k : Nat) (q : List Name) :
    PathPrimed p (s.addVar v k q) := by
  constructor
  · intro hne
    rcases h.1 hne with ⟨n, hn⟩
    exact map_addVar_preserves_lookup v k q hn
  · intro x hx
    rcases h.2 x hx with ⟨r, n, w, hn, hr, hw, he⟩
    rcases map_addVar_preserves_var v k q hn hw with ⟨n', w', hn', hw', he'⟩
    exact ⟨r, n', w', hn', hr, hw', he'.trans he⟩

/-- Old witnesses remain at subset keys while the actual newly inserted row
supplies a witness for the inserted name and current-key presence. -/
theorem pathPrimed_extend {p p' : List Name} {s : NormLevel}
    {v : Name} {k : Nat} (h : PathPrimed p s)
    (hi : Normalize.orderedInsert Name.cmp v p = some p') :
    PathPrimed p' (s.addNode v k p') := by
  rcases map_addNode_var s v k p' with ⟨n, w, hn, hw, he⟩
  constructor
  · intro _
    exact ⟨n, hn⟩
  · intro x hx
    rcases (orderedInsert_some_mem hi).mp hx with heq | hx
    · subst x
      exact ⟨p', n, w, hn, fun _ ha => ha, hw, he⟩
    · rcases (pathPrimed_addNode h v k p').2 x hx with
        ⟨q, n', w', hn', hq, hw', he'⟩
      exact ⟨q, n', w', hn', fun _ ha => orderedInsert_input_subset hi (hq ha), hw', he'⟩

theorem map_addConst_value_of_primed (ρ : Valuation) (s : NormLevel)
    (k : Nat) (p : List Name) (h : PathPrimed p s) :
    mapValue ρ (s.addConst k p) =
      max (mapValue ρ s) (if enabled ρ p then k else 0) := by
  apply map_addConst_value_of_covered ρ k p s
  intro _ hne hp
  exact pathPrimed_one_bound h hne hp

/-- One present positive edge is enough for the skipped constant one but
cannot cover a different current-path variable. The actual zero-offset
parameter branch returns this insufficient map unchanged. -/
theorem pathPrimed_one_edge_counterexample :
    let u : Name := .num .anonymous 0
    let v : Name := .num .anonymous 1
    let p : List Name := [u, v]
    let s : NormLevel := ({} : NormLevel).insert p { var := [⟨v, 0⟩] }
    let ρ : Valuation := fun x => if x = u then 3 else 1
    s.get? p = some { var := [⟨v, 0⟩] } ∧
      enabled ρ p = true ∧ mapValue ρ s = 1 ∧
      1 ≤ mapValue ρ s ∧ mapValue ρ s < ρ u ∧
      s.addConst 1 p = s ∧ Normalize.normalizeAux (.param u) p 0 s = s ∧
      ¬PathPrimed p s := by
  dsimp only
  have he : enabled (fun x : Name => if x = .num .anonymous 0 then 3 else 1)
      [.num .anonymous 0, .num .anonymous 1] = true := by decide
  have hm : mapValue (fun x : Name => if x = .num .anonymous 0 then 3 else 1)
      (({} : NormLevel).insert [.num .anonymous 0, .num .anonymous 1]
        { var := [⟨.num .anonymous 1, 0⟩] }) = 1 := by rfl
  refine ⟨?_, he, hm, ?_, ?_, rfl, ?_, ?_⟩
  · rfl
  · omega
  · simpa only [hm] using (show 1 < (if (.num .anonymous 0 : Name) =
      .num .anonymous 0 then 3 else 1) by decide)
  · simp only [Normalize.normalizeAux]
    rfl
  · intro h
    have hb := pathPrimed_variable_bound h
      (show (.num .anonymous 0 : Name) ∈ [.num .anonymous 0, .num .anonymous 1]
        by simp) he
    rw [hm] at hb
    have hf : (if (.num .anonymous 0 : Name) = .num .anonymous 0 then 3 else 1) = 3 := rfl
    rw [hf] at hb
    omega

/-- A genuine two-step extension stores the first witness at the strict
subset key [u], and the new variable at the new current key [u,v]. -/
theorem pathPrimed_extend_example :
    let u : Name := .num .anonymous 0
    let v : Name := .num .anonymous 1
    Normalize.orderedInsert Name.cmp v [u] = some [u, v] ∧
      PathPrimed [u, v]
        ((({} : NormLevel).addNode u 2 [u]).addNode v 4 [u, v]) := by
  dsimp only
  constructor
  · rfl
  · apply pathPrimed_extend (k := 4)
      (pathPrimed_extend (k := 2) (pathPrimed_empty ({} : NormLevel)) rfl)
    rfl

end Vesper.Lean4LeanLevel42
