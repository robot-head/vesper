-- Vesper Chroma (design §43.4): lossless source partition and contrast gates.
--
-- What is proved, and about which model:
-- * Spans are modelled as a list of `(start, end)` pairs of natural numbers
--   and the source as a list of bytes. `verify` is the model of the Rust
--   `verify_lossless_spans` (crates/vesper-highlight/src/lib.rs): it accepts
--   exactly the span lists that start at the cursor, are contiguous and
--   non-decreasing, and end at the source length. For an accepted list the
--   span lengths sum to the source length, and concatenating the slices
--   reconstructs the source exactly (`chroma_verify_reconstructs`).
-- * `gate` is the model of the Rust integer contrast test
--   (`palette.rs`, `gate`) over luminance thousandths:
--   `den * (max + 50) >= num * (min + 50)`. `verifyGates` models
--   `ChromaPalette::verify_contrast_gates` (7:1 for primary colours, 4.5:1
--   for comments, against the card background). The palettes' computed
--   luminance values are checked by `decide` at the end of the file.
-- These are theorems about these Lean models, not about the Rust code or
-- about perceived contrast; the Rust tests check the code against the same
-- numbers. No `sorry`, no `admit`, no axioms beyond Lean's core.
namespace Vesper.Chroma

/-! ## The two theorems listed in §43.4 -/

theorem chroma_contiguous_spans_lossless
    (s0 e0 s1 e1 srcLen : Nat)
    (h_start : s0 = 0) (h_le0 : s0 <= e0) (h_join : e0 = s1)
    (h_le1 : s1 <= e1) (h_end : e1 = srcLen) :
    (e0 - s0) + (e1 - s1) = srcLen := by
  omega

theorem chroma_wcag_aaa_gate_sound
    (lumFg lumBg : Nat)
    (h_gate : (Nat.max lumFg lumBg + 50) >= 7 * (Nat.min lumFg lumBg + 50)) :
    let lMax := Nat.max lumFg lumBg
    let lMin := Nat.min lumFg lumBg
    lMax >= lMin /\ (lMax + 50) >= 7 * (lMin + 50) := by
  constructor
  · omega
  · exact h_gate

/-! ## Lists of contiguous spans -/

/-- `Contiguous c spans e`: the spans start at `c`, each starts where the
previous one ended, none runs backwards, and the last ends at `e`. -/
def Contiguous : Nat → List (Nat × Nat) → Nat → Prop
  | c, [], e => c = e
  | c, (s, t) :: rest, e => s = c ∧ s ≤ t ∧ Contiguous t rest e

/-- The model of `verify_lossless_spans`, started at cursor `c`. -/
def verify : Nat → List (Nat × Nat) → Nat → Bool
  | c, [], n => c == n
  | c, (s, t) :: rest, n => s == c && decide (s ≤ t) && verify t rest n

def totalLen : List (Nat × Nat) → Nat
  | [] => 0
  | (s, t) :: rest => (t - s) + totalLen rest

/-- Concatenation of the slices `src[s..t]`. -/
def slices (src : List α) : List (Nat × Nat) → List α
  | [] => []
  | (s, t) :: rest => (src.drop s).take (t - s) ++ slices src rest

theorem verify_sound : ∀ (spans : List (Nat × Nat)) (c n : Nat),
    verify c spans n = true → Contiguous c spans n
  | [], c, n, h => by simpa [verify, Contiguous] using h
  | (s, t) :: rest, c, n, h => by
    simp only [verify, Bool.and_eq_true, beq_iff_eq, decide_eq_true_eq] at h
    exact ⟨h.1.1, h.1.2, verify_sound rest t n h.2⟩

theorem verify_complete : ∀ (spans : List (Nat × Nat)) (c n : Nat),
    Contiguous c spans n → verify c spans n = true
  | [], c, n, h => by simpa [verify, Contiguous] using h
  | (s, t) :: rest, c, n, h => by
    simp only [verify, Bool.and_eq_true, beq_iff_eq, decide_eq_true_eq]
    exact ⟨⟨h.1, h.2.1⟩, verify_complete rest t n h.2.2⟩

theorem contiguous_total : ∀ (spans : List (Nat × Nat)) (c e : Nat),
    Contiguous c spans e → c + totalLen spans = e
  | [], c, e, h => by simp [Contiguous, totalLen] at h ⊢; omega
  | (s, t) :: rest, c, e, h => by
    obtain ⟨hs, hle, hrest⟩ := h
    have ih := contiguous_total rest t e hrest
    simp only [totalLen]
    omega

