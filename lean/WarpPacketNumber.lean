/-!
# RFC 9000 Appendix A.3 packet-number decoder: arithmetic model

Model identifier: `vesper.warp.packet-number.a3-model/1`.

This file models `decode_quic_packet_number_rfc9000` from
`crates/vesper-warp/src/packet_number.rs` over unbounded `Nat`. It is **not** a
proof about the Rust code, its `u64` arithmetic, header-protection removal or
anything else in the QUIC stack; the Rust crate checks agreement with this
model on concrete inputs only (`crates/vesper-warp/tests/lean_model.rs`).

The branch structure is RFC 9000 Appendix A.3, with the two signed
comparisons rewritten so that no natural-number subtraction can truncate:

```text
expected  = largest + 1         (0 when nothing was received yet)
win       = 2^nbits, hwin = win / 2
candidate = (expected & ~(win - 1)) | truncated   -- here: expected / win * win + truncated
if candidate + hwin ≤ expected ∧ candidate + win < 2^62 then candidate + win
else if candidate > expected + hwin ∧ candidate ≥ win then candidate - win
else candidate
```

`candidate <= expected - hwin` (RFC, signed) is `candidate + hwin ≤ expected`
here; `candidate < 2^62 - win` is `candidate + win < 2^62`. The model returns
`none` for `nbits ∉ {8, 16, 24, 32}`, a truncated value that does not fit in
`nbits`, `expected > 2^62`, or a reconstructed value `≥ 2^62` (packet numbers
are below 2^62, RFC 9000 §12.3).

Proved (core Lean only: `omega`, `simp`, `decide`; no `sorry`, no axiom):

* `decode_congruent`: any `some r` result satisfies `r % 2^nbits = truncated`.
* `decode_bound`: any `some r` result satisfies `r < 2^62`.
* `decode_window`: if the true packet number `p` satisfies
  `expected < p + hwin`, `p ≤ expected + hwin`, `p < 2^62`, and
  `truncated = p % 2^nbits` for a valid `nbits`, then the decoder returns
  `some p`. This is RFC 9000 §17.1's recovery claim for the modelled function.
* `decode_rejects_invalid_bits`: invalid widths return `none`.
* `rfc_example`: the RFC A.3 example (`largest = 0xa82f30ea`, 16-bit
  `0x9b32`) decodes to `0xa82f9b32`.
-/

namespace Vesper.WarpPacketNumber

def modelId : String := "vesper.warp.packet-number.a3-model/1"
def modelClaim : String :=
  "RFC 9000 A.3 arithmetic over Nat only; not a proof about the Rust u64 code"

/-- 2^62: packet numbers are strictly below this. -/
def pnBound : Nat := 4611686018427387904

theorem pnBound_eq : pnBound = 2 ^ 62 := by decide

def validBits (nbits : Nat) : Bool :=
  nbits == 8 || nbits == 16 || nbits == 24 || nbits == 32

/-- The A.3 branch structure for a fixed window `win = 2^nbits`. -/
def reconstruct (expected truncated win : Nat) : Nat :=
  let hwin := win / 2
  let candidate := expected / win * win + truncated
  if candidate + hwin ≤ expected ∧ candidate + win < pnBound then candidate + win
  else if candidate > expected + hwin ∧ candidate ≥ win then candidate - win
  else candidate

/-- `expected` is `largest + 1`, or `0` before any packet in the space. -/
def decode (expected truncated nbits : Nat) : Option Nat :=
  if validBits nbits ∧ truncated < 2 ^ nbits ∧ expected ≤ pnBound then
    let r := reconstruct expected truncated (2 ^ nbits)
    if r < pnBound then some r else none
  else none

/-- Executable entry point for the differential fixture. -/
def decodeLargest (largest : Option Nat) (truncated nbits : Nat) : Option Nat :=
  decode (match largest with | none => 0 | some l => l + 1) truncated nbits

