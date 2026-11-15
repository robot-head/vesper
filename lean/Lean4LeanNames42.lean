import Lean

/-!
Fresh-name facts about the actual pinned Core `NameGenerator.curr` and `next`.
Reservations are a caller invariant: these lemmas do not establish that the
type checker's context or cache priming supplies that invariant.
-/
namespace Vesper.Lean4LeanNames42

open Lean

def Reserves (g : NameGenerator) (fv : FVarId) : Prop :=
  ∀ i, fv = ⟨Name.num g.namePrefix i⟩ → i < g.idx

def Extends (g h : NameGenerator) : Prop :=
  g.namePrefix = h.namePrefix ∧ g.idx ≤ h.idx

def advance : Nat → NameGenerator → NameGenerator
  | 0, g => g
  | n + 1, g => advance n g.next

def nameAt (g : NameGenerator) (i : Nat) : FVarId :=
  ⟨(advance i g).curr⟩

def emitted (g : NameGenerator) (n : Nat) : List FVarId :=
  (List.range n).map (nameAt g)

theorem next_reserves_self {g : NameGenerator} : Reserves g.next ⟨g.curr⟩ := by
  rintro _ ⟨⟩
  exact Nat.lt_succ_self _

theorem not_reserves_self {g : NameGenerator} : ¬Reserves g ⟨g.curr⟩ :=
  fun h => Nat.lt_irrefl _ (h _ rfl)

theorem extends_refl {g : NameGenerator} : Extends g g :=
  ⟨rfl, Nat.le_refl _⟩

theorem extends_trans {g h k : NameGenerator}
    (hgh : Extends g h) (hhk : Extends h k) : Extends g k :=
  ⟨hgh.1.trans hhk.1, Nat.le_trans hgh.2 hhk.2⟩

theorem reserves_mono {g h : NameGenerator} {fv : FVarId}
    (hgh : Extends g h) (hr : Reserves g fv) : Reserves h fv := by
  intro i hi
  apply Nat.lt_of_lt_of_le _ hgh.2
  exact hr i (by rw [hgh.1]; exact hi)

theorem extends_next {g : NameGenerator} : Extends g g.next :=
  ⟨rfl, Nat.le_succ _⟩

theorem advance_prefix (n : Nat) (g : NameGenerator) :
    (advance n g).namePrefix = g.namePrefix := by
  induction n generalizing g with
  | zero => rfl
  | succ n ih => simpa only [advance, NameGenerator.next] using ih g.next

theorem advance_index (n : Nat) (g : NameGenerator) :
    (advance n g).idx = g.idx + n := by
  induction n generalizing g with
  | zero => simp only [advance, Nat.add_zero]
  | succ n ih => simp only [advance, ih, NameGenerator.next]; omega

theorem advance_extends (n : Nat) (g : NameGenerator) : Extends g (advance n g) := by
  constructor
  · exact (advance_prefix n g).symm
  · rw [advance_index]
    omega

theorem reserves_advance {g : NameGenerator} {fv : FVarId} (n : Nat)
    (hr : Reserves g fv) : Reserves (advance n g) fv :=
  reserves_mono (advance_extends n g) hr

theorem nameAt_eq (g : NameGenerator) (i : Nat) :
    nameAt g i = ⟨Name.num g.namePrefix (g.idx + i)⟩ := by
  simp only [nameAt, NameGenerator.curr, advance_prefix, advance_index, Name.mkNum]

theorem nameAt_injective (g : NameGenerator) : Function.Injective (nameAt g) := by
  intro i j hij
  simp only [nameAt_eq, FVarId.mk.injEq, Name.num.injEq, true_and] at hij
  omega

theorem nameAt_ne_of_ne (g : NameGenerator) {i j : Nat} (hij : i ≠ j) :
    nameAt g i ≠ nameAt g j :=
  fun h => hij (nameAt_injective g h)

theorem reserves_not_nameAt {g : NameGenerator} {fv : FVarId}
    (hr : Reserves g fv) (i : Nat) : fv ≠ nameAt g i := by
  intro heq
  have hlt := hr (g.idx + i) (by rw [heq, nameAt_eq])
  omega

theorem nameAt_reserved_advance (g : NameGenerator) {i n : Nat} (hin : i < n) :
    Reserves (advance n g) (nameAt g i) := by
  intro j hj
  simp only [nameAt_eq, advance_prefix, FVarId.mk.injEq, Name.num.injEq, true_and] at hj
  rw [advance_index]
  omega

theorem emitted_length (g : NameGenerator) (n : Nat) : (emitted g n).length = n := by
  simp only [emitted, List.length_map, List.length_range]

theorem mem_emitted (g : NameGenerator) (n : Nat) (fv : FVarId) :
    fv ∈ emitted g n ↔ ∃ i, i < n ∧ fv = nameAt g i := by
  constructor
  · intro h
    rcases List.mem_map.1 h with ⟨i, hi, heq⟩
    exact ⟨i, List.mem_range.1 hi, heq.symm⟩
  · rintro ⟨i, hi, heq⟩
    exact List.mem_map.2 ⟨i, List.mem_range.2 hi, heq.symm⟩

theorem emitted_all_reserved (g : NameGenerator) (n : Nat) :
    ∀ fv ∈ emitted g n, Reserves (advance n g) fv := by
  intro fv hfv
  rcases (mem_emitted g n fv).1 hfv with ⟨i, hi, rfl⟩
  exact nameAt_reserved_advance g hi

theorem emitted_disjoint_reserved {g : NameGenerator} {fv : FVarId}
    (hr : Reserves g fv) (n : Nat) : fv ∉ emitted g n := by
  intro hfv
  rcases (mem_emitted g n fv).1 hfv with ⟨i, _, heq⟩
  exact reserves_not_nameAt hr i heq

theorem emitted_nodup (g : NameGenerator) (n : Nat) : (emitted g n).Nodup := by
  exact List.Pairwise.map (nameAt g)
    (fun _ _ hne heq => hne (nameAt_injective g heq))
    (List.nodup_range (n := n))

theorem emitted_zero (g : NameGenerator) : emitted g 0 = [] := by rfl

theorem emitted_two (g : NameGenerator) :
    emitted g 2 = [⟨g.curr⟩, ⟨g.next.curr⟩] := by rfl

end Vesper.Lean4LeanNames42
