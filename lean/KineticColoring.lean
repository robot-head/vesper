/-!
# Vesper Kinetic: greedy colouring disjointness and Morton interleaving

Model identifier: `vesper.kinetic.coloring-morton-model/3`.

This file models two pure algorithms of `crates/vesper-kinetic` over `Nat`:

* `colorAll cs` colours a list of constraints, each a pair of body ids (a
  single-body constraint against static geometry is the pair `(a, a)`), in
  list order. Each constraint takes `firstFree used`, the least colour below
  `used.length + 1` that is not in `used`, where `used` lists the colours of
  earlier constraints that share a body with it. (A free colour always exists
  below that bound by pigeonhole; the model falls back to `maxList used + 1`
  instead of proving it, and the theorems below hold for either choice.) The
  Rust `Colorer::color` visits constraints in its sorted order and takes the
  lowest free bit of the two bodies' colour masks, which is the same choice;
  the Rust test feeds the Rust visiting order to the executable model and
  compares colours.
* `colorAllSets cs` is the same greedy colouring of constraints given as
  lists of bodies of any length: the k-body batches of the solvers (ground
  contacts touch one body, contacts and joints two, soft-body tetrahedral
  volume constraints four). The Rust `MultiColorer::color` visits its
  constraints in its sorted order and takes the lowest free bit of the
  bodies' masks; the Rust test feeds that visiting order to the model and
  compares colours.
* `interleave n x y z` is the bit-by-bit Morton code over `n` bit positions
  (bit `3i` from `x`, `3i+1` from `y`, `3i+2` from `z`), and `deinterleaveX`,
  `deinterleaveY`, `deinterleaveZ` extract the axes again.

It proves, about these Lean definitions:

* `firstFree_not_mem`: the chosen colour is not used by any conflicting
  earlier constraint;
* `colorAll_pairwise_disjoint` (§36.4 Same-Color Body Non-Aliasing): in the
  coloured list, any two constraints with the same colour share no body;
* `colorAll_constraints`: colouring keeps the constraints, in order;
* `same_color_constraints_disjoint_bodies`: the index form, for positions
  `i < j` of the coloured list;
* `colorAllSets_pairwise_disjoint`, `colorAllSets_constraints` and
  `same_color_sets_disjoint_bodies`: the same three facts for batches of any
  arity (constraints of one colour touch pairwise disjoint body lists);
* `colorAll_eq_colorAllSets`: colouring pairs gives the colours that
  colouring their two-element body lists gives, so joints and contacts
  coloured as two-body batches agree with the pair model;
* `morton_deinterleave_interleave` and `morton_10bit_roundtrip` (§36.4
  Bit-Exact 10-Bit 3D Morton Interleaving): deinterleaving recovers each axis
  modulo `2^n`, hence exactly for 10-bit inputs; `morton_10bit_injective`:
  distinct 10-bit triples have distinct codes; `interleave_lt`: the code fits
  in `3n` bits.
* `morton_21bit_roundtrip`, `morton_21bit_injective` and `morton_21bit_lt`:
  the same for the LBVH's 21-bit axes, whose codes fit in 63 bits.
* `sorted_prefix_between` and `monotone_keys_prefix_range` (the ordering
  fact the Karras LBVH construction relies on): if two keys of a sorted
  array agree on every bit at position `k` and above (`a / 2^k = c / 2^k`),
  every key between them agrees with both, so the keys sharing a prefix
  form one contiguous index range; that range is what each internal node
  covers.
* `augKey_lt_iff` and `augKey_injective`: the tie-broken sort key
  `code · 2^32 + index` orders by code, then index, and is injective, so
  the sort order (and the tree) is unique even with duplicate codes;
  `augKey_div`: its bits at 32 and above are the code.

It is **not** a proof about the Rust code (the magic-mask `spread` /
`compact`, the BMI2 path, the bitmask colourer or the sort), nor about
solver stability or IEEE-754 arithmetic. The Rust tests compare the Rust
functions with these definitions on concrete inputs (a differential test,
not a refinement proof).
-/

namespace Vesper.KineticColoring

def modelId : String := "vesper.kinetic.coloring-morton-model/3"
def modelClaim : String :=
  "Lean model of greedy colouring and Morton codes; proofs cover the model, not Rust"

/-! ## Greedy colouring -/

/-- A constraint names two bodies; `(a, a)` touches one body. -/
abbrev Constraint := Nat × Nat

