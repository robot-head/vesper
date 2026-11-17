namespace VesperProofs.MultiFailContracts
open Vesper

theorem pick_contract : «pick$contract» := by
  intro which result run
  unfold pick at run
  dsimp only at run
  split at run
  · change Except.error (Sum.inl (11 : UInt64)) = Except.ok result at run
    contradiction
  · split at run
    · change Except.error (Sum.inr (Sum.inl (22 : UInt64))) = Except.ok result at run
      contradiction
    · split at run
      · change Except.error (Sum.inr (Sum.inr true)) = Except.ok result at run
        contradiction
      · change Except.ok (42 : UInt64) = Except.ok result at run
        cases run
        rfl

theorem forward_contract : «forward$contract» := by
  intro which result run
  exact pick_contract which result (by simpa [forward] using run)

theorem distinct_contract : «distinct$contract» := by
  intro n reject result run
  cases reject with
  | false =>
    simp [distinct] at run
    cases run
    simp [ensures_distinct]
  | true =>
    change Except.error (Sum.inr false) = Except.ok result at run
    contradiction

theorem descend_contract : «descend$contract» := by
  intro n which fuel
  revert n which
  induction fuel with
  | zero =>
    intro n which result run
    change (none : Option (Except (Sum UInt64 (Sum UInt64 Bool)) UInt64)) = some (.ok result) at run
    contradiction
  | succ fuel ih =>
    intro n which result run
    unfold «descend$fuel» at run
    by_cases zero : n = 0
    · simp [zero, ExceptT.run] at run
      unfold pick at run
      dsimp only at run
      split at run
      · change some (Except.error (Sum.inl (11 : UInt64))) = some (Except.ok result) at run
        simp at run
      · split at run
        · change some (Except.error (Sum.inr (Sum.inl (22 : UInt64)))) = some (Except.ok result) at run
          simp at run
        · split at run
          · change some (Except.error (Sum.inr (Sum.inr true))) = some (Except.ok result) at run
            simp at run
          · change some (Except.ok (42 : UInt64)) = some (Except.ok result) at run
            cases run
            rfl
    · simp [zero, ExceptT.run] at run
      exact ih (n - 1) which result run

theorem pick_input : «pick$admissible» := ⟨3, trivial⟩

end VesperProofs.MultiFailContracts
