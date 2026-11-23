//! Outline cold and asymmetric early-exit regions into standalone helper functions.
//!
//! When a function has a cold trap/panic branch or an asymmetric slow tail that
//! stays cold relative to a small register-resident fast path (or exits a loop
//! on an error path), keeping those cold calls, stack slots, and extra live
//! values inside the parent CFG forces Cranelift's register allocator to emit
//! callee-saved spills and stack frame setup in the parent's entry prologue.
//! Splitting the cold region out into an anonymous helper leaves the hot path
//! lean and spill-free.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use cranelift_codegen::dominator_tree::DominatorTree;
use cranelift_codegen::flowgraph::ControlFlowGraph;
use cranelift_codegen::ir::instructions::InstructionData;
use cranelift_codegen::ir::{
    self, AbiParam, BlockArg, BlockCall, ExtFuncData, ExternalName, GlobalValueData, JumpTableData,
    MemFlags, Opcode, StackSlot, TrapCode, Type, ValueDef, ValueList,
};
use cranelift_codegen::loop_analysis::LoopAnalysis;
use cranelift_module::{FuncId, Module};

const MAX_REGIONS_PER_FN: usize = 16;
const MAX_SHARED_TERMINALS: usize = 4;
const MAX_SHARED_TERM_INSTS: usize = 12;
const MAX_LIVE_INS: usize = 16;
const FAST_RETURN_MAX_NON_CONST: usize = 28;
const FAST_RETURN_MAX_TOTAL: usize = 48;
const EXPLICIT_COLD_MIN_INSTS: usize = 12;
const ASYM_MIN_COLD_INSTS: usize = 16;
const LOOP_EXIT_MIN_INSTS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutlineKind {
    ExplicitCold,
    AsymmetricTail,
    LoopEarlyExit,
}

struct CandidateRegion {
    root: ir::Block,
    owned: BTreeSet<ir::Block>,
    shared_tails: BTreeSet<ir::Block>,
    has_return: bool,
    first_trap: Option<TrapCode>,
}

/// Outline cold regions of `func` into anonymous helpers declared on `m`.
///
/// Returns `(cold_func_id, cold_key, cold_ir_function)` for each outlined
/// helper. Callers should observe (if needed) and define each returned helper
/// on `m`.
pub(crate) fn outline<M: Module>(
    m: &mut M,
    parent_key: &str,
    func: &mut ir::Function,
) -> Result<Vec<(FuncId, String, ir::Function)>, String> {
    if func.layout.blocks().count() < 3
        || !func.dynamic_stack_slots.is_empty()
        || !func.dfg.exception_tables.is_empty()
        || !func
            .signature
            .params
            .iter()
            .all(|p| matches!(p.purpose, ir::ArgumentPurpose::Normal))
        || !func
            .signature
            .returns
            .iter()
            .all(|r| matches!(r.purpose, ir::ArgumentPurpose::Normal))
    {
        return Ok(Vec::new());
    }

    let mut outlined = Vec::new();
    let mut stub_blocks: BTreeSet<ir::Block> = BTreeSet::new();

    for idx in 0..MAX_REGIONS_PER_FN {
        let Some(candidate) = find_candidate(func, &stub_blocks) else {
            break;
        };
        let cold_key = format!("{parent_key}::cold#{idx}");
        let root = candidate.root;
        let Some((cold_id, cold_func)) = extract_region(m, func, &candidate)? else {
            stub_blocks.insert(root);
            continue;
        };
        let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
        if let Err(errors) = cranelift_codegen::verify_function(&cold_func, &flags) {
            return Err(format!(
                "cold outline '{cold_key}' verifier errors:\n{errors}\n{}",
                cold_func.display()
            ));
        }
        stub_blocks.insert(root);
        outlined.push((cold_id, cold_key, cold_func));
    }

    Ok(outlined)
}

fn find_candidate(
    func: &ir::Function,
    stub_blocks: &BTreeSet<ir::Block>,
) -> Option<CandidateRegion> {
    let entry = func.layout.entry_block()?;
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut loops = LoopAnalysis::new();
    loops.compute(func, &cfg, &domtree);

    let reachable = bfs_reachable(entry, |b| block_succs(func, b));

    let blocks: Vec<ir::Block> = func.layout.blocks().collect();
    for &root in &blocks {
        if root == entry
            || stub_blocks.contains(&root)
            || !reachable.contains(&root)
            || loops.innermost_loop(root).is_some()
        {
            continue;
        }
        let Some(kind) = classify_root(func, entry, root, &domtree, &loops, &reachable) else {
            continue;
        };
        let Some(region) = build_closed_region(func, root, &domtree, &loops) else {
            continue;
        };
        if region_worth_outlining(func, &region, kind, &loops) {
            return Some(region);
        }
    }
    None
}

fn classify_root(
    func: &ir::Function,
    entry: ir::Block,
    root: ir::Block,
    domtree: &DominatorTree,
    loops: &LoopAnalysis,
    reachable: &BTreeSet<ir::Block>,
) -> Option<OutlineKind> {
    if func.layout.is_cold(root) {
        return Some(OutlineKind::ExplicitCold);
    }
    // Single reachable predecessor `pred` with a `brif` to `(fast_blk, root)`.
    let mut preds = Vec::new();
    for b in func.layout.blocks() {
        if !reachable.contains(&b) {
            continue;
        }
        if block_succs(func, b).contains(&root) {
            preds.push(b);
        }
    }
    if preds.len() != 1 {
        return None;
    }
    let pred = preds[0];
    if !domtree.dominates(pred, root, &func.layout) {
        return None;
    }
    let last = func.layout.last_inst(pred)?;
    let InstructionData::Brif { blocks, .. } = &func.dfg.insts[last] else {
        return None;
    };
    let b0 = blocks[0].block(&func.dfg.value_lists);
    let b1 = blocks[1].block(&func.dfg.value_lists);
    let fast_blk = if b0 == root && b1 != root {
        b1
    } else if b1 == root && b0 != root {
        b0
    } else {
        return None;
    };
    if fast_blk == entry || func.layout.is_cold(fast_blk) {
        return None;
    }
    if let Some(lp) = loops.innermost_loop(pred)
        && loops.loop_header(lp) != pred
        && loops.innermost_loop(fast_blk) == Some(lp)
    {
        return Some(OutlineKind::LoopEarlyExit);
    }
    if loops.innermost_loop(fast_blk).is_none()
        && is_fast_return_arm(func, fast_blk, loops).is_some()
    {
        return Some(OutlineKind::AsymmetricTail);
    }
    None
}