/-- Two constraints share a body. -/
def shares (p q : Constraint) : Bool :=
  p.1 == q.1 || p.1 == q.2 || p.2 == q.1 || p.2 == q.2

/-- Disjoint bodies: the property the solver needs within one colour. -/
def Disjoint (p q : Constraint) : Prop :=
  p.1 ≠ q.1 ∧ p.1 ≠ q.2 ∧ p.2 ≠ q.1 ∧ p.2 ≠ q.2

theorem disjoint_of_not_shares {p q : Constraint} (h : shares p q = false) :
    Disjoint p q := by
  simp [shares] at h
  exact ⟨h.1.1.1, h.1.1.2, h.1.2, h.2⟩

def maxList : List Nat → Nat
  | [] => 0
  | a :: l => max a (maxList l)

theorem le_maxList {x : Nat} : ∀ {l : List Nat}, x ∈ l → x ≤ maxList l
  | [], h => by cases h
  | a :: l, h => by
    simp only [maxList]
    cases List.mem_cons.mp h with
    | inl e => subst e; exact Nat.le_max_left _ _
    | inr m => exact Nat.le_trans (le_maxList m) (Nat.le_max_right _ _)

/-- Search `c, c+1, ...` for `fuel` steps; fall back to `maxList used + 1`. -/
def firstFreeFrom (used : List Nat) : Nat → Nat → Nat
  | _, 0 => maxList used + 1
  | c, fuel + 1 => if c ∈ used then firstFreeFrom used (c + 1) fuel else c

def firstFree (used : List Nat) : Nat := firstFreeFrom used 0 (used.length + 1)

theorem firstFreeFrom_not_mem (used : List Nat) :
    ∀ (fuel c : Nat), firstFreeFrom used c fuel ∉ used
  | 0, c => by
    intro h
    simp only [firstFreeFrom] at h
    have := le_maxList h
    omega
  | fuel + 1, c => by
    simp only [firstFreeFrom]
    split
    · exact firstFreeFrom_not_mem used fuel (c + 1)
    · assumption

theorem firstFree_not_mem (used : List Nat) : firstFree used ∉ used :=
  firstFreeFrom_not_mem used _ _

/-- Colours of the already-coloured constraints that conflict with `k`. -/
def usedBy (acc : List (Constraint × Nat)) (k : Constraint) : List Nat :=
  (acc.filter (fun x => shares x.1 k)).map (·.2)

def step (acc : List (Constraint × Nat)) (k : Constraint) : List (Constraint × Nat) :=
  acc ++ [(k, firstFree (usedBy acc k))]

def colorAll (cs : List Constraint) : List (Constraint × Nat) :=
  cs.foldl step []

/-- Same colour implies disjoint bodies. -/
def Ok (x y : Constraint × Nat) : Prop := x.2 = y.2 → Disjoint x.1 y.1

theorem step_pairwise (acc : List (Constraint × Nat)) (k : Constraint)
    (h : acc.Pairwise Ok) : (step acc k).Pairwise Ok := by
  unfold step
  rw [List.pairwise_append]
  refine ⟨h, List.pairwise_singleton _ _, ?_⟩
  intro x hx y hy
  simp only [List.mem_singleton] at hy
  subst hy
  intro hc
  cases hs : shares x.1 k with
  | false => exact disjoint_of_not_shares hs
  | true =>
    exfalso
    apply firstFree_not_mem (usedBy acc k)
    simp only [] at hc
    rw [← hc]
    unfold usedBy
    exact List.mem_map.mpr ⟨x, List.mem_filter.mpr ⟨hx, hs⟩, rfl⟩

theorem foldl_step_pairwise :
    ∀ (cs : List Constraint) (acc : List (Constraint × Nat)),
      acc.Pairwise Ok → (cs.foldl step acc).Pairwise Ok
  | [], _, h => h
  | k :: cs, acc, h => foldl_step_pairwise cs (step acc k) (step_pairwise acc k h)

theorem colorAll_pairwise_disjoint (cs : List Constraint) :
    (colorAll cs).Pairwise Ok :=
  foldl_step_pairwise cs [] List.Pairwise.nil

theorem foldl_step_fst :
    ∀ (cs : List Constraint) (acc : List (Constraint × Nat)),
      (cs.foldl step acc).map Prod.fst = acc.map Prod.fst ++ cs
  | [], acc => by simp
  | k :: cs, acc => by
    rw [List.foldl_cons, foldl_step_fst cs (step acc k)]
    simp [step]

