import Std

/-! The unsafe declaration even has a safe existing parent's name followed
by the compiler-helper suffix. It has no compiler implementation association.
This tests more than a substring filter. No unsafe code is executed. -/
namespace ResourceSnapshotFakeUnsafe

def ordinary : Nat := 7

unsafe def ordinary._unsafe_rec : Nat := 99

theorem anchor : ordinary = 7 := rfl

end ResourceSnapshotFakeUnsafe
