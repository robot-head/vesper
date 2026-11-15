import Lean4LeanLevel42.TreeStructure
import Lean4LeanLevel42.TreeReifyValue

/-! Root-first observations and value bounds for the actual inside-out tree
update. Payload setters preserve children; no replacement updater is defined. -/
namespace Vesper.Lean4LeanLevel42
open Lean Lean.Level Lean.Level.Normalize

def treePathAt (route : List Name) (t : Tree) : Option Tree :=
  route.foldl (fun current a => current.bind (fun u => treeEntriesLookup a u.child)) (some t)

def treePayloadAt (route : List Name) (t : Tree) : Node :=
  ((treePathAt route t).map (fun u => ({const := u.const, var := u.var} : Node))).getD {}

def treeWithPayload (n : Node) (t : Tree) : Tree :=
  {t with const := n.const, var := n.var}

def TreeChildrenSorted (t : Tree) : Prop :=
  ∀ route u, treePathAt route t = some u → KeySorted (u.child.map Prod.fst)

def treePathGate (ρ : Valuation) (route : List Name) (z : Nat) : Nat :=
  if enabled ρ route then z else 0

theorem tree_default_eq : (default : Tree) = ⟨0, [], []⟩ := rfl

theorem treePathAt_nil (t : Tree) : treePathAt [] t = some t := rfl

theorem treePathAt_fold_none (route : List Name) :
    route.foldl (fun current a => current.bind (fun u : Tree => treeEntriesLookup a u.child))
      (none : Option Tree) = none := by
  induction route with
  | nil => rfl
  | cons a route ih => simpa only [List.foldl_cons, Option.bind_none] using ih

theorem treePathAt_fold_bind (route : List Name) (current : Option Tree) :
    route.foldl (fun current a => current.bind (fun u => treeEntriesLookup a u.child)) current =
      current.bind (treePathAt route) := by
  cases current with
  | none => exact treePathAt_fold_none route
  | some u => rfl

theorem treePathAt_cons (a : Name) (route : List Name) (t : Tree) :
    treePathAt (a :: route) t = (treeEntriesLookup a t.child).bind (treePathAt route) := by
  change route.foldl _ (treeEntriesLookup a t.child) = _
  exact treePathAt_fold_bind route _

theorem treePathAt_append (front rest : List Name) (t : Tree) :
    treePathAt (front ++ rest) t = (treePathAt front t).bind (treePathAt rest) := by
  unfold treePathAt
  rw [List.foldl_append]
  exact treePathAt_fold_bind rest _

theorem treePayloadAt_cons (a : Name) (route : List Name) (t : Tree) :
    treePayloadAt (a :: route) t =
      (treeEntriesLookup a t.child).elim {} (treePayloadAt route) := by
  unfold treePayloadAt
  rw [treePathAt_cons]
  cases treeEntriesLookup a t.child <;> rfl

theorem tree_entries_mem {a : Name} {u : Tree} {xs : List (Name × Tree)}
    (h : treeEntriesLookup a xs = some u) : (a, u) ∈ xs := by
  induction xs with
  | nil => simp [treeEntriesLookup] at h
  | cons row xs ih =>
    rcases row with ⟨m, v⟩
    rw [treeEntriesLookup_cons] at h
    split at h
    next he => subst m; cases h; exact List.mem_cons_self
    next => exact List.mem_cons_of_mem _ (ih h)

theorem tree_entries_lookup_of_mem {a : Name} {u : Tree} {xs : List (Name × Tree)}
    (hs : KeySorted (xs.map Prod.fst)) (hu : (a, u) ∈ xs) :
    treeEntriesLookup a xs = some u := by
  induction xs with
  | nil => simp at hu
  | cons row xs ih =>
    rcases row with ⟨m, v⟩
    rcases List.mem_cons.mp hu with he | hrest
    · cases he
      simp [treeEntriesLookup_cons]
    · rcases List.pairwise_cons.mp hs with ⟨hm, ht⟩
      have hne : m ≠ a := by
        intro he
        subst m
        have hf := hm a (List.mem_map.mpr ⟨(a, u), hrest, rfl⟩)
        simp [name_cmp_refl] at hf
      rw [treeEntriesLookup_cons, if_neg hne]
      exact ih ht hrest

