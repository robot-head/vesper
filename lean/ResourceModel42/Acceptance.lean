import ResourceModel42

/-! Independent fixed semantic assertions. This model trace is not a backend
observation, and returned owners are not an exhaustion failure. -/
namespace Vesper.ResourceModel42.Acceptance

set_option maxRecDepth 8192

def unitProgram : RawProgram := ⟨#[], .atom (.unit 0)⟩
def statistics (raw : RawProgram) (limits : Limits := {}) :
    Except ValidationError SourceStats :=
  (validate raw limits).map ValidatedProgram.stats
def typeView (ty : Ty) (limits : Limits := {}) : Except TypeError (Ty × Nat × Nat) :=
  (TypeInfo.ofTy limits ty).map fun info => (info.ty, info.nodes, info.depth)

theorem unit_exact_admission :
    statistics unitProgram {rawNodes := 1, rawDepth := 1, context := 0, inputs := 0} =
      .ok ⟨1,1,0,0,0⟩ := by rfl

theorem raw_zero_refuses_required_body :
    statistics unitProgram {rawNodes := 0} = .error .rawNodes := by rfl

theorem out_of_bound_site_refuses :
    statistics ⟨#[], .atom (.unit 1)⟩ {rawNodes := 1} = .error (.site 1) := by rfl

theorem duplicate_site_in_unselected_branch_refuses :
    statistics ⟨#[], .ifz 0 (.nat 1 0) (.atom (.unit 2)) (.atom (.unit 2))⟩ =
      .error (.duplicateSite 2) := by rfl

theorem pair_exact_raw_budget :
    statistics ⟨#[], .pair 0 .copy (.unit 1) .linear (.nat 2 7) .linear⟩
      {rawNodes := 3, rawDepth := 2} = .ok ⟨3,2,0,0,0⟩ := by rfl

theorem pair_one_less_raw_budget :
    statistics ⟨#[], .pair 0 .copy (.unit 1) .linear (.nat 2 7) .linear⟩
      {rawNodes := 2} = .error .rawNodes := by rfl

theorem unbound_variable_is_left_for_semantic_checking :
    statistics ⟨#[], .atom (.var 0 0)⟩ = .ok ⟨1,1,0,0,0⟩ := by rfl

theorem excessive_variable_index_refuses_before_checking :
    statistics ⟨#[], .atom (.var 0 64)⟩ = .error (.variableBound 0) := by rfl

theorem zero_inputs_admits_empty_but_not_one_input :
    statistics ⟨#[.nat], .atom (.var 0 0)⟩ {inputs := 0} = .error .inputCount := by rfl

theorem resource_input_is_not_copy :
    statistics ⟨#[.ticket], .atom (.unit 0)⟩ = .error (.noncopyInput 0) := by rfl

theorem hidden_resource_field_input_refuses :
    statistics ⟨#[.pair .copy .ticket .copy .unit .linear], .atom (.unit 0)⟩ =
      .error (.inputType 0 .fieldMode) := by rfl

theorem raw_depth_budget_is_global :
    statistics ⟨#[], .letE 0 (.atom (.unit 1)) (.atom (.unit 2))⟩
      {rawDepth := 1} = .error .rawDepth := by rfl

theorem lexical_slots_do_not_overwrite_older_slots :
    statistics ⟨#[], .letE 0 (.mint 1)
      (.letE 2 (.atom (.nat 3 7)) (.consume 4 (.var 5 1)))⟩
      {context := 1} = .error .context := by rfl

theorem hard_limit_overrides_refuse :
    statistics unitProgram {rawNodes := 1025} = .error .invalidLimits := by rfl

theorem copy_field_cannot_hide_ticket :
    typeView (.pair .copy .ticket .copy .unit .linear) = .error .fieldMode := by rfl

theorem affine_field_cannot_hide_ticket :
    typeView (.pair .affine .ticket .affine .unit .linear) = .error .fieldMode := by rfl

theorem valid_pair_keeps_linear_child_kind :
    (typeView (.pair .copy .ticket .linear .nat .copy)).map (fun (ty, _, _) => ty.kind) =
      .ok .linear := by rfl

def doubledType : Nat → Ty
  | 0 => .nat
  | n + 1 => .pair .copy (doubledType n) .linear (doubledType n) .linear

