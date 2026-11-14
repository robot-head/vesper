import ResourceModel42.Evaluator

/-! The finite tombstone ledger embeds the actual Full resource store. These
lemmas describe actual push/modify operations, not an assumed trace or ledger. -/
namespace Vesper.ResourceModel42.LedgerProofs

theorem erase_live (c : Config) : c.erase.live = c.live := rfl
theorem erase_next (c : Config) : c.erase.next = c.ledger.size := rfl

theorem live_linear_iff (c : Config) (id : Nat) :
    c.live id = some .linear ↔
      ∃ entry, c.ledger[id]? = some entry ∧ entry.live = true := by
  cases lookup : c.ledger[id]? with
  | none => simp [Config.live, lookup]
  | some entry =>
      cases state : entry.live <;> simp [Config.live, lookup, state]

theorem live_lookup (c : Config) {id : Nat} {entry : LedgerEntry}
    (lookup : c.ledger[id]? = some entry) (state : entry.live = true) :
    c.live id = some .linear :=
  (live_linear_iff c id).2 ⟨entry, lookup, state⟩

theorem live_fresh (c : Config) : MetatheoryFull.Fresh c.live c.ledger.size := by
  intro id beyond
  simp [Config.live, Array.getElem?_eq_none beyond]

theorem erase_fresh (c : Config) : MetatheoryFull.Fresh c.erase.live c.erase.next :=
  live_fresh c

theorem live_push (c : Config) (site : Site) (expr : RuntimeExpr) :
    (Config.mk expr (c.ledger.push ⟨site, true⟩)).live =
      MetatheoryFull.RCtx.set c.live c.ledger.size (some .linear) := by
  funext id
  by_cases same : id = c.ledger.size
  · subst id
    simp [Config.live, MetatheoryFull.RCtx.set]
  · simp [Config.live, Array.getElem?_push, MetatheoryFull.RCtx.set, same]

/-- This equality also holds for an out-of-range index: modify leaves the array
unchanged and the finite embedded store was already empty at that index. -/
theorem live_modify_dead (c : Config) (id : Nat) (expr : RuntimeExpr) :
    (Config.mk expr (c.ledger.modify id (fun entry => {entry with live := false}))).live =
      MetatheoryFull.RCtx.set c.live id none := by
  funext other
  by_cases same : id = other
  · subst other
    cases lookup : c.ledger[id]? <;>
      simp [Config.live, Array.getElem?_modify, MetatheoryFull.RCtx.set, lookup]
  · simp [Config.live, Array.getElem?_modify, MetatheoryFull.RCtx.set,
      same, Ne.symm same]

theorem ledger_size_mint (ledger : Array LedgerEntry) (site : Site) :
    (PlannedAction.apply ledger (.mint site)).size = ledger.size + 1 := by
  simp [PlannedAction.apply]

theorem ledger_size_consume (ledger : Array LedgerEntry) (site : Site) (id : Nat) :
    (PlannedAction.apply ledger (.consume site id)).size = ledger.size := by
  simp [PlannedAction.apply]

theorem live_consumed (c : Config) (id : Nat) (expr : RuntimeExpr) :
    (Config.mk expr (c.ledger.modify id (fun entry => {entry with live := false}))).live id = none := by
  rw [live_modify_dead]
  simp [MetatheoryFull.RCtx.set]

theorem push_lookup_fresh (ledger : Array LedgerEntry) (site : Site) :
    (ledger.push ⟨site, true⟩)[ledger.size]? = some ⟨site, true⟩ := by
  simp

theorem push_lookup_history (ledger : Array LedgerEntry) (site : Site) (id : Nat)
    (old : id < ledger.size) :
    (ledger.push ⟨site, true⟩)[id]? = ledger[id]? := by
  simp [Array.getElem?_push, Nat.ne_of_lt old]

theorem consume_mintSite_history (ledger : Array LedgerEntry) (consumed id : Nat) :
    ((ledger.modify consumed (fun entry => {entry with live := false}))[id]?).map
      LedgerEntry.mintSite = (ledger[id]?).map LedgerEntry.mintSite := by
  by_cases same : consumed = id
  · subst id
    cases lookup : ledger[consumed]? <;> simp [Array.getElem?_modify, lookup]
  · simp [Array.getElem?_modify, same]

theorem live_id_lt (c : Config) {id : Nat} (live : c.live id = some .linear) :
    id < c.ledger.size := by
  obtain ⟨entry, lookup, _⟩ := (live_linear_iff c id).1 live
  obtain ⟨bound, _⟩ := Array.getElem?_eq_some_iff.1 lookup
  exact bound

theorem mint_id_not_old (ledger : Array LedgerEntry) {id : Nat} {entry : LedgerEntry}
    (lookup : ledger[id]? = some entry) : id ≠ ledger.size := by
  obtain ⟨bound, _⟩ := Array.getElem?_eq_some_iff.1 lookup
  exact Nat.ne_of_lt bound

/-- The serial counter is total allocation history, not the number of currently
live entries; consuming a tombstone never makes a prior id reusable. -/
theorem action_history_size_monotone (ledger : Array LedgerEntry) (action : PlannedAction) :
    ledger.size ≤ (action.apply ledger).size := by
  cases action <;> simp [PlannedAction.apply]

theorem action_live (c : Config) (action : PlannedAction) (expr : RuntimeExpr) :
    (Config.mk expr (action.apply c.ledger)).live =
      match action with
      | .silent => c.live
      | .mint _ => MetatheoryFull.RCtx.set c.live c.ledger.size (some .linear)
      | .consume _ id => MetatheoryFull.RCtx.set c.live id none := by
  cases action with
  | silent => rfl
  | mint site => exact live_push c site expr
  | consume _ id => exact live_modify_dead c id expr

theorem discard_entries_dead (c : Config) (discard : MetatheoryFull.DiscardR c.live) :
    ∀ (id : Nat) (entry : LedgerEntry), c.ledger[id]? = some entry → entry.live = false := by
  intro id entry lookup
  cases state : entry.live with
  | false => rfl
  | true => exact False.elim (discard id (live_lookup c lookup state))

theorem discard_residual_nil (result : EvalResult)
    (discard : MetatheoryFull.DiscardR result.config.live) : result.residual = [] := by
  unfold EvalResult.residual
  apply List.filterMap_eq_nil_iff.2
  intro pair member
  have lookup : result.config.ledger[pair.2]? = some pair.1 := by
    simpa using (List.mem_zipIdx_iff_getElem?.1 member)
  have dead := discard_entries_dead result.config discard pair.2 pair.1 lookup
  simp [dead]

end Vesper.ResourceModel42.LedgerProofs