theorem tree_children_sorted_root (t : Tree) (hs : TreeChildrenSorted t) :
    KeySorted (t.child.map Prod.fst) := hs [] t rfl

theorem tree_children_sorted_at (t : Tree) (hs : TreeChildrenSorted t)
    (route : List Name) (u : Tree) (hu : treePathAt route t = some u) :
    TreeChildrenSorted u := by
  intro rest v hv
  apply hs (route ++ rest) v
  simpa only [treePathAt_append, hu, Option.bind_some] using hv

theorem tree_empty_payload_sorted :
    TreeChildrenSorted (default : Tree) ∧ ∀ route, treePayloadAt route (default : Tree) = {} := by
  constructor
  · intro route u hu
    cases route with
    | nil => cases hu; exact List.Pairwise.nil
    | cons a route => simp [treePathAt_cons, treeEntriesLookup, tree_default_eq] at hu
  · intro route
    cases route with
    | nil => rfl
    | cons a route => simp [treePayloadAt_cons, treeEntriesLookup, tree_default_eq]

theorem tree_modify_append (front back : List Name) (f : Tree → Tree) (t : Tree) :
    t.modify (front ++ back) f = t.modify back (fun u => u.modify front f) := by
  induction front generalizing f with
  | nil => rfl
  | cons a front ih =>
    simpa only [List.cons_append, Tree.modify] using
      ih (fun u => {u with child := Normalize.modifyAt f a u.child})

theorem tree_modify_rootfirst (a : Name) (route : List Name) (f : Tree → Tree) (t : Tree) :
    t.modify (a :: route).reverse f =
      {t with child := Normalize.modifyAt (fun u => u.modify route.reverse f) a t.child} := by
  rw [List.reverse_cons, tree_modify_append]
  rfl

theorem treePathAt_withPayload_cons (a : Name) (route : List Name) (n : Node) (t : Tree) :
    treePathAt (a :: route) (treeWithPayload n t) = treePathAt (a :: route) t := rfl

theorem tree_withPayload_sorted (n : Node) (t : Tree) (hs : TreeChildrenSorted t) :
    TreeChildrenSorted (treeWithPayload n t) := by
  intro route u hu
  cases route with
  | nil => cases hu; exact tree_children_sorted_root t hs
  | cons a route => exact hs (a :: route) u (by simpa only [treePathAt_withPayload_cons] using hu)