theorem colorAll_constraints (cs : List Constraint) :
    (colorAll cs).map Prod.fst = cs := by
  simpa using foldl_step_fst cs []

/-- Index form: positions `i < j` with equal colours have disjoint bodies. -/
theorem same_color_constraints_disjoint_bodies (cs : List Constraint)
    (i j : Nat) (hij : i < j) (hj : j < (colorAll cs).length)
    (hcol : ((colorAll cs)[i]'(Nat.lt_trans hij hj)).2 = ((colorAll cs)[j]'hj).2) :
    Disjoint ((colorAll cs)[i]'(Nat.lt_trans hij hj)).1 ((colorAll cs)[j]'hj).1 :=
  (List.pairwise_iff_getElem.mp (colorAll_pairwise_disjoint cs) i j
    (Nat.lt_trans hij hj) hj hij) hcol

/-! ## k-body constraint batches (joints, contacts, soft-body tetrahedra)

A batch constraint is the list of bodies it touches: one body (a ground
contact), two (a contact or a joint), or four (a tetrahedral volume
constraint). The greedy colouring is the same choice over lists. -/

/-- A constraint touching the bodies in the list. -/
abbrev SetConstraint := List Nat

/-- Two body lists share a body. -/
def sharesSet (p q : SetConstraint) : Bool := p.any (fun x => q.contains x)

/-- No body of `p` is a body of `q`. -/
def DisjointSet (p q : SetConstraint) : Prop := ∀ x, x ∈ p → x ∉ q

theorem disjointSet_of_not_sharesSet {p q : SetConstraint} (h : sharesSet p q = false) :
    DisjointSet p q := by
  intro x hx hq
  have : sharesSet p q = true := by
    unfold sharesSet
    rw [List.any_eq_true]
    exact ⟨x, hx, List.elem_iff.mpr hq⟩
  rw [h] at this
  exact Bool.false_ne_true this

def usedBySet (acc : List (SetConstraint × Nat)) (k : SetConstraint) : List Nat :=
  (acc.filter (fun x => sharesSet x.1 k)).map (·.2)

def stepSet (acc : List (SetConstraint × Nat)) (k : SetConstraint) :
    List (SetConstraint × Nat) :=
  acc ++ [(k, firstFree (usedBySet acc k))]

def colorAllSets (cs : List SetConstraint) : List (SetConstraint × Nat) :=
  cs.foldl stepSet []

def OkSet (x y : SetConstraint × Nat) : Prop := x.2 = y.2 → DisjointSet x.1 y.1

theorem stepSet_pairwise (acc : List (SetConstraint × Nat)) (k : SetConstraint)
    (h : acc.Pairwise OkSet) : (stepSet acc k).Pairwise OkSet := by
  unfold stepSet
  rw [List.pairwise_append]
  refine ⟨h, List.pairwise_singleton _ _, ?_⟩
  intro x hx y hy
  simp only [List.mem_singleton] at hy
  subst hy
  intro hc
  cases hs : sharesSet x.1 k with
  | false => exact disjointSet_of_not_sharesSet hs
  | true =>
    exfalso
    apply firstFree_not_mem (usedBySet acc k)
    simp only [] at hc
    rw [← hc]
    unfold usedBySet
    exact List.mem_map.mpr ⟨x, List.mem_filter.mpr ⟨hx, hs⟩, rfl⟩

theorem foldl_stepSet_pairwise :
    ∀ (cs : List SetConstraint) (acc : List (SetConstraint × Nat)),
      acc.Pairwise OkSet → (cs.foldl stepSet acc).Pairwise OkSet
  | [], _, h => h
  | k :: cs, acc, h => foldl_stepSet_pairwise cs (stepSet acc k) (stepSet_pairwise acc k h)

/-- Same colour implies disjoint body lists, for constraints of any arity. -/
theorem colorAllSets_pairwise_disjoint (cs : List SetConstraint) :
    (colorAllSets cs).Pairwise OkSet :=
  foldl_stepSet_pairwise cs [] List.Pairwise.nil

theorem foldl_stepSet_fst :
    ∀ (cs : List SetConstraint) (acc : List (SetConstraint × Nat)),
      (cs.foldl stepSet acc).map Prod.fst = acc.map Prod.fst ++ cs
  | [], acc => by simp
  | k :: cs, acc => by
    rw [List.foldl_cons, foldl_stepSet_fst cs (stepSet acc k)]
    simp [stepSet]