/// Checks if `start` reaches `Return`/`Trap` cleanly within a small 0-call
/// acyclic snippet; returns its non-constant instruction count when true.
fn is_fast_return_arm(
    func: &ir::Function,
    start: ir::Block,
    loops: &LoopAnalysis,
) -> Option<usize> {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([start]);
    let mut non_const = 0usize;
    let mut total = 0usize;

    while let Some(b) = queue.pop_front() {
        if !seen.insert(b) || seen.len() > 4 || loops.innermost_loop(b).is_some() {
            return None;
        }
        for inst in func.layout.block_insts(b) {
            let data = &func.dfg.insts[inst];
            if data.opcode().is_call()
                || matches!(
                    data.opcode(),
                    Opcode::Udiv | Opcode::Sdiv | Opcode::Urem | Opcode::Srem
                )
            {
                return None;
            }
            if !is_terminator_or_nop(data.opcode()) {
                total += 1;
                if !is_const_op(data.opcode()) {
                    non_const += 1;
                }
                if total > FAST_RETURN_MAX_TOTAL || non_const > FAST_RETURN_MAX_NON_CONST {
                    return None;
                }
            }
            if data.opcode().is_branch() {
                for dest in
                    data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                {
                    let target = dest.block(&func.dfg.value_lists);
                    if target == b {
                        return None;
                    }
                    queue.push_back(target);
                }
            } else if data.opcode().is_terminator()
                && !matches!(data.opcode(), Opcode::Return | Opcode::Trap)
            {
                return None;
            }
        }
    }
    (non_const >= 2).then_some(non_const)
}

fn build_closed_region(
    func: &ir::Function,
    root: ir::Block,
    domtree: &DominatorTree,
    loops: &LoopAnalysis,
) -> Option<CandidateRegion> {
    let reach = bfs_reachable(root, |b| block_succs(func, b));
    for &b in &reach {
        // Reject back-edges or cycles involving `root`.
        if b != root && block_succs(func, b).contains(&root) {
            return None;
        }
        // Loops are fine only when they live entirely below `root`: every
        // enclosing loop header must be dominated by `root`.
        let mut lp = loops.innermost_loop(b);
        while let Some(l) = lp {
            if !domtree.dominates(root, loops.loop_header(l), &func.layout) {
                return None;
            }
            lp = loops.loop_parent(l);
        }
    }

    // Owned blocks are the ones `root` dominates; everything else reachable
    // from `root` is a shared tail that gets cloned into the helper.
    let owned: BTreeSet<ir::Block> = reach
        .iter()
        .copied()
        .filter(|&b| domtree.dominates(root, b, &func.layout))
        .collect();
    let shared_tails: BTreeSet<ir::Block> = reach.difference(&owned).copied().collect();
    if shared_tails.len() > MAX_SHARED_TERMINALS {
        return None;
    }
    // Owned blocks are deleted from the parent: no block outside the region
    // (reachable or not) may branch into one of them except into `root`.
    for b in func.layout.blocks() {
        if owned.contains(&b) {
            continue;
        }
        if block_succs(func, b)
            .iter()
            .any(|s| *s != root && owned.contains(s))
        {
            return None;
        }
    }
    // Shared tails stay in the parent too, so they must only flow to other
    // shared tails or exit.
    for &t in &shared_tails {
        if loops.innermost_loop(t).is_some()
            || block_succs(func, t)
                .iter()
                .any(|s| !shared_tails.contains(s))
            || func.layout.block_insts(t).any(|inst| {
                matches!(
                    func.dfg.insts[inst].opcode(),
                    Opcode::Udiv | Opcode::Sdiv | Opcode::Urem | Opcode::Srem
                )
            })
        {
            return None;
        }
    }

    let mut has_return = false;
    let mut first_trap = None;
    let mut all_slots = BTreeSet::new();

    for &b in &owned {
        if !check_block_supported(
            func,
            b,
            false,
            &mut has_return,
            &mut first_trap,
            &mut all_slots,
        ) {
            return None;
        }
    }
    for &b in &shared_tails {
        let mut slot_in_tail = BTreeSet::new();
        if !check_block_supported(
            func,
            b,
            true,
            &mut has_return,
            &mut first_trap,
            &mut slot_in_tail,
        ) {
            return None;
        }
    }

    Some(CandidateRegion {
        root,
        owned,
        shared_tails,
        has_return,
        first_trap,
    })
}

