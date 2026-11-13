import Std

/-!
Host numeric model for vesper-aether. This is not an extractor equivalence
proof, EPT/physical isolation theorem or proof of CAS/OS/cryptographic behavior.
No axiom asserting HKDF injectivity is valid for an arbitrary derivation.
-/
namespace Vesper.AetherMemory

def maxU64 : Nat := 18446744073709551615
def pageSize : Nat := 4096

/-- Exact checked half-open page interval, matching the Rust endpoint policy. -/
def checkedInterval (start len : Nat) : Option (Nat × Nat) :=
  if 0 < len ∧ start % pageSize = 0 ∧ len % pageSize = 0 ∧ start + len ≤ maxU64
  then some (start, start + len) else none

theorem checked_interval_sound (start len lo hi : Nat)
    (h : checkedInterval start len = some (lo, hi)) :
    lo = start ∧ hi = start + len ∧ lo < hi ∧ hi ≤ maxU64 ∧
      lo % pageSize = 0 ∧ len % pageSize = 0 := by
  unfold checkedInterval at h
  split at h
  next guard =>
    simp only [Option.some.injEq, Prod.mk.injEq] at h
    rcases h with ⟨rfl, rfl⟩
    exact ⟨rfl, rfl, by omega, guard.2.2.2, guard.2.1, guard.2.2.1⟩
  next => contradiction

/-- Passing the endpoint guard makes UInt64's wrapped addition exact. -/
theorem checked_endpoint_exact (start len : UInt64)
    (guard : start.toNat + len.toNat ≤ maxU64) :
    (start + len).toNat = start.toNat + len.toNat := by
  rw [UInt64.toNat_add]
  apply Nat.mod_eq_of_lt
  simp only [maxU64] at guard
  omega

/-- Numeric separation concerns only the supplied address space. -/
theorem separated_no_common_address (aStart aEnd bStart bEnd p : Nat)
    (sep : aEnd ≤ bStart ∨ bEnd ≤ aStart)
    (inA : aStart ≤ p ∧ p < aEnd) :
    ¬ (bStart ≤ p ∧ p < bEnd) := by
  omega

/-- Adjacent half-open intervals do not overlap at their common endpoint. -/
theorem adjacent_endpoint_excluded (lo hi : Nat) :
    ¬ (lo ≤ hi ∧ hi < hi) := by omega

/-- A fault in an aligned interval has its entire enclosing page in that
interval. This proves more than merely accepting the fault's first byte. -/
theorem fault_page_contained (start len fault : Nat)
    (alignedStart : start % 4096 = 0) (alignedLen : len % 4096 = 0)
    (inside : start ≤ fault ∧ fault < start + len) :
    start ≤ fault - fault % 4096 ∧
      fault - fault % 4096 + 4096 ≤ start + len := by
  omega

/-- Complete metadata length len/4096 therefore covers every in-range fault. -/
theorem fault_page_index_bounded (len delta : Nat)
    (inside : delta < len) (alignedLen : len % 4096 = 0) :
    delta / 4096 < len / 4096 := by
  omega

/-- A bounded page slice cannot exceed the declared blob endpoint. -/
theorem page_slice_contained (offset blobLen p : Nat)
    (checkedEnd : offset + 4096 ≤ blobLen)
    (insidePage : offset ≤ p ∧ p < offset + 4096) :
    p < blobLen := by omega


/-! ## KVM slot plans (wave six)

The Rust verifier sorts regions by start and compares each start with the
largest end seen so far. `running_reach_overlap` is the step it relies on: a
start below the reach of an earlier interval that does reach it overlaps that
interval. `slot_end_within_width` is the guest-address-width check. -/

theorem running_reach_overlap (aStart aEnd bStart bEnd : Nat)
    (sorted : aStart ≤ bStart) (below : bStart < aEnd) (nonempty : bStart < bEnd) :
    ¬ (aEnd ≤ bStart ∨ bEnd ≤ aStart) := by omega

theorem slot_end_within_width (gpa len bits p : Nat)
    (fits : gpa + len ≤ 2 ^ bits) (inside : gpa ≤ p ∧ p < gpa + len) :
    p < 2 ^ bits := by omega

/-! ## Snapshot-fork nonces (§37.3)

Model of the Rust `Sealer`: a nonce is `epoch * 2^32 + counter` (the
little-endian concatenation `epoch || counter`), with `counter < 2^32`. A
`ForkAuthority` hands out epochs `start, start+1, ...`. This proves nonce
uniqueness from distinct epochs alone. It assumes nothing about the derived
keys: no injectivity of BLAKE3, HKDF or any key derivation is used or assumed. -/

def counterSpace : Nat := 4294967296

def nonce (epoch counter : Nat) : Nat := epoch * counterSpace + counter

theorem nonce_injective (e₁ c₁ e₂ c₂ : Nat)
    (h₁ : c₁ < counterSpace) (h₂ : c₂ < counterSpace)
    (eq : nonce e₁ c₁ = nonce e₂ c₂) : e₁ = e₂ ∧ c₁ = c₂ := by
  unfold nonce counterSpace at *
  omega

theorem distinct_epochs_distinct_nonces (e₁ c₁ e₂ c₂ : Nat)
    (h₁ : c₁ < counterSpace) (h₂ : c₂ < counterSpace) (ne : e₁ ≠ e₂) :
    nonce e₁ c₁ ≠ nonce e₂ c₂ := by
  intro eq
  exact ne (nonce_injective e₁ c₁ e₂ c₂ h₁ h₂ eq).1

/-- Epochs issued by an authority starting at `start`: `start, ..., start+n-1`. -/
def issuedEpochs (start n : Nat) : List Nat := List.range' start n

theorem issued_epochs_nodup (start n : Nat) : (issuedEpochs start n).Nodup := by
  unfold issuedEpochs
  exact List.nodup_range' (s := start) (n := n)

/-- Clones `i ≠ j` of one authority never share a nonce, whatever counters
they reach (each below `2^32`). -/
theorem fork_clones_nonce_disjoint (start i j cᵢ cⱼ : Nat) (ne : i ≠ j)
    (hᵢ : cᵢ < counterSpace) (hⱼ : cⱼ < counterSpace) :
    nonce (start + i) cᵢ ≠ nonce (start + j) cⱼ :=
  distinct_epochs_distinct_nonces _ _ _ _ hᵢ hⱼ (by omega)

/-! ## Copy-on-write clone memory

A clone is the snapshot `base` plus a write overlay; writing one clone changes
only that page of that clone. -/

def cowWrite (mem : Nat → Nat) (page value : Nat) : Nat → Nat :=
  fun p => if p = page then value else mem p

theorem cow_write_reads_back (mem : Nat → Nat) (page value : Nat) :
    cowWrite mem page value page = value := by simp [cowWrite]

theorem cow_write_other_pages (mem : Nat → Nat) (page value q : Nat) (ne : q ≠ page) :
    cowWrite mem page value q = mem q := by simp [cowWrite, ne]

#print axioms checked_interval_sound
#print axioms checked_endpoint_exact
#print axioms separated_no_common_address
#print axioms adjacent_endpoint_excluded
#print axioms fault_page_contained
#print axioms fault_page_index_bounded
#print axioms page_slice_contained
#print axioms running_reach_overlap
#print axioms slot_end_within_width
#print axioms nonce_injective
#print axioms distinct_epochs_distinct_nonces
#print axioms issued_epochs_nodup
#print axioms fork_clones_nonce_disjoint
#print axioms cow_write_reads_back
#print axioms cow_write_other_pages
end Vesper.AetherMemory
