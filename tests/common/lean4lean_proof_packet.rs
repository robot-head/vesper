//! Shared runner for the twelve fixed audited proof packets.
//! These are logical foundation lemmas, not whole-checker verification.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};
/// The only supported packet choices. There is no arbitrary-module or weaker-policy mode.
pub enum Packet {
    Foundations,
    Construction,
    Order,
    Map,
    Normalize,
    Normalization,
    Comparison,
    Shape,
    Addability,
    Completion,
    Reconstruction,
    NormalizedCompletion,
}

struct PacketSpec {
    label: &'static str,
    module: &'static str,
    projected: &'static str,
    source_modules: &'static [&'static str],
    required: &'static [&'static str],
    scope: &'static str,
    marker: &'static str,
}

const FOUNDATIONS_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.enabled_eq_true_iff",
    "Vesper.Lean4LeanLevel42.enabled_of_subset",
    "Vesper.Lean4LeanLevel42.varsValue_nil",
    "Vesper.Lean4LeanLevel42.varsValue_cons",
    "Vesper.Lean4LeanLevel42.subset_sound",
    "Vesper.Lean4LeanLevel42.subsumeVars_union",
    "Vesper.Lean4LeanLevel42.offset_bound",
    "Vesper.Lean4LeanLevel42.node_subsumeBy_union",
    "Vesper.Lean4LeanLevel42.node_subsume_union",
    "Vesper.Lean4LeanLevel42.node_subsumeBy_self",
    "Vesper.Lean4LeanLevel42.node_subsume_self",
    "Vesper.Lean4LeanLevel42.invalid_wf_counterexample",
    "Vesper.Lean4LeanLevel42.enabled_variable_removal",
    "Vesper.Lean4LeanLevel42.larger_constant_removal",
    "Vesper.Lean4LeanLevel42.disabled_larger_key",
    "Vesper.Lean4LeanLevel42.valid_self_same_removal",
    "Vesper.Lean4LeanNames42.next_reserves_self",
    "Vesper.Lean4LeanNames42.not_reserves_self",
    "Vesper.Lean4LeanNames42.reserves_mono",
    "Vesper.Lean4LeanNames42.nameAt_injective",
    "Vesper.Lean4LeanNames42.reserves_not_nameAt",
    "Vesper.Lean4LeanNames42.nameAt_reserved_advance",
    "Vesper.Lean4LeanNames42.emitted_nodup",
    "Vesper.Lean4LeanNames42.emitted_all_reserved",
];
const CONSTRUCTION_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.enabled_eq_true_iff",
    "Vesper.Lean4LeanLevel42.enabled_of_subset",
    "Vesper.Lean4LeanLevel42.varsValue_nil",
    "Vesper.Lean4LeanLevel42.varsValue_cons",
    "Vesper.Lean4LeanLevel42.subset_sound",
    "Vesper.Lean4LeanLevel42.subsumeVars_union",
    "Vesper.Lean4LeanLevel42.offset_bound",
    "Vesper.Lean4LeanLevel42.orderedInsert_some_perm",
    "Vesper.Lean4LeanLevel42.orderedInsert_some_length",
    "Vesper.Lean4LeanLevel42.orderedInsert_some_mem",
    "Vesper.Lean4LeanLevel42.orderedInsert_none_mem",
    "Vesper.Lean4LeanLevel42.orderedInsert_some_of_not_mem",
    "Vesper.Lean4LeanLevel42.orderedInsert_enabled",
    "Vesper.Lean4LeanLevel42.orderedInsert_none_positive",
    "Vesper.Lean4LeanLevel42.orderedInsert_input_subset",
    "Vesper.Lean4LeanLevel42.orderedInsert_inserted_mem",
    "Vesper.Lean4LeanLevel42.orderedInsert_preserves_WF",
    "Vesper.Lean4LeanLevel42.orderedInsert_some_nodup",
    "Vesper.Lean4LeanLevel42.orderedInsert_unsorted_duplicate_counterexample",
    "Vesper.Lean4LeanLevel42.orderedInsert_enabled_example",
    "Vesper.Lean4LeanLevel42.orderedInsert_none_example",
    "Vesper.Lean4LeanLevel42.addVar_value",
    "Vesper.Lean4LeanLevel42.addVar_names",
    "Vesper.Lean4LeanLevel42.addVar_preserves_key",
    "Vesper.Lean4LeanLevel42.node_addVar_value",
    "Vesper.Lean4LeanLevel42.node_addVar_WF",
    "Vesper.Lean4LeanLevel42.numeric_name_insert_before",
    "Vesper.Lean4LeanLevel42.numeric_name_insert_after",
    "Vesper.Lean4LeanLevel42.numeric_name_merge",
    "Vesper.Lean4LeanLevel42.numeric_name_repeated_unsorted",
];

const ORDER_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.name_cmp_refl",
    "Vesper.Lean4LeanLevel42.name_cmp_swap",
    "Vesper.Lean4LeanLevel42.name_cmp_isLE_trans",
    "Vesper.Lean4LeanLevel42.name_cmp_eq_iff",
    "Vesper.Lean4LeanLevel42.name_cmp_lt_trans",
    "Vesper.Lean4LeanLevel42.name_cmp_lt_irrefl",
    "Vesper.Lean4LeanLevel42.nameCmpTrans",
    "Vesper.Lean4LeanLevel42.nameCmpLawfulEq",
    "Vesper.Lean4LeanLevel42.path_cmp_trans",
    "Vesper.Lean4LeanLevel42.path_cmp_lawfulEq",
    "Vesper.Lean4LeanLevel42.path_cmp_eq_iff",
    "Vesper.Lean4LeanLevel42.name_cmp_numeric_witness",
    "Vesper.Lean4LeanLevel42.name_cmp_string_witness",
    "Vesper.Lean4LeanLevel42.name_cmp_prefix_witness",
    "Vesper.Lean4LeanLevel42.path_cmp_prefix_witness",
    "Vesper.Lean4LeanLevel42.keySorted_nodup",
    "Vesper.Lean4LeanLevel42.orderedInsert_sorted",
    "Vesper.Lean4LeanLevel42.orderedInsert_none_iff_mem",
    "Vesper.Lean4LeanLevel42.orderedInsert_sorted_nodup",
    "Vesper.Lean4LeanLevel42.addVar_sorted",
    "Vesper.Lean4LeanLevel42.varsSorted_names_nodup",
    "Vesper.Lean4LeanLevel42.addVar_sorted_names_nodup",
    "Vesper.Lean4LeanLevel42.orderedInsert_numeric_sorted",
    "Vesper.Lean4LeanLevel42.orderedInsert_numeric_duplicate",
    "Vesper.Lean4LeanLevel42.addVar_numeric_sorted_merge",
    "Vesper.Lean4LeanLevel42.orderedInsert_unsorted_key_witness",
];

const MAP_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.mapValue_empty",
    "Vesper.Lean4LeanLevel42.nodeValue_le_mapValue",
    "Vesper.Lean4LeanLevel42.mapValue_le",
    "Vesper.Lean4LeanLevel42.mapValue_mono",
    "Vesper.Lean4LeanLevel42.mapValue_insert",
    "Vesper.Lean4LeanLevel42.mapValue_decompose",
    "Vesper.Lean4LeanLevel42.mapWF_empty",
    "Vesper.Lean4LeanLevel42.map_addNode_value",
    "Vesper.Lean4LeanLevel42.map_addVar_value",
    "Vesper.Lean4LeanLevel42.map_addConst_value",
    "Vesper.Lean4LeanLevel42.map_addConst_value_of_covered",
    "Vesper.Lean4LeanLevel42.map_addNode_WF",
    "Vesper.Lean4LeanLevel42.map_addVar_WF",
    "Vesper.Lean4LeanLevel42.map_addConst_WF",
    "Vesper.Lean4LeanLevel42.missing_key_addVar_counterexample",
    "Vesper.Lean4LeanLevel42.skipped_one_empty_counterexample",
    "Vesper.Lean4LeanLevel42.skipped_one_zero_node_counterexample",
    "Vesper.Lean4LeanLevel42.pathPrimed_empty",
    "Vesper.Lean4LeanLevel42.pathPrimed_variable_bound",
    "Vesper.Lean4LeanLevel42.pathPrimed_one_bound",
    "Vesper.Lean4LeanLevel42.pathPrimed_addConst",
    "Vesper.Lean4LeanLevel42.pathPrimed_addNode",
    "Vesper.Lean4LeanLevel42.pathPrimed_addVar",
    "Vesper.Lean4LeanLevel42.pathPrimed_extend",
    "Vesper.Lean4LeanLevel42.map_addConst_value_of_primed",
    "Vesper.Lean4LeanLevel42.pathPrimed_one_edge_counterexample",
    "Vesper.Lean4LeanLevel42.pathPrimed_extend_example",
];