/-- The joint observation proof covers newly created zero ancestors as well
as existing target descendants; no arbitrary-function frame is asserted. -/
theorem tree_modify_observation (route : List Name) (n : Node) (t : Tree)
    (hs : TreeChildrenSorted t) :
    TreeChildrenSorted (t.modify route.reverse (treeWithPayload n)) ∧
      (∃ u, treePathAt route (t.modify route.reverse (treeWithPayload n)) = some u) ∧
      treePayloadAt route (t.modify route.reverse (treeWithPayload n)) = n ∧
      (∀ q, q ≠ route → treePayloadAt q (t.modify route.reverse (treeWithPayload n)) =
        treePayloadAt q t) := by
  induction route generalizing t with
  | nil =>
    change TreeChildrenSorted (treeWithPayload n t) ∧ _
    refine ⟨tree_withPayload_sorted n t hs, ⟨treeWithPayload n t, rfl⟩, rfl, ?_⟩
    intro q hq
    cases q with
    | nil => exact False.elim (hq rfl)
    | cons a q =>
      change treePayloadAt (a :: q) (treeWithPayload n t) = treePayloadAt (a :: q) t
      simp only [treePayloadAt, treePathAt_withPayload_cons]
  | cons a route ih =>
    let old := (treeEntriesLookup a t.child).getD (default : Tree)
    have hroot := tree_children_sorted_root t hs
    have hold : TreeChildrenSorted old := by
      cases he : treeEntriesLookup a t.child with
      | none => simpa only [old, he, Option.getD_none] using tree_empty_payload_sorted.1
      | some u =>
        apply tree_children_sorted_at t hs [a] old
        simp [treePathAt_cons, treePathAt_nil, old, he]
    obtain ⟨hnew, hex, htarget, hframe⟩ := ih old hold
    let updated := t.modify (a :: route).reverse (treeWithPayload n)
    have hchild : treeEntriesLookup a updated.child =
        some (old.modify route.reverse (treeWithPayload n)) := by
      dsimp only [updated]
      rw [tree_modify_rootfirst]
      exact modifyAt_lookup_target _ a t.child hroot
    have hother : ∀ m, m ≠ a → treeEntriesLookup m updated.child = treeEntriesLookup m t.child := by
      intro m hm
      dsimp only [updated]
      rw [tree_modify_rootfirst]
      exact modifyAt_lookup_other _ a m t.child hm
    refine ⟨?_, ?_, ?_, ?_⟩
    · intro q u hu
      cases q with
      | nil =>
        cases hu
        rw [tree_modify_rootfirst]
        exact modifyAt_keysSorted _ a t.child hroot
      | cons m q =>
        rw [treePathAt_cons] at hu
        by_cases hm : m = a
        · subst m
          rw [hchild, Option.bind_some] at hu
          exact hnew q u hu
        · rw [hother m hm] at hu
          exact hs (m :: q) u (by simpa only [treePathAt_cons] using hu)
    · obtain ⟨u, hu⟩ := hex
      refine ⟨u, ?_⟩
      rw [treePathAt_cons, hchild, Option.bind_some]
      exact hu
    · rw [treePayloadAt_cons, hchild, Option.elim_some]
      exact htarget
    · intro q hq
      cases q with
      | nil => rw [tree_modify_rootfirst]; rfl
      | cons m q =>
        by_cases hm : m = a
        · subst m
          have hqr : q ≠ route := by intro he; exact hq (congrArg (List.cons a) he)
          rw [treePayloadAt_cons, hchild, Option.elim_some, hframe q hqr, treePayloadAt_cons]
          cases he : treeEntriesLookup a t.child with
          | none => simpa only [old, he, Option.getD_none, Option.elim_none] using tree_empty_payload_sorted.2 q
          | some u => simp only [old, he, Option.getD_some, Option.elim_some]
        · rw [treePayloadAt_cons, hother m hm, treePayloadAt_cons]

theorem tree_modify_payload_sorted (t : Tree) (path : List Name) (n : Node)
    (hs : TreeChildrenSorted t) : TreeChildrenSorted (t.modify path (treeWithPayload n)) := by
  simpa only [List.reverse_reverse] using (tree_modify_observation path.reverse n t hs).1

theorem tree_modify_payload_frame (t : Tree) (path : List Name) (n : Node)
    (hs : TreeChildrenSorted t) :
    (∃ u, treePathAt path.reverse (t.modify path (treeWithPayload n)) = some u) ∧
      treePayloadAt path.reverse (t.modify path (treeWithPayload n)) = n ∧
      (∀ q, q ≠ path.reverse → treePayloadAt q (t.modify path (treeWithPayload n)) =
        treePayloadAt q t) := by
  simpa only [List.reverse_reverse] using (tree_modify_observation path.reverse n t hs).2


theorem treePathGate_nil (ρ : Valuation) (z : Nat) : treePathGate ρ [] z = z := rfl

theorem treePathGate_zero (ρ : Valuation) (route : List Name) : treePathGate ρ route 0 = 0 := by
  unfold treePathGate
  split <;> rfl

theorem treePathGate_le (ρ : Valuation) (route : List Name) (z : Nat) :
    treePathGate ρ route z ≤ z := by
  unfold treePathGate
  split <;> omega

theorem treePathGate_mono (ρ : Valuation) (route : List Name) {x y : Nat} (h : x ≤ y) :
    treePathGate ρ route x ≤ treePathGate ρ route y := by
  unfold treePathGate
  split <;> omega

