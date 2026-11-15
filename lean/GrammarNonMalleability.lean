-- Vesper Grammar (§30.4): Strong non-malleability from roundtrip identity and
-- canonical LEB128 varint non-overlong uniqueness.
--
-- What is proved, and about which model:
-- * `canonical_roundtrip_implies_non_malleable`: generic proof over `ByteArray`
--   that whenever `serialize (parse b) = b` for all accepted encodings, two
--   distinct byte encodings cannot parse to the same message.
-- * `canonical_leb128_2byte_rejects_overlong`: proves any canonical 2-byte
--   varint with minimal non-zero final byte decodes to at least `128`.
-- * `decodeCanonicalLeb` / `encodeCanonicalLeb`: executable canonical unsigned
--   LEB128 codec rejecting overlong zero-padded continuations, proven injective
--   (`canonical_leb128_injective`) across arbitrary-length encodings.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.GrammarNonMalleability

/-- §30.4 Strong Non-Malleability from Serializer-After-Parser Identity. -/
theorem canonical_roundtrip_implies_non_malleable
    {Msg : Type}
    (parse : ByteArray → Option Msg)
    (serialize : Msg → ByteArray)
    (h_ser_parse : ∀ b v, parse b = some v → serialize v = b)
    (b1 b2 : ByteArray) (v : Msg)
    (hp1 : parse b1 = some v) (hp2 : parse b2 = some v) :
    b1 = b2 := by
  have h1 : serialize v = b1 := h_ser_parse b1 v hp1
  have h2 : serialize v = b2 := h_ser_parse b2 v hp2
  rw [← h1, ← h2]

/-- §30.4 Canonical 2-Byte Varint Lower-Bound Guard: values `< 128` must use 1 byte. -/
theorem canonical_leb128_2byte_rejects_overlong
    (b0 b1 : Nat)
    (_h_cont : 128 ≤ b0) (_h_b0_u8 : b0 < 256)
    (_h_last : b1 < 128)
    (h_minimal : b1 ≠ 0) :
    let decoded := (b0 - 128) + b1 * 128
    decoded ≥ 128 := by
  omega

def stepDecode (b : Nat) : Option Nat → Option Nat
  | some hi =>
      if 128 ≤ b ∧ b < 256 ∧ hi ≠ 0 then some ((b - 128) + 128 * hi) else none
  | none => none

/-- Executable canonical unsigned LEB128 decoder matching `crates/vesper-grammar`. -/
def decodeCanonicalLeb : List Nat → Option Nat
  | [] => none
  | [b] => if b < 128 then some b else none
  | b :: b' :: rest => stepDecode b (decodeCanonicalLeb (b' :: rest))

/-- Executable canonical unsigned LEB128 encoder. -/
def encodeCanonicalLeb (n : Nat) : List Nat :=
  if n < 128 then [n]
  else (n % 128 + 128) :: encodeCanonicalLeb (n / 128)
termination_by n
decreasing_by simp_wf; exact Nat.div_lt_self (by omega) (by decide)

theorem stepDecode_eq_some {b : Nat} {r : Option Nat} {v : Nat}
    (h : stepDecode b r = some v) :
    ∃ hi, r = some hi ∧ 128 ≤ b ∧ b < 256 ∧ hi ≠ 0 ∧ v = (b - 128) + 128 * hi := by
  cases r with
  | none => simp [stepDecode] at h
  | some hi =>
      simp only [stepDecode] at h
      split at h
      · rename_i hc
        injection h with hv
        exact ⟨hi, rfl, hc.1, hc.2.1, hc.2.2, hv.symm⟩
      · contradiction

theorem decodeCanonicalLeb_ge_128 {b0 b1 : Nat} {rest : List Nat} {v : Nat}
    (h : decodeCanonicalLeb (b0 :: b1 :: rest) = some v) : 128 ≤ v := by
  obtain ⟨hi, _, _, _, hne, hv⟩ := stepDecode_eq_some h
  have h1 : 1 ≤ hi := Nat.pos_of_ne_zero hne
  have h2 : 128 ≤ 128 * hi := by
    calc
      128 = 128 * 1 := by omega
      _ ≤ 128 * hi := Nat.mul_le_mul_left 128 h1
  omega

/-- Any two accepted canonical LEB128 byte lists decoding to the same value are identical. -/
theorem canonical_leb128_injective :
    ∀ (b1 b2 : List Nat) (v : Nat),
      decodeCanonicalLeb b1 = some v →
      decodeCanonicalLeb b2 = some v →
      b1 = b2
  | [], _, _, h1, _ => by simp [decodeCanonicalLeb] at h1
  | _, [], _, _, h2 => by simp [decodeCanonicalLeb] at h2
  | [x], [y], v, h1, h2 => by
      simp only [decodeCanonicalLeb] at h1 h2
      by_cases hx_lt : x < 128
      · rw [if_pos hx_lt] at h1
        by_cases hy_lt : y < 128
        · rw [if_pos hy_lt] at h2
          injection h1 with hx
          injection h2 with hy
          subst hx; subst hy; rfl
        · rw [if_neg hy_lt] at h2
          contradiction
      · rw [if_neg hx_lt] at h1
        contradiction
  | [x], y0 :: y1 :: ys, v, h1, h2 => by
      have hy := decodeCanonicalLeb_ge_128 h2
      simp only [decodeCanonicalLeb] at h1
      by_cases hx_lt : x < 128
      · rw [if_pos hx_lt] at h1
        injection h1 with hx
        omega
      · rw [if_neg hx_lt] at h1
        contradiction
  | x0 :: x1 :: xs, [y], v, h1, h2 => by
      have hx := decodeCanonicalLeb_ge_128 h1
      simp only [decodeCanonicalLeb] at h2
      by_cases hy_lt : y < 128
      · rw [if_pos hy_lt] at h2
        injection h2 with hy
        omega
      · rw [if_neg hy_lt] at h2
        contradiction
  | x0 :: x1 :: xs, y0 :: y1 :: ys, v, h1, h2 => by
      obtain ⟨hx_hi, hdec1, hx_lo, hx_u8, _, hv1⟩ := stepDecode_eq_some h1
      obtain ⟨hy_hi, hdec2, hy_lo, hy_u8, _, hv2⟩ := stepDecode_eq_some h2
      have hpair : x0 = y0 ∧ hx_hi = hy_hi := by omega
      have hdec2' : decodeCanonicalLeb (y1 :: ys) = some hx_hi := by
        rw [hpair.2]
        exact hdec2
      have htail := canonical_leb128_injective (x1 :: xs) (y1 :: ys) hx_hi hdec1 hdec2'
      rw [hpair.1, htail]

/-- Boundary non-malleability vector checks discharged by simplification. -/
theorem canonical_leb128_boundary_vectors :
    decodeCanonicalLeb [] = none ∧
    decodeCanonicalLeb [0x80] = none ∧
    decodeCanonicalLeb [0x80, 0x00] = none ∧
    decodeCanonicalLeb [0xff, 0x00] = none ∧
    decodeCanonicalLeb [0x81, 0x80, 0x00] = none ∧
    decodeCanonicalLeb (encodeCanonicalLeb 0) = some 0 ∧
    decodeCanonicalLeb (encodeCanonicalLeb 127) = some 127 ∧
    decodeCanonicalLeb (encodeCanonicalLeb 128) = some 128 ∧
    decodeCanonicalLeb (encodeCanonicalLeb 16384) = some 16384 := by
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩ <;>
    simp [encodeCanonicalLeb, decodeCanonicalLeb, stepDecode]

end Vesper.GrammarNonMalleability