theorem colorAllSets_constraints (cs : List SetConstraint) :
    (colorAllSets cs).map Prod.fst = cs := by
  simpa using foldl_stepSet_fst cs []

/-- Index form for batches of any arity. -/
theorem same_color_sets_disjoint_bodies (cs : List SetConstraint)
    (i j : Nat) (hij : i < j) (hj : j < (colorAllSets cs).length)
    (hcol : ((colorAllSets cs)[i]'(Nat.lt_trans hij hj)).2 = ((colorAllSets cs)[j]'hj).2) :
    DisjointSet ((colorAllSets cs)[i]'(Nat.lt_trans hij hj)).1 ((colorAllSets cs)[j]'hj).1 :=
  (List.pairwise_iff_getElem.mp (colorAllSets_pairwise_disjoint cs) i j
    (Nat.lt_trans hij hj) hj hij) hcol

/-- A pair constraint as a body list. -/
def pairSet (p : Constraint) : SetConstraint := [p.1, p.2]

theorem shares_eq_sharesSet (p q : Constraint) :
    shares p q = sharesSet (pairSet p) (pairSet q) := by
  simp only [shares, sharesSet, pairSet, List.any, List.contains, List.elem]
  cases p.1 == q.1 <;> cases p.1 == q.2 <;> cases p.2 == q.1 <;> cases p.2 == q.2 <;> rfl

theorem usedBy_eq_usedBySet :
    ∀ (acc : List (Constraint × Nat)) (k : Constraint),
      usedBy acc k = usedBySet (acc.map (fun x => (pairSet x.1, x.2))) (pairSet k)
  | [], _ => rfl
  | x :: acc, k => by
    have ih := usedBy_eq_usedBySet acc k
    unfold usedBy usedBySet at *
    simp only [List.map_cons, List.filter_cons]
    rw [← shares_eq_sharesSet]
    split <;> simp [ih]

theorem foldl_step_eq_stepSet :
    ∀ (cs : List Constraint) (acc : List (Constraint × Nat)),
      (cs.foldl step acc).map (fun x => (pairSet x.1, x.2)) =
        (cs.map pairSet).foldl stepSet (acc.map (fun x => (pairSet x.1, x.2)))
  | [], _ => rfl
  | k :: cs, acc => by
    simp only [List.foldl_cons, List.map_cons]
    rw [foldl_step_eq_stepSet cs (step acc k)]
    congr 1
    simp [step, stepSet, usedBy_eq_usedBySet]

/-- The pair colouring is the batch colouring of the pairs' body lists:
joints and contacts coloured as two-body batches get the colours the
original model gives them. -/
theorem colorAll_eq_colorAllSets (cs : List Constraint) :
    (colorAll cs).map (·.2) = (colorAllSets (cs.map pairSet)).map (·.2) := by
  have h := congrArg (List.map Prod.snd) (foldl_step_eq_stepSet cs [])
  simpa [colorAll, colorAllSets, List.map_map, Function.comp_def] using h

/-! ## Morton interleaving -/

def interleave : Nat → Nat → Nat → Nat → Nat
  | 0, _, _, _ => 0
  | n + 1, x, y, z =>
    x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2)

/-- Gather bits `0, 3, 6, ...` of `m` into consecutive bits. -/
def gather : Nat → Nat → Nat
  | 0, _ => 0
  | n + 1, m => m % 2 + 2 * gather n (m / 8)

def deinterleaveX (n m : Nat) : Nat := gather n m
def deinterleaveY (n m : Nat) : Nat := gather n (m / 2)
def deinterleaveZ (n m : Nat) : Nat := gather n (m / 4)

theorem mod_two_mul (x m : Nat) : x % (2 * m) = x % 2 + 2 * (x / 2 % m) :=
  Nat.mod_mul

theorem gather_interleave_x :
    ∀ (n x y z : Nat), gather n (interleave n x y z) = x % 2 ^ n
  | 0, x, _, _ => by simp [gather, Nat.mod_one]
  | n + 1, x, y, z => by
    simp only [gather, interleave]
    have h1 : (x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2)) % 2
        = x % 2 := by omega
    have h2 : (x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2)) / 8
        = interleave n (x / 2) (y / 2) (z / 2) := by omega
    rw [h1, h2, gather_interleave_x n (x / 2) (y / 2) (z / 2), Nat.pow_succ, Nat.mul_comm _ 2,
      mod_two_mul]

