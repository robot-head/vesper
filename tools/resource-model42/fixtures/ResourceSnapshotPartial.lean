import Std

/-! A source partial definition has a public opaque logical facade as well as
its compiler implementation. Its facade must not become a projected axiom. -/
namespace ResourceSnapshotPartial

partial def sourcePartial (n : Nat) : Nat := sourcePartial (n + 1)

theorem anchor : sourcePartial 0 = sourcePartial 0 := rfl

end ResourceSnapshotPartial