const NORMALIZE_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.levelValue_zero",
    "Vesper.Lean4LeanLevel42.levelValue_succ",
    "Vesper.Lean4LeanLevel42.levelValue_max",
    "Vesper.Lean4LeanLevel42.levelValue_imax",
    "Vesper.Lean4LeanLevel42.levelValue_param",
    "Vesper.Lean4LeanLevel42.levelValue_mvar",
    "Vesper.Lean4LeanLevel42.levelSize_pos",
    "Vesper.Lean4LeanLevel42.levelSize_imax_max_left",
    "Vesper.Lean4LeanLevel42.levelSize_imax_max_right",
    "Vesper.Lean4LeanLevel42.levelSize_imax_imax_left",
    "Vesper.Lean4LeanLevel42.levelSize_imax_imax_right",
    "Vesper.Lean4LeanLevel42.nat_imax_zero",
    "Vesper.Lean4LeanLevel42.nat_imax_succ",
    "Vesper.Lean4LeanLevel42.nat_imax_max",
    "Vesper.Lean4LeanLevel42.nat_imax_imax",
    "Vesper.Lean4LeanLevel42.max_add_offset",
    "Vesper.Lean4LeanLevel42.nat_imax_add_offset",
    "Vesper.Lean4LeanLevel42.normalizeAux_preserves_pathPrimed",
    "Vesper.Lean4LeanLevel42.normalizeAux_preserves_WF",
    "Vesper.Lean4LeanLevel42.normalizeAux_empty_WF",
    "Vesper.Lean4LeanLevel42.normalizeAux_value",
    "Vesper.Lean4LeanLevel42.normalizeAux_empty_value",
    "Vesper.Lean4LeanLevel42.normalizeAux_imax_max_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_imax_imax_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_member_zero_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_member_positive_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_disabled_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_retains_old_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_primed_duplicate_witness",
    "Vesper.Lean4LeanLevel42.normalizeAux_succ_mvar_counterexample",
];

const NORMALIZATION_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.subsumeVars_sublist",
    "Vesper.Lean4LeanLevel42.node_subsumeBy_var_sublist",
    "Vesper.Lean4LeanLevel42.node_subsume_var_sublist",
    "Vesper.Lean4LeanLevel42.node_subsume_WF",
    "Vesper.Lean4LeanLevel42.node_isEmpty_value_zero",
    "Vesper.Lean4LeanLevel42.node_subsume_vars_union",
    "Vesper.Lean4LeanLevel42.node_subsume_same_context",
    "Vesper.Lean4LeanLevel42.self_context_needs_vars_counterexample",
    "Vesper.Lean4LeanLevel42.minimize_var_sublist",
    "Vesper.Lean4LeanLevel42.minimize_WF",
    "Vesper.Lean4LeanLevel42.minimize_vars_context",
    "Vesper.Lean4LeanLevel42.minimize_value_context",
    "Vesper.Lean4LeanLevel42.minimize_missing_lookup_counterexample",
    "Vesper.Lean4LeanLevel42.minimizeEntry_value",
    "Vesper.Lean4LeanLevel42.minimizeEntry_WF",
    "Vesper.Lean4LeanLevel42.minimizeEntry_get_other",
    "Vesper.Lean4LeanLevel42.subsumption_value",
    "Vesper.Lean4LeanLevel42.subsumption_WF",
    "Vesper.Lean4LeanLevel42.subsumption_needs_WF_counterexample",
    "Vesper.Lean4LeanLevel42.subsumption_erases_dominated_row",
    "Vesper.Lean4LeanLevel42.normalize_value",
    "Vesper.Lean4LeanLevel42.normalize_WF",
    "Vesper.Lean4LeanLevel42.normalize_value_witness",
];

const COMPARISON_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.norm_all_eq_toList_all",
    "Vesper.Lean4LeanLevel42.norm_all_eq_true_iff",
    "Vesper.Lean4LeanLevel42.norm_all_lookup",
    "Vesper.Lean4LeanLevel42.compareStep_none_bound",
    "Vesper.Lean4LeanLevel42.compareStep_some_union",
    "Vesper.Lean4LeanLevel42.compareFold_none_bound",
    "Vesper.Lean4LeanLevel42.norm_le_sound",
    "Vesper.Lean4LeanLevel42.geq_sound",
    "Vesper.Lean4LeanLevel42.norm_le_needs_right_WF_counterexample",
    "Vesper.Lean4LeanLevel42.norm_le_split_dominators",
    "Vesper.Lean4LeanLevel42.norm_le_raw_empty_row_incomplete",
    "Vesper.Lean4LeanLevel42.geq_split_dominators",
];

const SHAPE_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.varNode_beq_iff_eq",
    "Vesper.Lean4LeanLevel42.varNode_lawfulBEq",
    "Vesper.Lean4LeanLevel42.node_beq_iff_eq",
    "Vesper.Lean4LeanLevel42.node_lawfulBEq",
    "Vesper.Lean4LeanLevel42.nodeOption_beq_iff_eq",
    "Vesper.Lean4LeanLevel42.norm_beq_iff_lookup_eq",
    "Vesper.Lean4LeanLevel42.norm_lookup_eq_mapValue",
    "Vesper.Lean4LeanLevel42.norm_beq_mapValue",
    "Vesper.Lean4LeanLevel42.map_keysSorted_empty",
    "Vesper.Lean4LeanLevel42.map_varsSorted_empty",
    "Vesper.Lean4LeanLevel42.map_addConst_keysSorted",
    "Vesper.Lean4LeanLevel42.map_addNode_keysSorted",
    "Vesper.Lean4LeanLevel42.map_addVar_keysSorted",
    "Vesper.Lean4LeanLevel42.map_addConst_varsSorted",
    "Vesper.Lean4LeanLevel42.map_addNode_varsSorted",
    "Vesper.Lean4LeanLevel42.map_addVar_varsSorted",
    "Vesper.Lean4LeanLevel42.normalizeAux_keysSorted",
    "Vesper.Lean4LeanLevel42.normalizeAux_varsSorted",
    "Vesper.Lean4LeanLevel42.subsumption_keysSorted",
    "Vesper.Lean4LeanLevel42.subsumption_varsSorted",
    "Vesper.Lean4LeanLevel42.subsumption_noEmpty",
    "Vesper.Lean4LeanLevel42.normalize_keysSorted",
    "Vesper.Lean4LeanLevel42.normalize_varsSorted",
    "Vesper.Lean4LeanLevel42.normalize_noEmpty",
    "Vesper.Lean4LeanLevel42.normalize_shape",
    "Vesper.Lean4LeanLevel42.normalized_beq_value",
];

const ADDABILITY_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.subset_sublist",
    "Vesper.Lean4LeanLevel42.keySorted_erase",
    "Vesper.Lean4LeanLevel42.subset_complete",
    "Vesper.Lean4LeanLevel42.subset_eq_true_iff",
    "Vesper.Lean4LeanLevel42.erase_subset_iff_subset_cons",
    "Vesper.Lean4LeanLevel42.subset_needs_left_sorted",
    "Vesper.Lean4LeanLevel42.subset_needs_right_sorted",
    "Vesper.Lean4LeanLevel42.subset_duplicate_left_counterexample",
    "Vesper.Lean4LeanLevel42.erase_subset_needs_nodup_counterexample",
    "Vesper.Lean4LeanLevel42.norm_any_eq_toList_any",
    "Vesper.Lean4LeanLevel42.norm_any_eq_true_iff",
    "Vesper.Lean4LeanLevel42.norm_any_witness",
    "Vesper.Lean4LeanLevel42.norm_any_of_lookup",
    "Vesper.Lean4LeanLevel42.norm_any_congr_lookup",
    "Vesper.Lean4LeanLevel42.addable_iff_witness",
    "Vesper.Lean4LeanLevel42.addable_iff_subset",
    "Vesper.Lean4LeanLevel42.addable_mono_sorted_acc",
    "Vesper.Lean4LeanLevel42.addable_mono_keys",
    "Vesper.Lean4LeanLevel42.addable_orderedInsert",
    "Vesper.Lean4LeanLevel42.addable_lookup_congr",
    "Vesper.Lean4LeanLevel42.addable_beq_congr",
    "Vesper.Lean4LeanLevel42.addable_needs_sorted_target",
    "Vesper.Lean4LeanLevel42.addable_needs_sorted_keys",
    "Vesper.Lean4LeanLevel42.addable_nonzero_offset_witness",
    "Vesper.Lean4LeanLevel42.reifyCycle_lookup",
    "Vesper.Lean4LeanLevel42.reifyCycle_WF",
    "Vesper.Lean4LeanLevel42.reifyCycle_keysSorted",
    "Vesper.Lean4LeanLevel42.reifyCycle_varsSorted",
    "Vesper.Lean4LeanLevel42.reifyCycle_noEmpty",
    "Vesper.Lean4LeanLevel42.reifyCycle_pathPrimed",
    "Vesper.Lean4LeanLevel42.reifyCycle_addable_a",
    "Vesper.Lean4LeanLevel42.reifyCycle_addable_b",
    "Vesper.Lean4LeanLevel42.reifyCycle_feasible",
    "Vesper.Lean4LeanLevel42.reifyCycle_lexChain",
    "Vesper.Lean4LeanLevel42.reifyCycle_toTree_reify",
    "Vesper.Lean4LeanLevel42.reifyCycle_mapValue",
    "Vesper.Lean4LeanLevel42.reifyCycle_reifiedValue",
    "Vesper.Lean4LeanLevel42.reifyCycle_counterexample",
];