theorem treePathGate_append (ρ : Valuation) (front rest : List Name) (z : Nat) :
    treePathGate ρ (front ++ rest) z = treePathGate ρ front (treePathGate ρ rest z) := by
  have he : enabled ρ (front ++ rest) = (enabled ρ front && enabled ρ rest) := by
    simp [enabled, List.all_append]
  unfold treePathGate
  rw [he]
  cases enabled ρ front <;> cases enabled ρ rest <;> rfl

theorem treePathGate_imax (ρ : Valuation) (prior : List Name) (a : Name) (z : Nat) :
    treePathGate ρ prior (Nat.imax z (ρ a)) =
      max (treePathGate ρ prior (ρ a)) (treePathGate ρ (prior ++ [a]) z) := by
  have he : enabled ρ (prior ++ [a]) = (enabled ρ prior && decide (0 < ρ a)) := by
    simp [enabled, List.all_append]
  unfold treePathGate
  rw [he]
  cases enabled ρ prior <;> cases ρ a <;> simp [Nat.imax, Nat.max_comm]

theorem tree_default_value (ρ : Valuation) : levelValue ρ (default : Tree).reify = 0 := by
  rw [tree_default_eq, tree_reify_leaf_value]
  rfl

theorem tree_imax_mono {x y a : Nat} (h : x ≤ y) : Nat.imax x a ≤ Nat.imax y a := by
  unfold Nat.imax
  split
  · exact Nat.le_refl _
  · exact Nat.max_le.mpr ⟨Nat.le_trans h (Nat.le_max_left _ _), Nat.le_max_right _ _⟩

theorem tree_max_mono (a x y : Nat) (h : x ≤ y) : max a x ≤ max a y :=
  Nat.max_le.mpr ⟨Nat.le_max_left _ _, Nat.le_trans h (Nat.le_max_right _ _)⟩

theorem tree_seed_sorted (t : Tree) (a : Name) (hs : TreeChildrenSorted t) :
    TreeChildrenSorted ((treeEntriesLookup a t.child).getD (default : Tree)) := by
  cases he : treeEntriesLookup a t.child with
  | none => simpa only [he, Option.getD_none] using tree_empty_payload_sorted.1
  | some u =>
    apply tree_children_sorted_at t hs [a] _
    simp [treePathAt_cons, treePathAt_nil, he]

theorem tree_seed_payload (t : Tree) (a : Name) (route : List Name) :
    treePayloadAt route ((treeEntriesLookup a t.child).getD (default : Tree)) =
      treePayloadAt (a :: route) t := by
  rw [treePayloadAt_cons]
  cases he : treeEntriesLookup a t.child with
  | none => simpa only [he, Option.getD_none, Option.elim_none] using tree_empty_payload_sorted.2 route
  | some u => rfl

/-- A bound on the full current tree bounds every gated child continuation. -/
theorem tree_gate_child_le (ρ : Valuation) (prior : List Name) (t : Tree)
    {a : Name} {u : Tree} (hu : (a, u) ∈ t.child) :
    treePathGate ρ (prior ++ [a]) (levelValue ρ u.reify) ≤
      treePathGate ρ prior (levelValue ρ t.reify) := by
  have h := treePathGate_mono ρ prior (tree_reify_child_bound ρ t hu)
  rw [treePathGate_imax] at h
  exact Nat.le_trans (Nat.le_max_right _ _) h

theorem tree_path_value_lower (ρ : Valuation) (route : List Name) (t u : Tree)
    (hu : treePathAt route t = some u) :
    treePathGate ρ route (levelValue ρ u.reify) ≤ levelValue ρ t.reify := by
  induction route generalizing t with
  | nil => cases hu; exact Nat.le_refl _
  | cons a route ih =>
    rw [treePathAt_cons] at hu
    cases he : treeEntriesLookup a t.child with
    | none => simp [he] at hu
    | some v =>
      rw [he, Option.bind_some] at hu
      have hi := ih v hu
      have hchild := tree_gate_child_le ρ [] t (tree_entries_mem he)
      change treePathGate ρ ([a] ++ route) (levelValue ρ u.reify) ≤ _
      rw [treePathGate_append]
      exact Nat.le_trans (treePathGate_mono ρ [a] hi) hchild

