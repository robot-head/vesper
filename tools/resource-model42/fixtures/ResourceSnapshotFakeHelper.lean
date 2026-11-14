import Std

/-! The suffix is just part of this SAFE user's name. A name-only exclusion
would silently drop a declaration that the theorem genuinely references. -/
namespace ResourceSnapshotFakeHelper

def user : Nat := 41

def user._unsafe_rec : Nat := user

theorem safe_suffix_correct : user._unsafe_rec + 1 = 42 := rfl

end ResourceSnapshotFakeHelper
