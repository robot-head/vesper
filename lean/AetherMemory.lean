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

#print axioms checked_interval_sound
#print axioms checked_endpoint_exact
#print axioms separated_no_common_address
#print axioms adjacent_endpoint_excluded
#print axioms fault_page_contained
#print axioms fault_page_index_bounded
#print axioms page_slice_contained
end Vesper.AetherMemory