theorem type_expansion_counts_shared_children_twice :
    (doubledType 8).nodes = 511 := by rfl

theorem expanded_type_refuses_default_budget :
    typeView (doubledType 8) = .error .nodes := by rfl

theorem exact_expanded_type_budget_succeeds :
    (typeView (doubledType 8) {typeNodes := 511}).map (fun (_, n, d) => (n,d)) =
      .ok (511,9) := by rfl

def copiedValue : RuntimeValue := .pair (.nat 7) (.nat 9)
def copyTarget : RuntimeExpr := .pair (.var 0) (.var 0)

theorem value_measure_is_size_not_invocation_range :
    (RuntimeValue.nat (2^64)).measure {} = .ok ⟨1,1⟩ := by rfl

theorem substitution_preflight_refuses_duplicated_value_before_copying :
    copyTarget.preflightSubst {valueNodes := 6} 0 copiedValue = .error .valueNodes := by rfl

theorem substitution_exact_value_budget_succeeds :
    (copyTarget.preflightSubst {valueNodes := 7} 0 copiedValue).map
      (fun stats => (stats.nodes, stats.depth, stats.maxValueNodes, stats.maxValueDepth)) =
      .ok (7,3,7,3) := by rfl

theorem enclosing_context_is_included_in_preflight :
    (RuntimeContext.letC .hole (.atom (.val .unit))).preflightPlug {expressionNodes := 8}
      ⟨7,3,7,3,some ⟨7,3⟩⟩ = .error .expressionNodes := by rfl

theorem unpack_preflight_includes_final_pair_value :
    (RuntimeExpr.pair (.var 0) (.var 1)).preflightUnpack {valueNodes := 4}
      .unit copiedValue = .error .valueNodes := by rfl

theorem unpack_binds_right_at_zero_and_left_at_one :
    ((RuntimeExpr.pair (.var 1) (.var 0)).subst 0 (.nat 9)).subst 0 (.nat 7) =
      .pair (.val (.nat 7)) (.val (.nat 9)) := by rfl

theorem input_zero_closes_innermost :
    closeInputs #[.nat 7, .nat 9] (.atom (.var 0)) =
      .letE (.atom (.val (.nat 9)))
        (.letE (.atom (.val (.nat 7))) (.atom (.var 0))) := by rfl

theorem source_labels_do_not_become_runtime_resource_ids :
    (SourceExpr.letE 20 (.mint 21) (.consume 22 (.var 23 0))).toRuntime =
      .letE (.mint 21) (.consume 22 (.var 0)) := by rfl

inductive TestFailure where
  | validation (error : ValidationError)
  | checking (error : CheckFailure)
  | invocation (error : InvocationError)

def closed (body : SourceExpr) : RawProgram := ⟨#[], body⟩
def checkedView (raw : RawProgram) (limits : Limits := {}) :
    Except TestFailure (Ty × Usage) :=
  match validate raw limits with
  | .error error => .error (.validation error)
  | .ok validated => match checkProgram validated with
    | .error error => .error (.checking error)
    | .ok checked => .ok (checked.resultType, checked.uses)

def mintConsume : RawProgram := closed (.letE 0 (.mint 1) (.consume 2 (.var 3 0)))
def movedOwner : RawProgram := closed
  (.letE 0 (.mint 1) (.letE 2 (.atom (.var 3 0)) (.consume 4 (.var 5 0))))
def twoOwners : RawProgram := closed (.letE 0 (.mint 1) (.letE 2 (.mint 3)
  (.pair 4 .copy (.var 5 1) .linear (.var 6 0) .linear)))
def reverseConsumes : RawProgram := closed (.letE 0 (.mint 1) (.letE 2 (.mint 3)
  (.letE 4 (.consume 5 (.var 6 0)) (.consume 7 (.var 8 2)))))
def unpackOwners : RawProgram := closed (.letE 0 (.mint 1) (.letE 2 (.mint 3)
  (.letE 4 (.pair 5 .copy (.var 6 1) .linear (.var 7 0) .linear)
    (.unpack 8 (.var 9 0) (.letE 10 (.consume 11 (.var 12 0))
      (.consume 13 (.var 14 2)))))))
