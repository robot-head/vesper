import Std

namespace ResourceSnapshotAliasAxiom
universe u

axiom forbidden : False

theorem alternate (α : Sort u) (a : α) : a = a := False.elim forbidden

theorem shared (α : Sort u) (a : α) : a = a := alternate α a

end ResourceSnapshotAliasAxiom