const COMPLETION_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.varSupport_refl",
    "Vesper.Lean4LeanLevel42.varSupport_trans",
    "Vesper.Lean4LeanLevel42.edgeCovered_acc_mono",
    "Vesper.Lean4LeanLevel42.edgeCovered_acc_congr",
    "Vesper.Lean4LeanLevel42.admissible_acc_mono",
    "Vesper.Lean4LeanLevel42.completes_acc_mono",
    "Vesper.Lean4LeanLevel42.edgeCovered_support",
    "Vesper.Lean4LeanLevel42.admissible_support",
    "Vesper.Lean4LeanLevel42.completes_support",
    "Vesper.Lean4LeanLevel42.mapCompletes_support",
    "Vesper.Lean4LeanLevel42.completes_nil",
    "Vesper.Lean4LeanLevel42.mapCompletes_empty",
    "Vesper.Lean4LeanLevel42.addable_edgeCovered",
    "Vesper.Lean4LeanLevel42.addable_iff_edgeCovered",
    "Vesper.Lean4LeanLevel42.feasible_insert_getD_mem",
    "Vesper.Lean4LeanLevel42.feasible_insert_getD_sorted",
    "Vesper.Lean4LeanLevel42.admissible_erase",
    "Vesper.Lean4LeanLevel42.completes_erase",
    "Vesper.Lean4LeanLevel42.completes_nonempty_edge",
    "Vesper.Lean4LeanLevel42.feasible_go_sound",
    "Vesper.Lean4LeanLevel42.feasible_go_complete",
    "Vesper.Lean4LeanLevel42.feasible_sound",
    "Vesper.Lean4LeanLevel42.feasible_complete",
    "Vesper.Lean4LeanLevel42.feasible_iff_completes",
    "Vesper.Lean4LeanLevel42.feasible_duplicate_present_witness",
    "Vesper.Lean4LeanLevel42.minimizeEntry_varSupport",
    "Vesper.Lean4LeanLevel42.minimizeEntry_key_origin",
    "Vesper.Lean4LeanLevel42.subsumption_varSupport",
    "Vesper.Lean4LeanLevel42.subsumption_key_origin",
    "Vesper.Lean4LeanLevel42.subsumption_mapCompletes",
    "Vesper.Lean4LeanLevel42.feasible_go_lookup_congr",
    "Vesper.Lean4LeanLevel42.feasible_lookup_congr",
    "Vesper.Lean4LeanLevel42.lexChain_lookup_congr",
    "Vesper.Lean4LeanLevel42.lexChain_perm",
    "Vesper.Lean4LeanLevel42.norm_toList_lookup_congr",
    "Vesper.Lean4LeanLevel42.norm_toTree_lookup_congr",
    "Vesper.Lean4LeanLevel42.norm_reify_lookup_congr",
    "Vesper.Lean4LeanLevel42.feasible_beq_congr",
    "Vesper.Lean4LeanLevel42.lexChain_beq_congr",
    "Vesper.Lean4LeanLevel42.norm_toTree_beq_congr",
    "Vesper.Lean4LeanLevel42.norm_reify_beq_congr",
];

const RECONSTRUCTION_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.normalizeAux_completion",
    "Vesper.Lean4LeanLevel42.normalizeAux_empty_completion",
    "Vesper.Lean4LeanLevel42.normalizeAux_preserves_completes",
    "Vesper.Lean4LeanLevel42.admissible_append_iff",
    "Vesper.Lean4LeanLevel42.completes_last_edge",
    "Vesper.Lean4LeanLevel42.lexChain_candidate_exists",
    "Vesper.Lean4LeanLevel42.lexChain_find_some",
    "Vesper.Lean4LeanLevel42.lexChain_no_nonempty_fallback",
    "Vesper.Lean4LeanLevel42.lexChain_nonempty_step",
    "Vesper.Lean4LeanLevel42.lexChain_reverse_admissible",
    "Vesper.Lean4LeanLevel42.lexChain_length_reverse_admissible",
    "Vesper.Lean4LeanLevel42.lexChain_reverse_order_witness",
    "Vesper.Lean4LeanLevel42.levelValue_ofNat",
    "Vesper.Lean4LeanLevel42.levelValue_addOffsetAux",
    "Vesper.Lean4LeanLevel42.levelValue_addOffset",
    "Vesper.Lean4LeanLevel42.reify_mkMax_value",
    "Vesper.Lean4LeanLevel42.reify_mkChild_value",
    "Vesper.Lean4LeanLevel42.reify_children_fold_value",
    "Vesper.Lean4LeanLevel42.reify_vars_fold_value",
    "Vesper.Lean4LeanLevel42.tree_reify_value",
    "Vesper.Lean4LeanLevel42.foldr_max_mem_bound",
    "Vesper.Lean4LeanLevel42.foldr_max_le",
    "Vesper.Lean4LeanLevel42.tree_reify_const_bound",
    "Vesper.Lean4LeanLevel42.tree_reify_vars_bound",
    "Vesper.Lean4LeanLevel42.tree_reify_payload_bound",
    "Vesper.Lean4LeanLevel42.tree_reify_var_bound",
    "Vesper.Lean4LeanLevel42.tree_reify_child_bound",
    "Vesper.Lean4LeanLevel42.tree_reify_edge_bound",
    "Vesper.Lean4LeanLevel42.tree_reify_leaf_value",
    "Vesper.Lean4LeanLevel42.tree_reify_le",
    "Vesper.Lean4LeanLevel42.treeEntriesLookup_cons",
    "Vesper.Lean4LeanLevel42.treeEntriesLookup_none_of_not_mem",
    "Vesper.Lean4LeanLevel42.modifyAt_keys",
    "Vesper.Lean4LeanLevel42.modifyAt_lookup_other",
    "Vesper.Lean4LeanLevel42.modifyAt_keysSorted",
    "Vesper.Lean4LeanLevel42.modifyAt_lookup_target",
    "Vesper.Lean4LeanLevel42.modifyAt_unsorted_target_witness",
];

const NORMALIZED_COMPLETION_REQUIRED: &[&str] = &[
    "Vesper.Lean4LeanLevel42.normalize_mapCompletes",
    "Vesper.Lean4LeanLevel42.normalize_mapCompletes_WF",
];