def strictInitializer : RawProgram := closed
  (.letE 0 (.letE 1 (.mint 2) (.consume 3 (.var 4 0))) (.atom (.unit 5)))
def sequentialResources : RawProgram := closed
  (.letE 0 (.letE 1 (.mint 2) (.consume 3 (.var 4 0)))
    (.letE 5 (.mint 6) (.consume 7 (.var 8 0))))
def branchConsumes : RawProgram := closed (.letE 0 (.mint 1)
  (.ifz 2 (.nat 3 0) (.consume 4 (.var 5 0)) (.consume 6 (.var 7 0))))
def branchMints : RawProgram := ⟨#[.nat], .ifz 0 (.var 1 0) (.mint 2) (.mint 3)⟩
def inputZero : RawProgram := ⟨#[.nat, .nat], .atom (.var 0 0)⟩

theorem checker_accepts_returned_owner :
    checkedView (closed (.mint 0)) = .ok (.ticket, []) := by rfl
theorem checker_accepts_moved_owner : checkedView movedOwner = .ok (.unit, []) := by rfl
theorem checker_accepts_pair_of_owned_results :
    checkedView twoOwners = .ok (.pair .copy .ticket .linear .ticket .linear, []) := by rfl
theorem checker_accepts_full_unpack : checkedView unpackOwners = .ok (.unit, []) := by rfl
theorem checker_rejects_aliasing_pair :
    checkedView (closed (.letE 0 (.mint 1)
      (.pair 2 .copy (.var 3 0) .linear (.var 4 0) .linear))) =
      .error (.checking (.semantic ⟨.duplicateUse,2,some 0⟩)) := by rfl
theorem checker_rejects_stale_owner :
    checkedView (closed (.letE 0 (.mint 1)
      (.letE 2 (.consume 3 (.var 4 0)) (.consume 5 (.var 6 1))))) =
      .error (.checking (.semantic ⟨.duplicateUse,2,some 0⟩)) := by rfl
theorem checker_rejects_linear_leak :
    checkedView (closed (.letE 0 (.mint 1) (.atom (.unit 2)))) =
      .error (.checking (.semantic ⟨.unusedLinear,0,some 0⟩)) := by rfl
theorem checker_accepts_strict_unused_copy_initializer :
    checkedView strictInitializer = .ok (.unit, []) := by rfl
theorem checker_distinguishes_new_scalar_from_older_owner :
    checkedView (closed (.letE 0 (.mint 1)
      (.letE 2 (.atom (.nat 3 7)) (.consume 4 (.var 5 1))))) = .ok (.unit, []) := by rfl
theorem checker_rejects_consuming_the_new_scalar :
    checkedView (closed (.letE 0 (.mint 1)
      (.letE 2 (.atom (.nat 3 7)) (.consume 4 (.var 5 0))))) =
      .error (.checking (.semantic ⟨.typeMismatch,4,none⟩)) := by rfl
theorem checker_reports_unknown_slot :
    checkedView (closed (.atom (.var 0 0))) =
      .error (.checking (.semantic ⟨.unknownVariable,0,some 0⟩)) := by rfl
theorem checker_checks_unselected_constant_branch :
    checkedView (closed (.ifz 0 (.nat 1 0) (.atom (.unit 2)) (.atom (.var 3 0)))) =
      .error (.checking (.semantic ⟨.unknownVariable,3,some 0⟩)) := by rfl
theorem checker_rejects_nonuniform_linear_branch :
    checkedView (closed (.letE 0 (.mint 1)
      (.ifz 2 (.nat 3 0) (.consume 4 (.var 5 0)) (.atom (.unit 6))))) =
      .error (.checking (.semantic ⟨.branchOwnership,2,some 0⟩)) := by rfl
theorem checker_accepts_both_consuming_arms : checkedView branchConsumes = .ok (.unit, []) := by rfl
theorem checker_reports_affine_only_difference_as_profile :
    checkedView (closed (.letE 0 (.pair 1 .affine (.unit 2) .linear (.unit 3) .linear)
      (.ifz 4 (.nat 5 0) (.atom (.var 6 0))
        (.pair 7 .affine (.unit 8) .linear (.unit 9) .linear)))) =
      .error (.checking (.profile (.affineBranchDemand 4 0))) := by rfl