fn check_block_supported(
    func: &ir::Function,
    block: ir::Block,
    is_shared_tail: bool,
    has_return: &mut bool,
    first_trap: &mut Option<TrapCode>,
    slots_seen: &mut BTreeSet<StackSlot>,
) -> bool {
    let mut inst_count = 0usize;
    let Some(last) = func.layout.last_inst(block) else {
        return false;
    };
    for inst in func.layout.block_insts(block) {
        let data = &func.dfg.insts[inst];
        let op = data.opcode();
        if matches!(
            op,
            Opcode::GetFramePointer
                | Opcode::GetStackPointer
                | Opcode::GetReturnAddress
                | Opcode::ReturnCall
                | Opcode::ReturnCallIndirect
                | Opcode::TryCall
                | Opcode::TryCallIndirect
                | Opcode::DynamicStackLoad
                | Opcode::DynamicStackStore
        ) {
            return false;
        }
        if let InstructionData::StackLoad { stack_slot, .. }
        | InstructionData::StackStore { stack_slot, .. } = *data
        {
            slots_seen.insert(stack_slot);
        }
        if is_shared_tail {
            if op.is_call() {
                return false;
            }
            if !is_terminator_or_nop(op) {
                inst_count += 1;
                if inst_count > MAX_SHARED_TERM_INSTS {
                    return false;
                }
            }
        }
    }
    let term = &func.dfg.insts[last];
    match term {
        InstructionData::MultiAry {
            opcode: Opcode::Return,
            ..
        } => {
            *has_return = true;
            true
        }
        InstructionData::Trap {
            opcode: Opcode::Trap,
            code,
        } => {
            if first_trap.is_none() {
                *first_trap = Some(*code);
            }
            true
        }
        InstructionData::Jump { .. } => true,
        InstructionData::Brif { .. } | InstructionData::BranchTable { .. } if !is_shared_tail => {
            true
        }
        _ => false,
    }
}

fn region_worth_outlining(
    func: &ir::Function,
    region: &CandidateRegion,
    kind: OutlineKind,
    loops: &LoopAnalysis,
) -> bool {
    let in_region = |b| region.owned.contains(&b) || region.shared_tails.contains(&b);
    let mut outside_slots = BTreeSet::new();
    let mut region_slots = BTreeSet::new();
    let mut calls = 0usize;
    let mut non_term_insts = 0usize;
    let mut has_loop = false;

    for b in func.layout.blocks() {
        let inside = in_region(b);
        if inside && loops.innermost_loop(b).is_some() {
            has_loop = true;
        }
        for inst in func.layout.block_insts(b) {
            let data = &func.dfg.insts[inst];
            if let InstructionData::StackLoad { stack_slot, .. }
            | InstructionData::StackStore { stack_slot, .. } = *data
            {
                if inside {
                    region_slots.insert(stack_slot);
                }
                if !region.owned.contains(&b) {
                    outside_slots.insert(stack_slot);
                }
            }
            if inside {
                if data.opcode().is_call() {
                    calls += 1;
                }
                if !is_terminator_or_nop(data.opcode()) && !is_const_op(data.opcode()) {
                    non_term_insts += 1;
                }
            }
        }
    }

    let exclusive_slots = region_slots.difference(&outside_slots).count();
    if has_loop || calls >= 2 || (exclusive_slots > 0 && (calls > 0 || non_term_insts >= 8)) {
        return true;
    }
    match kind {
        OutlineKind::ExplicitCold => calls > 0 || non_term_insts >= EXPLICIT_COLD_MIN_INSTS,
        OutlineKind::AsymmetricTail => {
            (calls > 0 || !region.has_return) && non_term_insts >= ASYM_MIN_COLD_INSTS
        }
        OutlineKind::LoopEarlyExit => calls > 0 || non_term_insts >= LOOP_EXIT_MIN_INSTS,
    }
}