theorem validBits_cases {nbits : Nat} (h : validBits nbits = true) :
    nbits = 8 ∨ nbits = 16 ∨ nbits = 24 ∨ nbits = 32 := by
  simp [validBits] at h
  omega

/-- Congruence for one literal window. -/
theorem reconstruct_mod (expected t win : Nat) (ht : t < win) :
    reconstruct expected t win % win = t := by
  unfold reconstruct
  simp only
  have hm : (expected / win * win + t) % win = t := by
    rw [Nat.add_comm, Nat.add_mul_mod_self_right, Nat.mod_eq_of_lt ht]
  split
  · rw [Nat.add_mod_right]; exact hm
  · split
    · rename_i h
      have hge : win ≤ expected / win * win + t := h.2
      have : expected / win * win + t - win + win = expected / win * win + t := by omega
      rw [← this, Nat.add_mod_right] at hm
      exact hm
    · exact hm

theorem decode_congruent {expected t nbits r : Nat}
    (h : decode expected t nbits = some r) : r % 2 ^ nbits = t := by
  unfold decode at h
  split at h
  · rename_i hv
    simp only at h
    split at h
    · cases h
      exact reconstruct_mod expected t (2 ^ nbits) hv.2.1
    · cases h
  · cases h

theorem decode_bound {expected t nbits r : Nat}
    (h : decode expected t nbits = some r) : r < 2 ^ 62 := by
  rw [← pnBound_eq]
  unfold decode at h
  split at h
  · simp only at h
    split at h
    · cases h; assumption
    · cases h
  · cases h

theorem decode_rejects_invalid_bits {expected t nbits : Nat}
    (h : validBits nbits = false) : decode expected t nbits = none := by
  simp [decode, h]

/-- The recovery claim for one literal window size; `omega` handles `/` and
`%` by the literal `win`. -/
theorem reconstruct_window_lit (expected p win : Nat)
    (hwin : win = 256 ∨ win = 65536 ∨ win = 16777216 ∨ win = 4294967296)
    (hlo : expected < p + win / 2) (hhi : p ≤ expected + win / 2)
    (hp : p < pnBound) :
    reconstruct expected (p % win) win = p := by
  unfold reconstruct pnBound at *
  rcases hwin with rfl | rfl | rfl | rfl <;> simp only <;> split <;> (try split) <;> omega

theorem decode_window {expected p nbits : Nat}
    (hv : validBits nbits = true)
    (hexp : expected ≤ pnBound)
    (hlo : expected < p + 2 ^ nbits / 2) (hhi : p ≤ expected + 2 ^ nbits / 2)
    (hp : p < 2 ^ 62) :
    decode expected (p % 2 ^ nbits) nbits = some p := by
  rw [← pnBound_eq] at hp
  have hwin : 2 ^ nbits = 256 ∨ 2 ^ nbits = 65536 ∨ 2 ^ nbits = 16777216 ∨
      2 ^ nbits = 4294967296 := by
    rcases validBits_cases hv with rfl | rfl | rfl | rfl <;> decide
  have hr := reconstruct_window_lit expected p (2 ^ nbits) hwin hlo hhi hp
  have ht : p % 2 ^ nbits < 2 ^ nbits := Nat.mod_lt _ (Nat.two_pow_pos nbits)
  unfold decode
  rw [if_pos ⟨hv, ht, hexp⟩]
  simp only
  rw [hr, if_pos hp]

theorem rfc_example : decodeLargest (some 0xa82f30ea) 0x9b32 16 = some 0xa82f9b32 := by
  decide

end Vesper.WarpPacketNumber

#print axioms Vesper.WarpPacketNumber.decode_congruent
#print axioms Vesper.WarpPacketNumber.decode_bound
#print axioms Vesper.WarpPacketNumber.decode_window
#print axioms Vesper.WarpPacketNumber.decode_rejects_invalid_bits
#print axioms Vesper.WarpPacketNumber.rfc_example
