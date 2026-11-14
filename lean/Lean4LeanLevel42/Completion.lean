import Lean4LeanLevel42.MapValue

/-! Combinatorial reconstruction certificates, independent of search success
and denotation correctness. Stored offsets are deliberately unrestricted. -/
namespace Vesper.Lean4LeanLevel42

open Lean Lean.Level.Normalize

def EdgeCovered (s : NormLevel) (acc : List Name) (a : Name) : Prop :=
  ∃ p n k, s.get? p = some n ∧ (⟨a, k⟩ : VarNode) ∈ n.var ∧ p ⊆ a :: acc

def Admissible (s : NormLevel) (acc : List Name) : List Name → Prop
  | [] => True
  | a :: rest => EdgeCovered s acc a ∧ Admissible s (a :: acc) rest

def Completes (s : NormLevel) (acc rem : List Name) : Prop :=
  ∃ order, order.Perm rem ∧ Admissible s acc order

def MapCompletes (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → Completes s [] p

def VarSupport (s t : NormLevel) : Prop :=
  ∀ p n a k, s.get? p = some n → (⟨a, k⟩ : VarNode) ∈ n.var →
    ∃ q m j, t.get? q = some m ∧ (⟨a, j⟩ : VarNode) ∈ m.var ∧ q ⊆ p

theorem varSupport_refl (s : NormLevel) : VarSupport s s := by
  intro p n a k hp hv
  exact ⟨p, n, k, hp, hv, fun _ h => h⟩

theorem varSupport_trans (s t u : NormLevel)
    (hst : VarSupport s t) (htu : VarSupport t u) : VarSupport s u := by
  intro p n a k hp hv
  obtain ⟨q, m, j, hq, hm, hqp⟩ := hst p n a k hp hv
  obtain ⟨r, o, i, hr, ho, hrq⟩ := htu q m a j hq hm
  exact ⟨r, o, i, hr, ho, fun _ h => hqp (hrq h)⟩

theorem edgeCovered_acc_mono (s : NormLevel) (a : Name) (acc acc' : List Name)
    (hinc : acc ⊆ acc') (h : EdgeCovered s acc a) : EdgeCovered s acc' a := by
  obtain ⟨p, n, k, hp, hv, hsub⟩ := h
  refine ⟨p, n, k, hp, hv, ?_⟩
  intro b hb
  have hmem := hsub hb
  simp only [List.mem_cons] at hmem ⊢
  exact hmem.imp id (fun hb => hinc hb)

theorem edgeCovered_acc_congr (s : NormLevel) (a : Name) (acc acc' : List Name)
    (h : ∀ b, b ∈ acc ↔ b ∈ acc') :
    EdgeCovered s acc a ↔ EdgeCovered s acc' a := by
  exact ⟨edgeCovered_acc_mono s a acc acc' (fun b hb => (h b).mp hb),
    edgeCovered_acc_mono s a acc' acc (fun b hb => (h b).mpr hb)⟩

theorem admissible_acc_mono (s : NormLevel) (acc acc' order : List Name)
    (hinc : acc ⊆ acc') (h : Admissible s acc order) :
    Admissible s acc' order := by
  induction order generalizing acc acc' with
  | nil => trivial
  | cons a rest ih =>
    rcases h with ⟨ha, hr⟩
    refine ⟨edgeCovered_acc_mono s a acc acc' hinc ha, ih _ _ ?_ hr⟩
    intro b hb
    simp only [List.mem_cons] at hb ⊢
    exact hb.imp id (fun hb => hinc hb)

theorem completes_acc_mono (s : NormLevel) (acc acc' rem : List Name)
    (hinc : acc ⊆ acc') (h : Completes s acc rem) : Completes s acc' rem := by
  obtain ⟨order, hp, ha⟩ := h
  exact ⟨order, hp, admissible_acc_mono s acc acc' order hinc ha⟩

theorem edgeCovered_support (s t : NormLevel) (hs : VarSupport s t)
    (acc : List Name) (a : Name) (h : EdgeCovered s acc a) :
    EdgeCovered t acc a := by
  obtain ⟨p, n, k, hp, hv, hsub⟩ := h
  obtain ⟨q, m, j, hq, hm, hqp⟩ := hs p n a k hp hv
  exact ⟨q, m, j, hq, hm, fun _ hb => hsub (hqp hb)⟩

theorem admissible_support (s t : NormLevel) (hs : VarSupport s t)
    (acc order : List Name) (h : Admissible s acc order) :
    Admissible t acc order := by
  induction order generalizing acc with
  | nil => trivial
  | cons a rest ih =>
    exact ⟨edgeCovered_support s t hs acc a h.1, ih _ h.2⟩

theorem completes_support (s t : NormLevel) (hs : VarSupport s t)
    (acc rem : List Name) (h : Completes s acc rem) : Completes t acc rem := by
  obtain ⟨order, hp, ha⟩ := h
  exact ⟨order, hp, admissible_support s t hs acc order ha⟩

/-- Support transports fixed keys; target key origin is additionally necessary. -/
theorem mapCompletes_support (s t : NormLevel) (hs : VarSupport s t)
    (horigin : ∀ p n, t.get? p = some n → ∃ m, s.get? p = some m)
    (h : MapCompletes s) : MapCompletes t := by
  intro p n hp
  obtain ⟨m, hm⟩ := horigin p n hp
  exact completes_support s t hs [] p (h p m hm)

theorem completes_nil (s : NormLevel) (acc : List Name) : Completes s acc [] := by
  exact ⟨[], List.Perm.refl _, trivial⟩

theorem mapCompletes_empty : MapCompletes ({} : NormLevel) := by
  intro p n hp
  rw [map_get_empty] at hp
  contradiction

end Vesper.Lean4LeanLevel42
