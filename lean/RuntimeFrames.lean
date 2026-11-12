import Std

namespace Vesper.RuntimeFrames

structure Address where
  frame : Nat
  offset : Nat
  deriving DecidableEq

structure Frame where
  base : Nat
  bytes : List UInt8

abbrev Heap := Nat → Option Frame

/-- Relocation changes physical bases, never stored frame identities or bytes. -/
def relocate (heap : Heap) (bases : Nat → Nat) : Heap := fun id =>
  (heap id).map fun frame => { frame with base := bases id }

def read (heap : Heap) (address : Address) (width : Nat) : Option (List UInt8) := do
  let frame ← heap address.frame
  if address.offset + width ≤ frame.bytes.length then
    some ((frame.bytes.drop address.offset).take width)
  else none

/-- This is a property of the frame representation, not a compiler theorem. -/
theorem relocation_preserves_reads (heap : Heap) (bases : Nat → Nat)
    (address : Address) (width : Nat) :
    read (relocate heap bases) address width = read heap address width := by
  simp only [read, relocate]
  cases heap address.frame <;> rfl

/-- Reused storage cannot revive an erased logical frame ID. -/
def remove (heap : Heap) (id : Nat) : Heap := fun queried =>
  if queried = id then none else heap queried

theorem removed_address_is_stale (heap : Heap) (address : Address) (width : Nat) :
    read (remove heap address.frame) address width = none := by
  simp [read, remove]

def disjoint (a : Address) (aWidth : Nat) (b : Address) (bWidth : Nat) : Prop :=
  a.frame ≠ b.frame ∨ a.offset + aWidth ≤ b.offset ∨ b.offset + bWidth ≤ a.offset

/-- Two disjoint projections within one frame stay disjoint at any new base. -/
theorem within_frame_disjoint_at_any_base (a b : Address) (aWidth bWidth base : Nat)
    (same : a.frame = b.frame) (separate : disjoint a aWidth b bWidth) :
    base + a.offset + aWidth ≤ base + b.offset ∨
    base + b.offset + bWidth ≤ base + a.offset := by
  simp [disjoint, same] at separate
  omega

end Vesper.RuntimeFrames