theorem tree_path_payload_lower (ρ : Valuation) (route : List Name) (t : Tree) :
    treePathGate ρ route (max (treePayloadAt route t).const
      (varsValue ρ (treePayloadAt route t).var)) ≤ levelValue ρ t.reify := by
  cases he : treePathAt route t with
  | none => simp [treePayloadAt, he, varsValue, treePathGate_zero]
  | some u =>
    have h := tree_path_value_lower ρ route t u he
    have hp := treePathGate_mono ρ route (tree_reify_payload_bound ρ u)
    simpa only [treePayloadAt, he, Option.map_some, Option.getD_some] using Nat.le_trans hp h

theorem treePathAt_singleton (a : Name) (t : Tree) :
    treePathAt [a] t = treeEntriesLookup a t.child := by
  rw [treePathAt_cons]
  cases treeEntriesLookup a t.child <;> rfl

theorem tree_path_incoming_lower (ρ : Valuation) (prior : List Name) (a : Name)
    (t u : Tree) (hu : treePathAt (prior ++ [a]) t = some u) :
    treePathGate ρ (prior ++ [a]) (ρ a) ≤ levelValue ρ t.reify := by
  rw [treePathAt_append] at hu
  cases he : treePathAt prior t with
  | none => simp [he] at hu
  | some v =>
    rw [he, Option.bind_some, treePathAt_singleton] at hu
    have hv := tree_path_value_lower ρ prior t v he
    have ha := treePathGate_mono ρ prior (tree_reify_edge_bound ρ v (tree_entries_mem hu))
    rw [treePathGate_append]
    exact Nat.le_trans (treePathGate_mono ρ prior (treePathGate_le ρ [a] (ρ a)))
      (Nat.le_trans ha hv)

theorem tree_modify_payload_lower (ρ : Valuation) (t : Tree) (path : List Name) (n : Node)
    (hs : TreeChildrenSorted t) :
    treePathGate ρ path.reverse (max n.const (varsValue ρ n.var)) ≤
        levelValue ρ (t.modify path (treeWithPayload n)).reify ∧
      (∀ a tail, path = a :: tail → treePathGate ρ path.reverse (ρ a) ≤
        levelValue ρ (t.modify path (treeWithPayload n)).reify) := by
  obtain ⟨hex, hp, _⟩ := tree_modify_payload_frame t path n hs
  constructor
  · have h := tree_path_payload_lower ρ path.reverse (t.modify path (treeWithPayload n))
    simpa only [hp] using h
  · intro a tail he
    obtain ⟨u, hu⟩ := hex
    subst path
    rw [List.reverse_cons] at hu ⊢
    exact tree_path_incoming_lower ρ tail.reverse a _ u hu

/-- A selected child's growth preserves the entire child-fold value. -/
theorem tree_modifyAt_children_mono (ρ : Valuation) (a : Name) (xs : List (Name × Tree))
    (f : Tree → Tree) (hs : KeySorted (xs.map Prod.fst))
    (hf : levelValue ρ ((treeEntriesLookup a xs).getD (default : Tree)).reify ≤
      levelValue ρ (f ((treeEntriesLookup a xs).getD (default : Tree))).reify) :
    xs.foldr (fun e z => max (reifyChildValue ρ e) z) 0 ≤
      (Normalize.modifyAt f a xs).foldr (fun e z => max (reifyChildValue ρ e) z) 0 := by
  apply foldr_max_le
  intro e he
  rcases e with ⟨m, u⟩
  have hlookup := tree_entries_lookup_of_mem hs he
  by_cases hm : m = a
  · subst m
    have hnew := modifyAt_lookup_target f a xs hs
    rw [hlookup, Option.getD_some] at hnew hf
    exact Nat.le_trans (tree_imax_mono hf)
      (foldr_max_mem_bound (reifyChildValue ρ) _ (tree_entries_mem hnew))
  · have hnew : treeEntriesLookup m (Normalize.modifyAt f a xs) = some u := by
      rw [modifyAt_lookup_other f a m xs hm]
      exact hlookup
    exact foldr_max_mem_bound (reifyChildValue ρ) _ (tree_entries_mem hnew)