theorem morton_deinterleave_interleave (n x y z : Nat) :
    deinterleaveX n (interleave n x y z) = x % 2 ^ n ∧
    deinterleaveY n (interleave n x y z) = y % 2 ^ n ∧
    deinterleaveZ n (interleave n x y z) = z % 2 ^ n := by
  refine ⟨gather_interleave_x n x y z, ?_, ?_⟩
  · unfold deinterleaveY
    induction n generalizing x y z with
    | zero => simp [gather, Nat.mod_one]
    | succ n ih =>
      simp only [gather, interleave]
      have h1 : (x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2))
          / 2 % 2 = y % 2 := by omega
      have h2 : (x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2))
          / 2 / 8 = interleave n (x / 2) (y / 2) (z / 2) / 2 := by omega
      rw [h1, h2, ih (x / 2) (y / 2) (z / 2), Nat.pow_succ, Nat.mul_comm _ 2, mod_two_mul]
  · unfold deinterleaveZ
    induction n generalizing x y z with
    | zero => simp [gather, Nat.mod_one]
    | succ n ih =>
      simp only [gather, interleave]
      have h1 : (x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2))
          / 4 % 2 = z % 2 := by omega
      have h2 : (x % 2 + 2 * (y % 2) + 4 * (z % 2) + 8 * interleave n (x / 2) (y / 2) (z / 2))
          / 4 / 8 = interleave n (x / 2) (y / 2) (z / 2) / 4 := by omega
      rw [h1, h2, ih (x / 2) (y / 2) (z / 2), Nat.pow_succ, Nat.mul_comm _ 2, mod_two_mul]

theorem morton_10bit_roundtrip (x y z : Nat)
    (hx : x < 1024) (hy : y < 1024) (hz : z < 1024) :
    deinterleaveX 10 (interleave 10 x y z) = x ∧
    deinterleaveY 10 (interleave 10 x y z) = y ∧
    deinterleaveZ 10 (interleave 10 x y z) = z := by
  have h := morton_deinterleave_interleave 10 x y z
  have p : (2 : Nat) ^ 10 = 1024 := by decide
  rw [p, Nat.mod_eq_of_lt hx, Nat.mod_eq_of_lt hy, Nat.mod_eq_of_lt hz] at h
  exact h