fn extract_region<M: Module>(
    m: &mut M,
    func: &mut ir::Function,
    region: &CandidateRegion,
) -> Result<Option<(FuncId, ir::Function)>, String> {
    let root = region.root;
    let region_blocks: BTreeSet<ir::Block> =
        region.owned.union(&region.shared_tails).copied().collect();

    // Classify stack slots into exclusive (moved into `cold_func`) and shared
    // (passed as `base_ptr` arguments from parent).
    let mut outside_slots = BTreeSet::new();
    let mut region_slots = BTreeSet::new();
    for b in func.layout.blocks() {
        let inside = region_blocks.contains(&b);
        for inst in func.layout.block_insts(b) {
            if let InstructionData::StackLoad { stack_slot, .. }
            | InstructionData::StackStore { stack_slot, .. } = func.dfg.insts[inst]
            {
                if inside {
                    region_slots.insert(stack_slot);
                }
                if !region.owned.contains(&b) {
                    outside_slots.insert(stack_slot);
                }
            }
        }
    }
    let shared_slots: Vec<StackSlot> = region_slots.intersection(&outside_slots).copied().collect();
    let exclusive_slots: Vec<StackSlot> =
        region_slots.difference(&outside_slots).copied().collect();

    // Gather live-ins and constants to rematerialize in `cold_func`.
    let mut live_ins: Vec<ir::Value> = Vec::new();
    let mut live_in_seen: BTreeSet<ir::Value> = BTreeSet::new();
    let mut remat_insts: Vec<(ir::Value, ir::Inst)> = Vec::new();
    let mut remat_seen: BTreeSet<ir::Value> = BTreeSet::new();

    for &b in &region_blocks {
        for inst in func.layout.block_insts(b) {
            let args = collect_inst_value_uses(func, inst);
            for raw_v in args {
                let v = func.dfg.resolve_aliases(raw_v);
                if is_defined_inside_region(func, v, root, &region_blocks) {
                    continue;
                }
                if let ValueDef::Result(def_inst, 0) = func.dfg.value_def(v)
                    && can_rematerialize(func, def_inst)
                {
                    if remat_seen.insert(v) {
                        remat_insts.push((v, def_inst));
                    }
                    continue;
                }
                let ty = func.dfg.value_type(v);
                if !is_portable_scalar_param(ty) {
                    return Ok(None);
                }
                if live_in_seen.insert(v) {
                    live_ins.push(v);
                }
            }
        }
    }

    if live_ins.len() + shared_slots.len() > MAX_LIVE_INS {
        return Ok(None);
    }

    let ptr_ty = m.target_config().pointer_type();
    let mut cold_sig = m.make_signature();
    cold_sig.call_conv = func.signature.call_conv;
    for &v in &live_ins {
        cold_sig.params.push(AbiParam::new(func.dfg.value_type(v)));
    }
    for _ in &shared_slots {
        cold_sig.params.push(AbiParam::new(ptr_ty));
    }
    if region.has_return {
        cold_sig.returns = func.signature.returns.clone();
    }

    let cold_id = m
        .declare_anonymous_function(&cold_sig)
        .map_err(|e| format!("cold outline declare: {e}"))?;
    let mut cold_func = ir::Function::with_name_signature(
        ir::UserFuncName::user(0, cold_id.as_u32()),
        cold_sig.clone(),
    );

    let cold_entry = cold_func.dfg.make_block();
    cold_func.layout.append_block(cold_entry);
    for p in &cold_sig.params {
        cold_func.dfg.append_block_param(cold_entry, p.value_type);
    }

    let mut val_map: BTreeMap<ir::Value, ir::Value> = BTreeMap::new();
    let entry_params = cold_func.dfg.block_params(cold_entry).to_vec();
    for (i, &v) in live_ins.iter().enumerate() {
        val_map.insert(v, entry_params[i]);
    }
    let mut shared_slot_bases: BTreeMap<StackSlot, ir::Value> = BTreeMap::new();
    for (i, &ss) in shared_slots.iter().enumerate() {
        shared_slot_bases.insert(ss, entry_params[live_ins.len() + i]);
    }

    let mut block_map: BTreeMap<ir::Block, ir::Block> = BTreeMap::new();
    block_map.insert(root, cold_entry);
    // Visit `region_blocks \ {root}` in reverse postorder from `root` so
    // dominating definitions are always cloned before their uses.
    let rpo_blocks = rpo_order(func, root, &region_blocks);
    let ordered_blocks: Vec<ir::Block> = rpo_blocks.into_iter().filter(|b| *b != root).collect();
    for &b in &ordered_blocks {
        let new_b = cold_func.dfg.make_block();
        cold_func.layout.append_block(new_b);
        for &p in func.dfg.block_params(b) {
            let new_p = cold_func
                .dfg
                .append_block_param(new_b, func.dfg.value_type(p));
            val_map.insert(func.dfg.resolve_aliases(p), new_p);
        }
        if func.layout.is_cold(b) {
            cold_func.layout.set_cold(new_b);
        }
        block_map.insert(b, new_b);
    }

    let mut cloner = Cloner::new(func, &mut cold_func, exclusive_slots, &shared_slot_bases);

    // Rematerialize constants at top of `cold_entry`.
    for (old_v, def_inst) in remat_insts {
        cloner.clone_inst(def_inst, cold_entry, &mut val_map, &block_map)?;
        debug_assert!(val_map.contains_key(&old_v));
    }

    // Clone instructions from `root` and every other block in the region.
    for old_b in std::iter::once(root).chain(ordered_blocks) {
        let new_b = block_map[&old_b];
        for inst in func.layout.block_insts(old_b) {
            cloner.clone_inst(inst, new_b, &mut val_map, &block_map)?;
        }
    }
    let moved_slots = cloner.moved_slots;

    // Rewrite `root` in `parent` to be a lean cold stub calling `cold_id`.
    let root_old_insts: Vec<ir::Inst> = func.layout.block_insts(root).collect();
    let stub_loc = root_old_insts
        .first()
        .map(|&inst| func.srcloc(inst))
        .unwrap_or_default();
    for inst in root_old_insts {
        func.layout.remove_inst(inst);
    }

    let mut call_args = Vec::with_capacity(live_ins.len() + shared_slots.len());
    call_args.extend(live_ins);
    for &ss in &shared_slots {
        let base_data = InstructionData::StackLoad {
            opcode: Opcode::StackAddr,
            stack_slot: ss,
            offset: 0.into(),
        };
        let base_inst = func.dfg.make_inst(base_data);
        func.dfg.make_inst_results(base_inst, ptr_ty);
        func.layout.append_inst(base_inst, root);
        func.set_srcloc(base_inst, stub_loc);
        call_args.push(func.dfg.first_result(base_inst));
    }

    let cold_fref = m.declare_func_in_func(cold_id, func);
    let mut call_arg_list = ValueList::default();
    call_arg_list.extend(call_args, &mut func.dfg.value_lists);
    let call_inst = func.dfg.make_inst(InstructionData::Call {
        opcode: Opcode::Call,
        args: call_arg_list,
        func_ref: cold_fref,
    });
    func.dfg.make_inst_results(call_inst, ir::types::INVALID);
    func.layout.append_inst(call_inst, root);
    func.set_srcloc(call_inst, stub_loc);

    if region.has_return {
        let results: Vec<ir::Value> = func.dfg.inst_results(call_inst).to_vec();
        let mut ret_list = ValueList::default();
        ret_list.extend(results, &mut func.dfg.value_lists);
        let ret_inst = func.dfg.make_inst(InstructionData::MultiAry {
            opcode: Opcode::Return,
            args: ret_list,
        });
        func.dfg.make_inst_results(ret_inst, ir::types::INVALID);
        func.layout.append_inst(ret_inst, root);
        func.set_srcloc(ret_inst, stub_loc);
    } else {
        let code = region
            .first_trap
            .unwrap_or_else(|| TrapCode::unwrap_user(1));
        let trap_inst = func.dfg.make_inst(InstructionData::Trap {
            opcode: Opcode::Trap,
            code,
        });
        func.dfg.make_inst_results(trap_inst, ir::types::INVALID);
        func.layout.append_inst(trap_inst, root);
        func.set_srcloc(trap_inst, stub_loc);
    }
    func.layout.set_cold(root);

    // Zero exclusive stack slots in parent and remove exclusively owned blocks.
    for &ss in &moved_slots {
        func.sized_stack_slots[ss].size = 0;
        func.sized_stack_slots[ss].align_shift = 0;
    }
    for &b in &region.owned {
        if b == root {
            continue;
        }
        let insts: Vec<ir::Inst> = func.layout.block_insts(b).collect();
        for inst in insts {
            func.layout.remove_inst(inst);
        }
        func.layout.remove_block(b);
    }

    Ok(Some((cold_id, cold_func)))
}

