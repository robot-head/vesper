-- Vesper Aura (§37.6): WCAG 4.1.2 accessible-label completeness and
-- transactional close safety for the pure `appView` / `appUpdate` UI model.
--
-- What is proved, and about which model:
-- * `appView_all_controls_accessible`: every interactive control emitted by
--   `appView s` (whether an uncommitted bookmark transaction is open or not)
--   carries a non-empty `AccessibleLabel`.
-- * `close_never_drops_uncommitted_txn`: whenever an uncommitted bookmark
--   transaction is open and `appUpdate s (.requestClose d)` succeeds, the
--   storage model commits the transaction cleanly without dropping it.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.InspectorUiSpecs

structure AccessibleLabel where
  chars : List Char
  deriving DecidableEq, Repr

def AccessibleLabel.isEmpty (l : AccessibleLabel) : Bool :=
  l.chars.isEmpty

structure UiControl where
  id : Nat
  accessibleLabel : AccessibleLabel
  deriving DecidableEq, Repr

inductive CloseDecision where
  | saveAndClose
  | discardChanges
  | cancelClose
  deriving DecidableEq, Repr

structure WriteTxn where
  txnId : Nat
  deriving DecidableEq, Repr

structure InspectorApp where
  unsavedBookmarkTx : Option WriteTxn
  deriving DecidableEq, Repr

structure RefStorageModel where
  committedCount : Nat
  abortedCount : Nat
  deriving DecidableEq, Repr

inductive StorageAction where
  | commitTxn (tx : WriteTxn)
  | noop
  | rejectClose (reason : String)
  deriving DecidableEq, Repr

inductive CloseResult where
  | ok
  | err (reason : String)
  deriving DecidableEq, Repr

def CloseResult.isOk : CloseResult → Bool
  | .ok => true
  | .err _ => false

def WriteTxn.commit (st : RefStorageModel) (_tx : WriteTxn) :
    RefStorageModel × CloseResult :=
  ({ st with committedCount := st.committedCount + 1 }, .ok)

def RefStorageModel.run (st : RefStorageModel) :
    StorageAction → RefStorageModel × CloseResult
  | .commitTxn tx => WriteTxn.commit st tx
  | .noop => (st, .ok)
  | .rejectClose msg => (st, .err msg)

inductive AppMsg where
  | requestClose (decision : CloseDecision)
  deriving DecidableEq, Repr

def commitButton : UiControl :=
  { id := 1, accessibleLabel := ⟨['S', 'a', 'v', 'e']⟩ }

def closeButton : UiControl :=
  { id := 2, accessibleLabel := ⟨['C', 'l', 'o', 's', 'e']⟩ }

def filterInput : UiControl :=
  { id := 3, accessibleLabel := ⟨['F', 'i', 'l', 't', 'e', 'r']⟩ }

def InteractiveControls (controls : List UiControl) : List UiControl := controls

def appView (s : InspectorApp) : List UiControl :=
  match s.unsavedBookmarkTx with
  | some _ => [commitButton, closeButton, filterInput]
  | none => [closeButton, filterInput]

def appUpdate (s : InspectorApp) : AppMsg → StorageAction
  | .requestClose .saveAndClose =>
      match s.unsavedBookmarkTx with
      | some tx => .commitTxn tx
      | none => .noop
  | .requestClose .discardChanges =>
      match s.unsavedBookmarkTx with
      | some _ => .rejectClose "uncommitted bookmark transaction must be committed before closing"
      | none => .noop
  | .requestClose .cancelClose =>
      match s.unsavedBookmarkTx with
      | some _ => .rejectClose "close cancelled while transaction active"
      | none => .noop

/-- §37.6 WCAG 4.1.2 accessible-label completeness invariant for `appView`. -/
theorem appView_all_controls_accessible (s : InspectorApp) :
    ∀ ctrl ∈ InteractiveControls (appView s),
      ctrl.accessibleLabel.isEmpty = false := by
  intro ctrl hmem
  cases htx : s.unsavedBookmarkTx with
  | none =>
      simp [appView, htx, InteractiveControls] at hmem
      rcases hmem with rfl | rfl <;> rfl
  | some _ =>
      simp [appView, htx, InteractiveControls] at hmem
      rcases hmem with rfl | rfl | rfl <;> rfl

/-- §37.6 Transactional close safety: closing an app with an active uncommitted
bookmark transaction cannot silently succeed without committing it. -/
theorem close_never_drops_uncommitted_txn
    (s : InspectorApp) (d : CloseDecision) (st : RefStorageModel) :
    s.unsavedBookmarkTx.isSome = true →
    (RefStorageModel.run st (appUpdate s (.requestClose d))).2.isOk = true →
    (RefStorageModel.run st (appUpdate s (.requestClose d))).1.committedCount =
      st.committedCount + 1 := by
  intro hsome hok
  cases htx : s.unsavedBookmarkTx with
  | none =>
      simp [htx] at hsome
  | some _ =>
      cases d <;>
        simp [appUpdate, htx, RefStorageModel.run, WriteTxn.commit, CloseResult.isOk] at hok ⊢

end Vesper.InspectorUiSpecs