impl Packet {
    fn spec(self) -> PacketSpec {
        match self {
            Self::Foundations => PacketSpec {
                label: "lean4lean_foundations42",
                module: "Lean4LeanFoundations42",
                projected: "Lean4LeanFoundations42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.Subsumption",
                    "Lean4LeanLevel42",
                    "Lean4LeanNames42",
                    "Lean4LeanFoundations42",
                ],
                required: FOUNDATIONS_REQUIRED,
                scope: "audited_named_foundation_proofs",
                marker: "LEAN4LEAN_FOUNDATIONS42_EVIDENCE",
            },
            Self::Construction => PacketSpec {
                label: "lean4lean_construction42",
                module: "Lean4LeanConstruction42",
                projected: "Lean4LeanConstruction42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanConstruction42",
                ],
                required: CONSTRUCTION_REQUIRED,
                scope: "audited_named_construction_proofs",
                marker: "LEAN4LEAN_CONSTRUCTION42_EVIDENCE",
            },
            Self::Order => PacketSpec {
                label: "lean4lean_order42",
                module: "Lean4LeanOrder42",
                projected: "Lean4LeanOrder42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.SortedKeys",
                    "Lean4LeanOrder42",
                ],
                required: ORDER_REQUIRED,
                scope: "audited_named_order_proofs",
                marker: "LEAN4LEAN_ORDER42_EVIDENCE",
            },
            Self::Map => PacketSpec {
                label: "lean4lean_map42",
                module: "Lean4LeanMap42",
                projected: "Lean4LeanMap42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanMap42",
                ],
                required: MAP_REQUIRED,
                scope: "audited_named_map_construction_proofs",
                marker: "LEAN4LEAN_MAP42_EVIDENCE",
            },
            Self::Normalize => PacketSpec {
                label: "lean4lean_normalize42",
                module: "Lean4LeanNormalize42",
                projected: "Lean4LeanNormalize42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.NormalizeInvariants",
                    "Lean4LeanLevel42.NormalizeValue",
                    "Lean4LeanNormalize42",
                ],
                required: NORMALIZE_REQUIRED,
                scope: "audited_pre_subsumption_normalization_proofs",
                marker: "LEAN4LEAN_NORMALIZE42_EVIDENCE",
            },
            Self::Normalization => PacketSpec {
                label: "lean4lean_normalization42",
                module: "Lean4LeanNormalization42",
                projected: "Lean4LeanNormalization42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.Subsumption",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.NormalizeInvariants",
                    "Lean4LeanLevel42.NormalizeValue",
                    "Lean4LeanLevel42.NodeResidual",
                    "Lean4LeanLevel42.MapMinimize",
                    "Lean4LeanLevel42.MapSubsumption",
                    "Lean4LeanLevel42.NormalizeFinal",
                    "Lean4LeanNormalization42",
                ],
                required: NORMALIZATION_REQUIRED,
                scope: "audited_normalization_map_denotation",
                marker: "LEAN4LEAN_NORMALIZATION42_EVIDENCE",
            },
            Self::Comparison => PacketSpec {
                label: "lean4lean_comparison42",
                module: "Lean4LeanComparison42",
                projected: "Lean4LeanComparison42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.Subsumption",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.NormalizeInvariants",
                    "Lean4LeanLevel42.NormalizeValue",
                    "Lean4LeanLevel42.NodeResidual",
                    "Lean4LeanLevel42.MapMinimize",
                    "Lean4LeanLevel42.MapSubsumption",
                    "Lean4LeanLevel42.NormalizeFinal",
                    "Lean4LeanLevel42.MapAll",
                    "Lean4LeanLevel42.CompareFold",
                    "Lean4LeanLevel42.MapComparison",
                    "Lean4LeanComparison42",
                ],
                required: COMPARISON_REQUIRED,
                scope: "audited_level_comparison_soundness",
                marker: "LEAN4LEAN_COMPARISON42_EVIDENCE",
            },
            Self::Shape => PacketSpec {
                label: "lean4lean_shape42",
                module: "Lean4LeanLevel42.ShapeFinal",
                projected: "Lean4LeanShape42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.Subsumption",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.SortedKeys",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.NormalizeInvariants",
                    "Lean4LeanLevel42.NormalizeValue",
                    "Lean4LeanLevel42.NodeResidual",
                    "Lean4LeanLevel42.MapMinimize",
                    "Lean4LeanLevel42.MapSubsumption",
                    "Lean4LeanLevel42.MapAll",
                    "Lean4LeanLevel42.Shape",
                    "Lean4LeanLevel42.MapEquality",
                    "Lean4LeanLevel42.ConstructionShape",
                    "Lean4LeanLevel42.SubsumptionShape",
                    "Lean4LeanLevel42.ShapeFinal",
                ],
                required: SHAPE_REQUIRED,
                scope: "audited_normalized_map_equality_and_shape",
                marker: "LEAN4LEAN_SHAPE42_EVIDENCE",
            },
            Self::Addability => PacketSpec {
                label: "lean4lean_addability42",
                module: "Lean4LeanLevel42.AddabilityFinal",
                projected: "Lean4LeanAddability42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.SortedKeys",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.MapAll",
                    "Lean4LeanLevel42.MapEquality",
                    "Lean4LeanLevel42.SortedSubset",
                    "Lean4LeanLevel42.MapAny",
                    "Lean4LeanLevel42.Addable",
                    "Lean4LeanLevel42.ReifyCycle",
                    "Lean4LeanLevel42.AddabilityFinal",
                ],
                required: ADDABILITY_REQUIRED,
                scope: "audited_addability_foundations_and_raw_reification_boundary",
                marker: "LEAN4LEAN_ADDABILITY42_EVIDENCE",
            },
            Self::Completion => PacketSpec {
                label: "lean4lean_completion42",
                module: "Lean4LeanLevel42.CompletionFinal",
                projected: "Lean4LeanCompletion42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.Subsumption",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.SortedKeys",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapAll",
                    "Lean4LeanLevel42.MapEquality",
                    "Lean4LeanLevel42.SortedSubset",
                    "Lean4LeanLevel42.MapAny",
                    "Lean4LeanLevel42.Addable",
                    "Lean4LeanLevel42.NodeResidual",
                    "Lean4LeanLevel42.MapMinimize",
                    "Lean4LeanLevel42.MapSubsumption",
                    "Lean4LeanLevel42.Completion",
                    "Lean4LeanLevel42.Feasible",
                    "Lean4LeanLevel42.VariableSupport",
                    "Lean4LeanLevel42.ReifyCongruence",
                    "Lean4LeanLevel42.CompletionFinal",
                ],
                required: COMPLETION_REQUIRED,
                scope: "audited_completion_prerequisites_and_reconstruction_congruence",
                marker: "LEAN4LEAN_COMPLETION42_EVIDENCE",
            },
            Self::Reconstruction => PacketSpec {
                label: "lean4lean_reconstruction42",
                module: "Lean4LeanLevel42.ReconstructionFinal",
                projected: "Lean4LeanReconstruction42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.SortedKeys",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.MapAll",
                    "Lean4LeanLevel42.MapEquality",
                    "Lean4LeanLevel42.SortedSubset",
                    "Lean4LeanLevel42.MapAny",
                    "Lean4LeanLevel42.Addable",
                    "Lean4LeanLevel42.Completion",
                    "Lean4LeanLevel42.Feasible",
                    "Lean4LeanLevel42.ReifyCongruence",
                    "Lean4LeanLevel42.ConstructionCompletion",
                    "Lean4LeanLevel42.LexChainCompletion",
                    "Lean4LeanLevel42.TreeReifyValue",
                    "Lean4LeanLevel42.TreeStructure",
                    "Lean4LeanLevel42.ReconstructionFinal",
                ],
                required: RECONSTRUCTION_REQUIRED,
                scope: "audited_construction_completion_admissible_chains_and_tree_primitives",
                marker: "LEAN4LEAN_RECONSTRUCTION42_EVIDENCE",
            },
            Self::NormalizedCompletion => PacketSpec {
                label: "lean4lean_normalized_completion42",
                module: "Lean4LeanLevel42.NormalizedCompletionFinal",
                projected: "Lean4LeanNormalizedCompletion42Kernel",
                source_modules: &[
                    "Lean4Lean.List",
                    "Lean4Lean.Level",
                    "Lean4LeanLevel42.Semantics",
                    "Lean4LeanLevel42.ListProofs",
                    "Lean4LeanLevel42.NameOrder",
                    "Lean4LeanLevel42.MapValue",
                    "Lean4LeanLevel42.Completion",
                    "Lean4LeanLevel42.VarInsert",
                    "Lean4LeanLevel42.MapHelpers",
                    "Lean4LeanLevel42.LevelSemantics",
                    "Lean4LeanLevel42.OrderedInsert",
                    "Lean4LeanLevel42.ConstructionCompletion",
                    "Lean4LeanLevel42.Subsumption",
                    "Lean4LeanLevel42.NodeResidual",
                    "Lean4LeanLevel42.MapMinimize",
                    "Lean4LeanLevel42.MapSubsumption",
                    "Lean4LeanLevel42.VariableSupport",
                    "Lean4LeanLevel42.PathPriming",
                    "Lean4LeanLevel42.NormalizeInvariants",
                    "Lean4LeanLevel42.NormalizedCompletion",
                    "Lean4LeanLevel42.NormalizedCompletionFinal",
                ],
                required: NORMALIZED_COMPLETION_REQUIRED,
                scope: "audited_normalized_completion_and_wellformedness",
                marker: "LEAN4LEAN_NORMALIZED_COMPLETION42_EVIDENCE",
            },
        }
    }
}

const POLICY: [&str; 3] = ["Classical.choice", "Quot.sound", "propext"];
const CAPTURE: usize = 8 * 1024 * 1024;
const SOURCE_CAP: usize = 4 * 1024 * 1024;
const SOURCE_COUNT: usize = 24;
const TOTAL_SECONDS: u64 = 300;
const SNAPSHOT_MARKER: &str = "VESPER_RESOURCE_KERNEL_SNAPSHOT_V1:";
struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl Captured {
    fn diagnostic(&self) -> String {
        format!(
            "{}\n{}",
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
    fn success(&self) {
        assert!(self.status.success(), "{}", self.diagnostic());
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.stdout).unwrap_or_else(|error| {
            panic!("invalid actual audit JSON: {error}\n{}", self.diagnostic())
        })
    }
}

struct CaptureFiles {
    root: PathBuf,
}
impl CaptureFiles {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-lean4lean-proof-packet-output-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }
    fn file(&self, name: &str) -> fs::File {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join(name))
            .unwrap()
    }
    fn overflow(&self) -> bool {
        ["stdout", "stderr"]
            .into_iter()
            .any(|name| fs::metadata(self.root.join(name)).unwrap().len() > CAPTURE as u64)
    }
    fn read(&self, name: &str) -> Vec<u8> {
        let mut output = Vec::new();
        fs::File::open(self.root.join(name))
            .unwrap()
            .take((CAPTURE + 1) as u64)
            .read_to_end(&mut output)
            .unwrap();
        output
    }
}
impl Drop for CaptureFiles {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn windows_environment(command: &mut Command) {
    for name in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn execute(command: &mut Command, seconds: u64, target_start: Instant) -> Captured {
    let start = Instant::now();
    let remaining = Duration::from_secs(TOTAL_SECONDS)
        .checked_sub(target_start.elapsed())
        .expect("finite target deadline");
    let deadline = Duration::from_secs(seconds).min(remaining);
    // Regular capture files prevent a descendant holding an inherited pipe
    // from causing an unbounded reader-thread join after the direct child dies.
    // They live outside the artifact root, so the auditor never snapshots its
    // own changing stdout. This does not claim descendant-tree cancellation.
    let capture = CaptureFiles::new();
    let mut child = command
        .stdout(Stdio::from(capture.file("stdout")))
        .stderr(Stdio::from(capture.file("stderr")))
        .spawn()
        .unwrap_or_else(|error| panic!("required real process {command:?}: {error}"));
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut refused = false;
    let status = loop {
        if capture.overflow() || start.elapsed() > deadline {
            refused = true;
            let _ = child.kill();
            break child.wait().expect("reap refused real process");
        }
        if let Some(status) = child.try_wait().expect("poll real process") {
            break status;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = capture.read("stdout");
    let stderr = capture.read("stderr");
    let overflow = stdout.len() > CAPTURE || stderr.len() > CAPTURE;
    let captured = Captured {
        status,
        stdout,
        stderr,
    };
    assert!(
        !captured.diagnostic().contains("PANIC"),
        "actual Lean panic is never success: {}",
        captured.diagnostic()
    );
    assert!(
        !refused && !overflow,
        "real process exceeded finite time/output budget: {command:?}\n{}",
        captured.diagnostic()
    );
    eprintln!(
        "lean4lean_proof_packet process {:.2}s: {command:?}",
        start.elapsed().as_secs_f64()
    );
    captured
}

fn module_path(module: &str, suffix: &str) -> PathBuf {
    assert!(module.split('.').all(valid_name));
    let mut path = PathBuf::new();
    let mut components = module.split('.').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            path.push(component);
        } else {
            path.push(format!("{component}.{suffix}"));
        }
    }
    path
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || byte == b'_'
                || (index > 0 && (byte.is_ascii_digit() || byte == b'\''))
        })
}

