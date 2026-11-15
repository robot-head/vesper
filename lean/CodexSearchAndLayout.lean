-- Vesper Codex (§19.4): Substructural Hoogle search soundness and
-- contiguous zero-hole wire struct byte-ruler coverage.
--
-- What is proved, and about which model:
-- * `codex_hoogle_mode_match_sound`: contravariant input rank and covariant
--   return rank substitutability under `[T-SubMode]`.
-- * `modeSearchMatches_sound`: concrete 3-tier `ModeRank` (`copy`, `affine`,
--   `linear`) and effect-budget matcher soundness.
-- * `codex_wire_diagram_zero_holes`: adjacent two-field byte-ruler partition
--   exactness.
-- * `wire_spans_zero_holes`: general N-field `WireSpan` contiguity theorem
--   establishing zero padding holes across arbitrary wire structs.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.CodexSearchAndLayout

inductive ModeRank where
  | copy
  | affine
  | linear
  deriving DecidableEq, Repr

def ModeRank.rank : ModeRank → Nat
  | .copy => 0
  | .affine => 1
  | .linear => 2

def modeSearchMatches
    (qParam qRet fnParam fnRet : ModeRank) (qEffCeiling fnEffCost : Nat) : Bool :=
  decide (qParam.rank ≤ fnParam.rank ∧ fnRet.rank ≤ qRet.rank ∧ fnEffCost ≤ qEffCeiling)

/-- §19.4 Substructural Hoogle Search Soundness. -/
theorem codex_hoogle_mode_match_sound
    (qParam fnParam fnRet qRet callerArgRank : Nat)
    (h_caller_has : callerArgRank ≤ qParam)
    (h_param_contra : qParam ≤ fnParam)
    (h_ret_cov : fnRet ≤ qRet) :
    callerArgRank ≤ fnParam ∧ fnRet ≤ qRet := by
  omega

theorem modeSearchMatches_sound
    (qParam qRet fnParam fnRet : ModeRank)
    (qEffCeiling fnEffCost callerArgRank : Nat)
    (h_caller_has : callerArgRank ≤ qParam.rank)
    (h_match : modeSearchMatches qParam qRet fnParam fnRet qEffCeiling fnEffCost = true) :
    callerArgRank ≤ fnParam.rank ∧ fnRet.rank ≤ qRet.rank ∧ fnEffCost ≤ qEffCeiling := by
  simp only [modeSearchMatches, decide_eq_true_eq] at h_match
  omega

/-- §19.4 Wire-Struct Byte Ruler Partition Exactness. -/
theorem codex_wire_diagram_zero_holes
    (off0 len0 off1 len1 : Nat) (h_contig : off1 = off0 + len0) :
    (off1 - off0) + len1 = len0 + len1 ∧ off0 + len0 ≤ off1 := by
  omega

structure WireSpan where
  offset : Nat
  size : Nat
  deriving DecidableEq, Repr

def contiguousSpans : Nat → List WireSpan → Nat → Prop
  | cursor, [], totalEnd => cursor = totalEnd
  | cursor, s :: rest, totalEnd =>
      s.offset = cursor ∧ contiguousSpans (cursor + s.size) rest totalEnd

def checkContiguousSpans : Nat → List WireSpan → Nat → Bool
  | cursor, [], totalEnd => cursor == totalEnd
  | cursor, s :: rest, totalEnd =>
      s.offset == cursor && checkContiguousSpans (cursor + s.size) rest totalEnd

def totalWireSize : List WireSpan → Nat
  | [] => 0
  | s :: rest => s.size + totalWireSize rest

theorem checkContiguousSpans_sound :
    ∀ (spans : List WireSpan) (cursor totalEnd : Nat),
      checkContiguousSpans cursor spans totalEnd = true →
      contiguousSpans cursor spans totalEnd
  | [], cursor, totalEnd, h => by simpa [checkContiguousSpans, contiguousSpans] using h
  | s :: rest, cursor, totalEnd, h => by
      simp only [checkContiguousSpans, Bool.and_eq_true, beq_iff_eq] at h
      exact ⟨h.1, checkContiguousSpans_sound rest (cursor + s.size) totalEnd h.2⟩

theorem contiguous_spans_total :
    ∀ (spans : List WireSpan) (cursor totalEnd : Nat),
      contiguousSpans cursor spans totalEnd →
      cursor + totalWireSize spans = totalEnd
  | [], cursor, totalEnd, h => by simp [contiguousSpans, totalWireSize] at h ⊢; omega
  | s :: rest, cursor, totalEnd, h => by
      obtain ⟨_, hrest⟩ := h
      have ih := contiguous_spans_total rest (cursor + s.size) totalEnd hrest
      simp only [totalWireSize]
      omega

theorem wire_spans_zero_holes (spans : List WireSpan) (totalEnd : Nat)
    (h : checkContiguousSpans 0 spans totalEnd = true) :
    totalWireSize spans = totalEnd := by
  have := contiguous_spans_total spans 0 totalEnd (checkContiguousSpans_sound spans 0 totalEnd h)
  omega

end Vesper.CodexSearchAndLayout