struct Cloner<'a> {
    src: &'a ir::Function,
    dst: &'a mut ir::Function,
    slot_map: BTreeMap<StackSlot, StackSlot>,
    shared_bases: &'a BTreeMap<StackSlot, ir::Value>,
    moved_slots: Vec<StackSlot>,
    user_ref_map: BTreeMap<ir::UserExternalNameRef, ir::UserExternalNameRef>,
    sig_map: BTreeMap<ir::SigRef, ir::SigRef>,
    func_map: BTreeMap<ir::FuncRef, ir::FuncRef>,
    gv_map: BTreeMap<ir::GlobalValue, ir::GlobalValue>,
    const_map: BTreeMap<ir::Constant, ir::Constant>,
    imm_map: BTreeMap<ir::Immediate, ir::Immediate>,
    jt_map: BTreeMap<ir::JumpTable, ir::JumpTable>,
}

impl<'a> Cloner<'a> {
    fn new(
        src: &'a ir::Function,
        dst: &'a mut ir::Function,
        exclusive_slots: Vec<StackSlot>,
        shared_bases: &'a BTreeMap<StackSlot, ir::Value>,
    ) -> Self {
        let mut slot_map = BTreeMap::new();
        for &ss in &exclusive_slots {
            let data = src.sized_stack_slots[ss].clone();
            let new_ss = dst.create_sized_stack_slot(data);
            slot_map.insert(ss, new_ss);
        }
        Self {
            src,
            dst,
            slot_map,
            shared_bases,
            moved_slots: exclusive_slots,
            user_ref_map: BTreeMap::new(),
            sig_map: BTreeMap::new(),
            func_map: BTreeMap::new(),
            gv_map: BTreeMap::new(),
            const_map: BTreeMap::new(),
            imm_map: BTreeMap::new(),
            jt_map: BTreeMap::new(),
        }
    }

    fn remap_val(&self, val_map: &BTreeMap<ir::Value, ir::Value>, v: ir::Value) -> ir::Value {
        let canon = self.src.dfg.resolve_aliases(v);
        val_map[&canon]
    }

    fn remap_user_ref(&mut self, r: ir::UserExternalNameRef) -> ir::UserExternalNameRef {
        if let Some(&mapped) = self.user_ref_map.get(&r) {
            return mapped;
        }
        let name = self.src.params.user_named_funcs()[r].clone();
        let mapped = self.dst.declare_imported_user_function(name);
        self.user_ref_map.insert(r, mapped);
        mapped
    }

    fn remap_ext_name(&mut self, name: &ExternalName) -> ExternalName {
        match name {
            ExternalName::User(u) => ExternalName::User(self.remap_user_ref(*u)),
            other => other.clone(),
        }
    }

    fn remap_sig(&mut self, s: ir::SigRef) -> ir::SigRef {
        if let Some(&mapped) = self.sig_map.get(&s) {
            return mapped;
        }
        let sig = self.src.dfg.signatures[s].clone();
        let mapped = self.dst.import_signature(sig);
        self.sig_map.insert(s, mapped);
        mapped
    }

    fn remap_func(&mut self, f: ir::FuncRef) -> ir::FuncRef {
        if let Some(&mapped) = self.func_map.get(&f) {
            return mapped;
        }
        let ext = &self.src.dfg.ext_funcs[f];
        let name = self.remap_ext_name(&ext.name);
        let signature = self.remap_sig(ext.signature);
        let mapped = self.dst.import_function(ExtFuncData {
            name,
            signature,
            colocated: ext.colocated,
        });
        self.func_map.insert(f, mapped);
        mapped
    }

    fn remap_gv(&mut self, gv: ir::GlobalValue) -> ir::GlobalValue {
        if let Some(&mapped) = self.gv_map.get(&gv) {
            return mapped;
        }
        let data = match self.src.global_values[gv].clone() {
            GlobalValueData::VMContext => GlobalValueData::VMContext,
            GlobalValueData::Load {
                base,
                offset,
                global_type,
                flags,
            } => GlobalValueData::Load {
                base: self.remap_gv(base),
                offset,
                global_type,
                flags,
            },
            GlobalValueData::IAddImm {
                base,
                offset,
                global_type,
            } => GlobalValueData::IAddImm {
                base: self.remap_gv(base),
                offset,
                global_type,
            },
            GlobalValueData::Symbol {
                name,
                offset,
                colocated,
                tls,
            } => GlobalValueData::Symbol {
                name: self.remap_ext_name(&name),
                offset,
                colocated,
                tls,
            },
            other => other,
        };
        let mapped = self.dst.create_global_value(data);
        self.gv_map.insert(gv, mapped);
        mapped
    }

