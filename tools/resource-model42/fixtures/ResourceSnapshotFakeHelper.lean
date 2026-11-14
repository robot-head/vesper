import Std

/-! The suffix is just part of this SAFE user's name. A name-only exclusion
would silently drop a declaration that the theorem genuinely references. -/
namespace ResourceSnapshotFakeHelper

def user_unsafe_rec : Nat := 41

theorem safe_suffix_correct : user_unsafe_rec + 1 = 42 := rfl

end ResourceSnapshotFakeHelper
