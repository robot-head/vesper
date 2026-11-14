import ResourceModel42.Syntax

/-! Bridges from the finite executable fragment to the existing Full calculus.
Every named theorem in this module is public and belongs in the audit catalog.
-/
namespace Vesper.ResourceModel42.ErasureProofs

theorem runtimeValue_isVal (v : RuntimeValue) : MetatheoryFull.IsVal v.erase := by
  induction v with
  | unit => exact .unitV
  | nat n => exact .natV n
  | hdl r => exact .hdlV r
  | pair a b iha ihb => exact .pair iha ihb

theorem sourceAtom_erase_toRuntime (a : SourceAtom) : a.toRuntime.erase = a.erase := by
  cases a <;> rfl

theorem sourceExpr_erase_toRuntime (e : SourceExpr) : e.toRuntime.erase = e.erase := by
  induction e with
  | atom a => exact sourceAtom_erase_toRuntime a
  | pair _ _ a _ b _ => simp [SourceExpr.toRuntime, RuntimeExpr.erase, SourceExpr.erase,
      sourceAtom_erase_toRuntime]
  | letE _ a b iha ihb => simp [SourceExpr.toRuntime, RuntimeExpr.erase, SourceExpr.erase, iha, ihb]
  | unpack _ a b ihb => simp [SourceExpr.toRuntime, RuntimeExpr.erase, SourceExpr.erase,
      sourceAtom_erase_toRuntime, ihb]
  | ifz _ a z n ihz ihn => simp [SourceExpr.toRuntime, RuntimeExpr.erase, SourceExpr.erase,
      sourceAtom_erase_toRuntime, ihz, ihn]
  | mint _ => rfl
  | consume _ a => simp [SourceExpr.toRuntime, RuntimeExpr.erase, SourceExpr.erase,
      sourceAtom_erase_toRuntime]

theorem runtimeAtom_erase_subst (k : Nat) (v : RuntimeValue) (a : RuntimeAtom) :
    (a.subst k v).erase = MetatheoryFull.subst k v.erase a.erase := by
  cases a with
  | var i =>
    by_cases hi : i = k
    · simp [RuntimeAtom.subst, RuntimeAtom.erase, MetatheoryFull.subst, hi]
    · by_cases hk : k < i <;>
        simp [RuntimeAtom.subst, RuntimeAtom.erase, MetatheoryFull.subst, hi, hk]
  | val w =>
    change w.erase = MetatheoryFull.subst k v.erase w.erase
    exact (MetatheoryFull.subst_val (runtimeValue_isVal w) k v.erase).symm

theorem runtimeExpr_erase_subst (k : Nat) (v : RuntimeValue) (e : RuntimeExpr) :
    (e.subst k v).erase = MetatheoryFull.subst k v.erase e.erase := by
  induction e generalizing k with
  | atom a => exact runtimeAtom_erase_subst k v a
  | pair a b => simp [RuntimeExpr.subst, RuntimeExpr.erase, MetatheoryFull.subst,
      runtimeAtom_erase_subst]
  | letE a b iha ihb => simp [RuntimeExpr.subst, RuntimeExpr.erase, MetatheoryFull.subst, iha, ihb]
  | unpack a b ihb => simp [RuntimeExpr.subst, RuntimeExpr.erase, MetatheoryFull.subst,
      runtimeAtom_erase_subst, ihb]
  | ifz a z n ihz ihn => simp [RuntimeExpr.subst, RuntimeExpr.erase, MetatheoryFull.subst,
      runtimeAtom_erase_subst, ihz, ihn]
  | mint _ => rfl
  | consume _ a => simp [RuntimeExpr.subst, RuntimeExpr.erase, MetatheoryFull.subst,
      runtimeAtom_erase_subst]

theorem runtimeExpr_erase_unpack_subst (left right : RuntimeValue) (body : RuntimeExpr) :
    ((body.subst 0 right).subst 0 left).erase =
      MetatheoryFull.subst 0 left.erase (MetatheoryFull.subst 0 right.erase body.erase) := by
  rw [runtimeExpr_erase_subst, runtimeExpr_erase_subst]

theorem runtimeContext_erase_plug (context : RuntimeContext) (e : RuntimeExpr) :
    (context.plug e).erase = MetatheoryFull.plug context.erase e.erase := by
  induction context with
  | hole => rfl
  | letC inner body ih => simp [RuntimeContext.plug, RuntimeContext.erase, RuntimeExpr.erase,
      MetatheoryFull.plug, ih]

theorem runtimeAtom_value_of_isVal {a : RuntimeAtom} (h : MetatheoryFull.IsVal a.erase) :
    ∃ v, a = .val v := by
  cases a with
  | var i => cases h
  | val v => exact ⟨v, rfl⟩

theorem asValue_erase {e : RuntimeExpr} {v : RuntimeValue} (h : e.asValue = some v) :
    e.erase = v.erase := by
  cases e with
  | atom a =>
    cases a with
    | var i => cases h
    | val w => cases h; rfl
  | pair a b =>
    cases a <;> cases b <;> simp [RuntimeExpr.asValue] at h
    subst v
    rfl
  | letE _ _ => cases h
  | unpack _ _ => cases h
  | ifz _ _ _ => cases h
  | mint _ => cases h
  | consume _ _ => cases h

theorem asValue_sound {e : RuntimeExpr} {v : RuntimeValue} (h : e.asValue = some v) :
    MetatheoryFull.IsVal e.erase := by
  rw [asValue_erase h]
  exact runtimeValue_isVal v

theorem asValue_complete {e : RuntimeExpr} (h : MetatheoryFull.IsVal e.erase) :
    ∃ v, e.asValue = some v := by
  cases e with
  | atom a =>
    obtain ⟨v, rfl⟩ := runtimeAtom_value_of_isVal h
    exact ⟨v, rfl⟩
  | pair a b =>
    cases h with
    | pair ha hb =>
      obtain ⟨va, rfl⟩ := runtimeAtom_value_of_isVal ha
      obtain ⟨vb, rfl⟩ := runtimeAtom_value_of_isVal hb
      exact ⟨.pair va vb, rfl⟩
  | letE _ _ => cases h
  | unpack _ _ => cases h
  | ifz _ _ _ => cases h
  | mint _ => cases h
  | consume _ _ => cases h

end Vesper.ResourceModel42.ErasureProofs
