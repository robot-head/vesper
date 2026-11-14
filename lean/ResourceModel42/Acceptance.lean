import ResourceModel42.Syntax

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

end Vesper.ResourceModel42.Acceptance