theorem tree_modify_payload_mono_rootfirst (ρ : Valuation) (route : List Name) (t : Tree)
    (n : Node) (hs : TreeChildrenSorted t) (hz : treePayloadAt route t = {}) :
    levelValue ρ t.reify ≤ levelValue ρ (t.modify route.reverse (treeWithPayload n)).reify := by
  induction route generalizing t with
  | nil =>
    change ({const := t.const, var := t.var} : Node) = {} at hz
    have hc : t.const = 0 := congrArg Node.const hz
    have hv : t.var = [] := congrArg Node.var hz
    rw [tree_reify_value ρ t, tree_reify_value]
    simp only [Tree.modify, List.reverse_nil, treeWithPayload, hc, hv, varsValue_nil,
      Nat.zero_max]
    omega
  | cons a route ih =>
    let old := (treeEntriesLookup a t.child).getD (default : Tree)
    have hzero : treePayloadAt route old = {} := by rw [tree_seed_payload]; exact hz
    have hmono := ih old (tree_seed_sorted t a hs) hzero
    have hchildren := tree_modifyAt_children_mono ρ a t.child
      (fun u => u.modify route.reverse (treeWithPayload n)) (tree_children_sorted_root t hs) hmono
    rw [tree_modify_rootfirst, tree_reify_value ρ t, tree_reify_value]
    exact tree_max_mono _ _ _ (tree_max_mono _ _ _ hchildren)

theorem tree_modify_payload_mono (ρ : Valuation) (t : Tree) (path : List Name) (n : Node)
    (hs : TreeChildrenSorted t) (hz : treePayloadAt path.reverse t = {}) :
    levelValue ρ t.reify ≤ levelValue ρ (t.modify path (treeWithPayload n)).reify := by
  simpa only [List.reverse_reverse] using tree_modify_payload_mono_rootfirst ρ path.reverse t n hs hz

/-- One-layer gated upper bound; child terms are actual reifications. -/
theorem tree_reify_gate_le (ρ : Valuation) (prior : List Name) (t : Tree) (b : Nat)
    (hp : treePathGate ρ prior (max t.const (varsValue ρ t.var)) ≤ b)
    (hc : ∀ a u, (a, u) ∈ t.child →
      treePathGate ρ prior (Nat.imax (levelValue ρ u.reify) (ρ a)) ≤ b) :
    treePathGate ρ prior (levelValue ρ t.reify) ≤ b := by
  cases he : enabled ρ prior with
  | false => simp [treePathGate, he]
  | true =>
    simp only [treePathGate, he, ↓reduceIte] at hp hc ⊢
    exact tree_reify_le ρ t b (Nat.le_trans (Nat.le_max_left _ _) hp)
      (Nat.le_trans (Nat.le_max_right _ _) hp) hc

theorem tree_modifyAt_child_gate_bound (ρ : Valuation) (prior : List Name) (a : Name)
    (xs : List (Name × Tree)) (f : Tree → Tree) (b : Nat)
    (hs : KeySorted (xs.map Prod.fst))
    (hold : ∀ m u, (m, u) ∈ xs →
      treePathGate ρ prior (Nat.imax (levelValue ρ u.reify) (ρ m)) ≤ b)
    (hnew : treePathGate ρ prior (Nat.imax
      (levelValue ρ (f ((treeEntriesLookup a xs).getD (default : Tree))).reify) (ρ a)) ≤ b) :
    ∀ m u, (m, u) ∈ Normalize.modifyAt f a xs →
      treePathGate ρ prior (Nat.imax (levelValue ρ u.reify) (ρ m)) ≤ b := by
  intro m u hu
  have hlook := tree_entries_lookup_of_mem (modifyAt_keysSorted f a xs hs) hu
  by_cases hm : m = a
  · subst m
    have ht := modifyAt_lookup_target f a xs hs
    have he := Option.some.inj (hlook.symm.trans ht)
    subst u
    exact hnew
  · rw [modifyAt_lookup_other f a m xs hm] at hlook
    exact hold m u (tree_entries_mem hlook)