    fn remap_imm(&mut self, imm: ir::Immediate) -> ir::Immediate {
        if let Some(&mapped) = self.imm_map.get(&imm) {
            return mapped;
        }
        let data = self.src.dfg.immediates[imm].clone();
        let mapped = self.dst.dfg.immediates.push(data);
        self.imm_map.insert(imm, mapped);
        mapped
    }

    fn remap_const(&mut self, c: ir::Constant) -> ir::Constant {
        if let Some(&mapped) = self.const_map.get(&c) {
            return mapped;
        }
        let data = self.src.dfg.constants.get(c).clone();
        let mapped = self.dst.dfg.constants.insert(data);
        self.const_map.insert(c, mapped);
        mapped
    }

    fn remap_block_call(
        &mut self,
        call: BlockCall,
        val_map: &BTreeMap<ir::Value, ir::Value>,
        block_map: &BTreeMap<ir::Block, ir::Block>,
    ) -> BlockCall {
        let block = block_map[&call.block(&self.src.dfg.value_lists)];
        let args: Vec<BlockArg> = call
            .args(&self.src.dfg.value_lists)
            .map(|a| match a {
                BlockArg::Value(v) => BlockArg::Value(self.remap_val(val_map, v)),
                BlockArg::TryCallRet(i) => BlockArg::TryCallRet(i),
                BlockArg::TryCallExn(i) => BlockArg::TryCallExn(i),
            })
            .collect();
        BlockCall::new(block, args, &mut self.dst.dfg.value_lists)
    }

    fn remap_jt(
        &mut self,
        jt: ir::JumpTable,
        val_map: &BTreeMap<ir::Value, ir::Value>,
        block_map: &BTreeMap<ir::Block, ir::Block>,
    ) -> ir::JumpTable {
        if let Some(&mapped) = self.jt_map.get(&jt) {
            return mapped;
        }
        let table = &self.src.dfg.jump_tables[jt];
        let default = self.remap_block_call(table.default_block(), val_map, block_map);
        let cases: Vec<BlockCall> = table
            .as_slice()
            .iter()
            .map(|&c| self.remap_block_call(c, val_map, block_map))
            .collect();
        let mapped = self
            .dst
            .create_jump_table(JumpTableData::new(default, &cases));
        self.jt_map.insert(jt, mapped);
        mapped
    }

    fn remap_list(
        &mut self,
        list: &ValueList,
        val_map: &BTreeMap<ir::Value, ir::Value>,
    ) -> ValueList {
        let vals: Vec<ir::Value> = list
            .as_slice(&self.src.dfg.value_lists)
            .iter()
            .map(|&v| self.remap_val(val_map, v))
            .collect();
        let mut out = ValueList::default();
        out.extend(vals, &mut self.dst.dfg.value_lists);
        out
    }

    fn clone_inst(
        &mut self,
        inst: ir::Inst,
        dst_block: ir::Block,
        val_map: &mut BTreeMap<ir::Value, ir::Value>,
        block_map: &BTreeMap<ir::Block, ir::Block>,
    ) -> Result<(), String> {
        let data = self.src.dfg.insts[inst];
        let ctrl = self.src.dfg.ctrl_typevar(inst);
        let old_results: Vec<ir::Value> = self.src.dfg.inst_results(inst).to_vec();

        // Shared stack slots were lowered to explicit base-pointer parameters.
        match data {
            InstructionData::StackLoad {
                opcode: Opcode::StackAddr,
                stack_slot,
                offset,
            } if self.shared_bases.contains_key(&stack_slot) => {
                let base = self.shared_bases[&stack_slot];
                let off = i64::from(offset);
                if off == 0 {
                    val_map.insert(old_results[0], base);
                    return Ok(());
                }
                let ptr_ty = self.src.dfg.value_type(old_results[0]);
                let new_data = InstructionData::BinaryImm64 {
                    opcode: Opcode::IaddImm,
                    arg: base,
                    imm: off.into(),
                };
                return self.emit_mapped(inst, dst_block, new_data, ptr_ty, &old_results, val_map);
            }
            InstructionData::StackLoad {
                opcode: Opcode::StackLoad,
                stack_slot,
                offset,
            } if self.shared_bases.contains_key(&stack_slot) => {
                let base = self.shared_bases[&stack_slot];
                let ty = self.src.dfg.value_type(old_results[0]);
                let new_data = InstructionData::Load {
                    opcode: Opcode::Load,
                    flags: MemFlags::trusted(),
                    arg: base,
                    offset,
                };
                return self.emit_mapped(inst, dst_block, new_data, ty, &old_results, val_map);
            }
            InstructionData::StackStore {
                opcode: Opcode::StackStore,
                arg,
                stack_slot,
                offset,
            } if self.shared_bases.contains_key(&stack_slot) => {
                let base = self.shared_bases[&stack_slot];
                let val = self.remap_val(val_map, arg);
                let new_data = InstructionData::Store {
                    opcode: Opcode::Store,
                    flags: MemFlags::trusted(),
                    args: [val, base],
                    offset,
                };
                return self.emit_mapped(inst, dst_block, new_data, ctrl, &old_results, val_map);
            }
            _ => {}
        }

        let new_data = self.remap_inst_data(data, val_map, block_map)?;
        self.emit_mapped(inst, dst_block, new_data, ctrl, &old_results, val_map)
    }