struct FixedSource {
    text: String,
    imports: Vec<String>,
}

fn read_source(path: &Path) -> FixedSource {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(
        metadata.is_file(),
        "regular fixed source: {}",
        path.display()
    );
    assert!(metadata.len() <= SOURCE_CAP as u64, "bounded fixed source");
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take((SOURCE_CAP + 1) as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= SOURCE_CAP, "bounded fixed source");
    let text = String::from_utf8(bytes).unwrap();
    let imports = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("import "))
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect();
    FixedSource { text, imports }
}

fn fixed_sources(spec: &PacketSpec) -> BTreeMap<String, FixedSource> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut sources = BTreeMap::new();
    assert!(spec.source_modules.len() <= SOURCE_COUNT);
    for &module in spec.source_modules {
        let base = if matches!(module, "Lean4Lean.List" | "Lean4Lean.Level") {
            repo.join("tools/lean4lean-foundations42/upstream")
        } else {
            repo.join("lean")
        };
        assert!(
            sources
                .insert(
                    module.to_owned(),
                    read_source(&base.join(module_path(module, "lean")))
                )
                .is_none()
        );
    }
    sources
}

fn digest(value: &Value) -> Vec<u8> {
    let a = value.as_array().unwrap();
    assert_eq!(a.len(), 32);
    a.iter()
        .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
        .collect()
}
fn matches_record(bytes: &[u8], record: &Value) -> bool {
    record["bytes"].as_u64() == Some(bytes.len() as u64)
        && digest(&record["blake3"]) == blake3::hash(bytes).as_bytes().as_slice()
}
fn provenance(sources: &BTreeMap<String, FixedSource>) -> Value {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let read = |relative: &str| {
        let p = repo.join(relative);
        assert!(fs::symlink_metadata(&p).unwrap().file_type().is_file());
        let b = fs::read(p).unwrap();
        assert!(b.len() <= SOURCE_CAP);
        b
    };
    let bytes = read("tools/lean4lean-foundations42/source-provenance.json");
    let p: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(p["schema_version"], 1);
    assert_eq!(p["scope"], "fixed_checker_helper_source_correspondence");
    assert_eq!(
        p["upstream_commit"],
        "4faf51403bc4cb35cf6666d91fc9c938ae46d3aa"
    );
    assert_eq!(p["lean_commit"], "797c613eb9b6d4ec95db23e3e00af9ac6657f24b");
    let upstream: Value =
        serde_json::from_slice(&read("tools/lean4lean42/source-manifest.json")).unwrap();
    let entries = p["files"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let mut seen = BTreeSet::new();
    for entry in entries {
        let module = entry["module"].as_str().unwrap();
        assert!(seen.insert(module));
        assert!(matches!(module, "Lean4Lean.List" | "Lean4Lean.Level"));
        let expected = format!(
            "tools/lean4lean-foundations42/upstream/{}",
            module_path(module, "lean")
                .to_string_lossy()
                .replace('\\', "/")
        );
        assert_eq!(entry["vendored_path"], expected);
        assert_eq!(
            entry["source_path"],
            module_path(module, "lean")
                .to_string_lossy()
                .replace('\\', "/")
        );
        let actual = read(&expected);
        assert_eq!(actual, sources[module].text.as_bytes());
        assert!(matches_record(&actual, entry));
        let pinned = upstream["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == entry["source_path"])
            .expect("selected helper remains bound to existing checker manifest");
        assert_eq!(pinned["bytes"], entry["bytes"]);
        assert_eq!(pinned["blake3"], entry["blake3"]);
        assert_eq!(entry["sha256"].as_str().unwrap().len(), 64);
    }
    for (field, expected_path) in [
        ("kernel_patch", "tools/lean4lean42/patches/kernel.patch"),
        ("license", "tools/lean4lean-foundations42/upstream/LICENSE"),
    ] {
        let entry = &p[field];
        let path = entry["path"].as_str().unwrap();
        assert_eq!(path, expected_path);
        let actual = read(path);
        assert!(matches_record(&actual, entry));
    }
    json!({"path":"tools/lean4lean-foundations42/source-provenance.json","bytes":bytes.len(),"blake3":blake3::hash(&bytes).to_hex().to_string()})
}
fn compile_order(spec: &PacketSpec, sources: &BTreeMap<String, FixedSource>) -> Vec<String> {
    let mut imported = BTreeSet::new();
    let mut pending = vec![spec.module.to_owned()];
    while let Some(module) = pending.pop() {
        if imported.insert(module.clone()) {
            for dependency in &sources[&module].imports {
                if sources.contains_key(dependency) {
                    pending.push(dependency.clone());
                }
            }
        }
    }
    assert_eq!(
        imported,
        sources.keys().cloned().collect(),
        "every fixed model definition must enter the replayed artifact closure"
    );
    let mut remaining: BTreeSet<_> = sources.keys().cloned().collect();
    let mut done = BTreeSet::new();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .find(|module| {
                sources[*module].imports.iter().all(|dependency| {
                    if dependency == "Lean"
                        || dependency.starts_with("Lean.")
                        || dependency == "Std"
                        || dependency == "Init"
                        || dependency.starts_with("Std.")
                        || dependency.starts_with("Init.")
                    {
                        true
                    } else {
                        assert!(
                            sources.contains_key(dependency),
                            "fixed dependency absent: {dependency}"
                        );
                        done.contains(dependency)
                    }
                })
            })
            .cloned()
            .expect("acyclic complete fixed module imports");
        remaining.remove(&ready);
        done.insert(ready.clone());
        order.push(ready);
    }
    order
}

// Strip nested block/line comments and strings before inventorying the fixed
// source's explicit commands. Unsupported layouts fail rather than omit roots.
fn commands(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let (mut index, mut depth, mut string) = (0, 0usize, false);
    while index < bytes.len() {
        if depth > 0 {
            if bytes[index..].starts_with(b"/-") {
                depth += 1;
                output.extend_from_slice(b"  ");
                index += 2;
            } else if bytes[index..].starts_with(b"-/") {
                depth -= 1;
                output.extend_from_slice(b"  ");
                index += 2;
            } else {
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
        } else if string {
            if bytes[index] == b'\\' && index + 1 < bytes.len() {
                output.extend_from_slice(b"  ");
                index += 2;
            } else {
                if bytes[index] == b'"' {
                    string = false;
                }
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/-") {
            depth = 1;
            output.extend_from_slice(b"  ");
            index += 2;
        } else if bytes[index..].starts_with(b"--") {
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(b' ');
                index += 1;
            }
        } else if bytes[index] == b'"' {
            string = true;
            output.push(b' ');
            index += 1;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    assert_eq!(depth, 0, "closed fixed-source comment");
    assert!(!string, "closed fixed-source string");
    String::from_utf8(output).unwrap()
}

fn theorem_catalog(spec: &PacketSpec, sources: &BTreeMap<String, FixedSource>) -> Vec<String> {
    let mut roots = BTreeSet::new();
    for (module, source) in sources.iter().filter(|(module, _)| {
        module.starts_with("Lean4LeanLevel42") || module.starts_with("Lean4LeanNames42")
    }) {
        let mut scopes: Vec<Option<String>> = Vec::new();
        for line in commands(&source.text).lines() {
            let mut line = line.trim();
            while let Some(attribute) = line.strip_prefix("@[") {
                line = attribute
                    .split_once(']')
                    .expect("fixed complete inline attribute")
                    .1
                    .trim();
            }
            if let Some(namespace) = line.strip_prefix("namespace ") {
                let name = namespace.split_whitespace().next().unwrap();
                assert!(name.split('.').all(valid_name));
                scopes.push(Some(name.to_owned()));
            } else if line == "section"
                || line.starts_with("section ")
                || line == "noncomputable section"
                || line == "mutual"
            {
                scopes.push(None);
            } else if line == "end" || line.starts_with("end ") {
                scopes
                    .pop()
                    .expect("balanced fixed-source namespace/section");
            } else {
                let declaration = line.strip_prefix("protected ").unwrap_or(line);
                assert!(
                    !declaration.starts_with("private theorem ")
                        && !declaration.starts_with("local theorem "),
                    "all new named theorems public: {module}"
                );
                if let Some(theorem) = declaration.strip_prefix("theorem ") {
                    let name = theorem
                        .split(|c: char| c.is_whitespace() || matches!(c, '(' | '{' | ':' | '['))
                        .next()
                        .unwrap();
                    assert!(
                        name.split('.').all(valid_name),
                        "auditable ASCII theorem {name}"
                    );
                    let mut parts = scopes
                        .iter()
                        .filter_map(Option::as_deref)
                        .collect::<Vec<_>>();
                    parts.push(name);
                    let root = parts.join(".");
                    assert!(
                        root.starts_with("Vesper.Lean4LeanLevel42.")
                            || root.starts_with("Vesper.Lean4LeanNames42."),
                        "actual namespace: {root}"
                    );
                    assert!(roots.insert(root), "duplicate explicit theorem in {module}");
                } else {
                    assert!(
                        !declaration
                            .split_whitespace()
                            .any(|token| matches!(token, "theorem" | "lemma")),
                        "unsupported theorem command layout in {module}: {line}"
                    );
                }
            }
        }
        assert!(scopes.is_empty(), "closed scopes in {module}");
    }
    for required in spec.required.iter().copied() {
        assert!(
            roots.contains(required),
            "required actual packet theorem {required}"
        );
    }
    assert!(!roots.is_empty());
    roots.into_iter().collect()
}

struct Fixture {
    root: PathBuf,
    lean: PathBuf,
    vesper: PathBuf,
    prefix: PathBuf,
    started: Instant,
}

type ArtifactHashes = BTreeMap<String, (u64, String)>;

fn artifact_hashes(root: &Path) -> ArtifactHashes {
    fn visit(root: &Path, directory: &Path, hashes: &mut ArtifactHashes, total: &mut u64) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), hashes, total);
                continue;
            }
            assert!(
                entry.file_type().unwrap().is_file(),
                "private artifact must be regular"
            );
            assert!(hashes.len() < 4096);
            let size = entry.metadata().unwrap().len();
            *total = total.checked_add(size).unwrap();
            assert!(
                *total <= 128 * 1024 * 1024,
                "bounded actual artifact inventory"
            );
            let mut hasher = blake3::Hasher::new();
            let mut file = fs::File::open(entry.path()).unwrap();
            let mut buffer = [0; 8192];
            let mut read = 0u64;
            loop {
                let count = file.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                read += count as u64;
                assert!(read <= size, "artifact changed while hashing");
                hasher.update(&buffer[..count]);
            }
            assert_eq!(read, size);
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            assert!(
                hashes
                    .insert(relative, (size, hasher.finalize().to_hex().to_string()))
                    .is_none()
            );
        }
    }
    let mut hashes = BTreeMap::new();
    visit(root, root, &mut hashes, &mut 0);
    hashes
}

