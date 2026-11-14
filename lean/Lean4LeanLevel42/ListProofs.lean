import Lean4LeanLevel42.Semantics

/-! Soundness of the pinned list helpers. No sortedness premise is needed:
missed dominators can reduce pruning but cannot justify an invalid removal. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level Lean.Level.Normalize

theorem name_cmp_eq : ∀ (a b : Name), Name.cmp a b = .eq → a = b
  | .anonymous, .anonymous, _ => rfl
  | .anonymous, .str _ _, h => by simp [Name.cmp] at h
  | .anonymous, .num _ _, h => by simp [Name.cmp] at h
  | .str _ _, .anonymous, h => by simp [Name.cmp] at h
  | .num _ _, .anonymous, h => by simp [Name.cmp] at h
  | .str _ _, .num _ _, h => by simp [Name.cmp] at h
  | .num _ _, .str _ _, h => by simp [Name.cmp] at h
  | .str a x, .str b y, h => by
    cases hc : Name.cmp a b with
    | lt => simp [Name.cmp, hc] at h
    | gt => simp [Name.cmp, hc] at h
    | eq =>
      have hv : compare x y = .eq := by simpa [Name.cmp, hc] using h
      have hp : a = b := name_cmp_eq a b hc
      have hx : x = y := Std.LawfulEqOrd.eq_of_compare hv
      cases hp
      cases hx
      rfl
  | .num a x, .num b y, h => by
    cases hc : Name.cmp a b with
    | lt => simp [Name.cmp, hc] at h
    | gt => simp [Name.cmp, hc] at h
    | eq =>
      have hv : compare x y = .eq := by simpa [Name.cmp, hc] using h
      have hp : a = b := name_cmp_eq a b hc
      have hx : x = y := Std.LawfulEqOrd.eq_of_compare hv
      cases hp
      cases hx
      rfl

theorem subset_sound {p₂ p₁ : List Name}
    (h : Normalize.subset Name.cmp p₂ p₁ = true) : p₂ ⊆ p₁ := by
  cases hp₂ : p₂ with
  | nil => exact List.nil_subset _
  | cons a as =>
    cases hp₁ : p₁ with
    | nil => simp [hp₂, hp₁, Normalize.subset] at h
    | cons b bs =>
      rw [hp₂, hp₁] at h
      cases hc : Name.cmp a b with
      | lt => simp [Normalize.subset, hc] at h
      | eq =>
        have hab : a = b := name_cmp_eq a b hc
        have hs : Normalize.subset Name.cmp as bs = true := by
          simpa [Normalize.subset, hc] using h
        intro v hv
        rcases List.mem_cons.mp hv with ha | ha
        · exact List.mem_cons.mpr (Or.inl (ha.trans hab))
        · exact List.mem_cons.mpr (Or.inr (subset_sound hs ha))
      | gt =>
        have hs : Normalize.subset Name.cmp (a :: as) bs = true := by
          simpa [Normalize.subset, hc] using h
        intro v hv
        exact List.mem_cons.mpr (Or.inr (subset_sound hs hv))
termination_by p₂.length + p₁.length
decreasing_by all_goals (simp_all <;> omega)

theorem subsumeVars_union (ρ : Valuation) (a b : List VarNode) :
    max (varsValue ρ (Normalize.subsumeVars a b)) (varsValue ρ b) =
      max (varsValue ρ a) (varsValue ρ b) := by
  cases a with
  | nil => simp [Normalize.subsumeVars, varsValue_nil]
  | cons x xs =>
    cases b with
    | nil => simp [Normalize.subsumeVars, varsValue_nil]
    | cons y ys =>
      cases hc : Name.cmp x.var y.var with
      | lt =>
        have ih := subsumeVars_union ρ xs (y :: ys)
        simp only [Normalize.subsumeVars, hc, varsValue_cons] at ih ⊢
        omega
      | eq =>
        have ih := subsumeVars_union ρ xs ys
        have hn : x.var = y.var := name_cmp_eq x.var y.var hc
        by_cases ho : x.offset ≤ y.offset
        · have hv : ρ x.var + x.offset ≤ ρ y.var + y.offset := by rw [hn]; omega
          simp only [Normalize.subsumeVars, hc, if_pos ho, varsValue_cons]
          omega
        · simp only [Normalize.subsumeVars, hc, if_neg ho, varsValue_cons]
          omega
      | gt =>
        have ih := subsumeVars_union ρ (x :: xs) ys
        simp only [Normalize.subsumeVars, hc, varsValue_cons] at ih ⊢
        omega
termination_by a.length + b.length
decreasing_by all_goals (simp_all <;> omega)

theorem offset_fold_bound (vs : List VarNode) (acc b : Nat)
    (ha : acc + 1 ≤ b) (hv : ∀ v ∈ vs, v.offset + 1 ≤ b) :
    vs.foldl (fun (n : Nat) (v : VarNode) => Nat.max n v.offset) acc + 1 ≤ b := by
  induction vs generalizing acc with
  | nil => exact ha
  | cons v vs ih =>
    apply ih (acc := max acc v.offset)
    · have h := hv v (List.mem_cons_self)
      omega
    · intro w hw
      exact hv w (List.mem_cons_of_mem v hw)

theorem offset_bound (ρ : Valuation) (vs : List VarNode)
    (hne : vs ≠ []) (hp : ∀ v ∈ vs, 0 < ρ v.var) :
    vs.foldl (fun (acc : Nat) (v : VarNode) => Nat.max acc v.offset) 0 + 1 ≤ varsValue ρ vs := by
  have hpos : 1 ≤ varsValue ρ vs := by
    cases vs with
    | nil => exact False.elim (hne rfl)
    | cons v vs =>
      have h := hp v List.mem_cons_self
      have hb := varsValue_mem_bound (ρ := ρ) (List.mem_cons_self (a := v) (l := vs))
      omega
  apply offset_fold_bound vs 0 (varsValue ρ vs) (by omega)
  intro v hv
  have h := hp v hv
  have hb := varsValue_mem_bound (ρ := ρ) hv
  omega

end Vesper.Lean4LeanLevel42