    fn emit_mapped(
        &mut self,
        old_inst: ir::Inst,
        dst_block: ir::Block,
        new_data: InstructionData,
        ctrl: Type,
        old_results: &[ir::Value],
        val_map: &mut BTreeMap<ir::Value, ir::Value>,
    ) -> Result<(), String> {
        let new_inst = self.dst.dfg.make_inst(new_data);
        self.dst.dfg.make_inst_results(new_inst, ctrl);
        self.dst.layout.append_inst(new_inst, dst_block);
        self.dst.set_srcloc(new_inst, self.src.srcloc(old_inst));
        let new_results = self.dst.dfg.inst_results(new_inst);
        if new_results.len() != old_results.len() {
            return Err(format!(
                "cold outline: result count mismatch on {:?}",
                self.src.dfg.insts[old_inst].opcode()
            ));
        }
        for (&old_r, &new_r) in old_results.iter().zip(new_results) {
            val_map.insert(self.src.dfg.resolve_aliases(old_r), new_r);
        }
        Ok(())
    }

    fn remap_inst_data(
        &mut self,
        data: InstructionData,
        val_map: &BTreeMap<ir::Value, ir::Value>,
        block_map: &BTreeMap<ir::Block, ir::Block>,
    ) -> Result<InstructionData, String> {
        let r = |v| self.remap_val(val_map, v);
        Ok(match data {
            InstructionData::NullAry { opcode } => InstructionData::NullAry { opcode },
            InstructionData::Unary { opcode, arg } => InstructionData::Unary {
                opcode,
                arg: r(arg),
            },
            InstructionData::UnaryImm { opcode, imm } => InstructionData::UnaryImm { opcode, imm },
            InstructionData::UnaryIeee16 { opcode, imm } => {
                InstructionData::UnaryIeee16 { opcode, imm }
            }
            InstructionData::UnaryIeee32 { opcode, imm } => {
                InstructionData::UnaryIeee32 { opcode, imm }
            }
            InstructionData::UnaryIeee64 { opcode, imm } => {
                InstructionData::UnaryIeee64 { opcode, imm }
            }
            InstructionData::UnaryConst {
                opcode,
                constant_handle,
            } => InstructionData::UnaryConst {
                opcode,
                constant_handle: self.remap_const(constant_handle),
            },
            InstructionData::UnaryGlobalValue {
                opcode,
                global_value,
            } => InstructionData::UnaryGlobalValue {
                opcode,
                global_value: self.remap_gv(global_value),
            },
            InstructionData::Binary { opcode, args } => InstructionData::Binary {
                opcode,
                args: [r(args[0]), r(args[1])],
            },
            InstructionData::BinaryImm8 { opcode, arg, imm } => InstructionData::BinaryImm8 {
                opcode,
                arg: r(arg),
                imm,
            },
            InstructionData::BinaryImm64 { opcode, arg, imm } => InstructionData::BinaryImm64 {
                opcode,
                arg: r(arg),
                imm,
            },
            InstructionData::Ternary { opcode, args } => InstructionData::Ternary {
                opcode,
                args: [r(args[0]), r(args[1]), r(args[2])],
            },
            InstructionData::TernaryImm8 { opcode, args, imm } => InstructionData::TernaryImm8 {
                opcode,
                args: [r(args[0]), r(args[1])],
                imm,
            },
            InstructionData::Shuffle { opcode, args, imm } => InstructionData::Shuffle {
                opcode,
                args: [r(args[0]), r(args[1])],
                imm: self.remap_imm(imm),
            },
            InstructionData::IntCompare { opcode, cond, args } => InstructionData::IntCompare {
                opcode,
                cond,
                args: [r(args[0]), r(args[1])],
            },
            InstructionData::IntCompareImm {
                opcode,
                cond,
                arg,
                imm,
            } => InstructionData::IntCompareImm {
                opcode,
                cond,
                arg: r(arg),
                imm,
            },
            InstructionData::FloatCompare { opcode, cond, args } => InstructionData::FloatCompare {
                opcode,
                cond,
                args: [r(args[0]), r(args[1])],
            },
            InstructionData::Load {
                opcode,
                flags,
                arg,
                offset,
            } => InstructionData::Load {
                opcode,
                flags,
                arg: r(arg),
                offset,
            },
            InstructionData::LoadNoOffset { opcode, flags, arg } => InstructionData::LoadNoOffset {
                opcode,
                flags,
                arg: r(arg),
            },
            InstructionData::Store {
                opcode,
                flags,
                args,
                offset,
            } => InstructionData::Store {
                opcode,
                flags,
                args: [r(args[0]), r(args[1])],
                offset,
            },
            InstructionData::StoreNoOffset {
                opcode,
                flags,
                args,
            } => InstructionData::StoreNoOffset {
                opcode,
                flags,
                args: [r(args[0]), r(args[1])],
            },
            InstructionData::StackLoad {
                opcode,
                stack_slot,
                offset,
            } => InstructionData::StackLoad {
                opcode,
                stack_slot: self.slot_map[&stack_slot],
                offset,
            },
            InstructionData::StackStore {
                opcode,
                arg,
                stack_slot,
                offset,
            } => InstructionData::StackStore {
                opcode,
                arg: r(arg),
                stack_slot: self.slot_map[&stack_slot],
                offset,
            },
            InstructionData::AtomicCas {
                opcode,
                flags,
                args,
            } => InstructionData::AtomicCas {
                opcode,
                flags,
                args: [r(args[0]), r(args[1]), r(args[2])],
            },
            InstructionData::AtomicRmw {
                opcode,
                flags,
                op,
                args,
            } => InstructionData::AtomicRmw {
                opcode,
                flags,
                op,
                args: [r(args[0]), r(args[1])],
            },
            InstructionData::IntAddTrap { opcode, args, code } => InstructionData::IntAddTrap {
                opcode,
                args: [r(args[0]), r(args[1])],
                code,
            },
            InstructionData::CondTrap { opcode, arg, code } => InstructionData::CondTrap {
                opcode,
                arg: r(arg),
                code,
            },
            InstructionData::Trap { opcode, code } => InstructionData::Trap { opcode, code },
            InstructionData::FuncAddr { opcode, func_ref } => InstructionData::FuncAddr {
                opcode,
                func_ref: self.remap_func(func_ref),
            },
            InstructionData::Call {
                opcode,
                args,
                func_ref,
            } => {
                let func_ref = self.remap_func(func_ref);
                let args = self.remap_list(&args, val_map);
                InstructionData::Call {
                    opcode,
                    args,
                    func_ref,
                }
            }
            InstructionData::CallIndirect {
                opcode,
                args,
                sig_ref,
            } => {
                let sig_ref = self.remap_sig(sig_ref);
                let args = self.remap_list(&args, val_map);
                InstructionData::CallIndirect {
                    opcode,
                    args,
                    sig_ref,
                }
            }
            InstructionData::MultiAry { opcode, args } => {
                let args = self.remap_list(&args, val_map);
                InstructionData::MultiAry { opcode, args }
            }
            InstructionData::Jump {
                opcode,
                destination,
            } => InstructionData::Jump {
                opcode,
                destination: self.remap_block_call(destination, val_map, block_map),
            },
            InstructionData::Brif {
                opcode,
                arg,
                blocks,
            } => {
                let arg = r(arg);
                let b0 = self.remap_block_call(blocks[0], val_map, block_map);
                let b1 = self.remap_block_call(blocks[1], val_map, block_map);
                InstructionData::Brif {
                    opcode,
                    arg,
                    blocks: [b0, b1],
                }
            }
            InstructionData::BranchTable { opcode, arg, table } => {
                let arg = r(arg);
                let table = self.remap_jt(table, val_map, block_map);
                InstructionData::BranchTable { opcode, arg, table }
            }
            other => {
                return Err(format!(
                    "cold outline: unsupported instruction {:?}",
                    other.opcode()
                ));
            }
        })
    }
}