theorem morton_10bit_injective (x y z x' y' z' : Nat)
    (hx : x < 1024) (hy : y < 1024) (hz : z < 1024)
    (hx' : x' < 1024) (hy' : y' < 1024) (hz' : z' < 1024)
    (h : interleave 10 x y z = interleave 10 x' y' z') :
    x = x' ∧ y = y' ∧ z = z' := by
  have a := morton_10bit_roundtrip x y z hx hy hz
  have b := morton_10bit_roundtrip x' y' z' hx' hy' hz'
  rw [h] at a
  exact ⟨a.1.symm.trans b.1, a.2.1.symm.trans b.2.1, a.2.2.symm.trans b.2.2⟩

theorem interleave_lt : ∀ (n x y z : Nat), interleave n x y z < 2 ^ (3 * n)
  | 0, _, _, _ => by simp [interleave]
  | n + 1, x, y, z => by
    have ih := interleave_lt n (x / 2) (y / 2) (z / 2)
    have e : 2 ^ (3 * (n + 1)) = 8 * 2 ^ (3 * n) := by
      rw [show 3 * (n + 1) = 3 * n + 3 by omega, Nat.pow_add]
      omega
    simp only [interleave]
    rw [e]
    omega

theorem morton_21bit_roundtrip (x y z : Nat)
    (hx : x < 2097152) (hy : y < 2097152) (hz : z < 2097152) :
    deinterleaveX 21 (interleave 21 x y z) = x ∧
    deinterleaveY 21 (interleave 21 x y z) = y ∧
    deinterleaveZ 21 (interleave 21 x y z) = z := by
  have h := morton_deinterleave_interleave 21 x y z
  have p : (2 : Nat) ^ 21 = 2097152 := by decide
  rw [p, Nat.mod_eq_of_lt hx, Nat.mod_eq_of_lt hy, Nat.mod_eq_of_lt hz] at h
  exact h

theorem morton_21bit_injective (x y z x' y' z' : Nat)
    (hx : x < 2097152) (hy : y < 2097152) (hz : z < 2097152)
    (hx' : x' < 2097152) (hy' : y' < 2097152) (hz' : z' < 2097152)
    (h : interleave 21 x y z = interleave 21 x' y' z') :
    x = x' ∧ y = y' ∧ z = z' := by
  have a := morton_21bit_roundtrip x y z hx hy hz
  have b := morton_21bit_roundtrip x' y' z' hx' hy' hz'
  rw [h] at a
  exact ⟨a.1.symm.trans b.1, a.2.1.symm.trans b.2.1, a.2.2.symm.trans b.2.2⟩

theorem morton_21bit_lt (x y z : Nat) : interleave 21 x y z < 2 ^ 63 :=
  interleave_lt 21 x y z

/-! ## LBVH key order -/

/-- Keys between two keys that agree above bit `k` agree with both. -/
theorem sorted_prefix_between {k a b c : Nat} (hab : a ≤ b) (hbc : b ≤ c)
    (hac : a / 2 ^ k = c / 2 ^ k) :
    a / 2 ^ k = b / 2 ^ k ∧ b / 2 ^ k = c / 2 ^ k := by
  have h1 : a / 2 ^ k ≤ b / 2 ^ k := Nat.div_le_div_right hab
  have h2 : b / 2 ^ k ≤ c / 2 ^ k := Nat.div_le_div_right hbc
  omega

/-- For a sorted array (`key` monotone in the index), the indices whose keys
share the prefix of `key i` above bit `k` are contiguous: if `key m` shares
it, so does every `key j` with `i ≤ j ≤ m`. -/
theorem monotone_keys_prefix_range (key : Nat → Nat)
    (mono : ∀ i j, i ≤ j → key i ≤ key j) {k i j m : Nat}
    (hij : i ≤ j) (hjm : j ≤ m) (h : key i / 2 ^ k = key m / 2 ^ k) :
    key i / 2 ^ k = key j / 2 ^ k :=
  (sorted_prefix_between (mono i j hij) (mono j m hjm) h).1

/-- The LBVH sort key: Morton code, then primitive index (below `2^32`). -/
def augKey (code index : Nat) : Nat := code * 4294967296 + index

theorem augKey_lt_iff {c1 c2 i1 i2 : Nat} (h1 : i1 < 4294967296) (h2 : i2 < 4294967296) :
    augKey c1 i1 < augKey c2 i2 ↔ c1 < c2 ∨ (c1 = c2 ∧ i1 < i2) := by
  unfold augKey
  constructor
  · intro h
    by_cases hc : c1 < c2
    · exact Or.inl hc
    · right
      have : c1 = c2 := by
        rcases Nat.lt_or_ge c2 c1 with g | g
        · have : c2 * 4294967296 + 4294967296 ≤ c1 * 4294967296 := by
            have := Nat.mul_le_mul_right 4294967296 g
            simpa [Nat.succ_mul] using this
          omega
        · omega
      subst this
      exact ⟨rfl, by omega⟩
  · intro h
    rcases h with hc | ⟨rfl, hi⟩
    · have : c1 * 4294967296 + 4294967296 ≤ c2 * 4294967296 := by
        have := Nat.mul_le_mul_right 4294967296 hc
        simpa [Nat.succ_mul] using this
      omega
    · omega

theorem augKey_div {c i : Nat} (h : i < 4294967296) : augKey c i / 4294967296 = c := by
  unfold augKey
  omega

theorem augKey_injective {c1 c2 i1 i2 : Nat} (h1 : i1 < 4294967296) (h2 : i2 < 4294967296)
    (h : augKey c1 i1 = augKey c2 i2) : c1 = c2 ∧ i1 = i2 := by
  have e1 := augKey_div (c := c1) h1
  have e2 := augKey_div (c := c2) h2
  rw [h] at e1
  have hc : c1 = c2 := e1.symm.trans e2
  subst hc
  unfold augKey at h
  exact ⟨rfl, by omega⟩

#print axioms colorAll_pairwise_disjoint
#print axioms colorAll_constraints
#print axioms same_color_constraints_disjoint_bodies
#print axioms colorAllSets_pairwise_disjoint
#print axioms colorAllSets_constraints
#print axioms same_color_sets_disjoint_bodies
#print axioms colorAll_eq_colorAllSets
#print axioms morton_deinterleave_interleave
#print axioms morton_10bit_roundtrip
#print axioms morton_10bit_injective
#print axioms interleave_lt
#print axioms morton_21bit_roundtrip
#print axioms morton_21bit_injective
#print axioms morton_21bit_lt
#print axioms sorted_prefix_between
#print axioms monotone_keys_prefix_range
#print axioms augKey_lt_iff
#print axioms augKey_div
#print axioms augKey_injective

end Vesper.KineticColoring