theorem tree_modify_payload_upper_gated (ρ : Valuation) (prior route : List Name)
    (t : Tree) (n : Node) (b : Nat) (hs : TreeChildrenSorted t)
    (hold : treePathGate ρ prior (levelValue ρ t.reify) ≤ b)
    (hpayload : treePathGate ρ (prior ++ route) (max n.const (varsValue ρ n.var)) ≤ b)
    (hedges : ∀ front a rest, route = front ++ a :: rest →
      treePathGate ρ (prior ++ front) (ρ a) ≤ b) :
    treePathGate ρ prior (levelValue ρ (t.modify route.reverse (treeWithPayload n)).reify) ≤ b := by
  induction route generalizing prior t with
  | nil =>
    change treePathGate ρ prior (levelValue ρ (treeWithPayload n t).reify) ≤ b
    apply tree_reify_gate_le
    · simpa only [List.append_nil] using hpayload
    · intro a u hu
      exact Nat.le_trans (treePathGate_mono ρ prior (tree_reify_child_bound ρ t hu)) hold
  | cons a route ih =>
    let old := (treeEntriesLookup a t.child).getD (default : Tree)
    have hseed : treePathGate ρ (prior ++ [a]) (levelValue ρ old.reify) ≤ b := by
      cases he : treeEntriesLookup a t.child with
      | none => simp [old, he, tree_default_value, treePathGate_zero]
      | some u =>
        have hc := tree_gate_child_le ρ prior t (tree_entries_mem he)
        simpa only [old, he, Option.getD_some] using Nat.le_trans hc hold
    have hp : treePathGate ρ ((prior ++ [a]) ++ route)
        (max n.const (varsValue ρ n.var)) ≤ b := by
      simpa only [List.append_assoc, List.singleton_append] using hpayload
    have he : ∀ front c rest, route = front ++ c :: rest →
        treePathGate ρ ((prior ++ [a]) ++ front) (ρ c) ≤ b := by
      intro front c rest hr
      have hx := hedges (a :: front) c rest (congrArg (List.cons a) hr)
      simpa only [List.append_assoc, List.singleton_append] using hx
    have hsub := ih (prior ++ [a]) old (tree_seed_sorted t a hs) hseed hp he
    have htarget : treePathGate ρ prior (Nat.imax
        (levelValue ρ (old.modify route.reverse (treeWithPayload n)).reify) (ρ a)) ≤ b := by
      rw [treePathGate_imax]
      exact Nat.max_le.mpr ⟨by simpa only [List.append_nil] using hedges [] a route rfl, hsub⟩
    rw [tree_modify_rootfirst]
    apply tree_reify_gate_le
    · exact Nat.le_trans (treePathGate_mono ρ prior (tree_reify_payload_bound ρ t)) hold
    · apply tree_modifyAt_child_gate_bound ρ prior a t.child _ b
        (tree_children_sorted_root t hs)
      · intro m u hu
        exact Nat.le_trans (treePathGate_mono ρ prior (tree_reify_child_bound ρ t hu)) hold
      · exact htarget

theorem tree_modify_payload_upper (ρ : Valuation) (t : Tree) (path : List Name) (n : Node)
    (b : Nat) (hs : TreeChildrenSorted t) (hold : levelValue ρ t.reify ≤ b)
    (hpayload : treePathGate ρ path.reverse (max n.const (varsValue ρ n.var)) ≤ b)
    (hedges : ∀ front a rest, path.reverse = front ++ a :: rest →
      treePathGate ρ front (ρ a) ≤ b) :
    levelValue ρ (t.modify path (treeWithPayload n)).reify ≤ b := by
  simpa only [List.nil_append, List.reverse_reverse, treePathGate_nil] using
    tree_modify_payload_upper_gated ρ [] path.reverse t n b hs hold hpayload hedges

end Vesper.Lean4LeanLevel42
