import Std

/-! The unrelated safe theorem must not hide an explicit unsafe declaration
elsewhere in this source module's inventory. No unsafe code is executed. -/
namespace ResourceSnapshotUnsafe

unsafe def explicitUnsafe : Nat := 17

theorem anchor : (17 : Nat) = 17 := rfl

end ResourceSnapshotUnsafe