struct Projection {
    fixture: Fixture,
    module: String,
    report: Value,
    original_hashes: ArtifactHashes,
    projected_hashes: ArtifactHashes,
    tool_blake3: String,
}
impl Fixture {
    fn new(started: Instant) -> Self {
        let launcher = std::env::var_os("VESPER_LEAN").unwrap_or_else(|| "lean".into());
        let prefix = execute(
            Command::new(launcher)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .arg("--print-prefix"),
            30,
            started,
        );
        prefix.success();
        let prefix = PathBuf::from(String::from_utf8(prefix.stdout).unwrap().trim())
            .canonicalize()
            .unwrap();
        let lean = prefix
            .join("bin")
            .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
        let version = execute(Command::new(&lean).arg("--version"), 10, started);
        version.success();
        assert!(
            String::from_utf8_lossy(&version.stdout).contains("version 4.24.0,")
                && String::from_utf8_lossy(&version.stdout).contains("commit 797c613eb9b6"),
            "required real Lean4.24: {}",
            version.diagnostic()
        );
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-lean4lean-proof-packet-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            lean,
            vesper: resolve_vesper(),
            prefix,
            started,
        }
    }
    fn compile(&self, module: &str, source: &str) -> Captured {
        assert!(source.len() <= SOURCE_CAP);
        let path = self.root.join(module_path(module, "lean"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, source).unwrap();
        let mut command = Command::new(&self.lean);
        command
            .env_clear()
            .current_dir(&self.root)
            .env(
                "LEAN_PATH",
                std::env::join_paths([self.root.clone(), self.prefix.join("lib/lean")]).unwrap(),
            )
            .env("LEAN_NUM_THREADS", "1")
            .env("LEAN_ABORT_ON_PANIC", "1");
        windows_environment(&mut command);
        execute(
            command
                .args(["-j1", "-M4096", "-o"])
                .arg(module_path(module, "olean"))
                .arg(module_path(module, "lean")),
            300,
            self.started,
        )
    }
    fn sibling(&self, label: &str, create: bool) -> Self {
        assert!(valid_name(label));
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "vesper-lean4lean-proof-packet-case-{}-{}-{label}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!root.exists());
        if create {
            fs::create_dir(&root).unwrap();
        }
        Self {
            root,
            lean: self.lean.clone(),
            vesper: self.vesper.clone(),
            prefix: self.prefix.clone(),
            started: self.started,
        }
    }
    fn snapshot_attempt(
        &self,
        module: &str,
        output_module: &str,
    ) -> (Captured, Self, ArtifactHashes, String) {
        let output = self.sibling("Projection", false);
        let request_directory = CaptureFiles::new();
        let request_path = request_directory.root.join("request.json");
        let request = json!({"schema_version":1,"root":self.root.canonicalize().unwrap(),
            "toolchain_prefix":self.prefix,"module":module,
            "output_root":output.root,"output_module":output_module});
        fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
        let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tools/resource-model42/KernelSnapshot.lean")
            .canonicalize()
            .unwrap();
        let tool_source = read_source(&tool).text;
        let tool_blake3 = blake3::hash(tool_source.as_bytes()).to_hex().to_string();
        let before = artifact_hashes(&self.root);
        assert!(
            before.keys().all(|path| !path.ends_with(".ir")
                && !path.ends_with(".olean.private")
                && !path.ends_with(".olean.server")),
            "snapshot acceptance profile uses fresh legacy objects without package sidecars"
        );
        let mut command = Command::new(&self.lean);
        command
            .env_clear()
            .current_dir(&request_directory.root)
            .env("LEAN_PATH", self.prefix.join("lib/lean"))
            .env("LEAN_NUM_THREADS", "1")
            .env("LEAN_ABORT_ON_PANIC", "1");
        windows_environment(&mut command);
        let captured = execute(
            command
                .args(["-j1", "-M4096", "--run"])
                .arg(&tool)
                .arg(&request_path),
            300,
            self.started,
        );
        assert_eq!(
            before,
            artifact_hashes(&self.root),
            "snapshot must not change original input bytes"
        );
        assert_eq!(
            tool_blake3,
            blake3::hash(read_source(&tool).text.as_bytes())
                .to_hex()
                .to_string()
        );
        (captured, output, before, tool_blake3)
    }
    fn project(
        &self,
        module: &str,
        output_module: &str,
        expected_modules: &BTreeSet<String>,
    ) -> Projection {
        let (captured, fixture, original_hashes, tool_blake3) =
            self.snapshot_attempt(module, output_module);
        captured.success();
        assert!(captured.stderr.is_empty(), "{}", captured.diagnostic());
        let stdout = String::from_utf8(captured.stdout).unwrap();
        let lines = stdout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "exactly one real snapshot success record");
        let report: Value = serde_json::from_str(
            lines[0]
                .strip_prefix(SNAPSHOT_MARKER)
                .expect("actual snapshot marker"),
        )
        .unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["scope"], "kernel_logical_projection");
        assert_eq!(report["original_module"], module);
        assert_eq!(report["projection_module"], output_module);
        let artifact = fixture
            .root
            .join(module_path(output_module, "olean"))
            .canonicalize()
            .unwrap();
        assert_eq!(
            PathBuf::from(report["projection_path"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            artifact
        );
        for flag in [
            "exact_inventory",
            "constant_info_equal",
            "kernel_replayed",
            "reimport_verified",
            "extensions_empty",
            "ir_empty",
        ] {
            assert_eq!(report[flag], true, "real snapshot flag {flag}");
        }
        let original = report["original_constants"].as_u64().unwrap();
        let retained = report["retained_constants"].as_u64().unwrap();
        let omitted = report["omitted_constants"].as_u64().unwrap();
        let occurrences = report["original_constant_occurrences"].as_u64().unwrap();
        let duplicate_count = report["duplicate_constants"].as_u64().unwrap();
        assert!(retained > 0 && original == retained + omitted);
        assert_eq!(occurrences, original.checked_add(duplicate_count).unwrap());
        assert_eq!(report["alias_scope"], "synthetic_theorem_proof_variants");
        let alias_count = report["proof_alias_count"].as_u64().unwrap();
        assert_eq!(
            report["output_constants"].as_u64().unwrap(),
            retained.checked_add(alias_count).unwrap()
        );
        assert!(alias_count <= duplicate_count);
        assert!(report["logical_expression_nodes"].as_u64().unwrap() > 0);
        let mut actual_modules = BTreeSet::new();
        for record in report["original_modules"].as_array().unwrap() {
            let name = record["name"].as_str().unwrap();
            assert!(actual_modules.insert(name.to_owned()));
            let path = PathBuf::from(record["path"].as_str().unwrap())
                .canonicalize()
                .unwrap();
            assert_eq!(
                path,
                self.root
                    .join(module_path(name, "olean"))
                    .canonicalize()
                    .unwrap()
            );
            let relative = path
                .strip_prefix(self.root.canonicalize().unwrap())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            assert!(original_hashes.contains_key(&relative));
        }
        assert_eq!(&actual_modules, expected_modules);
        let duplicates = report["duplicates"].as_array().unwrap();
        assert_eq!(duplicates.len() as u64, duplicate_count);
        for record in duplicates {
            assert!(!record["name"].as_str().unwrap().is_empty());
            assert!(matches!(
                record["reason"].as_str().unwrap(),
                "identical_safe_theorem_realization" | "safe_theorem_proof_variant"
            ));
            for field in ["first_module", "repeated_module"] {
                assert!(actual_modules.contains(record[field].as_str().unwrap()));
            }
        }
        let trusted = report["trusted_modules"].as_array().unwrap();
        assert!(!trusted.is_empty());
        let mut trusted_names = BTreeSet::new();
        let library = self.prefix.join("lib/lean").canonicalize().unwrap();
        for record in trusted {
            assert!(trusted_names.insert(record["name"].as_str().unwrap()));
            assert!(
                PathBuf::from(record["path"].as_str().unwrap())
                    .canonicalize()
                    .unwrap()
                    .starts_with(&library)
            );
        }
        let omissions = report["omitted"].as_array().unwrap();
        assert_eq!(omissions.len() as u64, omitted);
        let mut names = BTreeSet::new();
        for record in omissions {
            let name = record["name"].as_str().unwrap();
            let parent = record["parent"].as_str().unwrap();
            assert!(names.insert(name));
            assert_eq!(name, format!("{parent}._unsafe_rec"));
            assert_eq!(record["reason"], "matched_partial_unsafe_rec");
            assert!(actual_modules.contains(record["module"].as_str().unwrap()));
        }
        let aliases = report["proof_aliases"].as_array().unwrap();
        assert_eq!(aliases.len() as u64, alias_count);
        let mut alias_names = BTreeSet::new();
        for alias in aliases {
            let name = alias["name"].as_str().unwrap();
            let ordinal = name
                .strip_prefix("ResourceKernelProofAliases.realization_")
                .expect("isolated synthetic theorem namespace");
            assert!(!ordinal.is_empty() && ordinal.bytes().all(|byte| byte.is_ascii_digit()));
            assert!(alias_names.insert(name));
            assert_eq!(alias["reason"], "checked_safe_theorem_variant");
            assert!(!alias["original_name"].as_str().unwrap().is_empty());
            for field in ["source_module", "canonical_module"] {
                assert!(actual_modules.contains(alias[field].as_str().unwrap()));
            }
            let _ = alias["source_occurrence"].as_u64().unwrap();
            let group = alias["original_group"]
                .as_array()
                .unwrap()
                .iter()
                .map(|member| member.as_str().unwrap())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                group.len(),
                alias["original_group"].as_array().unwrap().len()
            );
            assert!(group.contains(alias["original_name"].as_str().unwrap()));
        }
        let records = report["occurrences"].as_array().unwrap();
        assert_eq!(records.len() as u64, occurrences);
        let mut positions = BTreeSet::new();
        let mut original_names = BTreeSet::new();
        let mut aliased_occurrences = BTreeSet::new();
        let mut omitted_occurrences = BTreeSet::new();
        for record in records {
            let name = record["name"].as_str().unwrap();
            let module = record["module"].as_str().unwrap();
            let index = record["index"].as_u64().unwrap();
            assert!(actual_modules.contains(module));
            assert!(positions.insert((module, index)));
            original_names.insert(name);
            match record["disposition"].as_str().unwrap() {
                "retained_canonical_info" => assert!(record["alias"].is_null()),
                "omitted_auxiliary" => {
                    assert!(record["alias"].is_null());
                    assert!(names.contains(name));
                    assert!(omitted_occurrences.insert(name));
                }
                "proof_alias" => {
                    let alias_name = record["alias"].as_str().unwrap();
                    assert!(aliased_occurrences.insert(alias_name));
                    let alias = aliases
                        .iter()
                        .find(|alias| alias["name"] == alias_name)
                        .expect("every alternate occurrence maps to one actual alias");
                    assert_eq!(alias["original_name"], name);
                    assert_eq!(alias["source_module"], module);
                    assert_eq!(alias["source_occurrence"], index);
                }
                other => panic!("unexpected original occurrence disposition {other}"),
            }
        }
        assert_eq!(original_names.len() as u64, original);
        assert_eq!(omitted_occurrences, names);
        assert_eq!(aliased_occurrences, alias_names);
        let projected_hashes = artifact_hashes(&fixture.root);
        assert_eq!(
            projected_hashes.len(),
            1,
            "standalone projection has no IR/extensions/side artifacts"
        );
        assert!(
            projected_hashes.contains_key(
                &module_path(output_module, "olean")
                    .to_string_lossy()
                    .replace('\\', "/")
            )
        );
        Projection {
            fixture,
            module: output_module.to_owned(),
            report,
            original_hashes,
            projected_hashes,
            tool_blake3,
        }
    }
    fn audit(&self, module: &str, roots: &[String]) -> Captured {
        assert!(!roots.is_empty() && roots.len() <= 128);
        let mut command = Command::new(&self.vesper);
        command
            .env_clear()
            .current_dir(&self.root)
            .args(["proof-audit", module, "--root"])
            .arg(&self.root)
            .arg("--lean")
            .arg(&self.lean)
            .args(["--timeout", "300", "--json"]);
        windows_environment(&mut command);
        for root in roots {
            command.args(["--theorem", root]);
        }
        for axiom in POLICY {
            command.args(["--allow-axiom", axiom]);
        }
        execute(&mut command, 360, self.started)
    }
    fn clone_artifacts(&self, label: &str) -> Self {
        let sibling = self.sibling(label, true);
        fn copy(from: &Path, to: &Path) {
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let ty = entry.file_type().unwrap();
                let target = to.join(entry.file_name());
                if ty.is_dir() {
                    fs::create_dir(&target).unwrap();
                    copy(&entry.path(), &target);
                } else {
                    assert!(ty.is_file());
                    fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        copy(&self.root, &sibling.root);
        assert_eq!(artifact_hashes(&self.root), artifact_hashes(&sibling.root));
        sibling
    }
}

fn resolve_vesper() -> PathBuf {
    let named = PathBuf::from(env!("CARGO_BIN_EXE_vesper"));
    if named.is_absolute() || named.components().count() > 1 {
        return named
            .canonicalize()
            .expect("actual Cargo/Bazel vesper binary");
    }
    // Bazel deliberately supplies a bare name and its launcher installs the
    // real binary directory on PATH. Resolve it before isolating child envs.
    for directory in std::env::split_paths(&std::env::var_os("PATH").expect("tool PATH")) {
        let candidate = directory.join(&named);
        if candidate.is_file() {
            return candidate.canonicalize().unwrap();
        }
        if cfg!(windows) && named.extension().is_none() {
            let candidate = directory.join(format!("{}.exe", named.to_str().unwrap()));
            if candidate.is_file() {
                return candidate.canonicalize().unwrap();
            }
        }
    }
    panic!(
        "required actual vesper executable {} absent from PATH",
        named.display()
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if std::thread::panicking() && self.root.exists() {
            eprintln!(
                "lean4lean_proof_packet retained failed artifact root: {}",
                self.root.display()
            );
            return;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_audit_error(output: Captured, module: &str) {
    assert!(!output.status.success());
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "error");
    assert_eq!(report["error"]["kind"], "audit_failed");
    let message = report["error"]["message"].as_str().unwrap();
    assert!(!message.is_empty() && message.contains(module), "{message}");
    assert!(
        !message.contains("timed out") && !message.contains("output limit"),
        "actual artifact refusal: {message}"
    );
    assert_ne!(report["accepted"], true);
}

fn all_projection_roots(projected: &Projection, manual: &[String]) -> Vec<String> {
    let mut roots = manual.iter().cloned().collect::<BTreeSet<_>>();
    assert_eq!(roots.len(), manual.len());
    for alias in projected.report["proof_aliases"].as_array().unwrap() {
        assert!(roots.insert(alias["name"].as_str().unwrap().to_owned()));
    }
    assert_eq!(
        roots.len(),
        manual.len() + projected.report["proof_alias_count"].as_u64().unwrap() as usize
    );
    roots.into_iter().collect()
}

fn positive_audit(fixture: &Fixture, module: &str, roots: &[String]) -> Value {
    let before = artifact_hashes(&fixture.root);
    let output = fixture.audit(module, roots);
    output.success();
    let report = output.json();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["status"], "audited");
    assert_eq!(report["accepted"], true);
    assert_eq!(report["kernel_replayed"], true);
    assert_eq!(report["cache_eligible"], false);
    assert_eq!(report["module"], module);
    assert_eq!(report["allowed_axioms"], json!(POLICY));
    assert_eq!(report["forbidden_axioms"], json!([]));
    assert!(report["checked_constants"].as_u64().unwrap() > 0);
    assert_eq!(report["lean_binary_blake3"].as_str().unwrap().len(), 64);
    assert_eq!(report["auditor_blake3"].as_str().unwrap().len(), 64);
    let artifact_modules = report["imports"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["scope"] == "artifact")
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(artifact_modules, BTreeSet::from([module.to_owned()]));
    let mut actual = BTreeSet::new();
    for theorem in report["theorems"].as_array().unwrap() {
        assert!(actual.insert(theorem["name"].as_str().unwrap().to_owned()));
        for axiom in theorem["axioms"].as_array().unwrap() {
            assert!(POLICY.contains(&axiom.as_str().unwrap()), "{theorem}");
        }
    }
    assert_eq!(actual, roots.iter().cloned().collect());
    let path = module_path(module, "olean")
        .to_string_lossy()
        .replace('\\', "/");
    let file = report["artifact_files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == path)
        .expect("actual audited output object");
    assert_eq!(file["bytes"].as_u64().unwrap(), before[&path].0);
    assert_eq!(file["blake3"].as_str().unwrap(), before[&path].1);
    assert_eq!(
        before,
        artifact_hashes(&fixture.root),
        "strict audit is read-only"
    );
    report
}

fn source_gate(fixture: &Fixture, sources: &BTreeMap<String, FixedSource>) -> bool {
    sources.iter().all(|(module, source)| {
        let path = fixture.root.join(module_path(module, "lean"));
        let ty = fs::symlink_metadata(&path).unwrap().file_type();
        ty.is_file() && fs::read(path).unwrap() == source.text.as_bytes()
    })
}

fn artifact_refusals(
    spec: &PacketSpec,
    original: &Fixture,
    projected: &Projection,
    roots: &[String],
) {
    for (label, corrupt) in [("MissingOriginal", false), ("CorruptOriginal", true)] {
        let changed = original.clone_artifacts(label);
        let path = changed.root.join(module_path("Lean4Lean.Level", "olean"));
        if corrupt {
            fs::write(path, b"not a Lean object\n").unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        let (out, attempted, _, _) = changed.snapshot_attempt(spec.module, spec.projected);
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains(SNAPSHOT_MARKER));
        let message = out.diagnostic();
        assert!(
            message.contains("kernel snapshot failed:") && message.contains("Level"),
            "{message}"
        );
        assert!(!message.contains("timed out") && !message.contains("output limit"));
        assert!(
            !attempted
                .root
                .join(module_path(spec.projected, "olean"))
                .exists()
        );
    }
    for (label, corrupt) in [("MissingProjection", false), ("CorruptProjection", true)] {
        let changed = projected.fixture.clone_artifacts(label);
        let path = changed.root.join(module_path(spec.projected, "olean"));
        if corrupt {
            fs::write(path, b"not a Lean object\n").unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        assert_audit_error(changed.audit(spec.projected, &roots[..1]), spec.projected);
    }
}

fn alias_axiom_control(base: &Fixture) {
    let original = base.sibling("AliasPolicy", true);
    let module = "ResourceSnapshotAliasAxiom";
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/resource-model42/fixtures");
    let modules = [
        "ResourceSnapshotAliasAxiomB",
        "ResourceSnapshotAliasAxiomA",
        module,
    ];
    for name in modules {
        original
            .compile(
                name,
                &read_source(&directory.join(format!("{name}.lean"))).text,
            )
            .success();
    }
    let projected = original.project(
        module,
        "FoundationsAliasAxiomKernel",
        &modules.into_iter().map(str::to_owned).collect(),
    );
    let canonical = "ResourceSnapshotAliasAxiom.shared".to_owned();
    assert_eq!(projected.report["proof_alias_count"], 1);
    let clean = positive_audit(
        &projected.fixture,
        &projected.module,
        std::slice::from_ref(&canonical),
    );
    assert_eq!(clean["theorems"][0]["axioms"], json!([]));
    let roots = all_projection_roots(&projected, std::slice::from_ref(&canonical));
    let out = projected.fixture.audit(&projected.module, &roots);
    assert!(!out.status.success());
    let denied = out.json();
    assert_eq!(denied["status"], "audited");
    assert_eq!(denied["accepted"], false);
    assert_eq!(denied["kernel_replayed"], true);
    assert_eq!(denied["cache_eligible"], false);
    assert_eq!(
        denied["checked_constants"],
        projected.report["output_constants"]
    );
    assert_eq!(
        denied["forbidden_axioms"],
        json!(["ResourceSnapshotAliasAxiom.forbidden"])
    );
    let actual = denied["theorems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, roots.into_iter().collect());
}

pub fn run(packet: Packet) {
    let spec = packet.spec();
    let started = Instant::now();
    let sources = fixed_sources(&spec);
    let provenance = provenance(&sources);
    let order = compile_order(&spec, &sources);
    let manual = theorem_catalog(&spec, &sources);
    let fixture = Fixture::new(started);
    for module in &order {
        fixture.compile(module, &sources[module].text).success();
    }
    let expected_files = sources
        .keys()
        .flat_map(|module| {
            ["lean", "olean"].map(|suffix| {
                module_path(module, suffix)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        artifact_hashes(&fixture.root)
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected_files
    );
    assert!(source_gate(&fixture, &sources));
    let projected = fixture.project(
        spec.module,
        spec.projected,
        &sources.keys().cloned().collect(),
    );
    let roots = all_projection_roots(&projected, &manual);
    let mut audited = BTreeSet::new();
    let mut batches = Vec::new();
    for chunk in roots.chunks(128) {
        let report = positive_audit(&projected.fixture, spec.projected, chunk);
        assert_eq!(
            report["checked_constants"],
            projected.report["output_constants"]
        );
        for theorem in report["theorems"].as_array().unwrap() {
            assert!(audited.insert(theorem["name"].as_str().unwrap().to_owned()));
        }
        batches.push(report);
    }
    assert_eq!(audited, roots.iter().cloned().collect());
    assert!(source_gate(&fixture, &sources));
    assert_eq!(artifact_hashes(&fixture.root), projected.original_hashes);
    assert_eq!(
        artifact_hashes(&projected.fixture.root),
        projected.projected_hashes
    );
    let evidence = json!({"schema_version":1,"scope":spec.scope,"whole_checker_verified":false,"provenance":provenance,"source_modules":order,"authored_theorems":manual,"audited_roots":roots,"snapshot_tool_blake3":projected.tool_blake3,"original_files":projected.original_hashes,"projected_files":projected.projected_hashes,"snapshot":projected.report,"audits":batches});
    let encoded = serde_json::to_string(&evidence).unwrap();
    assert!(encoded.len() <= CAPTURE);
    println!("{} {encoded}", spec.marker);

    // These are changed real files, not caller-supplied acceptance flags.
    let source = fixture.root.join(module_path("Lean4Lean.Level", "lean"));
    let before = fs::read(&source).unwrap();
    let mut changed = before.clone();
    changed.extend_from_slice(b"\n");
    fs::write(&source, &changed).unwrap();
    assert!(!source_gate(&fixture, &sources));
    let provenance_source: Value = serde_json::from_slice(
        &fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tools/lean4lean-foundations42/source-provenance.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let level = provenance_source["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["module"] == "Lean4Lean.Level")
        .unwrap();
    assert!(!matches_record(&fs::read(&source).unwrap(), level));
    fs::write(&source, before).unwrap();
    assert!(source_gate(&fixture, &sources));
    artifact_refusals(&spec, &fixture, &projected, &roots);
    alias_axiom_control(&fixture);
    assert_eq!(artifact_hashes(&fixture.root), projected.original_hashes);
    assert_eq!(
        artifact_hashes(&projected.fixture.root),
        projected.projected_hashes
    );
    eprintln!(
        "{} complete acceptance {:.2}s",
        spec.label,
        started.elapsed().as_secs_f64()
    );
}