fn bfs_reachable<F>(start: ir::Block, mut succs: F) -> BTreeSet<ir::Block>
where
    F: FnMut(ir::Block) -> Vec<ir::Block>,
{
    let mut visited = BTreeSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(b) = queue.pop_front() {
        for s in succs(b) {
            if visited.insert(s) {
                queue.push_back(s);
            }
        }
    }
    visited
}

fn rpo_order(func: &ir::Function, root: ir::Block, region: &BTreeSet<ir::Block>) -> Vec<ir::Block> {
    fn visit(
        func: &ir::Function,
        b: ir::Block,
        region: &BTreeSet<ir::Block>,
        seen: &mut BTreeSet<ir::Block>,
        post: &mut Vec<ir::Block>,
    ) {
        if !seen.insert(b) {
            return;
        }
        for s in block_succs(func, b) {
            if region.contains(&s) {
                visit(func, s, region, seen, post);
            }
        }
        post.push(b);
    }
    let mut seen = BTreeSet::new();
    let mut post = Vec::new();
    visit(func, root, region, &mut seen, &mut post);
    post.reverse();
    post
}

fn block_succs(func: &ir::Function, block: ir::Block) -> Vec<ir::Block> {
    let Some(last) = func.layout.last_inst(block) else {
        return Vec::new();
    };
    let data = &func.dfg.insts[last];
    data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
        .iter()
        .map(|dest| dest.block(&func.dfg.value_lists))
        .collect()
}

fn collect_inst_value_uses(func: &ir::Function, inst: ir::Inst) -> Vec<ir::Value> {
    let data = &func.dfg.insts[inst];
    let mut out: Vec<ir::Value> = data.arguments(&func.dfg.value_lists).to_vec();
    for dest in data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables) {
        for arg in dest.args(&func.dfg.value_lists) {
            if let BlockArg::Value(v) = arg {
                out.push(v);
            }
        }
    }
    out
}

fn is_defined_inside_region(
    func: &ir::Function,
    v: ir::Value,
    root: ir::Block,
    region: &BTreeSet<ir::Block>,
) -> bool {
    match func.dfg.value_def(v) {
        ValueDef::Param(b, _) => b != root && region.contains(&b),
        ValueDef::Result(inst, _) => func
            .layout
            .inst_block(inst)
            .is_some_and(|b| region.contains(&b)),
        ValueDef::Union(_, _) => false,
    }
}

fn can_rematerialize(func: &ir::Function, inst: ir::Inst) -> bool {
    match func.dfg.insts[inst] {
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            ..
        }
        | InstructionData::UnaryIeee32 {
            opcode: Opcode::F32const,
            ..
        }
        | InstructionData::UnaryIeee64 {
            opcode: Opcode::F64const,
            ..
        }
        | InstructionData::UnaryConst { .. }
        | InstructionData::FuncAddr { .. } => true,
        InstructionData::UnaryGlobalValue { global_value, .. } => {
            global_is_pure_symbol(func, global_value)
        }
        _ => false,
    }
}

fn global_is_pure_symbol(func: &ir::Function, gv: ir::GlobalValue) -> bool {
    match &func.global_values[gv] {
        GlobalValueData::Symbol { .. } => true,
        GlobalValueData::IAddImm { base, .. } => global_is_pure_symbol(func, *base),
        _ => false,
    }
}

fn is_portable_scalar_param(ty: Type) -> bool {
    !ty.is_vector() && (ty.is_int() || ty.is_float()) && ty.bits() <= 64
}

fn is_const_op(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::Iconst
            | Opcode::F32const
            | Opcode::F64const
            | Opcode::F128const
            | Opcode::Vconst
            | Opcode::SymbolValue
            | Opcode::FuncAddr
    )
}

fn is_terminator_or_nop(op: Opcode) -> bool {
    op.is_terminator() || matches!(op, Opcode::Nop)
}