theorem checker_linear_difference_precedes_earlier_affine_difference :
    checkedView (closed (.letE 0 (.mint 1)
      (.letE 2 (.pair 3 .affine (.unit 4) .linear (.unit 5) .linear)
        (.ifz 6 (.nat 7 0)
          (.pair 8 .copy (.var 9 1) .linear (.var 10 0) .linear)
          (.letE 11 (.mint 12)
            (.letE 13 (.pair 14 .affine (.unit 15) .linear (.unit 16) .linear)
              (.pair 17 .copy (.var 18 1) .linear (.var 19 0) .linear))))))) =
      .error (.checking (.semantic ⟨.branchOwnership,6,some 1⟩)) := by rfl
theorem checker_field_override_is_semantic_error :
    checkedView (closed (.letE 0 (.mint 1)
      (.pair 2 .copy (.var 3 0) .copy (.unit 4) .linear))) =
      .error (.checking (.semantic ⟨.fieldModality,2,none⟩)) := by rfl
theorem checker_accepts_largest_u64_literal :
    checkedView (closed (.atom (.nat 0 (2^64-1)))) = .ok (.nat, []) := by rfl
theorem checker_rejects_out_of_range_literal :
    checkedView (closed (.atom (.nat 0 (2^64)))) =
      .error (.checking (.semantic ⟨.literalOutOfRange,0,none⟩)) := by rfl
theorem checker_zero_type_budget_is_profile_refusal :
    checkedView unitProgram {typeNodes := 0} =
      .error (.checking (.profile (.typeBound 0 .nodes))) := by rfl
theorem checker_result_is_independent_of_invocation_values :
    checkedView branchMints = .ok (.ticket, [true]) := by rfl

def doubleLocals : Nat → Nat → SourceExpr
  | 0, site => .atom (.unit site)
  | n + 1, site => .letE site
      (.pair (site+1) .copy (.var (site+2) 0) .linear (.var (site+3) 0) .linear)
      (doubleLocals n (site+4))
def compactDoubling : RawProgram := closed (.letE 0 (.atom (.nat 1 7)) (doubleLocals 8 2))
theorem checker_prepays_compact_type_doubling :
    checkedView compactDoubling = .error (.checking (.profile (.typeBound 31 .nodes))) := by rfl
theorem checker_type_and_dynamic_value_caps_are_independent :
    checkedView compactDoubling {typeNodes := 511} = .ok (.unit, []) := by rfl

structure Observation where
  status : EvalStatus
  events : List ResourceEvent
  residual : List (Nat × LedgerEntry)
  actions : List MetatheoryFull.Act
  expression : RuntimeExpr
def Observation.ofResult (result : EvalResult) : Observation :=
  ⟨result.status,result.events,result.residual,result.actions,result.config.expr⟩
def evaluate (raw : RawProgram) (values : Array RuntimeValue) (fuel : Nat)
    (limits : Limits := {}) : Except TestFailure Observation :=
  match validate raw limits with
  | .error error => .error (.validation error)
  | .ok validated => match checkProgram validated with
    | .error error => .error (.checking error)
    | .ok checked => match validateInvocation checked values with
      | .error error => .error (.invocation error)
      | .ok invocation => match evalSteps invocation fuel with
        | .error error => .error (.invocation error)
        | .ok result => .ok (Observation.ofResult result)

theorem terminal_unit_needs_no_fuel_or_resource_capacity :
    evaluate unitProgram #[] 0 {fuel := 0,resources := 0,events := 0} =
      .ok ⟨.completed .unit,[],[],[],.atom (.val .unit)⟩ := by rfl
theorem terminal_owner_does_not_require_empty_residual :
    evaluate (closed (.mint 0)) #[] 1 =
      .ok ⟨.completed (.hdl 0),[⟨.mint,0,0⟩],[(0,⟨0,true⟩)],
        [.mint 0],.atom (.val (.hdl 0))⟩ := by rfl
theorem mint_consume_takes_three_full_steps :
    evaluate mintConsume #[] 3 =
      .ok ⟨.completed .unit,[⟨.mint,1,0⟩,⟨.consume,2,0⟩],[],
        [.mint 0,.silent,.consume 0],.atom (.val .unit)⟩ := by rfl
