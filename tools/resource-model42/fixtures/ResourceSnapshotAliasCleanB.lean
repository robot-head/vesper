import Std

namespace ResourceSnapshotAliasClean
universe u

theorem alternate (α : Sort u) (a : α) : a = a := Eq.refl a

theorem shared (α : Sort u) (a : α) : a = a := alternate α a

end ResourceSnapshotAliasClean