/-- Generalised §43.4 length theorem: accepted spans cover the source length. -/
theorem chroma_spans_lossless (spans : List (Nat × Nat)) (srcLen : Nat)
    (h : verify 0 spans srcLen = true) : totalLen spans = srcLen := by
  have := contiguous_total spans 0 srcLen (verify_sound spans 0 srcLen h)
  omega

theorem contiguous_slices (src : List α) : ∀ (spans : List (Nat × Nat)) (c : Nat),
    Contiguous c spans src.length → slices src spans = src.drop c
  | [], c, h => by
    simp only [Contiguous] at h
    simp [slices, h]
  | (s, t) :: rest, c, h => by
    obtain ⟨hs, hle, hrest⟩ := h
    subst hs
    have ih := contiguous_slices src rest t hrest
    simp only [slices, ih]
    have ht : t = s + (t - s) := by omega
    conv => rhs; rw [← List.take_append_drop (t - s) (src.drop s)]
    rw [List.drop_drop, ← ht]

/-- Byte-exact clipboard fidelity on the model: concatenating the slices of
accepted spans gives back the source. -/
theorem chroma_verify_reconstructs (src : List α) (spans : List (Nat × Nat))
    (h : verify 0 spans src.length = true) : slices src spans = src := by
  have := contiguous_slices src spans 0 (verify_sound spans 0 src.length h)
  simpa using this

/-! ## The integer contrast gate the Rust code uses -/

/-- `den * (max a b + 50) ≥ num * (min a b + 50)`. -/
def gate (a b num den : Nat) : Bool :=
  decide (num * (Nat.min a b + 50) ≤ den * (Nat.max a b + 50))

theorem gate_symm (a b num den : Nat) : gate a b num den = gate b a num den := by
  simp [gate, Nat.min_comm, Nat.max_comm]

theorem gate_sound (a b num den : Nat) (h : gate a b num den = true) :
    Nat.min a b ≤ Nat.max a b ∧ num * (Nat.min a b + 50) ≤ den * (Nat.max a b + 50) := by
  simp only [gate, decide_eq_true_eq] at h
  exact ⟨Nat.le_trans (Nat.min_le_left a b) (Nat.le_max_left a b), h⟩

/-- The primary-token gate is §43.4's WCAG AAA statement. -/
theorem gate_aaa_sound (fg bg : Nat) (h : gate fg bg 7 1 = true) :
    Nat.max fg bg >= Nat.min fg bg ∧ (Nat.max fg bg + 50) >= 7 * (Nat.min fg bg + 50) := by
  have := gate_sound fg bg 7 1 h
  omega

/-- The comment gate: ratio at least 9/2 = 4.5. -/
theorem gate_comment_sound (fg bg : Nat) (h : gate fg bg 9 2 = true) :
    2 * (Nat.max fg bg + 50) >= 9 * (Nat.min fg bg + 50) := by
  have := gate_sound fg bg 9 2 h
  omega

/-- Model of `verify_contrast_gates`. -/
def verifyGates (bg : Nat) (primary : List Nat) (comment : Nat) : Bool :=
  primary.all (fun l => gate l bg 7 1) && gate comment bg 9 2

theorem verifyGates_sound (bg : Nat) (primary : List Nat) (comment : Nat)
    (h : verifyGates bg primary comment = true) :
    (∀ l ∈ primary, (Nat.max l bg + 50) >= 7 * (Nat.min l bg + 50)) ∧
    2 * (Nat.max comment bg + 50) >= 9 * (Nat.min comment bg + 50) := by
  simp only [verifyGates, Bool.and_eq_true, List.all_eq_true] at h
  exact ⟨fun l hl => (gate_aaa_sound l bg (h.1 l hl)).2, gate_comment_sound comment bg h.2⟩

/-! ## The shipped palettes (luminance thousandths computed by the Rust code;
`tests/highlight_lean.rs` checks these literals against it) -/

-- vesper-dusk: bg_card, [fg, linear, inout, effect, wire, keyword], comment
theorem dusk_gates : verifyGates 8 [805, 545, 526, 482, 639, 433] 309 = true := by decide
-- vesper-dawn
theorem dawn_gates : verifyGates 941 [15, 87, 88, 88, 88, 88] 117 = true := by decide

-- §43.3's original Dawn accents fail the 7:1 gate (they reach 4.7-6.6:1).
theorem design_dawn_accents_fail : verifyGates 941 [15, 162, 121, 100, 129, 112] 117 = false := by
  decide

end Vesper.Chroma