theorem moving_to_a_new_slot_neither_remints_nor_consumes :
    evaluate movedOwner #[] 4 =
      .ok ⟨.completed .unit,[⟨.mint,1,0⟩,⟨.consume,4,0⟩],[],
        [.mint 0,.silent,.silent,.consume 0],.atom (.val .unit)⟩ := by rfl
theorem predecessor_fuel_retains_current_term_and_owner :
    evaluate mintConsume #[] 2 =
      .ok ⟨.fuelExhausted,[⟨.mint,1,0⟩],[(0,⟨1,true⟩)],
        [.mint 0,.silent],.consume 2 (.val (.hdl 0))⟩ := by rfl
theorem no_transition_is_committed_at_zero_fuel :
    evaluate mintConsume #[] 0 =
      .ok ⟨.fuelExhausted,[],[],[],.letE (.mint 1) (.consume 2 (.var 0))⟩ := by rfl
theorem pair_of_owners_returns_two_live_distinct_serials :
    evaluate twoOwners #[] 4 =
      .ok ⟨.completed (.pair (.hdl 0) (.hdl 1)),[⟨.mint,1,0⟩,⟨.mint,3,1⟩],
        [(0,⟨1,true⟩),(1,⟨3,true⟩)],[.mint 0,.silent,.mint 1,.silent],
        .pair (.val (.hdl 0)) (.val (.hdl 1))⟩ := by rfl
theorem reverse_consumes_preserve_identity_and_order :
    evaluate reverseConsumes #[] 7 =
      .ok ⟨.completed .unit,[⟨.mint,1,0⟩,⟨.mint,3,1⟩,⟨.consume,5,1⟩,⟨.consume,7,0⟩],[],
        [.mint 0,.silent,.mint 1,.silent,.consume 1,.silent,.consume 0],
        .atom (.val .unit)⟩ := by rfl
theorem full_unpack_preserves_both_identities_without_reminting :
    evaluate unpackOwners #[] 9 =
      .ok ⟨.completed .unit,[⟨.mint,1,0⟩,⟨.mint,3,1⟩,⟨.consume,11,1⟩,⟨.consume,13,0⟩],[],
        [.mint 0,.silent,.mint 1,.silent,.silent,.silent,.consume 1,.silent,.consume 0],
        .atom (.val .unit)⟩ := by rfl
theorem unused_copy_initializer_still_executes_its_resource_effects :
    evaluate strictInitializer #[] 4 =
      .ok ⟨.completed .unit,[⟨.mint,2,0⟩,⟨.consume,3,0⟩],[],
        [.mint 0,.silent,.consume 0,.silent],.atom (.val .unit)⟩ := by rfl
theorem selected_branch_only_executes_one_consume :
    evaluate branchConsumes #[] 4 =
      .ok ⟨.completed .unit,[⟨.mint,1,0⟩,⟨.consume,4,0⟩],[],
        [.mint 0,.silent,.silent,.consume 0],.atom (.val .unit)⟩ := by rfl
theorem zero_invocation_selects_zero_branch_with_fresh_zero_serial :
    evaluate branchMints #[.nat 0] 3 =
      .ok ⟨.completed (.hdl 0),[⟨.mint,2,0⟩],[(0,⟨2,true⟩)],
        [.silent,.silent,.mint 0],.atom (.val (.hdl 0))⟩ := by rfl
theorem nonzero_invocation_selects_other_branch_without_eager_mint :
    evaluate branchMints #[.nat 7] 3 =
      .ok ⟨.completed (.hdl 0),[⟨.mint,3,0⟩],[(0,⟨3,true⟩)],
        [.silent,.silent,.mint 0],.atom (.val (.hdl 0))⟩ := by rfl
theorem initial_closing_lets_count_as_global_fuel :
    evaluate inputZero #[.nat 7,.nat 9] 1 =
      .ok ⟨.fuelExhausted,[],[],[.silent],.letE (.atom (.val (.nat 7))) (.atom (.var 0))⟩ := by rfl
theorem input_zero_remains_innermost_after_two_closing_steps :
    evaluate inputZero #[.nat 7,.nat 9] 2 =
      .ok ⟨.completed (.nat 7),[],[],[.silent,.silent],.atom (.val (.nat 7))⟩ := by rfl
theorem out_of_range_invocation_is_distinct_from_static_literal_error :
    evaluate branchMints #[.nat (2^64)] 3 =
      .error (.invocation (.inputValue 0 .naturalRange)) := by rfl
theorem invocation_cannot_supply_a_resource_identity :
    evaluate branchMints #[.hdl 0] 3 = .error (.invocation (.inputValue 0 .handle)) := by rfl
theorem invocation_shape_is_checked_before_execution :
    evaluate branchMints #[.unit] 3 = .error (.invocation (.inputValue 0 .shape)) := by rfl
theorem initial_aggregate_budget_is_independent :
    evaluate branchMints #[.nat 0] 3 {initialValueNodes := 0} =
      .error (.invocation (.capacity .initialValues)) := by rfl
theorem requested_fuel_must_be_admitted_even_for_terminal_values :
    evaluate unitProgram #[] 4 {fuel := 3} = .error (.invocation (.fuelBound 4 3)) := by rfl
theorem zero_resource_cap_refuses_without_committing_mint :
    evaluate (closed (.mint 0)) #[] 1 {resources := 0} =
      .ok ⟨.resourceLimit (.resources 1 0),[],[],[],.mint 0⟩ := by rfl
theorem zero_event_cap_refuses_without_committing_mint :
    evaluate (closed (.mint 0)) #[] 1 {events := 0} =
      .ok ⟨.resourceLimit (.events 1 0),[],[],[],.mint 0⟩ := by rfl
theorem consume_event_refusal_keeps_owner_and_prior_silent_step :
    evaluate mintConsume #[] 3 {events := 1} =
      .ok ⟨.resourceLimit (.events 2 1),[⟨.mint,1,0⟩],[(0,⟨1,true⟩)],
        [.mint 0,.silent],.consume 2 (.val (.hdl 0))⟩ := by rfl
theorem consumed_serials_are_not_reused_by_later_mints :
    evaluate sequentialResources #[] 7 {resources := 2,events := 4} =
      .ok ⟨.completed .unit,[⟨.mint,2,0⟩,⟨.consume,3,0⟩,⟨.mint,6,1⟩,⟨.consume,7,1⟩],[],
        [.mint 0,.silent,.consume 0,.silent,.mint 1,.silent,.consume 1],
        .atom (.val .unit)⟩ := by rfl
theorem resource_cap_counts_consumed_tombstones_without_cleanup :
    evaluate sequentialResources #[] 7 {resources := 1} =
      .ok ⟨.resourceLimit (.resources 2 1),[⟨.mint,2,0⟩,⟨.consume,3,0⟩],[],
        [.mint 0,.silent,.consume 0,.silent],
        .letE (.mint 6) (.consume 7 (.var 0))⟩ := by rfl

def dynamicGrowth : RawProgram := closed (.letE 0 (.mint 1)
  (.letE 2 (.atom (.nat 3 7))
    (.letE 4 (.pair 5 .copy (.var 6 0) .linear (.var 7 0) .linear)
      (.letE 8 (.pair 9 .copy (.var 10 0) .linear (.var 11 0) .linear)
        (.consume 12 (.var 13 3))))))
theorem dynamic_growth_checks_before_substitution_or_ledger_commit :
    evaluate dynamicGrowth #[] 6 {valueNodes := 4} =
      .ok ⟨.resourceLimit (.capacity .valueNodes),[⟨.mint,1,0⟩],[(0,⟨1,true⟩)],
        [.mint 0,.silent,.silent],
        .letE (.pair (.val (.nat 7)) (.val (.nat 7)))
          (.letE (.pair (.var 0) (.var 0)) (.consume 12 (.val (.hdl 0))))⟩ := by rfl
theorem exact_dynamic_value_budget_executes_strict_initializers :
    evaluate dynamicGrowth #[] 6 {valueNodes := 7} =
      .ok ⟨.completed .unit,[⟨.mint,1,0⟩,⟨.consume,12,0⟩],[],
        [.mint 0,.silent,.silent,.silent,.silent,.consume 0],.atom (.val .unit)⟩ := by rfl

end Vesper.ResourceModel42.Acceptance
