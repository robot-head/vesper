//! CLIF clean-up for the optimized native path (#1426).
//!
//! Cranelift 0.125 keeps every stack slot in memory, lowers every load even
//! when its result is unused (a load may trap), and does not thread jumps.
//! The native lowering leans on all three: locals, `Option` temporaries and
//! split aggregates live in small explicit slots, pattern tests load fields
//! they never read, and `&&`, `||` and match tests produce a 0/1 value in a
//! join block that only branches on it. This pass, run on each function
//! just before Cranelift compiles it, does what Cranelift does not:
//!
//! 1. [`promote_slots`]: an explicit slot whose address is only used as the
//!    base of loads and stores at constant offsets, with one type per
//!    offset and no overlapping accesses, becomes SSA values (mem2reg).
//! 2. [`thread_branches`]: an edge into a block that only `brif`s on one of
//!    its parameters goes straight to the chosen successor when the
//!    argument is a constant, and a `jump` there becomes the `brif` itself.
//! 3. [`remove_dead`]: loads (and pure instructions) whose results are
//!    unused are deleted. Vesper code never relies on a load trapping.
//!
//! 4. [`share_invariant_loads`]: a read-only word that may move (a header
//!    word of a borrowed view, `invariant_word` in the native lowering) is
//!    read once where every read of it is dominated.
//!
//! 5. [`fold_constant_branches`] and [`sink_slot_inits`]: a `brif` on a
//!    constant becomes a `jump`, and a slot's constant initial stores move
//!    from the entry block to the block that dominates every other use, so
//!    a cold branch's locals cost the hot path nothing.
//!
//! 6. [`float_constants`]: `from_bits_f64(K)` and `from_bits_f32(K)`
//!    become float constants, which Cranelift reads from its constant pool
//!    as a memory operand instead of building them in a general register.
//!
//! Each step keeps the function valid CLIF on its own.
use cranelift_codegen::cursor::Cursor as _;
use cranelift_codegen::dominator_tree::DominatorTree;
use cranelift_codegen::flowgraph::ControlFlowGraph;
use cranelift_codegen::ir::{
    self, Block, BlockArg, BlockCall, Inst, InstBuilder, InstructionData, JumpTableData, Opcode,
    StackSlot, StackSlotKind, Type, Value, types,
};
use cranelift_codegen::loop_analysis::{Loop, LoopAnalysis};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Run every step on `func`.
pub fn run(func: &mut ir::Function) {
    if std::env::var_os("VESPER_CLIF_OPT").is_some_and(|v| v == "off") {
        return;
    }
    remove_unreachable(func);
    merge_blocks(func);
    remove_constant_params(func);
    float_constants(func);
    // Vector slots at one type per offset (#1500, src/native_vector.rs).
    crate::native::vector_registers(func);
    split_join_slots(func);
    promote_slots(func);
    remove_constant_params(func);
    fold_narrow_extends(func);
    fold_trivial_arith_and_bool_extends(func);
    sink_compares(func);
    thread_branches(func);
    thread_decided_branches(func);
    for _ in 0..3 {
        merge_blocks(func);
        fold_constant_branches(func);
        remove_unreachable(func);
    }
    eliminate_guarded_loop_add_overflows(func);
    for _ in 0..2 {
        merge_blocks(func);
        fold_constant_branches(func);
        remove_unreachable(func);
    }
    remove_dead(func);
    hoist_loop_invariants(func);
    share_invariant_loads(func);
    hoist_loop_invariants(func);
    reuse_seq_header_loads(func);
    if_convert_pure_diamonds(func);
    thread_constant_param_jumps(func);
    propagate_branch_conditions(func);
    remove_constant_params(func);
    fold_narrow_extends(func);
    fold_trivial_arith_and_bool_extends(func);
    fold_constant_branches(func);
    merge_blocks(func);
    canonicalize_imm_ops(func);
    dense_icmp_chains_to_br_table(func);
    remove_dead(func);
    sink_slot_inits(func);
    merge_tails(func);
    shrink_unused_slots(func);
}

/// Cranelift lays out every declared slot, used or not: a promoted slot
/// would still cost frame space, so slots nothing addresses get size 0.
pub fn shrink_unused_slots(func: &mut ir::Function) {
    let mut used: HashSet<StackSlot> = HashSet::new();
    for (_, inst) in insts(func) {
        if let InstructionData::StackLoad { stack_slot, .. }
        | InstructionData::StackStore { stack_slot, .. } = func.dfg.insts[inst]
        {
            used.insert(stack_slot);
        }
    }
    for (slot, data) in func.sized_stack_slots.iter_mut() {
        if !used.contains(&slot) {
            data.size = 0;
        }
    }
}

fn remove_unreachable(func: &mut ir::Function) {
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let blocks: Vec<Block> = func.layout.blocks().collect();
    for block in blocks {
        if domtree.is_reachable(block) {
            continue;
        }
        while let Some(inst) = func.layout.first_inst(block) {
            func.layout.remove_inst(inst);
        }
        func.layout.remove_block(block);
    }
}

/// Block parameters that receive one value on every edge (or themselves,
/// around a loop) become aliases of that value. A slot address that flows
/// through such a parameter is then seen as the slot itself.
const MAX_CONST_PARAM_ROUNDS: usize = 16;

pub fn remove_constant_params(func: &mut ir::Function) {
    let entry = func.layout.entry_block();
    for _round in 0..MAX_CONST_PARAM_ROUNDS {
        let mut edges: HashMap<Block, Vec<(Inst, usize)>> = HashMap::new();
        for (_, inst) in insts(func) {
            let dests = func.dfg.insts[inst]
                .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
            for (k, call) in dests.iter().enumerate() {
                edges
                    .entry(call.block(&func.dfg.value_lists))
                    .or_default()
                    .push((inst, k));
            }
        }
        let mut changed = false;
        let blocks: Vec<Block> = func.layout.blocks().collect();
        for block in blocks {
            if Some(block) == entry {
                continue;
            }
            let Some(incoming) = edges.get(&block) else {
                continue;
            };
            let params = func.dfg.block_params(block).to_vec();
            for i in (0..params.len()).rev() {
                let p = params[i];
                let p_ty = func.dfg.value_type(p);
                let is_int_ty = matches!(p_ty, types::I8 | types::I16 | types::I32 | types::I64);
                let mut only_val: Option<Value> = None;
                let mut only_const: Option<i64> = None;
                let mut saw_non_self = false;
                let mut same_val = true;
                let mut same_const = is_int_ty;
                for &(inst, k) in incoming {
                    let call = func.dfg.insts[inst]
                        .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)[k];
                    match call.args(&func.dfg.value_lists).nth(i) {
                        Some(BlockArg::Value(v)) => {
                            let v = func.dfg.resolve_aliases(v);
                            if v == p {
                                continue;
                            }
                            saw_non_self = true;
                            match only_val {
                                None => only_val = Some(v),
                                Some(o) if o == v => {}
                                Some(_) => same_val = false,
                            }
                            if same_const {
                                if let Some(c) = constant(func, v, 1) {
                                    match only_const {
                                        None => only_const = Some(c),
                                        Some(oc) if oc == c => {}
                                        Some(_) => same_const = false,
                                    }
                                } else {
                                    same_const = false;
                                }
                            }
                        }
                        _ => {
                            same_val = false;
                            same_const = false;
                        }
                    }
                    if !same_val && !same_const {
                        break;
                    }
                }
                if !saw_non_self || (!same_val && !same_const) {
                    continue;
                }
                let replacement = if same_val {
                    let Some(a) = only_val else {
                        continue;
                    };
                    a
                } else {
                    let (Some(c), Some(first)) = (only_const, func.layout.first_inst(block)) else {
                        continue;
                    };
                    let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(first);
                    pos.ins().iconst(p_ty, c)
                };
                for &(inst, k) in incoming {
                    let ir::DataFlowGraph {
                        insts,
                        jump_tables,
                        exception_tables,
                        value_lists,
                        ..
                    } = &mut func.dfg;
                    insts[inst].branch_destination_mut(jump_tables, exception_tables)[k]
                        .remove(i, value_lists);
                }
                func.dfg.remove_block_param(p);
                func.dfg.change_to_alias(p, replacement);
                changed = true;
            }
        }
        if !changed {
            return;
        }
    }
}

/// Extend `seeds` (value -> (key, offset)) through `iadd_imm` chains.
fn derive<K: Copy>(
    func: &ir::Function,
    all: &[(Block, Inst)],
    seeds: &mut HashMap<Value, (K, i64)>,
) {
    loop {
        let before = seeds.len();
        for &(_, inst) in all {
            if let InstructionData::BinaryImm64 {
                opcode: Opcode::IaddImm,
                arg,
                imm,
            } = func.dfg.insts[inst]
            {
                let r = func.dfg.first_result(inst);
                if seeds.contains_key(&r) {
                    continue;
                }
                if let Some(&(k, off)) = seeds.get(&func.dfg.resolve_aliases(arg)) {
                    seeds.insert(r, (k, off.wrapping_add(imm.bits())));
                }
            }
        }
        if seeds.len() == before {
            return;
        }
    }
}

/// An aggregate chosen by an `if` or `match` reaches its join block as a
/// block parameter holding one of several slot addresses, which keeps
/// every one of those slots in memory. When each incoming slot is used only
/// in the block that jumps to the join (stored before it is read there),
/// and the parameter is only read and written at constant offsets, each
/// predecessor copies the words the join reads into one new slot instead,
/// and the parameter becomes that slot's address. [`promote_slots`] then
/// turns all of them into SSA values.
pub fn split_join_slots(func: &mut ir::Function) {
    let mut split_merged: HashSet<StackSlot> = HashSet::new();
    for _ in 0..8 {
        if !split_one_join_slot(func, &mut split_merged) {
            break;
        }
    }
}

fn split_one_join_slot(func: &mut ir::Function, split_merged: &mut HashSet<StackSlot>) -> bool {
    let all = insts(func);
    let mut slot_addr: HashMap<Value, (StackSlot, i64)> = HashMap::new();
    for &(_, inst) in &all {
        if let InstructionData::StackLoad {
            opcode: Opcode::StackAddr,
            stack_slot,
            offset,
        } = func.dfg.insts[inst]
        {
            slot_addr.insert(func.dfg.first_result(inst), (stack_slot, i64::from(offset)));
        }
    }
    if slot_addr.is_empty() {
        return false;
    }
    derive(func, &all, &mut slot_addr);
    // Blocks in which each slot's addresses are used, and how often each
    // slot address appears as a branch argument.
    let mut used_in: HashMap<StackSlot, HashSet<Block>> = HashMap::new();
    let mut as_arg: HashMap<StackSlot, usize> = HashMap::new();
    for &(block, inst) in &all {
        if let InstructionData::StackLoad { stack_slot, .. }
        | InstructionData::StackStore { stack_slot, .. } = func.dfg.insts[inst]
        {
            used_in.entry(stack_slot).or_default().insert(block);
        }
        for v in values_of(func, inst) {
            if let Some(&(slot, _)) = slot_addr.get(&v) {
                used_in.entry(slot).or_default().insert(block);
            }
        }
        let dests = func.dfg.insts[inst]
            .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
        for call in dests {
            for a in call.args(&func.dfg.value_lists) {
                if let BlockArg::Value(v) = a
                    && let Some(&(slot, _)) = slot_addr.get(&func.dfg.resolve_aliases(v))
                {
                    *as_arg.entry(slot).or_default() += 1;
                }
            }
        }
    }
    let mut edges: HashMap<Block, Vec<Inst>> = HashMap::new();
    let mut bad_target: HashSet<Block> = HashSet::new();
    for &(_, inst) in &all {
        let data = &func.dfg.insts[inst];
        let dests = data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
        for call in dests {
            let b = call.block(&func.dfg.value_lists);
            if data.opcode() == Opcode::Jump {
                edges.entry(b).or_default().push(inst);
            } else {
                bad_target.insert(b);
            }
        }
    }
    let mut did_split = false;
    let blocks: Vec<Block> = func.layout.blocks().collect();
    for join in blocks {
        if bad_target.contains(&join) {
            continue;
        }
        let Some(incoming) = edges.get(&join).cloned() else {
            continue;
        };
        if incoming.len() < 2 {
            continue;
        }
        let params = func.dfg.block_params(join).to_vec();
        for i in (0..params.len()).rev() {
            let p = params[i];
            // Every edge passes a distinct whole slot, used only in its block
            // (or a slot created by splitting a prior join).
            let mut sources: Vec<(Inst, StackSlot, Value)> = vec![];
            let mut ok = true;
            for &inst in &incoming {
                let call = func.dfg.insts[inst]
                    .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)[0];
                let Some(BlockArg::Value(v)) = call.args(&func.dfg.value_lists).nth(i) else {
                    ok = false;
                    break;
                };
                let v = func.dfg.resolve_aliases(v);
                let Some(&(slot, 0)) = slot_addr.get(&v) else {
                    ok = false;
                    break;
                };
                let block = func.layout.inst_block(inst).unwrap();
                let only_here = split_merged.contains(&slot)
                    || used_in
                        .get(&slot)
                        .is_some_and(|b| b.len() == 1 && b.contains(&block));
                if !only_here
                    || as_arg.get(&slot).copied().unwrap_or(0) != 1
                    || sources.iter().any(|s| s.1 == slot)
                    || func.sized_stack_slots[slot].kind != StackSlotKind::ExplicitSlot
                {
                    ok = false;
                    break;
                }
                sources.push((inst, slot, v));
            }
            if !ok {
                continue;
            }
            // The parameter (and any downstream unconditional join forwards):
            // read and written at constant offsets only.
            let mut through: HashMap<Value, ((), i64)> = HashMap::new();
            through.insert(p, ((), 0));
            loop {
                let before = through.len();
                derive(func, &all, &mut through);
                for &(_, inst) in &all {
                    let data = &func.dfg.insts[inst];
                    if data.opcode() != Opcode::Jump {
                        continue;
                    }
                    let call = data
                        .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)[0];
                    let target = call.block(&func.dfg.value_lists);
                    if target == join {
                        continue;
                    }
                    let t_params = func.dfg.block_params(target);
                    for (k, a) in call.args(&func.dfg.value_lists).enumerate() {
                        if let BlockArg::Value(v) = a
                            && let Some(&((), 0)) = through.get(&func.dfg.resolve_aliases(v))
                            && let Some(&tp) = t_params.get(k)
                        {
                            through.entry(tp).or_insert(((), 0));
                        }
                    }
                }
                if through.len() == before {
                    break;
                }
            }
            let mut reads: BTreeMap<i64, Type> = BTreeMap::new();
            for &(_, inst) in &all {
                let data = func.dfg.insts[inst];
                let vals = values_of(func, inst);
                if !vals.iter().any(|v| through.contains_key(v)) {
                    continue;
                }
                match data {
                    InstructionData::BinaryImm64 {
                        opcode: Opcode::IaddImm,
                        ..
                    } => {}
                    InstructionData::Jump { .. } => {
                        let call = data
                            .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                            [0];
                        let target = call.block(&func.dfg.value_lists);
                        if target == join {
                            ok = false;
                        }
                    }
                    InstructionData::Load {
                        opcode: Opcode::Load,
                        arg,
                        offset,
                        ..
                    } => {
                        let (_, off) = through[&func.dfg.resolve_aliases(arg)];
                        let ty = func.dfg.value_type(func.dfg.first_result(inst));
                        let off = off + i64::from(offset);
                        if reads.get(&off).is_some_and(|t| *t != ty) {
                            ok = false;
                        }
                        reads.insert(off, ty);
                    }
                    InstructionData::Store {
                        opcode: Opcode::Store,
                        args,
                        ..
                    } if !through.contains_key(&func.dfg.resolve_aliases(args[0])) => {}
                    _ => ok = false,
                }
                if !ok {
                    break;
                }
            }
            if !ok {
                continue;
            }
            let mut end = 0i64;
            for (&off, &ty) in &reads {
                if off < end || off < 0 {
                    ok = false;
                }
                end = off + i64::from(ty.bytes());
            }
            let size = sources
                .iter()
                .map(|s| func.sized_stack_slots[s.1].size)
                .max()
                .unwrap_or(0);
            if !ok || end > i64::from(size) {
                continue;
            }
            // Each source block stores before it reads its slot (prior split
            // slots already have every `reads` offset stored at their edges).
            for &(inst, slot, _) in &sources {
                if split_merged.contains(&slot) {
                    continue;
                }
                let block = func.layout.inst_block(inst).unwrap();
                let mut stored: HashSet<i64> = HashSet::new();
                for b_inst in func.layout.block_insts(block) {
                    match func.dfg.insts[b_inst] {
                        InstructionData::Load { arg, offset, .. } => {
                            if let Some(&(s, off)) = slot_addr.get(&func.dfg.resolve_aliases(arg))
                                && s == slot
                                && !stored.contains(&(off + i64::from(offset)))
                            {
                                ok = false;
                            }
                        }
                        InstructionData::Store { args, offset, .. } => {
                            if let Some(&(s, off)) =
                                slot_addr.get(&func.dfg.resolve_aliases(args[1]))
                                && s == slot
                            {
                                stored.insert(off + i64::from(offset));
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !ok {
                continue;
            }
            let merged = func.create_sized_stack_slot(ir::StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                3,
            ));
            split_merged.insert(merged);
            let flags = ir::MemFlags::trusted();
            for &(inst, _, from) in &sources {
                let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(inst);
                let to = pos.ins().stack_addr(types::I64, merged, 0);
                for (&off, &ty) in &reads {
                    let off = i32::try_from(off).unwrap();
                    let v = pos.ins().load(ty, flags, from, off);
                    pos.ins().store(flags, v, to, off);
                }
            }
            for &(inst, _, _) in &sources {
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    value_lists,
                    ..
                } = &mut func.dfg;
                insts[inst].branch_destination_mut(jump_tables, exception_tables)[0]
                    .remove(i, value_lists);
            }
            func.dfg.remove_block_param(p);
            let first = func.layout.first_inst(join).unwrap();
            let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(first);
            let a = pos.ins().stack_addr(types::I64, merged, 0);
            func.dfg.change_to_alias(p, a);
            did_split = true;
            // The analysis above is stale for this block now; one
            // parameter per join per run.
            break;
        }
    }
    did_split
}

/// A block reached only by a `jump` from one other block joins that block.
pub fn merge_blocks(func: &mut ir::Function) {
    let mut preds: HashMap<Block, usize> = HashMap::new();
    for (_, inst) in insts(func) {
        let dests = func.dfg.insts[inst]
            .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
        for call in dests {
            *preds.entry(call.block(&func.dfg.value_lists)).or_default() += 1;
        }
    }
    let entry = func.layout.entry_block();
    let blocks: Vec<Block> = func.layout.blocks().collect();
    for a in blocks {
        if !func.layout.is_block_inserted(a) {
            continue;
        }
        while let Some(last) = func.layout.last_inst(a) {
            let InstructionData::Jump { destination, .. } = func.dfg.insts[last] else {
                break;
            };
            let b = destination.block(&func.dfg.value_lists);
            if b == a || Some(b) == entry || preds.get(&b).copied() != Some(1) {
                break;
            }
            let args: Vec<BlockArg> = destination.args(&func.dfg.value_lists).collect();
            if args.iter().any(|x| !matches!(x, BlockArg::Value(_))) {
                break;
            }
            let params = func.dfg.block_params(b).to_vec();
            func.layout.remove_inst(last);
            for (p, arg) in params.iter().zip(args).rev() {
                let BlockArg::Value(v) = arg else {
                    unreachable!()
                };
                func.dfg.remove_block_param(*p);
                func.dfg.change_to_alias(*p, v);
            }
            let moved: Vec<Inst> = func.layout.block_insts(b).collect();
            for inst in moved {
                func.layout.remove_inst(inst);
                func.layout.append_inst(inst, a);
            }
            func.layout.remove_block(b);
        }
    }
}

/// Blocks with the same instructions on the same inputs, ending in the same
/// branch or return, are one block: every `return no_recv()` or failure
/// exit of a function is otherwise its own copy of the same stores.
pub fn merge_tails(func: &mut ir::Function) {
    for _round in 0..4 {
        let mut def_block: HashMap<Value, Block> = HashMap::new();
        for block in func.layout.blocks() {
            for &p in func.dfg.block_params(block) {
                def_block.insert(p, block);
            }
            for inst in func.layout.block_insts(block) {
                for &r in func.dfg.inst_results(inst) {
                    def_block.insert(r, block);
                }
            }
        }
        // Values used outside the block that defines them.
        let mut escapes: HashSet<Block> = HashSet::new();
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                for v in values_of(func, inst) {
                    if let Some(&d) = def_block.get(&v)
                        && d != block
                    {
                        escapes.insert(d);
                    }
                }
            }
        }
        let entry = func.layout.entry_block();
        let mut groups: HashMap<String, Block> = HashMap::new();
        let mut replace: HashMap<Block, Block> = HashMap::new();
        let blocks: Vec<Block> = func.layout.blocks().collect();
        for block in blocks {
            if Some(block) == entry
                || escapes.contains(&block)
                || !func.dfg.block_params(block).is_empty()
            {
                continue;
            }
            let mut local: HashMap<Value, usize> = HashMap::new();
            let mut key = String::new();
            let mut self_ref = false;
            for inst in func.layout.block_insts(block) {
                for &r in func.dfg.inst_results(inst) {
                    let n = local.len();
                    local.insert(r, n);
                }
                let dests = func.dfg.insts[inst]
                    .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
                if dests
                    .iter()
                    .any(|c| c.block(&func.dfg.value_lists) == block)
                {
                    self_ref = true;
                }
                let text = func.dfg.display_inst(inst).to_string();
                let mut out = String::with_capacity(text.len());
                let mut rest = text.as_str();
                while let Some(at) = rest.find('v') {
                    let (head, tail) = rest.split_at(at);
                    out.push_str(head);
                    let digits = tail[1..].bytes().take_while(u8::is_ascii_digit).count();
                    let boundary = head
                        .chars()
                        .last()
                        .is_none_or(|c| !c.is_ascii_alphanumeric());
                    if digits == 0 || !boundary {
                        out.push('v');
                        rest = &tail[1..];
                        continue;
                    }
                    let n: u32 = tail[1..1 + digits].parse().unwrap();
                    let v = func.dfg.resolve_aliases(Value::from_u32(n));
                    match local.get(&v) {
                        Some(i) => out.push_str(&format!("L{i}")),
                        None => out.push_str(&format!("v{}", v.as_u32())),
                    }
                    rest = &tail[1 + digits..];
                }
                out.push_str(rest);
                key.push_str(&out);
                key.push('\n');
            }
            if self_ref {
                continue;
            }
            match groups.get(&key) {
                Some(&first) => {
                    replace.insert(block, first);
                }
                None => {
                    groups.insert(key, block);
                }
            }
        }
        if replace.is_empty() {
            return;
        }
        for (_, inst) in insts(func) {
            let ir::DataFlowGraph {
                insts,
                jump_tables,
                exception_tables,
                value_lists,
                ..
            } = &mut func.dfg;
            for call in insts[inst].branch_destination_mut(jump_tables, exception_tables) {
                if let Some(&to) = replace.get(&call.block(value_lists)) {
                    call.set_block(to, value_lists);
                }
            }
        }
        for block in replace.keys() {
            while let Some(inst) = func.layout.first_inst(*block) {
                func.layout.remove_inst(inst);
            }
            func.layout.remove_block(*block);
        }
    }
}

/// The resolved value arguments of `inst`, block-call arguments included.
fn values_of(func: &ir::Function, inst: Inst) -> Vec<Value> {
    func.dfg
        .inst_values(inst)
        .map(|v| func.dfg.resolve_aliases(v))
        .collect()
}

fn insts(func: &ir::Function) -> Vec<(Block, Inst)> {
    func.layout
        .blocks()
        .flat_map(|b| func.layout.block_insts(b).map(move |i| (b, i)))
        .collect()
}

#[derive(Clone, Copy)]
enum Access {
    Load { offset: i64, ty: Type },
    Store { offset: i64, ty: Type, value: Value },
}

fn promotable_type(ty: Type) -> bool {
    matches!(
        ty,
        types::I8 | types::I16 | types::I32 | types::I64 | types::F32 | types::F64
    ) || (ty.is_vector() && ty.bits() == 128)
}

/// mem2reg over explicit slots whose address does not escape.
pub fn promote_slots(func: &mut ir::Function) {
    if func.sized_stack_slots.is_empty() {
        return;
    }
    let all = insts(func);
    // Addresses: value -> (slot, offset from the slot's start).
    let mut addr: HashMap<Value, (StackSlot, i64)> = HashMap::new();
    let mut escaped: HashSet<StackSlot> = HashSet::new();
    loop {
        let before = addr.len();
        for &(_, inst) in &all {
            match func.dfg.insts[inst] {
                InstructionData::StackLoad {
                    opcode: Opcode::StackAddr,
                    stack_slot,
                    offset,
                } => {
                    let r = func.dfg.first_result(inst);
                    addr.insert(r, (stack_slot, i64::from(offset)));
                }
                InstructionData::StackLoad { stack_slot, .. }
                | InstructionData::StackStore { stack_slot, .. } => {
                    escaped.insert(stack_slot);
                }
                InstructionData::BinaryImm64 {
                    opcode: Opcode::IaddImm,
                    arg,
                    imm,
                } => {
                    let arg = func.dfg.resolve_aliases(arg);
                    if let Some(&(slot, off)) = addr.get(&arg) {
                        let r = func.dfg.first_result(inst);
                        addr.insert(r, (slot, off.wrapping_add(imm.bits())));
                    }
                }
                _ => {}
            }
        }
        if addr.len() == before {
            break;
        }
    }
    if addr.is_empty() {
        return;
    }
    let mut accesses: HashMap<Inst, (StackSlot, Access)> = HashMap::new();
    for &(_, inst) in &all {
        let data = &func.dfg.insts[inst];
        match *data {
            InstructionData::StackLoad {
                opcode: Opcode::StackAddr,
                ..
            } => continue,
            InstructionData::BinaryImm64 {
                opcode: Opcode::IaddImm,
                arg,
                ..
            } if addr.contains_key(&func.dfg.resolve_aliases(arg)) => continue,
            InstructionData::Load {
                opcode: Opcode::Load,
                arg,
                offset,
                ..
            } => {
                let arg = func.dfg.resolve_aliases(arg);
                if let Some(&(slot, off)) = addr.get(&arg) {
                    let ty = func.dfg.value_type(func.dfg.first_result(inst));
                    accesses.insert(
                        inst,
                        (
                            slot,
                            Access::Load {
                                offset: off + i64::from(offset),
                                ty,
                            },
                        ),
                    );
                }
                continue;
            }
            InstructionData::Store {
                opcode: Opcode::Store,
                args,
                offset,
                ..
            } => {
                let value = func.dfg.resolve_aliases(args[0]);
                let base = func.dfg.resolve_aliases(args[1]);
                if let Some(&(slot, _)) = addr.get(&value) {
                    escaped.insert(slot);
                }
                if let Some(&(slot, off)) = addr.get(&base) {
                    let ty = func.dfg.value_type(value);
                    accesses.insert(
                        inst,
                        (
                            slot,
                            Access::Store {
                                offset: off + i64::from(offset),
                                ty,
                                value,
                            },
                        ),
                    );
                }
                continue;
            }
            _ => {}
        }
        for v in values_of(func, inst) {
            if let Some(&(slot, _)) = addr.get(&v) {
                escaped.insert(slot);
            }
        }
    }
    // Cells: (slot, offset) -> type, checked for one type per offset, no
    // overlap and accesses inside the slot.
    let mut cells: BTreeMap<StackSlot, BTreeMap<i64, Type>> = BTreeMap::new();
    for (slot, access) in accesses.values() {
        let (offset, ty) = match *access {
            Access::Load { offset, ty } | Access::Store { offset, ty, .. } => (offset, ty),
        };
        let entry = cells.entry(*slot).or_default();
        match entry.get(&offset) {
            Some(&t) if t != ty => {
                escaped.insert(*slot);
            }
            _ => {
                entry.insert(offset, ty);
            }
        }
        if !promotable_type(ty) {
            escaped.insert(*slot);
        }
    }
    for (slot, map) in &cells {
        let data = &func.sized_stack_slots[*slot];
        if data.kind != StackSlotKind::ExplicitSlot {
            escaped.insert(*slot);
            continue;
        }
        let size = i64::from(data.size);
        let mut end = 0i64;
        for (&offset, &ty) in map {
            if offset < end || offset < 0 || offset + i64::from(ty.bytes()) > size {
                escaped.insert(*slot);
                break;
            }
            end = offset + i64::from(ty.bytes());
        }
    }
    let mut cell_ids: HashMap<(StackSlot, i64), usize> = HashMap::new();
    let mut cell_types: Vec<Type> = vec![];
    for (slot, map) in &cells {
        if escaped.contains(slot) {
            continue;
        }
        for (&offset, &ty) in map {
            cell_ids.insert((*slot, offset), cell_types.len());
            cell_types.push(ty);
        }
    }
    if cell_types.is_empty() {
        return;
    }
    let cell_of = |slot: StackSlot, access: &Access| -> Option<usize> {
        let offset = match *access {
            Access::Load { offset, .. } | Access::Store { offset, .. } => offset,
        };
        cell_ids.get(&(slot, offset)).copied()
    };
    let n = cell_types.len();
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let blocks: Vec<Block> = func.layout.blocks().collect();
    let index: HashMap<Block, usize> = blocks.iter().enumerate().map(|(i, b)| (*b, i)).collect();
    let words = n.div_ceil(64);
    let mut defs = vec![vec![0u64; words]; blocks.len()];
    let mut upward = vec![vec![0u64; words]; blocks.len()];
    let set = |bits: &mut [u64], c: usize| bits[c / 64] |= 1 << (c % 64);
    let has = |bits: &[u64], c: usize| bits[c / 64] & (1 << (c % 64)) != 0;
    for (bi, &block) in blocks.iter().enumerate() {
        for inst in func.layout.block_insts(block) {
            if let Some((slot, access)) = accesses.get(&inst)
                && let Some(c) = cell_of(*slot, access)
            {
                match access {
                    Access::Load { .. } => {
                        if !has(&defs[bi], c) {
                            set(&mut upward[bi], c);
                        }
                    }
                    Access::Store { .. } => set(&mut defs[bi], c),
                }
            }
        }
    }
    let succs: Vec<Vec<usize>> = blocks
        .iter()
        .map(|b| {
            cfg.succ_iter(*b)
                .filter_map(|s| index.get(&s).copied())
                .collect()
        })
        .collect();
    let mut live_in = upward.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for &block in domtree.cfg_postorder() {
            let bi = index[&block];
            let mut out = vec![0u64; words];
            for &s in &succs[bi] {
                for w in 0..words {
                    out[w] |= live_in[s][w];
                }
            }
            for w in 0..words {
                let new = upward[bi][w] | (out[w] & !defs[bi][w]);
                if new != live_in[bi][w] {
                    live_in[bi][w] = new;
                    changed = true;
                }
            }
        }
    }
    // Dominance frontiers.
    let mut frontier: Vec<Vec<usize>> = vec![vec![]; blocks.len()];
    for (bi, &block) in blocks.iter().enumerate() {
        let preds: Vec<Block> = cfg.pred_iter(block).map(|p| p.block).collect();
        if preds.len() < 2 {
            continue;
        }
        let Some(idom) = domtree.idom(block) else {
            continue;
        };
        for p in preds {
            let mut runner = p;
            while runner != idom {
                let ri = index[&runner];
                if !frontier[ri].contains(&bi) {
                    frontier[ri].push(bi);
                }
                match domtree.idom(runner) {
                    Some(up) => runner = up,
                    None => break,
                }
            }
        }
    }
    // Phi placement, pruned by liveness.
    let entry = blocks[0];
    let mut phis: Vec<Vec<usize>> = vec![vec![]; blocks.len()];
    for c in 0..n {
        let mut work: Vec<usize> = (0..blocks.len()).filter(|&b| has(&defs[b], c)).collect();
        let mut placed = vec![false; blocks.len()];
        let mut queued = vec![false; blocks.len()];
        for &b in &work {
            queued[b] = true;
        }
        while let Some(b) = work.pop() {
            for &f in &frontier[b] {
                if placed[f] || !has(&live_in[f], c) {
                    continue;
                }
                placed[f] = true;
                phis[f].push(c);
                if !queued[f] {
                    queued[f] = true;
                    work.push(f);
                }
            }
        }
    }
    let mut phi_params: Vec<Vec<Value>> = vec![vec![]; blocks.len()];
    for (bi, cs) in phis.iter().enumerate() {
        for &c in cs {
            phi_params[bi].push(func.dfg.append_block_param(blocks[bi], cell_types[c]));
        }
    }
    // Initial values: zero, defined at the top of the entry block.
    let first = func
        .layout
        .first_inst(entry)
        .expect("entry block has a terminator");
    let mut zero_of: HashMap<Type, Value> = HashMap::new();
    let mut initial = Vec::with_capacity(n);
    for &ty in &cell_types {
        let v = *zero_of.entry(ty).or_insert_with(|| {
            if ty.is_vector() {
                let zero = func.dfg.constants.insert(vec![0u8; 16].into());
                let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(first);
                return pos.ins().vconst(ty, zero);
            }
            let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(first);
            match ty {
                types::F32 => pos.ins().f32const(0.0),
                types::F64 => pos.ins().f64const(0.0),
                _ => pos.ins().iconst(ty, 0),
            }
        });
        initial.push(v);
    }
    // Rename along the dominator tree.
    let mut children: Vec<Vec<usize>> = vec![vec![]; blocks.len()];
    for (bi, &block) in blocks.iter().enumerate() {
        if let Some(idom) = domtree.idom(block) {
            children[index[&idom]].push(bi);
        }
    }
    let mut current = initial;
    enum Step {
        Enter(usize),
        Leave(Vec<(usize, Value)>),
    }
    let mut stack = vec![Step::Enter(0)];
    while let Some(step) = stack.pop() {
        let bi = match step {
            Step::Leave(undo) => {
                for (c, v) in undo.into_iter().rev() {
                    current[c] = v;
                }
                continue;
            }
            Step::Enter(bi) => bi,
        };
        let block = blocks[bi];
        let mut undo = vec![];
        for (k, &c) in phis[bi].iter().enumerate() {
            undo.push((c, current[c]));
            current[c] = phi_params[bi][k];
        }
        let body: Vec<Inst> = func.layout.block_insts(block).collect();
        for inst in body {
            if let Some((slot, access)) = accesses.get(&inst).copied()
                && let Some(c) = cell_of(slot, &access)
            {
                match access {
                    Access::Load { .. } => {
                        let r = func.dfg.first_result(inst);
                        func.dfg.detach_inst_results(inst);
                        func.layout.remove_inst(inst);
                        func.dfg.change_to_alias(r, current[c]);
                    }
                    Access::Store { value, .. } => {
                        undo.push((c, current[c]));
                        current[c] = value;
                        func.layout.remove_inst(inst);
                    }
                }
                continue;
            }
            if func.dfg.insts[inst].opcode().is_branch() {
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    value_lists,
                    ..
                } = &mut func.dfg;
                for call in insts[inst].branch_destination_mut(jump_tables, exception_tables) {
                    let target = call.block(value_lists);
                    let Some(&ti) = index.get(&target) else {
                        continue;
                    };
                    for &c in &phis[ti] {
                        call.append_argument(current[c], value_lists);
                    }
                }
            }
        }
        stack.push(Step::Leave(undo));
        for &child in children[bi].iter().rev() {
            stack.push(Step::Enter(child));
        }
    }
}

/// A join block that only compares one parameter with a constant and
/// branches on the result (a match on a tag chosen by an `if`) takes the
/// comparison's result instead: each predecessor compares, so
/// [`thread_branches`] can fold the test where the tag is a constant.
pub fn sink_compares(func: &mut ir::Function) {
    let mut uses: HashMap<Value, usize> = HashMap::new();
    for (_, inst) in insts(func) {
        for v in values_of(func, inst) {
            *uses.entry(v).or_default() += 1;
        }
    }
    let mut edges: HashMap<Block, Vec<(Inst, usize)>> = HashMap::new();
    let mut table_target: HashSet<Block> = HashSet::new();
    for (_, inst) in insts(func) {
        let data = &func.dfg.insts[inst];
        let dests = data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
        for (k, call) in dests.iter().enumerate() {
            let b = call.block(&func.dfg.value_lists);
            if matches!(data.opcode(), Opcode::Jump | Opcode::Brif) {
                edges.entry(b).or_default().push((inst, k));
            } else {
                table_target.insert(b);
            }
        }
    }
    let blocks: Vec<Block> = func.layout.blocks().collect();
    for block in blocks {
        if table_target.contains(&block) {
            continue;
        }
        let body: Vec<Inst> = func.layout.block_insts(block).collect();
        let [cmp, br] = body[..] else {
            continue;
        };
        let InstructionData::IntCompareImm {
            opcode: Opcode::IcmpImm,
            cond,
            arg,
            imm,
        } = func.dfg.insts[cmp]
        else {
            continue;
        };
        let InstructionData::Brif { arg: test, .. } = func.dfg.insts[br] else {
            continue;
        };
        let flag = func.dfg.first_result(cmp);
        let p = func.dfg.resolve_aliases(arg);
        let params = func.dfg.block_params(block).to_vec();
        let Some(i) = params.iter().position(|&q| q == p) else {
            continue;
        };
        if func.dfg.resolve_aliases(test) != flag
            || uses.get(&flag).copied() != Some(1)
            || uses.get(&p).copied() != Some(1)
        {
            continue;
        }
        let Some(incoming) = edges.get(&block).cloned() else {
            continue;
        };
        let mut ok = true;
        let mut values = vec![];
        for &(inst, k) in &incoming {
            let call = func.dfg.insts[inst]
                .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)[k];
            match call.args(&func.dfg.value_lists).nth(i) {
                Some(BlockArg::Value(v)) => values.push(v),
                _ => ok = false,
            }
        }
        if !ok {
            continue;
        }
        let flag_ty = func.dfg.value_type(flag);
        let new_param = func.dfg.append_block_param(block, flag_ty);
        for (&(inst, k), v) in incoming.iter().zip(values) {
            let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(inst);
            let c = pos.ins().icmp_imm(cond, v, imm);
            let ir::DataFlowGraph {
                insts,
                jump_tables,
                exception_tables,
                value_lists,
                ..
            } = &mut func.dfg;
            let call = &mut insts[inst].branch_destination_mut(jump_tables, exception_tables)[k];
            call.remove(i, value_lists);
            call.append_argument(c, value_lists);
        }
        func.dfg.remove_block_param(p);
        func.layout.remove_inst(cmp);
        if let InstructionData::Brif { arg, .. } = &mut func.dfg.insts[br] {
            *arg = new_param;
        }
    }
}

/// A constant integer value of `v`, folding the few operations the
/// lowering builds its 0/1 tests from.
fn constant(func: &ir::Function, v: Value, depth: u32) -> Option<i64> {
    constant_with(func, &HashMap::new(), v, depth)
}

/// [`constant`], where the values in `known` hold the given constants.
fn constant_with(
    func: &ir::Function,
    known: &HashMap<Value, i64>,
    v: Value,
    depth: u32,
) -> Option<i64> {
    if depth > 8 {
        return None;
    }
    let v = func.dfg.resolve_aliases(v);
    if let Some(&k) = known.get(&v) {
        return Some(k);
    }
    let inst = func.dfg.value_def(v).inst()?;
    let ty = func.dfg.value_type(v);
    let mask = |x: i64| -> i64 {
        match ty.bits() {
            64 => x,
            b if b < 64 => x & ((1i64 << b) - 1),
            _ => x,
        }
    };
    match func.dfg.insts[inst] {
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => Some(mask(imm.bits())),
        InstructionData::Unary {
            opcode: Opcode::Uextend | Opcode::Ireduce,
            arg,
        } => constant_with(func, known, arg, depth + 1).map(mask),
        InstructionData::IntCompareImm {
            opcode: Opcode::IcmpImm,
            cond,
            arg,
            imm,
        } => {
            let a = constant_with(func, known, arg, depth + 1)?;
            let arg_ty = func.dfg.value_type(func.dfg.resolve_aliases(arg));
            let b = match arg_ty.bits() {
                64 => imm.bits(),
                bits => imm.bits() & ((1i64 << bits) - 1),
            };
            match cond {
                ir::condcodes::IntCC::Equal => Some(i64::from(a == b)),
                ir::condcodes::IntCC::NotEqual => Some(i64::from(a != b)),
                _ => None,
            }
        }
        InstructionData::Binary { opcode, args } => {
            let a = constant_with(func, known, args[0], depth + 1)?;
            let b = constant_with(func, known, args[1], depth + 1)?;
            match opcode {
                Opcode::Band => Some(a & b),
                Opcode::Bor => Some(a | b),
                Opcode::Bxor => Some(a ^ b),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Jump threading through blocks that only branch on a parameter.
pub fn thread_branches(func: &mut ir::Function) {
    for _round in 0..8 {
        // Uses of each block parameter.
        let mut uses: HashMap<Value, usize> = HashMap::new();
        for (_, inst) in insts(func) {
            for v in values_of(func, inst) {
                *uses.entry(v).or_default() += 1;
            }
        }
        // Candidate blocks: a lone `brif` on a parameter, whose parameters
        // are used nowhere else.
        let mut candidates: HashMap<Block, Inst> = HashMap::new();
        for block in func.layout.blocks() {
            let Some(inst) = func.layout.first_inst(block) else {
                continue;
            };
            if func.layout.last_inst(block) != Some(inst) {
                continue;
            }
            let InstructionData::Brif { arg, blocks, .. } = func.dfg.insts[inst] else {
                continue;
            };
            let params = func.dfg.block_params(block);
            if !params.contains(&func.dfg.resolve_aliases(arg)) {
                continue;
            }
            let local: usize = values_of(func, inst)
                .iter()
                .filter(|v| params.contains(v))
                .count();
            let total: usize = params
                .iter()
                .map(|p| uses.get(p).copied().unwrap_or(0))
                .sum();
            if local != total {
                continue;
            }
            let pool = &func.dfg.value_lists;
            if blocks.iter().any(|c| c.block(pool) == block) {
                continue;
            }
            candidates.insert(block, inst);
        }
        if candidates.is_empty() {
            return;
        }
        let mut changed = false;
        for (_, inst) in insts(func) {
            let opcode = func.dfg.insts[inst].opcode();
            if !opcode.is_branch() || opcode == Opcode::BrTable {
                continue;
            }
            let calls: Vec<ir::BlockCall> = func.dfg.insts[inst]
                .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                .to_vec();
            if opcode != Opcode::Jump && opcode != Opcode::Brif {
                continue;
            }
            for (k, call) in calls.iter().enumerate() {
                let target = call.block(&func.dfg.value_lists);
                let Some(&brif) = candidates.get(&target) else {
                    continue;
                };
                if target == func.layout.inst_block(inst).unwrap() {
                    continue;
                }
                let args: Vec<BlockArg> = call.args(&func.dfg.value_lists).collect();
                let params: Vec<Value> = func.dfg.block_params(target).to_vec();
                if args.len() != params.len() {
                    continue;
                }
                let mut map: HashMap<Value, Value> = HashMap::new();
                let mut plain = true;
                for (p, a) in params.iter().zip(&args) {
                    match a {
                        BlockArg::Value(v) => {
                            map.insert(*p, *v);
                        }
                        _ => plain = false,
                    }
                }
                if !plain {
                    continue;
                }
                let InstructionData::Brif {
                    arg: cond,
                    blocks: succ,
                    ..
                } = func.dfg.insts[brif]
                else {
                    continue;
                };
                let cond = map[&func.dfg.resolve_aliases(cond)];
                let remap = |func: &ir::Function, c: ir::BlockCall| -> (Block, Vec<BlockArg>) {
                    let pool = &func.dfg.value_lists;
                    let args = c
                        .args(pool)
                        .map(|a| match a {
                            BlockArg::Value(v) => {
                                let v = func.dfg.resolve_aliases(v);
                                BlockArg::Value(map.get(&v).copied().unwrap_or(v))
                            }
                            other => other,
                        })
                        .collect();
                    (c.block(pool), args)
                };
                let (then_block, then_args) = remap(func, succ[0]);
                let (else_block, else_args) = remap(func, succ[1]);
                if let Some(value) = constant(func, cond, 0) {
                    let (b, a) = if value != 0 {
                        (then_block, then_args)
                    } else {
                        (else_block, else_args)
                    };
                    let new = func.dfg.block_call(b, &a);
                    let ir::DataFlowGraph {
                        insts,
                        jump_tables,
                        exception_tables,
                        ..
                    } = &mut func.dfg;
                    insts[inst].branch_destination_mut(jump_tables, exception_tables)[k] = new;
                    changed = true;
                } else if opcode == Opcode::Jump {
                    func.dfg
                        .replace(inst)
                        .brif(cond, then_block, &then_args, else_block, &else_args);
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return;
        }
    }
}

/// A 0/1 value: a comparison, or bit operations and extensions of them.
fn is_flag(func: &ir::Function, v: Value, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    let v = func.dfg.resolve_aliases(v);
    let Some(inst) = func.dfg.value_def(v).inst() else {
        return false;
    };
    match func.dfg.insts[inst] {
        InstructionData::IntCompare { .. } | InstructionData::IntCompareImm { .. } => true,
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => matches!(imm.bits(), 0 | 1),
        InstructionData::Unary {
            opcode: Opcode::Uextend,
            arg,
        } => is_flag(func, arg, depth + 1),
        InstructionData::Binary {
            opcode: Opcode::Band | Opcode::Bor | Opcode::Bxor,
            args,
        } => is_flag(func, args[0], depth + 1) && is_flag(func, args[1], depth + 1),
        _ => false,
    }
}

/// What the branch ending `pred` says about its condition on edge `k`
/// (0 taken when nonzero, 1 when zero).
fn edge_fact(func: &ir::Function, pred: Inst, k: usize) -> Option<(Value, i64)> {
    let InstructionData::Brif { arg, blocks, .. } = func.dfg.insts[pred] else {
        return None;
    };
    let pool = &func.dfg.value_lists;
    if blocks[0].block(pool) == blocks[1].block(pool) {
        return None;
    }
    let arg = func.dfg.resolve_aliases(arg);
    match k {
        0 if is_flag(func, arg, 0) => Some((arg, 1)),
        1 => Some((arg, 0)),
        _ => None,
    }
}

/// Jump threading on a condition a dominating branch already decided.
///
/// A block with no parameters that computes a test from values defined
/// elsewhere and branches on it (`try_write`'s ok flag tested again after
/// the write, a `match` arm re-testing its tag) is skipped on each incoming
/// edge where the test folds to a constant: the edge leaves a `brif` on a
/// value the test reads, or a jump from a block whose only way in is such
/// an edge.
pub fn thread_decided_branches(func: &mut ir::Function) {
    let mut cfg = ControlFlowGraph::with_function(func);
    let mut uses_outside: HashSet<Value> = HashSet::new();
    for (block, inst) in insts(func) {
        for v in values_of(func, inst) {
            if let Some(def) = func.dfg.value_def(v).inst()
                && func.layout.inst_block(def) != Some(block)
            {
                uses_outside.insert(v);
            }
        }
    }
    let blocks: Vec<Block> = func.layout.blocks().collect();
    for block in blocks {
        if !func.dfg.block_params(block).is_empty() {
            continue;
        }
        let body: Vec<Inst> = func.layout.block_insts(block).collect();
        let Some((&brif, rest)) = body.split_last() else {
            continue;
        };
        let InstructionData::Brif {
            arg: cond,
            blocks: succ,
            ..
        } = func.dfg.insts[brif]
        else {
            continue;
        };
        let pure = rest.iter().all(|&i| {
            matches!(
                func.dfg.insts[i].opcode(),
                Opcode::Iconst
                    | Opcode::Uextend
                    | Opcode::Ireduce
                    | Opcode::IcmpImm
                    | Opcode::Icmp
                    | Opcode::Band
                    | Opcode::Bor
                    | Opcode::Bxor
            ) && func
                .dfg
                .inst_results(i)
                .iter()
                .all(|r| !uses_outside.contains(r))
        });
        let local = |v: Value| {
            func.dfg
                .value_def(func.dfg.resolve_aliases(v))
                .inst()
                .is_some_and(|d| func.layout.inst_block(d) == Some(block))
        };
        let pool = &func.dfg.value_lists;
        let outside_args = succ.iter().all(|c| {
            c.block(pool) != block
                && c.args(pool).all(|a| match a {
                    BlockArg::Value(v) => !local(v),
                    _ => false,
                })
        });
        if !pure || !outside_args {
            continue;
        }
        let succ: Vec<(Block, Vec<BlockArg>)> = succ
            .iter()
            .map(|c| (c.block(pool), c.args(pool).collect()))
            .collect();
        let preds: Vec<Inst> = cfg.pred_iter(block).map(|p| p.inst).collect();
        let mut changed = false;
        for pred in preds {
            let calls: Vec<ir::BlockCall> = func.dfg.insts[pred]
                .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                .to_vec();
            if !matches!(func.dfg.insts[pred].opcode(), Opcode::Jump | Opcode::Brif) {
                continue;
            }
            for (k, call) in calls.iter().enumerate() {
                if call.block(&func.dfg.value_lists) != block {
                    continue;
                }
                let mut fact = edge_fact(func, pred, k);
                if fact.is_none()
                    && func.dfg.insts[pred].opcode() == Opcode::Jump
                    && let Some(pred_block) = func.layout.inst_block(pred)
                    && let mut outer = cfg.pred_iter(pred_block)
                    && let Some(only) = outer.next()
                    && outer.next().is_none()
                {
                    let into: Vec<ir::BlockCall> = func.dfg.insts[only.inst]
                        .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                        .to_vec();
                    let edges: Vec<usize> = (0..into.len())
                        .filter(|&e| into[e].block(&func.dfg.value_lists) == pred_block)
                        .collect();
                    if let [e] = edges[..] {
                        fact = edge_fact(func, only.inst, e);
                    }
                }
                let Some((value, known)) = fact else {
                    continue;
                };
                let known = HashMap::from([(value, known)]);
                let Some(taken) = constant_with(func, &known, cond, 0) else {
                    continue;
                };
                let (target, args) = &succ[usize::from(taken == 0)];
                let new = func.dfg.block_call(*target, args);
                let ir::DataFlowGraph {
                    insts,
                    jump_tables,
                    exception_tables,
                    ..
                } = &mut func.dfg;
                insts[pred].branch_destination_mut(jump_tables, exception_tables)[k] = new;
                changed = true;
            }
        }
        // A redirected edge gives its target a new way in, which the
        // single-predecessor test above must see.
        if changed {
            cfg.compute(func);
        }
    }
}

fn removable(func: &ir::Function, inst: Inst) -> bool {
    let op = func.dfg.insts[inst].opcode();
    if func.dfg.inst_results(inst).is_empty() {
        return false;
    }
    match op {
        Opcode::Load
        | Opcode::Uload8
        | Opcode::Sload8
        | Opcode::Uload16
        | Opcode::Sload16
        | Opcode::Uload32
        | Opcode::Sload32 => true,
        _ => {
            !(op.can_trap()
                || op.can_load()
                || op.can_store()
                || op.other_side_effects()
                || op.is_call()
                || op.is_branch()
                || op.is_terminator())
        }
    }
}

/// Delete unused loads and pure instructions.
pub fn remove_dead(func: &mut ir::Function) {
    let all = insts(func);
    let mut uses: HashMap<Value, usize> = HashMap::new();
    for &(_, inst) in &all {
        for v in values_of(func, inst) {
            *uses.entry(v).or_default() += 1;
        }
    }
    let dead = |func: &ir::Function, uses: &HashMap<Value, usize>, inst: Inst| {
        removable(func, inst)
            && func
                .dfg
                .inst_results(inst)
                .iter()
                .all(|r| uses.get(r).copied().unwrap_or(0) == 0)
    };
    let mut work: Vec<Inst> = all
        .iter()
        .map(|&(_, i)| i)
        .filter(|&i| dead(func, &uses, i))
        .collect();
    let mut removed: HashSet<Inst> = HashSet::new();
    while let Some(inst) = work.pop() {
        if removed.contains(&inst) || !dead(func, &uses, inst) {
            continue;
        }
        let args = values_of(func, inst);
        func.layout.remove_inst(inst);
        removed.insert(inst);
        for v in args {
            if let Some(count) = uses.get_mut(&v) {
                *count -= 1;
                if *count == 0
                    && let Some(def) = func.dfg.value_def(v).inst()
                    && func.layout.inst_block(def).is_some()
                {
                    work.push(def);
                }
            }
        }
    }
}

/// Read each `readonly can_move notrap` word that is loaded more than once
/// a single time, in the nearest block that dominates every load of it,
/// and reuse that value at every load.
///
/// Cranelift's egraph merges such loads but elaborates a value only where a
/// side-effecting instruction uses it: a header word first used by a store
/// in one arm of a branch is loaded again in every sibling arm. `can_move`
/// says the word is valid and unchanged wherever its address is, and the
/// address definition dominates every load, so it dominates their common
/// dominator too. The kept load loses `readonly`, so the egraph leaves it
/// where it is. A word loaded once is left to Cranelift.
pub fn share_invariant_loads(func: &mut ir::Function) {
    // Address, offset, type index and alias region.
    type WordKey = (Value, i32, usize, u32);
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut rank: HashMap<Block, usize> = HashMap::new();
    for (n, &block) in domtree.cfg_postorder().iter().rev().enumerate() {
        rank.insert(block, n);
    }
    let common = |mut a: Block, mut b: Block| {
        while a != b {
            if rank[&a] > rank[&b] {
                a = domtree.idom(a).expect("reachable block");
            } else {
                b = domtree.idom(b).expect("reachable block");
            }
        }
        a
    };
    // A merged word can be the address of another (a slice's handle, then
    // its data pointer): repeat until no word is loaded twice.
    loop {
        let mut groups: BTreeMap<WordKey, Vec<(Block, Inst)>> = BTreeMap::new();
        for (block, inst) in insts(func) {
            let InstructionData::Load {
                opcode: Opcode::Load,
                arg,
                flags,
                offset,
            } = func.dfg.insts[inst]
            else {
                continue;
            };
            if !(flags.readonly() && flags.can_move() && flags.notrap())
                || !rank.contains_key(&block)
            {
                continue;
            }
            let ty = func.dfg.value_type(func.dfg.first_result(inst));
            let region = flags.alias_region().map_or(0, |r| r as u32 + 1);
            let key = (
                func.dfg.resolve_aliases(arg),
                i32::from(offset),
                ty.index(),
                region,
            );
            groups.entry(key).or_default().push((block, inst));
        }
        let mut changed = false;
        for (_, loads) in groups {
            if loads.len() < 2 {
                continue;
            }
            let target = loads.iter().map(|&(b, _)| b).reduce(&common).unwrap();
            let (_, kept) = loads[0];
            let before = func
                .layout
                .block_insts(target)
                .find(|i| loads.iter().any(|&(_, l)| l == *i))
                .or_else(|| func.layout.last_inst(target))
                .unwrap();
            if before != kept {
                func.layout.remove_inst(kept);
                func.layout.insert_inst(kept, before);
            }
            if let InstructionData::Load { flags, .. } = &mut func.dfg.insts[kept] {
                *flags = ir::MemFlags::trusted().with_alias_region(flags.alias_region());
            }
            let shared = func.dfg.first_result(kept);
            for &(_, inst) in &loads[1..] {
                let result = func.dfg.first_result(inst);
                func.layout.remove_inst(inst);
                func.dfg.clear_results(inst);
                func.dfg.change_to_alias(result, shared);
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

/// A `brif` whose condition folds to a constant (a drop flag of a local
/// that only a cold branch sets, so it is 0 on every other path) becomes a
/// `jump`: Cranelift 0.125 lowers it as a compare of a materialized zero.
pub fn fold_constant_branches(func: &mut ir::Function) {
    for (_, inst) in insts(func) {
        let InstructionData::Brif { arg, blocks, .. } = func.dfg.insts[inst] else {
            continue;
        };
        let Some(value) = constant(func, arg, 0) else {
            continue;
        };
        let pool = &func.dfg.value_lists;
        let call = blocks[usize::from(value == 0)];
        let target = call.block(pool);
        let args: Vec<BlockArg> = call.args(pool).collect();
        func.dfg.replace(inst).jump(target, &args);
    }
}

/// Move a slot's initial stores of constants out of the entry block into
/// the nearest block that dominates every other use of the slot, when that
/// block is in no cycle (so it runs at most once, like the entry).
///
/// The lowering zeroes every local's slot at function entry. A sequence
/// local or an inlined callee's aggregate that only a cold branch uses
/// (kinetic's `fastmath_sin` cache, #1404 gap 8) then costs every call the
/// stores, and the address it keeps live costs a register.
pub fn sink_slot_inits(func: &mut ir::Function) {
    let Some(entry) = func.layout.entry_block() else {
        return;
    };
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut rank: HashMap<Block, usize> = HashMap::new();
    for (n, &block) in domtree.cfg_postorder().iter().rev().enumerate() {
        rank.insert(block, n);
    }
    let common = |mut a: Block, mut b: Block| {
        while a != b {
            if rank[&a] > rank[&b] {
                a = domtree.idom(a).expect("reachable block");
            } else {
                b = domtree.idom(b).expect("reachable block");
            }
        }
        a
    };
    let all = insts(func);
    // Stack addresses of each slot.
    let mut addresses: HashMap<Value, StackSlot> = HashMap::new();
    let mut mixed: HashSet<StackSlot> = HashSet::new();
    for &(_, inst) in &all {
        if let InstructionData::StackLoad {
            opcode: Opcode::StackAddr,
            stack_slot,
            ..
        } = func.dfg.insts[inst]
        {
            addresses.insert(func.dfg.first_result(inst), stack_slot);
        }
    }
    // A block parameter that receives a slot's address on some edge is
    // that slot's address too: its uses count as the slot's.
    loop {
        let mut grew = false;
        for &(_, inst) in &all {
            let data = &func.dfg.insts[inst];
            if !data.opcode().is_branch() {
                continue;
            }
            let pool = &func.dfg.value_lists;
            for call in data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables) {
                let params = func.dfg.block_params(call.block(pool));
                for (a, &p) in call.args(pool).zip(params) {
                    if let BlockArg::Value(v) = a
                        && let Some(&slot) = addresses.get(&func.dfg.resolve_aliases(v))
                    {
                        match addresses.get(&p) {
                            Some(&other) if other != slot => {
                                // Two slots meet in one parameter: keep
                                // both out of the analysis.
                                mixed.insert(slot);
                                mixed.insert(other);
                            }
                            Some(_) => {}
                            None => {
                                addresses.insert(p, slot);
                                grew = true;
                            }
                        }
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    struct Uses {
        inits: Vec<Inst>,
        blocks: Vec<Block>,
        unsafe_use: bool,
    }
    let mut slots: BTreeMap<StackSlot, Uses> = BTreeMap::new();
    for &(block, inst) in &all {
        let data = &func.dfg.insts[inst];
        if let InstructionData::StackLoad { stack_slot, .. }
        | InstructionData::StackStore { stack_slot, .. } = *data
            && data.opcode() != Opcode::StackAddr
        {
            slots
                .entry(stack_slot)
                .or_insert(Uses {
                    inits: vec![],
                    blocks: vec![],
                    unsafe_use: false,
                })
                .unsafe_use = true;
            continue;
        }
        let mut seen: Vec<StackSlot> = vec![];
        for v in func.dfg.inst_values(inst) {
            if let Some(&slot) = addresses.get(&func.dfg.resolve_aliases(v)) {
                seen.push(slot);
            }
        }
        if seen.is_empty() {
            continue;
        }
        let init = block == entry
            && match *data {
                InstructionData::Store { args, .. } => {
                    addresses.contains_key(&func.dfg.resolve_aliases(args[1]))
                        && !addresses.contains_key(&func.dfg.resolve_aliases(args[0]))
                        && constant(func, args[0], 0).is_some()
                }
                _ => false,
            };
        for slot in seen {
            let uses = slots.entry(slot).or_insert(Uses {
                inits: vec![],
                blocks: vec![],
                unsafe_use: false,
            });
            if init {
                uses.inits.push(inst);
            } else {
                uses.blocks.push(block);
            }
        }
    }
    for (slot, uses) in slots {
        if uses.unsafe_use
            || mixed.contains(&slot)
            || uses.inits.is_empty()
            || uses.blocks.is_empty()
        {
            continue;
        }
        if uses.blocks.iter().any(|b| !rank.contains_key(b)) {
            continue;
        }
        let target = uses.blocks.iter().copied().reduce(&common).unwrap();
        if target == entry || in_cycle(func, target) {
            continue;
        }
        let Some(first) = func.layout.first_inst(target) else {
            continue;
        };
        for inst in uses.inits {
            func.layout.remove_inst(inst);
            func.layout.insert_inst(inst, first);
        }
    }
}

/// Whether `block` can reach itself.
fn in_cycle(func: &ir::Function, block: Block) -> bool {
    let successors = |b: Block| -> Vec<Block> {
        let Some(last) = func.layout.last_inst(b) else {
            return vec![];
        };
        let pool = &func.dfg.value_lists;
        func.dfg.insts[last]
            .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
            .iter()
            .map(|c| c.block(pool))
            .collect()
    };
    let mut seen: HashSet<Block> = HashSet::new();
    let mut work = successors(block);
    while let Some(b) = work.pop() {
        if b == block {
            return true;
        }
        if seen.insert(b) {
            work.extend(successors(b));
        }
    }
    false
}

/// A `bitcast` of an integer constant to `f64` or `f32` (Vesper's
/// `from_bits_f64(K)`, how ported math code spells its coefficients)
/// becomes `f64const` / `f32const`. Cranelift 0.125 does not fold it: it
/// builds each coefficient with `movabs` and `movq` through a general
/// register, where a float constant is one memory operand of the
/// instruction that uses it. Commutes `fadd(C, x)` / `fmul(C, x)` into
/// `(x, C)` for non-NaN constants so Cranelift's x86-64 backend folds `C`
/// as the memory operand instead of loading it with `movabs + movq`.
pub fn float_constants(func: &mut ir::Function) {
    use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
    for (_, inst) in insts(func) {
        let InstructionData::LoadNoOffset {
            opcode: Opcode::Bitcast,
            arg,
            ..
        } = func.dfg.insts[inst]
        else {
            continue;
        };
        let ty = func.dfg.value_type(func.dfg.first_result(inst));
        if ty != types::F64 && ty != types::F32 {
            continue;
        }
        let Some(bits) = constant(func, arg, 0) else {
            continue;
        };
        if ty == types::F64 {
            func.dfg
                .replace(inst)
                .f64const(Ieee64::with_bits(bits as u64));
        } else {
            func.dfg
                .replace(inst)
                .f32const(Ieee32::with_bits(bits as u32));
        }
    }
    let non_nan_float_const = |func: &ir::Function, v: ir::Value| -> bool {
        let ir::ValueDef::Result(def, 0) = func.dfg.value_def(func.dfg.resolve_aliases(v)) else {
            return false;
        };
        match func.dfg.insts[def] {
            InstructionData::UnaryIeee64 {
                opcode: Opcode::F64const,
                imm,
            } => !f64::from_bits(imm.bits()).is_nan(),
            InstructionData::UnaryIeee32 {
                opcode: Opcode::F32const,
                imm,
            } => !f32::from_bits(imm.bits()).is_nan(),
            _ => false,
        }
    };
    for (_, inst) in insts(func) {
        if let InstructionData::Binary {
            opcode: opcode @ (Opcode::Fadd | Opcode::Fmul),
            args: [lhs, rhs],
        } = func.dfg.insts[inst]
            && non_nan_float_const(func, lhs)
            && !non_nan_float_const(func, rhs)
        {
            func.dfg.insts[inst] = InstructionData::Binary {
                opcode,
                args: [rhs, lhs],
            };
        }
    }
}

/// Fold `ireduce`/`uextend` constant chains and `ireduce(uextend(x))`
/// round-trips emitted by narrow-integer lowering.
fn fold_narrow_extends(func: &mut ir::Function) {
    let mask_for = |ty: ir::Type| -> Option<u64> {
        match ty {
            types::I8 => Some(0xff),
            types::I16 => Some(0xffff),
            types::I32 => Some(0xffff_ffff),
            types::I64 => Some(u64::MAX),
            _ => None,
        }
    };
    let narrow_operand =
        |func: &ir::Function, v: Value, dst_ty: ir::Type| -> Option<(Option<Value>, Option<i64>)> {
            let v = func.dfg.resolve_aliases(v);
            let ir::ValueDef::Result(d, 0) = func.dfg.value_def(v) else {
                return None;
            };
            match func.dfg.insts[d] {
                InstructionData::Unary {
                    opcode: Opcode::Uextend | Opcode::Sextend,
                    arg,
                } => {
                    let inner = func.dfg.resolve_aliases(arg);
                    (func.dfg.value_type(inner) == dst_ty).then_some((Some(inner), None))
                }
                InstructionData::UnaryImm {
                    opcode: Opcode::Iconst,
                    imm,
                } => Some((None, Some(imm.bits()))),
                _ => None,
            }
        };
    for _ in 0..10 {
        let mut changed = false;
        for (_, inst) in insts(func) {
            let InstructionData::Unary { opcode, arg } = func.dfg.insts[inst] else {
                continue;
            };
            if !matches!(opcode, Opcode::Ireduce | Opcode::Uextend | Opcode::Sextend) {
                continue;
            }
            let dest = func.dfg.first_result(inst);
            let dst_ty = func.dfg.value_type(dest);
            let Some(dst_mask) = mask_for(dst_ty) else {
                continue;
            };
            let src = func.dfg.resolve_aliases(arg);
            let src_ty = func.dfg.value_type(src);
            let Some(src_mask) = mask_for(src_ty) else {
                continue;
            };
            let ir::ValueDef::Result(def, 0) = func.dfg.value_def(src) else {
                continue;
            };
            match (opcode, func.dfg.insts[def]) {
                (
                    Opcode::Ireduce,
                    InstructionData::UnaryImm {
                        opcode: Opcode::Iconst,
                        imm,
                    },
                ) => {
                    let v = (imm.bits() as u64) & dst_mask;
                    func.dfg.replace(inst).iconst(dst_ty, v as i64);
                    changed = true;
                }
                (
                    Opcode::Uextend,
                    InstructionData::UnaryImm {
                        opcode: Opcode::Iconst,
                        imm,
                    },
                ) => {
                    let v = (imm.bits() as u64) & src_mask;
                    func.dfg.replace(inst).iconst(dst_ty, v as i64);
                    changed = true;
                }
                (
                    Opcode::Ireduce,
                    InstructionData::Unary {
                        opcode: Opcode::Uextend,
                        arg: inner,
                    },
                ) => {
                    let inner = func.dfg.resolve_aliases(inner);
                    let inner_ty = func.dfg.value_type(inner);
                    if inner_ty == dst_ty {
                        func.layout.remove_inst(inst);
                        func.dfg.clear_results(inst);
                        func.dfg.change_to_alias(dest, inner);
                        changed = true;
                    } else if inner_ty.bits() < dst_ty.bits() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Uextend,
                            arg: inner,
                        };
                        changed = true;
                    }
                }
                (
                    Opcode::Sextend,
                    InstructionData::Unary {
                        opcode: Opcode::Uextend,
                        arg: inner,
                    },
                ) => {
                    let inner = func.dfg.resolve_aliases(inner);
                    if func.dfg.value_type(inner).bits() < src_ty.bits() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Uextend,
                            arg: inner,
                        };
                        changed = true;
                    }
                }
                (
                    Opcode::Sextend,
                    InstructionData::UnaryImm {
                        opcode: Opcode::Iconst,
                        imm,
                    },
                ) => {
                    let shift = 64 - src_ty.bits();
                    let v = (((imm.bits() << shift) >> shift) as u64) & dst_mask;
                    func.dfg.replace(inst).iconst(dst_ty, v as i64);
                    changed = true;
                }
                (
                    Opcode::Ireduce,
                    InstructionData::Unary {
                        opcode: Opcode::Sextend,
                        arg: inner,
                    },
                ) => {
                    let inner = func.dfg.resolve_aliases(inner);
                    let inner_ty = func.dfg.value_type(inner);
                    if inner_ty == dst_ty {
                        func.layout.remove_inst(inst);
                        func.dfg.clear_results(inst);
                        func.dfg.change_to_alias(dest, inner);
                        changed = true;
                    } else if inner_ty.bits() < dst_ty.bits() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Sextend,
                            arg: inner,
                        };
                        changed = true;
                    } else if mask_for(inner_ty).is_some() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Ireduce,
                            arg: inner,
                        };
                        changed = true;
                    }
                }
                (
                    Opcode::Ireduce,
                    InstructionData::Binary {
                        opcode: bin_op @ (Opcode::Ushr | Opcode::Sshr),
                        args: [a, b],
                    },
                ) => {
                    let a_inner = match func.dfg.value_def(func.dfg.resolve_aliases(a)) {
                        ir::ValueDef::Result(d, 0) => match (bin_op, func.dfg.insts[d]) {
                            (
                                Opcode::Ushr,
                                InstructionData::Unary {
                                    opcode: Opcode::Uextend,
                                    arg,
                                },
                            )
                            | (
                                Opcode::Sshr,
                                InstructionData::Unary {
                                    opcode: Opcode::Sextend,
                                    arg,
                                },
                            ) => {
                                let inner = func.dfg.resolve_aliases(arg);
                                (func.dfg.value_type(inner) == dst_ty).then_some(inner)
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(va) = a_inner
                        && let Some((None, Some(s))) = narrow_operand(func, b, dst_ty)
                        && s >= 0
                        && s < i64::from(dst_ty.bits())
                    {
                        use cranelift_codegen::cursor::{Cursor, FuncCursor};
                        use cranelift_codegen::ir::InstBuilder;
                        let vb = {
                            let mut pos = FuncCursor::new(func).at_inst(inst);
                            pos.ins().iconst(types::I32, s)
                        };
                        if bin_op == Opcode::Ushr {
                            func.dfg.replace(inst).ushr(va, vb);
                        } else {
                            func.dfg.replace(inst).sshr(va, vb);
                        }
                        changed = true;
                    }
                }
                (
                    Opcode::Ireduce,
                    InstructionData::Binary {
                        opcode:
                            bin_op @ (Opcode::Band
                            | Opcode::Bor
                            | Opcode::Bxor
                            | Opcode::Iadd
                            | Opcode::Isub
                            | Opcode::Imul
                            | Opcode::Ishl),
                        args: [a, b],
                    },
                ) => {
                    if let Some(na) = narrow_operand(func, a, dst_ty)
                        && let Some(nb) = narrow_operand(func, b, dst_ty)
                        && (bin_op != Opcode::Ishl
                            || matches!(nb, (None, Some(s)) if s >= 0 && s < i64::from(dst_ty.bits())))
                    {
                        use cranelift_codegen::cursor::{Cursor, FuncCursor};
                        use cranelift_codegen::ir::InstBuilder;
                        let materialize = |pos: &mut FuncCursor<'_>,
                                           op: (Option<Value>, Option<i64>),
                                           ty: ir::Type| {
                            match op {
                                (Some(v), _) => v,
                                (None, Some(c)) => {
                                    let m = mask_for(ty).unwrap_or(u64::MAX);
                                    pos.ins().iconst(ty, ((c as u64) & m) as i64)
                                }
                                _ => unreachable!(),
                            }
                        };
                        let (va, vb) = {
                            let mut pos = FuncCursor::new(func).at_inst(inst);
                            let va = materialize(&mut pos, na, dst_ty);
                            let vb = materialize(
                                &mut pos,
                                nb,
                                if bin_op == Opcode::Ishl {
                                    types::I32
                                } else {
                                    dst_ty
                                },
                            );
                            (va, vb)
                        };
                        match bin_op {
                            Opcode::Band => {
                                func.dfg.replace(inst).band(va, vb);
                            }
                            Opcode::Bor => {
                                func.dfg.replace(inst).bor(va, vb);
                            }
                            Opcode::Bxor => {
                                func.dfg.replace(inst).bxor(va, vb);
                            }
                            Opcode::Iadd => {
                                func.dfg.replace(inst).iadd(va, vb);
                            }
                            Opcode::Isub => {
                                func.dfg.replace(inst).isub(va, vb);
                            }
                            Opcode::Imul => {
                                func.dfg.replace(inst).imul(va, vb);
                            }
                            Opcode::Ishl => {
                                func.dfg.replace(inst).ishl(va, vb);
                            }
                            _ => unreachable!(),
                        }
                        changed = true;
                    }
                }
                (
                    Opcode::Sextend,
                    InstructionData::Unary {
                        opcode: Opcode::Sextend,
                        arg: inner,
                    },
                ) => {
                    let inner = func.dfg.resolve_aliases(inner);
                    if mask_for(func.dfg.value_type(inner)).is_some() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Sextend,
                            arg: inner,
                        };
                        changed = true;
                    }
                }
                (
                    Opcode::Uextend,
                    InstructionData::Unary {
                        opcode: Opcode::Uextend,
                        arg: inner,
                    },
                ) => {
                    let inner = func.dfg.resolve_aliases(inner);
                    if mask_for(func.dfg.value_type(inner)).is_some() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Uextend,
                            arg: inner,
                        };
                        changed = true;
                    }
                }
                (
                    Opcode::Ireduce,
                    InstructionData::Unary {
                        opcode: Opcode::Ireduce,
                        arg: inner,
                    },
                ) => {
                    let inner = func.dfg.resolve_aliases(inner);
                    if mask_for(func.dfg.value_type(inner)).is_some() {
                        func.dfg.insts[inst] = InstructionData::Unary {
                            opcode: Opcode::Ireduce,
                            arg: inner,
                        };
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if !changed {
            break;
        }
    }
}

fn eval_icmp(cc: ir::condcodes::IntCC, ty: Type, a: i64, b: i64) -> Option<bool> {
    use cranelift_codegen::ir::condcodes::IntCC;
    let bits = ty.bits();
    if !matches!(bits, 8 | 16 | 32 | 64) {
        return None;
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let ua = (a as u64) & mask;
    let ub = (b as u64) & mask;
    let shift = 64 - bits;
    let sa = ((ua << shift) as i64) >> shift;
    let sb = ((ub << shift) as i64) >> shift;
    Some(match cc {
        IntCC::Equal => ua == ub,
        IntCC::NotEqual => ua != ub,
        IntCC::UnsignedLessThan => ua < ub,
        IntCC::UnsignedLessThanOrEqual => ua <= ub,
        IntCC::UnsignedGreaterThan => ua > ub,
        IntCC::UnsignedGreaterThanOrEqual => ua >= ub,
        IntCC::SignedLessThan => sa < sb,
        IntCC::SignedLessThanOrEqual => sa <= sb,
        IntCC::SignedGreaterThan => sa > sb,
        IntCC::SignedGreaterThanOrEqual => sa >= sb,
    })
}

fn eval_binary_int(op: Opcode, ty: Type, a: i64, b: i64) -> Option<i64> {
    let bits = ty.bits();
    if !matches!(bits, 8 | 16 | 32 | 64) {
        return None;
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let ua = (a as u64) & mask;
    let ub = (b as u64) & mask;
    let shift = 64 - bits;
    let sa = ((ua << shift) as i64) >> shift;
    let shift_amt = (b as u32) & (bits - 1);
    let res = match op {
        Opcode::Iadd => ua.wrapping_add(ub) & mask,
        Opcode::Isub => ua.wrapping_sub(ub) & mask,
        Opcode::Imul => ua.wrapping_mul(ub) & mask,
        Opcode::Band => ua & ub,
        Opcode::Bor => ua | ub,
        Opcode::Bxor => ua ^ ub,
        Opcode::Ishl => ua.wrapping_shl(shift_amt) & mask,
        Opcode::Ushr => ua.wrapping_shr(shift_amt) & mask,
        Opcode::Sshr => (sa.wrapping_shr(shift_amt) as u64) & mask,
        _ => return None,
    };
    Some(res as i64)
}

fn is_bool_i8(func: &ir::Function, v: Value, depth: u32) -> bool {
    if depth > 4 {
        return false;
    }
    let v = func.dfg.resolve_aliases(v);
    if func.dfg.value_type(v) != types::I8 {
        return false;
    }
    let ir::ValueDef::Result(def, 0) = func.dfg.value_def(v) else {
        return false;
    };
    match func.dfg.insts[def] {
        InstructionData::IntCompare { .. } | InstructionData::IntCompareImm { .. } => true,
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => matches!(imm.bits(), 0 | 1),
        InstructionData::Binary {
            opcode: Opcode::Band | Opcode::Bor | Opcode::Bxor,
            args: [a, b],
        } => is_bool_i8(func, a, depth + 1) && is_bool_i8(func, b, depth + 1),
        _ => false,
    }
}

/// Fold identity/constant/same-operand integer ops, `select`, and
/// zero-comparisons or branches on `uextend`/`sextend` of narrower boolean values.
fn fold_trivial_arith_and_bool_extends(func: &mut ir::Function) {
    let unwrap_ext = |func: &ir::Function, v: Value| -> Option<(ir::Inst, Value)> {
        let v = func.dfg.resolve_aliases(v);
        let ir::ValueDef::Result(def, 0) = func.dfg.value_def(v) else {
            return None;
        };
        if let InstructionData::Unary {
            opcode: Opcode::Uextend | Opcode::Sextend,
            arg,
        } = func.dfg.insts[def]
        {
            Some((def, func.dfg.resolve_aliases(arg)))
        } else {
            None
        }
    };
    for _ in 0..3 {
        let mut changed = false;
        for (_, inst) in insts(func) {
            match func.dfg.insts[inst] {
                InstructionData::Unary {
                    opcode: op @ (Opcode::Bnot | Opcode::Ineg),
                    arg,
                } => {
                    let arg = func.dfg.resolve_aliases(arg);
                    let dest = func.dfg.first_result(inst);
                    let ty = func.dfg.value_type(dest);
                    if !matches!(ty, types::I8 | types::I16 | types::I32 | types::I64) {
                        continue;
                    }
                    let mask = if ty.bits() == 64 {
                        u64::MAX
                    } else {
                        (1u64 << ty.bits()) - 1
                    };
                    if let Some(a) = constant(func, arg, 1) {
                        let ua = (a as u64) & mask;
                        let res = if op == Opcode::Bnot {
                            (!ua) & mask
                        } else {
                            ua.wrapping_neg() & mask
                        };
                        func.dfg.replace(inst).iconst(ty, res as i64);
                        changed = true;
                    } else if let ir::ValueDef::Result(def, 0) = func.dfg.value_def(arg)
                        && let InstructionData::Unary {
                            opcode: inner_op,
                            arg: inner,
                        } = func.dfg.insts[def]
                        && inner_op == op
                    {
                        let inner = func.dfg.resolve_aliases(inner);
                        func.layout.remove_inst(inst);
                        func.dfg.clear_results(inst);
                        func.dfg.change_to_alias(dest, inner);
                        changed = true;
                    }
                }
                InstructionData::BinaryImm64 {
                    opcode: imm_op,
                    arg,
                    imm,
                } if matches!(
                    imm_op,
                    Opcode::IaddImm
                        | Opcode::ImulImm
                        | Opcode::BandImm
                        | Opcode::BorImm
                        | Opcode::BxorImm
                        | Opcode::IshlImm
                        | Opcode::UshrImm
                        | Opcode::SshrImm
                ) =>
                {
                    let bin_op = match imm_op {
                        Opcode::IaddImm => Opcode::Iadd,
                        Opcode::ImulImm => Opcode::Imul,
                        Opcode::BandImm => Opcode::Band,
                        Opcode::BorImm => Opcode::Bor,
                        Opcode::BxorImm => Opcode::Bxor,
                        Opcode::IshlImm => Opcode::Ishl,
                        Opcode::UshrImm => Opcode::Ushr,
                        Opcode::SshrImm => Opcode::Sshr,
                        _ => unreachable!(),
                    };
                    let arg = func.dfg.resolve_aliases(arg);
                    let dest = func.dfg.first_result(inst);
                    let ty = func.dfg.value_type(dest);
                    if !matches!(ty, types::I8 | types::I16 | types::I32 | types::I64) {
                        continue;
                    }
                    if let Some(a) = constant(func, arg, 1)
                        && let Some(folded) = eval_binary_int(bin_op, ty, a, imm.bits())
                    {
                        func.dfg.replace(inst).iconst(ty, folded);
                        changed = true;
                        continue;
                    }
                    let mask = if ty.bits() == 64 {
                        u64::MAX
                    } else {
                        (1u64 << ty.bits()) - 1
                    };
                    let all_ones = mask as i64;
                    let imm_bits = ((imm.bits() as u64) & mask) as i64;
                    let is_identity = match imm_op {
                        Opcode::IaddImm
                        | Opcode::BorImm
                        | Opcode::BxorImm
                        | Opcode::IshlImm
                        | Opcode::UshrImm
                        | Opcode::SshrImm => imm_bits == 0,
                        Opcode::ImulImm => imm_bits == 1,
                        Opcode::BandImm => imm_bits == all_ones,
                        _ => false,
                    };
                    if is_identity {
                        func.layout.remove_inst(inst);
                        func.dfg.clear_results(inst);
                        func.dfg.change_to_alias(dest, arg);
                        changed = true;
                    } else if matches!(imm_op, Opcode::ImulImm | Opcode::BandImm) && imm_bits == 0 {
                        func.dfg.replace(inst).iconst(ty, 0);
                        changed = true;
                    } else if imm_op == Opcode::BorImm && imm_bits == all_ones {
                        func.dfg.replace(inst).iconst(ty, all_ones);
                        changed = true;
                    }
                }
                InstructionData::Binary {
                    opcode: op,
                    args: [lhs, rhs],
                } if matches!(
                    op,
                    Opcode::Iadd
                        | Opcode::Isub
                        | Opcode::Imul
                        | Opcode::Band
                        | Opcode::Bor
                        | Opcode::Bxor
                        | Opcode::Ishl
                        | Opcode::Ushr
                        | Opcode::Sshr
                ) =>
                {
                    let lhs = func.dfg.resolve_aliases(lhs);
                    let rhs = func.dfg.resolve_aliases(rhs);
                    let dest = func.dfg.first_result(inst);
                    let ty = func.dfg.value_type(dest);
                    if !matches!(ty, types::I8 | types::I16 | types::I32 | types::I64) {
                        continue;
                    }
                    let lc = constant(func, lhs, 1);
                    let rc = constant(func, rhs, 1);
                    if let (Some(a), Some(b)) = (lc, rc)
                        && let Some(folded) = eval_binary_int(op, ty, a, b)
                    {
                        func.dfg.replace(inst).iconst(ty, folded);
                        changed = true;
                        continue;
                    }
                    let all_ones = if ty.bits() == 64 {
                        -1
                    } else {
                        (1i64 << ty.bits()) - 1
                    };
                    let alias_to = match op {
                        Opcode::Iadd | Opcode::Bor | Opcode::Bxor => {
                            if rc == Some(0) {
                                Some(lhs)
                            } else if lc == Some(0) {
                                Some(rhs)
                            } else if lhs == rhs && op == Opcode::Bor {
                                Some(lhs)
                            } else {
                                None
                            }
                        }
                        Opcode::Isub | Opcode::Ishl | Opcode::Ushr | Opcode::Sshr => {
                            if rc == Some(0) { Some(lhs) } else { None }
                        }
                        Opcode::Imul => {
                            if rc == Some(1) {
                                Some(lhs)
                            } else if lc == Some(1) {
                                Some(rhs)
                            } else {
                                None
                            }
                        }
                        Opcode::Band => {
                            if rc == Some(all_ones)
                                || lhs == rhs
                                || (ty == types::I8 && rc == Some(1) && is_bool_i8(func, lhs, 0))
                            {
                                Some(lhs)
                            } else if lc == Some(all_ones)
                                || (ty == types::I8 && lc == Some(1) && is_bool_i8(func, rhs, 0))
                            {
                                Some(rhs)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(target) = alias_to {
                        func.layout.remove_inst(inst);
                        func.dfg.clear_results(inst);
                        func.dfg.change_to_alias(dest, target);
                        changed = true;
                    } else if ((op == Opcode::Imul || op == Opcode::Band)
                        && (lc == Some(0) || rc == Some(0)))
                        || (lhs == rhs && matches!(op, Opcode::Isub | Opcode::Bxor))
                    {
                        func.dfg.replace(inst).iconst(ty, 0);
                        changed = true;
                    } else if op == Opcode::Bor && (lc == Some(all_ones) || rc == Some(all_ones)) {
                        func.dfg.replace(inst).iconst(ty, all_ones);
                        changed = true;
                    }
                }
                InstructionData::Ternary {
                    opcode: Opcode::Select,
                    args: [cond, t, f],
                } => {
                    let cond = func.dfg.resolve_aliases(cond);
                    let t = func.dfg.resolve_aliases(t);
                    let f = func.dfg.resolve_aliases(f);
                    let dest = func.dfg.first_result(inst);
                    let cond_is_cmp = matches!(
                        func.dfg.value_def(cond),
                        ir::ValueDef::Result(def, 0)
                            if matches!(
                                func.dfg.insts[def],
                                InstructionData::IntCompare { .. }
                                    | InstructionData::IntCompareImm { .. }
                            )
                    );
                    let tc = constant(func, t, 1);
                    let fc = constant(func, f, 1);
                    let dest_ty = func.dfg.value_type(dest);
                    let target = if t == f {
                        Some(t)
                    } else if let Some(c) = constant(func, cond, 1) {
                        Some(if c != 0 { t } else { f })
                    } else if cond_is_cmp && dest_ty == types::I8 && tc == Some(1) && fc == Some(0)
                    {
                        Some(cond)
                    } else {
                        None
                    };
                    if let Some(target) = target {
                        func.layout.remove_inst(inst);
                        func.dfg.clear_results(inst);
                        func.dfg.change_to_alias(dest, target);
                        changed = true;
                    } else if cond_is_cmp
                        && matches!(dest_ty, types::I16 | types::I32 | types::I64)
                        && tc == Some(1)
                        && fc == Some(0)
                    {
                        func.dfg.replace(inst).uextend(dest_ty, cond);
                        changed = true;
                    } else if cond_is_cmp && dest_ty == types::I8 && tc == Some(0) && fc == Some(1)
                    {
                        func.dfg
                            .replace(inst)
                            .icmp_imm(ir::condcodes::IntCC::Equal, cond, 0);
                        changed = true;
                    }
                }
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond,
                    args: [lhs, rhs],
                } => {
                    let lhs = func.dfg.resolve_aliases(lhs);
                    let rhs = func.dfg.resolve_aliases(rhs);
                    let ty = func.dfg.value_type(lhs);
                    let res = if lhs == rhs {
                        eval_icmp(cond, ty, 0, 0)
                    } else if let (Some(a), Some(b)) =
                        (constant(func, lhs, 1), constant(func, rhs, 1))
                    {
                        eval_icmp(cond, ty, a, b)
                    } else {
                        None
                    };
                    if let Some(res) = res {
                        func.dfg.replace(inst).iconst(types::I8, i64::from(res));
                        changed = true;
                    }
                }
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    cond,
                    arg,
                    imm,
                } => {
                    let arg = func.dfg.resolve_aliases(arg);
                    let ty = func.dfg.value_type(arg);
                    if let Some(a) = constant(func, arg, 1)
                        && let Some(res) = eval_icmp(cond, ty, a, imm.bits())
                    {
                        func.dfg.replace(inst).iconst(types::I8, i64::from(res));
                        changed = true;
                        continue;
                    }
                    if imm.bits() == 0
                        && matches!(
                            cond,
                            ir::condcodes::IntCC::Equal | ir::condcodes::IntCC::NotEqual
                        )
                        && let Some((_, inner)) = unwrap_ext(func, arg)
                    {
                        let inner_is_cmp = matches!(
                            func.dfg.value_def(inner),
                            ir::ValueDef::Result(def, 0)
                                if matches!(
                                    func.dfg.insts[def],
                                    InstructionData::IntCompare { .. }
                                        | InstructionData::IntCompareImm { .. }
                                )
                        );
                        let dest = func.dfg.first_result(inst);
                        if cond == ir::condcodes::IntCC::NotEqual
                            && inner_is_cmp
                            && func.dfg.value_type(inner) == func.dfg.value_type(dest)
                        {
                            func.layout.remove_inst(inst);
                            func.dfg.clear_results(inst);
                            func.dfg.change_to_alias(dest, inner);
                        } else {
                            func.dfg.insts[inst] = InstructionData::IntCompareImm {
                                opcode: Opcode::IcmpImm,
                                cond,
                                arg: inner,
                                imm,
                            };
                        }
                        changed = true;
                    }
                }
                InstructionData::Brif {
                    opcode: br_op,
                    arg,
                    blocks,
                } => {
                    if let Some((_, inner)) = unwrap_ext(func, arg) {
                        func.dfg.insts[inst] = InstructionData::Brif {
                            opcode: br_op,
                            arg: inner,
                            blocks,
                        };
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if !changed {
            break;
        }
    }
}

fn value_is_loop_invariant(
    func: &ir::Function,
    domtree: &DominatorTree,
    loops: &LoopAnalysis,
    lp: Loop,
    preheader: Block,
    v: Value,
) -> bool {
    let v = func.dfg.resolve_aliases(v);
    let def_block = match func.dfg.value_def(v) {
        ir::ValueDef::Result(d, _) => func.layout.inst_block(d),
        ir::ValueDef::Param(pb, _) => Some(pb),
        ir::ValueDef::Union(_, _) => None,
    };
    def_block.is_some_and(|db| {
        let mut cur = loops.innermost_loop(db);
        while let Some(l) = cur {
            if l == lp {
                return false;
            }
            cur = loops.loop_parent(l);
        }
        domtree.dominates(db, preheader, &func.layout)
    })
}

/// Hoist pure loop-invariant instructions (and `notrap readonly can_move`
/// loads when the preheader ends with an unconditional `Jump`) into the loop
/// preheader, reassociating invariant offsets/scales out of address arithmetic.
fn hoist_loop_invariants(func: &mut ir::Function) {
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut loops = LoopAnalysis::new();
    loops.compute(func, &cfg, &domtree);

    let mut seen: HashSet<Loop> = HashSet::new();
    let mut ordered: Vec<(usize, Loop)> = vec![];
    for block in func.layout.blocks() {
        let mut cur = loops.innermost_loop(block);
        while let Some(lp) = cur {
            if seen.insert(lp) {
                let mut depth = 0usize;
                let mut p = loops.loop_parent(lp);
                while let Some(parent) = p {
                    depth += 1;
                    p = loops.loop_parent(parent);
                }
                ordered.push((depth, lp));
            }
            cur = loops.loop_parent(lp);
        }
    }
    ordered.sort_by_key(|a| std::cmp::Reverse(a.0));

    let in_loop = |b: Block, lp: Loop| -> bool {
        let mut cur = loops.innermost_loop(b);
        while let Some(l) = cur {
            if l == lp {
                return true;
            }
            cur = loops.loop_parent(l);
        }
        false
    };

    for (_, lp) in ordered {
        let header = loops.loop_header(lp);
        let mut outside_preds: Vec<Block> = cfg
            .pred_iter(header)
            .map(|p| p.block)
            .filter(|&p| !in_loop(p, lp))
            .collect();
        outside_preds.sort_unstable();
        outside_preds.dedup();
        let [preheader] = outside_preds[..] else {
            continue;
        };
        if !domtree.dominates(preheader, header, &func.layout) {
            continue;
        }
        let Some(term) = func.layout.last_inst(preheader) else {
            continue;
        };
        let is_jump_preheader = matches!(
            func.dfg.insts[term],
            InstructionData::Jump {
                opcode: Opcode::Jump,
                ..
            }
        );
        let is_brif_preheader = matches!(
            func.dfg.insts[term],
            InstructionData::Brif {
                opcode: Opcode::Brif,
                ..
            }
        );
        if !is_jump_preheader && !is_brif_preheader {
            continue;
        }

        let loop_blocks: Vec<Block> = func.layout.blocks().filter(|&b| in_loop(b, lp)).collect();

        for _ in 0..4 {
            let mut changed = false;
            for &b in &loop_blocks {
                let body: Vec<Inst> = func.layout.block_insts(b).collect();
                for inst in body {
                    if func.dfg.inst_results(inst).is_empty() {
                        continue;
                    }
                    let hoistable = match &func.dfg.insts[inst] {
                        InstructionData::Load {
                            opcode:
                                Opcode::Load
                                | Opcode::Uload8
                                | Opcode::Sload8
                                | Opcode::Uload16
                                | Opcode::Sload16
                                | Opcode::Uload32
                                | Opcode::Sload32,
                            flags,
                            ..
                        } => {
                            is_jump_preheader
                                && flags.notrap()
                                && flags.readonly()
                                && flags.can_move()
                        }
                        data => matches!(
                            data.opcode(),
                            Opcode::Iconst
                                | Opcode::F32const
                                | Opcode::F64const
                                | Opcode::Iadd
                                | Opcode::IaddImm
                                | Opcode::Isub
                                | Opcode::Imul
                                | Opcode::ImulImm
                                | Opcode::Ishl
                                | Opcode::Ushr
                                | Opcode::Sshr
                                | Opcode::Band
                                | Opcode::Bor
                                | Opcode::Bxor
                                | Opcode::Bnot
                                | Opcode::Ineg
                                | Opcode::Uextend
                                | Opcode::Sextend
                                | Opcode::Ireduce
                                | Opcode::Fadd
                                | Opcode::Fsub
                                | Opcode::Fmul
                                | Opcode::Fneg
                                | Opcode::Fabs
                                | Opcode::Bitcast
                                | Opcode::FcvtFromUint
                                | Opcode::FcvtFromSint
                                | Opcode::Icmp
                                | Opcode::IcmpImm
                                | Opcode::Fcmp
                                | Opcode::Select
                        ),
                    };
                    if !hoistable {
                        continue;
                    }
                    let all_outside = values_of(func, inst).into_iter().all(|v| {
                        let def_block = match func.dfg.value_def(v) {
                            ir::ValueDef::Result(d, _) => func.layout.inst_block(d),
                            ir::ValueDef::Param(pb, _) => Some(pb),
                            ir::ValueDef::Union(_, _) => None,
                        };
                        def_block.is_some_and(|db| {
                            !in_loop(db, lp) && domtree.dominates(db, preheader, &func.layout)
                        })
                    });
                    if !all_outside {
                        // Reassociate `iadd(inv1, iadd(inv2, var))` -> `iadd(iadd(inv1, inv2), var)`
                        // and distribute `imul_imm(iadd(inv, var), imm)` -> `iadd(imul_imm(inv, imm), imul_imm(var, imm))`
                        // so invariant base offsets and scaled strides hoist into `preheader`.
                        let term = func.layout.last_inst(preheader).unwrap();
                        let inv_pair = match func.dfg.insts[inst] {
                            InstructionData::Binary {
                                opcode: Opcode::Iadd,
                                args: [a, b],
                            } => {
                                let a = func.dfg.resolve_aliases(a);
                                let b = func.dfg.resolve_aliases(b);
                                [
                                    (
                                        a,
                                        b,
                                        value_is_loop_invariant(
                                            func, &domtree, &loops, lp, preheader, a,
                                        ),
                                    ),
                                    (
                                        b,
                                        a,
                                        value_is_loop_invariant(
                                            func, &domtree, &loops, lp, preheader, b,
                                        ),
                                    ),
                                ]
                                .into_iter()
                                .find(|(_, _, is_inv)| *is_inv)
                                .and_then(|(inv1, inner, _)| {
                                    let ir::ValueDef::Result(inner_inst, 0) =
                                        func.dfg.value_def(func.dfg.resolve_aliases(inner))
                                    else {
                                        return None;
                                    };
                                    match func.dfg.insts[inner_inst] {
                                        InstructionData::Binary {
                                            opcode: Opcode::Iadd,
                                            args: [c, d],
                                        } => {
                                            let c = func.dfg.resolve_aliases(c);
                                            let d = func.dfg.resolve_aliases(d);
                                            let c_inv = value_is_loop_invariant(
                                                func, &domtree, &loops, lp, preheader, c,
                                            );
                                            let d_inv = value_is_loop_invariant(
                                                func, &domtree, &loops, lp, preheader, d,
                                            );
                                            if c_inv && !d_inv {
                                                Some((inv1, Some(c), None, d))
                                            } else if d_inv && !c_inv {
                                                Some((inv1, Some(d), None, c))
                                            } else {
                                                None
                                            }
                                        }
                                        InstructionData::BinaryImm64 {
                                            opcode: Opcode::IaddImm,
                                            arg,
                                            imm,
                                        } if !value_is_loop_invariant(
                                            func, &domtree, &loops, lp, preheader, arg,
                                        ) =>
                                        {
                                            Some((
                                                inv1,
                                                None,
                                                Some(imm),
                                                func.dfg.resolve_aliases(arg),
                                            ))
                                        }
                                        _ => None,
                                    }
                                })
                            }
                            _ => None,
                        };
                        if let Some((inv1, inv2_opt, imm_opt, var)) = inv_pair {
                            use cranelift_codegen::cursor::{Cursor, FuncCursor};
                            use cranelift_codegen::ir::InstBuilder;
                            let new_base = {
                                let mut pos = FuncCursor::new(func).at_inst(term);
                                if let Some(inv2) = inv2_opt {
                                    if pos.func.dfg.value_type(inv1)
                                        == pos.func.dfg.value_type(inv2)
                                    {
                                        Some(pos.ins().iadd(inv1, inv2))
                                    } else {
                                        None
                                    }
                                } else {
                                    imm_opt.map(|imm| pos.ins().iadd_imm(inv1, imm))
                                }
                            };
                            if let Some(new_base) = new_base {
                                func.dfg.replace(inst).iadd(new_base, var);
                                changed = true;
                            }
                            continue;
                        }
                        if let InstructionData::BinaryImm64 {
                            opcode: Opcode::ImulImm,
                            arg,
                            imm,
                        } = func.dfg.insts[inst]
                        {
                            let arg = func.dfg.resolve_aliases(arg);
                            if !value_is_loop_invariant(func, &domtree, &loops, lp, preheader, arg)
                                && let ir::ValueDef::Result(inner_inst, 0) = func.dfg.value_def(arg)
                                && let InstructionData::Binary {
                                    opcode: Opcode::Iadd,
                                    args: [c, d],
                                } = func.dfg.insts[inner_inst]
                            {
                                let c = func.dfg.resolve_aliases(c);
                                let d = func.dfg.resolve_aliases(d);
                                let c_inv = value_is_loop_invariant(
                                    func, &domtree, &loops, lp, preheader, c,
                                );
                                let d_inv = value_is_loop_invariant(
                                    func, &domtree, &loops, lp, preheader, d,
                                );
                                let pair = if c_inv && !d_inv {
                                    Some((c, d))
                                } else if d_inv && !c_inv {
                                    Some((d, c))
                                } else {
                                    None
                                };
                                if let Some((inv, var)) = pair {
                                    use cranelift_codegen::cursor::{Cursor, FuncCursor};
                                    use cranelift_codegen::ir::InstBuilder;
                                    let inv_scaled = {
                                        let mut pos = FuncCursor::new(func).at_inst(term);
                                        pos.ins().imul_imm(inv, imm)
                                    };
                                    let var_scaled = {
                                        let mut pos = FuncCursor::new(func).at_inst(inst);
                                        pos.ins().imul_imm(var, imm)
                                    };
                                    func.dfg.replace(inst).iadd(inv_scaled, var_scaled);
                                    changed = true;
                                }
                            }
                        }
                        continue;
                    }
                    let term = func.layout.last_inst(preheader).unwrap();
                    func.layout.remove_inst(inst);
                    func.layout.insert_inst(inst, term);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }
}

/// Eliminate `iadd` unsigned-overflow checks (`icmp ult (iadd base, iv), base`)
/// inside `bounds_hoist`-versioned counted loops whose preheader guard already
/// proved `uadd_overflow(hi - 1, base) == 0`.
fn eliminate_guarded_loop_add_overflows(func: &mut ir::Function) {
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut loops = LoopAnalysis::new();
    loops.compute(func, &cfg, &domtree);

    let mut seen: HashSet<Loop> = HashSet::new();
    let mut all_loops: Vec<Loop> = vec![];
    for block in func.layout.blocks() {
        let mut cur = loops.innermost_loop(block);
        while let Some(lp) = cur {
            if seen.insert(lp) {
                all_loops.push(lp);
            }
            cur = loops.loop_parent(lp);
        }
    }

    let in_loop = |b: Block, lp: Loop| -> bool {
        let mut cur = loops.innermost_loop(b);
        while let Some(l) = cur {
            if l == lp {
                return true;
            }
            cur = loops.loop_parent(l);
        }
        false
    };

    let def_inst = |func: &ir::Function, v: Value| -> Option<Inst> {
        match func.dfg.value_def(func.dfg.resolve_aliases(v)) {
            ir::ValueDef::Result(inst, 0) => Some(inst),
            _ => None,
        }
    };

    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum BaseKey {
        Val(Value),
        AddConst(Value, i64),
    }

    let const_val = |func: &ir::Function, v: Value| -> Option<i64> {
        def_inst(func, v).and_then(|inst| match func.dfg.insts[inst] {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } => Some(imm.bits()),
            _ => None,
        })
    };

    let base_key = |func: &ir::Function, v: Value| -> BaseKey {
        let v = func.dfg.resolve_aliases(v);
        let Some(inst) = def_inst(func, v) else {
            return BaseKey::Val(v);
        };
        match func.dfg.insts[inst] {
            InstructionData::BinaryImm64 {
                opcode: Opcode::IaddImm,
                arg,
                imm,
            } => BaseKey::AddConst(func.dfg.resolve_aliases(arg), imm.bits()),
            InstructionData::Binary {
                opcode: Opcode::Iadd,
                args: [a, b],
            } => {
                if let Some(c) = const_val(func, b) {
                    BaseKey::AddConst(func.dfg.resolve_aliases(a), c)
                } else if let Some(c) = const_val(func, a) {
                    BaseKey::AddConst(func.dfg.resolve_aliases(b), c)
                } else {
                    BaseKey::Val(v)
                }
            }
            _ => BaseKey::Val(v),
        }
    };

    let is_hi_minus_one = |func: &ir::Function, v: Value, hi: Value| -> bool {
        if let (Some(vc), Some(hc)) = (const_val(func, v), const_val(func, hi))
            && (hc as u64).wrapping_sub(1) == (vc as u64)
        {
            return true;
        }
        let Some(inst) = def_inst(func, v) else {
            return false;
        };
        match func.dfg.insts[inst] {
            InstructionData::BinaryImm64 {
                opcode: Opcode::IaddImm,
                arg,
                imm,
            } => imm.bits() == -1 && func.dfg.resolve_aliases(arg) == hi,
            InstructionData::Binary {
                opcode: Opcode::Isub,
                args: [lhs, rhs],
            } => {
                func.dfg.resolve_aliases(lhs) == hi
                    && def_inst(func, rhs).is_some_and(|c| {
                        matches!(
                            func.dfg.insts[c],
                            InstructionData::UnaryImm {
                                opcode: Opcode::Iconst,
                                imm,
                            } if imm.bits() == 1
                        )
                    })
            }
            _ => false,
        }
    };

    let collect_ovf_flags =
        |func: &ir::Function,
         flag: Value,
         hi: Value,
         out: &mut HashSet<BaseKey>,
         proven_add_consts: &mut HashSet<(Value, i64)>| {
            let mut work = vec![func.dfg.resolve_aliases(flag)];
            let mut visited = HashSet::new();
            let mut ovf_insts = HashSet::new();
            while let Some(cur) = work.pop() {
                if !visited.insert(cur) {
                    continue;
                }
                match func.dfg.value_def(cur) {
                    ir::ValueDef::Result(inst, 0) => {
                        if let InstructionData::Binary {
                            opcode: Opcode::Bor,
                            args: [a, b],
                        } = func.dfg.insts[inst]
                        {
                            work.push(func.dfg.resolve_aliases(a));
                            work.push(func.dfg.resolve_aliases(b));
                        }
                    }
                    ir::ValueDef::Result(inst, 1) => {
                        if matches!(
                            func.dfg.insts[inst],
                            InstructionData::Binary {
                                opcode: Opcode::UaddOverflow,
                                ..
                            }
                        ) {
                            ovf_insts.insert(inst);
                        }
                    }
                    _ => {}
                }
            }
            let uadd_args = |v: Value| -> Option<(Value, Value)> {
                let inst = def_inst(func, v)?;
                if !ovf_insts.contains(&inst) {
                    return None;
                }
                match func.dfg.insts[inst] {
                    InstructionData::Binary {
                        opcode: Opcode::UaddOverflow,
                        args: [u, w],
                    } => Some((func.dfg.resolve_aliases(u), func.dfg.resolve_aliases(w))),
                    _ => None,
                }
            };
            let record_hi_pair =
                |other: Value,
                 out: &mut HashSet<BaseKey>,
                 proven_add_consts: &mut HashSet<(Value, i64)>| {
                    out.insert(BaseKey::Val(other));
                    out.insert(base_key(func, other));
                    if let Some((u, w)) = uadd_args(other) {
                        if let Some(c) = const_val(func, w) {
                            out.insert(BaseKey::AddConst(u, c));
                            proven_add_consts.insert((u, c));
                        }
                        if let Some(c) = const_val(func, u) {
                            out.insert(BaseKey::AddConst(w, c));
                            proven_add_consts.insert((w, c));
                        }
                    }
                };
            for &inst in &ovf_insts {
                let InstructionData::Binary {
                    opcode: Opcode::UaddOverflow,
                    args: [a, b],
                } = func.dfg.insts[inst]
                else {
                    continue;
                };
                let a = func.dfg.resolve_aliases(a);
                let b = func.dfg.resolve_aliases(b);
                if is_hi_minus_one(func, a, hi) {
                    record_hi_pair(b, out, proven_add_consts);
                }
                if is_hi_minus_one(func, b, hi) {
                    record_hi_pair(a, out, proven_add_consts);
                }
                for (sum_part, const_part) in [(a, b), (b, a)] {
                    if let Some(c) = const_val(func, const_part)
                        && let Some((u, w)) = uadd_args(sum_part)
                    {
                        if is_hi_minus_one(func, u, hi) {
                            out.insert(BaseKey::AddConst(w, c));
                            proven_add_consts.insert((w, c));
                        }
                        if is_hi_minus_one(func, w, hi) {
                            out.insert(BaseKey::AddConst(u, c));
                            proven_add_consts.insert((u, c));
                        }
                    }
                }
            }
        };

    for lp in all_loops {
        let header = loops.loop_header(lp);
        let mut outside_preds: Vec<Block> = cfg
            .pred_iter(header)
            .map(|p| p.block)
            .filter(|&p| !in_loop(p, lp))
            .collect();
        outside_preds.sort_unstable();
        outside_preds.dedup();
        let [preheader] = outside_preds[..] else {
            continue;
        };
        if !domtree.dominates(preheader, header, &func.layout) {
            continue;
        }
        let Some(pre_term) = func.layout.last_inst(preheader) else {
            continue;
        };
        let InstructionData::Brif {
            blocks: pre_blocks, ..
        } = func.dfg.insts[pre_term]
        else {
            continue;
        };
        let b0 = pre_blocks[0].block(&func.dfg.value_lists);
        let b1 = pre_blocks[1].block(&func.dfg.value_lists);
        if b0 != header || b1 == header {
            continue;
        }

        let Some(hdr_term) = func.layout.last_inst(header) else {
            continue;
        };
        let InstructionData::Brif {
            arg: hdr_cmp,
            blocks: hdr_blocks,
            ..
        } = func.dfg.insts[hdr_term]
        else {
            continue;
        };
        let body = hdr_blocks[0].block(&func.dfg.value_lists);
        let exit = hdr_blocks[1].block(&func.dfg.value_lists);
        if !in_loop(body, lp) || in_loop(exit, lp) {
            continue;
        }
        let Some(hdr_cmp_inst) = def_inst(func, hdr_cmp) else {
            continue;
        };
        let InstructionData::IntCompare {
            opcode: Opcode::Icmp,
            cond: ir::condcodes::IntCC::UnsignedLessThan,
            args: [iv, hi],
        } = func.dfg.insts[hdr_cmp_inst]
        else {
            continue;
        };
        let iv = func.dfg.resolve_aliases(iv);
        let hi = func.dfg.resolve_aliases(hi);
        let Some(iv_idx) = func.dfg.block_params(header).iter().position(|&p| p == iv) else {
            continue;
        };
        let Some(BlockArg::Value(lo)) = pre_blocks[0].args(&func.dfg.value_lists).nth(iv_idx)
        else {
            continue;
        };
        let lo = func.dfg.resolve_aliases(lo);

        let mut proven_bases: HashSet<BaseKey> = HashSet::new();
        let mut proven_add_consts: HashSet<(Value, i64)> = HashSet::new();
        // Walk up chained single-predecessor `bounds_hoist` guard blocks
        // (`guard_1 -> join_1 -> guard_2 -> ... -> fast_header`), resolving any
        // `Jump` block-param forwarding along the chain.
        let mut target = header;
        for _ in 0..16 {
            let mut preds: Vec<Block> = cfg
                .pred_iter(target)
                .map(|p| p.block)
                .filter(|&p| !in_loop(p, lp))
                .collect();
            preds.sort_unstable();
            preds.dedup();
            let [pred] = preds[..] else {
                break;
            };
            if !domtree.dominates(pred, header, &func.layout) {
                break;
            }
            let Some(pred_term) = func.layout.last_inst(pred) else {
                break;
            };
            let resolve_guard_val = |val: Value, next_target: &mut Block| -> Option<Value> {
                let p_val = func.dfg.resolve_aliases(val);
                let Some(p_idx) = func.dfg.block_params(pred).iter().position(|&p| p == p_val)
                else {
                    return Some(p_val);
                };
                let mut live_incoming = vec![];
                for edge in cfg.pred_iter(pred) {
                    let p = edge.block;
                    let Some(p_term) = func.layout.last_inst(p) else {
                        continue;
                    };
                    for call in func.dfg.insts[p_term]
                        .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                    {
                        if call.block(&func.dfg.value_lists) != pred {
                            continue;
                        }
                        if let Some(BlockArg::Value(v)) =
                            call.args(&func.dfg.value_lists).nth(p_idx)
                        {
                            let is_zero = def_inst(func, v).is_some_and(|vi| {
                                matches!(
                                    func.dfg.insts[vi],
                                    InstructionData::UnaryImm {
                                        opcode: Opcode::Iconst,
                                        imm,
                                    } if imm.bits() == 0
                                )
                            });
                            if !is_zero {
                                live_incoming.push((p, func.dfg.resolve_aliases(v)));
                            }
                        }
                    }
                }
                let [(only_pred, only_val)] = live_incoming[..] else {
                    return None;
                };
                *next_target = only_pred;
                Some(only_val)
            };

            let mut next_target = pred;
            let guard = match &func.dfg.insts[pred_term] {
                InstructionData::Brif {
                    arg,
                    blocks: pred_blocks,
                    ..
                } => {
                    let pb0 = pred_blocks[0].block(&func.dfg.value_lists);
                    let pb1 = pred_blocks[1].block(&func.dfg.value_lists);
                    if pb0 == target && pb1 != target {
                        let Some(g) = resolve_guard_val(*arg, &mut next_target) else {
                            break;
                        };
                        g
                    } else if pb1 == target && pb0 != target {
                        let resolved = func.dfg.resolve_aliases(*arg);
                        if let Some(ci) = def_inst(func, resolved)
                            && let InstructionData::IntCompareImm {
                                opcode: Opcode::IcmpImm,
                                cond: ir::condcodes::IntCC::Equal,
                                arg: inner,
                                imm,
                            } = func.dfg.insts[ci]
                            && imm.bits() == 0
                        {
                            let Some(g) = resolve_guard_val(inner, &mut next_target) else {
                                break;
                            };
                            g
                        } else {
                            target = pred;
                            continue;
                        }
                    } else {
                        break;
                    }
                }
                _ => break,
            };

            let mut conj = guard;
            while let Some(gi) = def_inst(func, conj)
                && let InstructionData::Unary {
                    opcode: Opcode::Uextend | Opcode::Sextend,
                    arg,
                } = func.dfg.insts[gi]
            {
                conj = func.dfg.resolve_aliases(arg);
            }
            // Unwrap `bor(uge(lo, hi), guard_conjunction)` when present.
            if let Some(g_inst) = def_inst(func, conj)
                && let InstructionData::Binary {
                    opcode: Opcode::Bor,
                    args: [a, b],
                } = func.dfg.insts[g_inst]
            {
                let is_empty_range = |v: Value| -> bool {
                    def_inst(func, v).is_some_and(|ci| {
                        matches!(
                            func.dfg.insts[ci],
                            InstructionData::IntCompare {
                                opcode: Opcode::Icmp,
                                cond: ir::condcodes::IntCC::UnsignedGreaterThanOrEqual,
                                args: [x, y],
                            } if func.dfg.resolve_aliases(x) == lo
                                && func.dfg.resolve_aliases(y) == hi
                        )
                    })
                };
                if is_empty_range(a) {
                    conj = func.dfg.resolve_aliases(b);
                } else if is_empty_range(b) {
                    conj = func.dfg.resolve_aliases(a);
                } else {
                    target = next_target;
                    continue;
                }
            }

            let mut work = vec![conj];
            let mut visited = HashSet::new();
            while let Some(cur) = work.pop() {
                if !visited.insert(cur) {
                    continue;
                }
                let Some(inst) = def_inst(func, cur) else {
                    continue;
                };
                match func.dfg.insts[inst] {
                    InstructionData::Binary {
                        opcode: Opcode::Band,
                        args: [a, b],
                    } => {
                        work.push(func.dfg.resolve_aliases(a));
                        work.push(func.dfg.resolve_aliases(b));
                    }
                    InstructionData::IntCompareImm {
                        opcode: Opcode::IcmpImm,
                        cond: ir::condcodes::IntCC::Equal,
                        arg,
                        imm,
                    } if imm.bits() == 0 => {
                        collect_ovf_flags(func, arg, hi, &mut proven_bases, &mut proven_add_consts);
                    }
                    _ => {}
                }
            }
            target = next_target;
        }
        if proven_bases.is_empty() {
            continue;
        }

        let loop_blocks: Vec<Block> = func
            .layout
            .blocks()
            .filter(|&b| in_loop(b, lp) && b != header && domtree.dominates(body, b, &func.layout))
            .collect();
        for b in loop_blocks {
            let insts_in_b: Vec<Inst> = func.layout.block_insts(b).collect();
            for inst in insts_in_b {
                let InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond: ir::condcodes::IntCC::UnsignedLessThan,
                    args: [sum, bound],
                } = func.dfg.insts[inst]
                else {
                    continue;
                };
                let sum = func.dfg.resolve_aliases(sum);
                let bound = func.dfg.resolve_aliases(bound);
                let Some(sum_inst) = def_inst(func, sum) else {
                    continue;
                };
                let InstructionData::Binary {
                    opcode: Opcode::Iadd,
                    args: [x, y],
                } = func.dfg.insts[sum_inst]
                else {
                    continue;
                };
                let x = func.dfg.resolve_aliases(x);
                let y = func.dfg.resolve_aliases(y);
                if bound != x && bound != y {
                    continue;
                }
                let is_proven_iv_sum = (x == iv
                    && (proven_bases.contains(&BaseKey::Val(y))
                        || proven_bases.contains(&base_key(func, y))))
                    || (y == iv
                        && (proven_bases.contains(&BaseKey::Val(x))
                            || proven_bases.contains(&base_key(func, x))));
                let is_proven_const_sum = matches!(
                    base_key(func, sum),
                    BaseKey::AddConst(base, c) if proven_add_consts.contains(&(base, c))
                );
                if is_proven_iv_sum || is_proven_const_sum {
                    func.dfg.replace(inst).iconst(types::I8, 0);
                }
            }
        }
    }
}

fn is_pure_if_convert_op(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::Iconst
            | Opcode::F32const
            | Opcode::F64const
            | Opcode::Iadd
            | Opcode::IaddImm
            | Opcode::Isub
            | Opcode::Imul
            | Opcode::ImulImm
            | Opcode::Band
            | Opcode::Bor
            | Opcode::Bxor
            | Opcode::Ishl
            | Opcode::Ushr
            | Opcode::Sshr
            | Opcode::Icmp
            | Opcode::IcmpImm
            | Opcode::Ireduce
            | Opcode::Uextend
            | Opcode::Sextend
            | Opcode::Bnot
            | Opcode::Ineg
            | Opcode::Select
            | Opcode::Clz
            | Opcode::Ctz
            | Opcode::Popcnt
            | Opcode::Bitrev
            | Opcode::Fadd
            | Opcode::Fsub
            | Opcode::Fmul
    )
}

fn is_if_convert_scalar_ty(ty: Type) -> bool {
    matches!(
        ty,
        types::I8 | types::I16 | types::I32 | types::I64 | types::F32 | types::F64
    )
}

fn is_if_convert_cond_ty(ty: Type) -> bool {
    matches!(ty, types::I8 | types::I16 | types::I32 | types::I64)
}

/// Converts small pure, non-trapping `brif` diamonds and triangles into
/// branchless `select`s (e.g. `if s < 64 { x << s } else { 0 }`, `min`, `max`).
fn if_convert_pure_diamonds(func: &mut ir::Function) {
    loop {
        let mut cfg = ControlFlowGraph::with_function(func);
        let blocks: Vec<Block> = func.layout.blocks().collect();
        let mut changed = false;

        for block in blocks {
            if !func.layout.is_block_inserted(block) {
                continue;
            }
            let Some(brif_inst) = func.layout.last_inst(block) else {
                continue;
            };
            let InstructionData::Brif {
                opcode: _,
                arg: cond,
                blocks: [t_call, f_call],
            } = func.dfg.insts[brif_inst]
            else {
                continue;
            };
            if !is_if_convert_cond_ty(func.dfg.value_type(cond)) {
                continue;
            }
            let t_blk = t_call.block(&func.dfg.value_lists);
            let f_blk = f_call.block(&func.dfg.value_lists);
            if t_blk == f_blk || t_blk == block || f_blk == block {
                continue;
            }
            if t_call.len(&func.dfg.value_lists) != 0 || f_call.len(&func.dfg.value_lists) != 0 {
                // Only handle empty-arg brif arms; triangle arm to join may carry args below.
                // Check triangle case where one arm jumps directly to join with args.
                let t_preds = cfg.pred_iter(t_blk).count();
                let f_preds = cfg.pred_iter(f_blk).count();
                let (arm_blk, arm_call, join_call, arm_is_then) =
                    if t_preds == 1 && t_call.len(&func.dfg.value_lists) == 0 {
                        (t_blk, t_call, f_call, true)
                    } else if f_preds == 1 && f_call.len(&func.dfg.value_lists) == 0 {
                        (f_blk, f_call, t_call, false)
                    } else {
                        continue;
                    };
                let _ = arm_call;
                let Some(arm_term) = func.layout.last_inst(arm_blk) else {
                    continue;
                };
                let InstructionData::Jump {
                    opcode: Opcode::Jump,
                    destination: arm_dest,
                } = func.dfg.insts[arm_term]
                else {
                    continue;
                };
                let join_blk = join_call.block(&func.dfg.value_lists);
                if arm_dest.block(&func.dfg.value_lists) != join_blk {
                    continue;
                }
                let arm_insts: Vec<Inst> = func
                    .layout
                    .block_insts(arm_blk)
                    .take_while(|&i| i != arm_term)
                    .collect();
                if arm_insts.is_empty()
                    || arm_insts.len() > 6
                    || !arm_insts
                        .iter()
                        .all(|&i| is_pure_if_convert_op(func.dfg.insts[i].opcode()))
                {
                    continue;
                }
                let arm_args: Vec<Value> = arm_dest
                    .args(&func.dfg.value_lists)
                    .filter_map(|a| a.as_value())
                    .collect();
                let join_args: Vec<Value> = join_call
                    .args(&func.dfg.value_lists)
                    .filter_map(|a| a.as_value())
                    .collect();
                if arm_args.len() != join_args.len()
                    || arm_args.len() > 2
                    || arm_args.len() != arm_dest.len(&func.dfg.value_lists)
                    || join_args.len() != join_call.len(&func.dfg.value_lists)
                    || arm_args.iter().zip(&join_args).any(|(&a, &b)| {
                        let ta = func.dfg.value_type(a);
                        ta != func.dfg.value_type(b) || !is_if_convert_scalar_ty(ta)
                    })
                {
                    continue;
                }
                for inst in arm_insts {
                    func.layout.remove_inst(inst);
                    func.layout.insert_inst(inst, brif_inst);
                }
                let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(brif_inst);
                let mut selected = Vec::with_capacity(arm_args.len());
                for (&a, &b) in arm_args.iter().zip(&join_args) {
                    let (t_v, f_v) = if arm_is_then { (a, b) } else { (b, a) };
                    selected.push(BlockArg::Value(pos.ins().select(cond, t_v, f_v)));
                }
                pos.ins().jump(join_blk, &selected);
                func.layout.remove_inst(brif_inst);
                func.layout.remove_inst(arm_term);
                func.layout.remove_block(arm_blk);
                changed = true;
                break;
            }

            let t_preds = cfg.pred_iter(t_blk).count();
            let f_preds = cfg.pred_iter(f_blk).count();
            let Some(t_term) = func.layout.last_inst(t_blk) else {
                continue;
            };
            let Some(f_term) = func.layout.last_inst(f_blk) else {
                continue;
            };
            let (t_jump, f_jump) = match (func.dfg.insts[t_term], func.dfg.insts[f_term]) {
                (
                    InstructionData::Jump {
                        opcode: Opcode::Jump,
                        destination: td,
                    },
                    InstructionData::Jump {
                        opcode: Opcode::Jump,
                        destination: fd,
                    },
                ) => (td, fd),
                _ => continue,
            };

            let t_dest_blk = t_jump.block(&func.dfg.value_lists);
            let f_dest_blk = f_jump.block(&func.dfg.value_lists);

            if t_preds == 1
                && f_preds == 1
                && t_dest_blk == f_dest_blk
                && t_dest_blk != t_blk
                && t_dest_blk != f_blk
            {
                // Full diamond: block -> {t_blk, f_blk} -> join
                let t_insts: Vec<Inst> = func
                    .layout
                    .block_insts(t_blk)
                    .take_while(|&i| i != t_term)
                    .collect();
                let f_insts: Vec<Inst> = func
                    .layout
                    .block_insts(f_blk)
                    .take_while(|&i| i != f_term)
                    .collect();
                if (t_insts.is_empty() && f_insts.is_empty())
                    || t_insts.len() + f_insts.len() > 6
                    || !t_insts
                        .iter()
                        .chain(&f_insts)
                        .all(|&i| is_pure_if_convert_op(func.dfg.insts[i].opcode()))
                {
                    continue;
                }
                let t_args: Vec<Value> = t_jump
                    .args(&func.dfg.value_lists)
                    .filter_map(|a| a.as_value())
                    .collect();
                let f_args: Vec<Value> = f_jump
                    .args(&func.dfg.value_lists)
                    .filter_map(|a| a.as_value())
                    .collect();
                if t_args.len() != f_args.len()
                    || t_args.len() > 2
                    || t_args.len() != t_jump.len(&func.dfg.value_lists)
                    || f_args.len() != f_jump.len(&func.dfg.value_lists)
                    || t_args.iter().zip(&f_args).any(|(&a, &b)| {
                        let ta = func.dfg.value_type(a);
                        ta != func.dfg.value_type(b) || !is_if_convert_scalar_ty(ta)
                    })
                {
                    continue;
                }
                for inst in t_insts.into_iter().chain(f_insts) {
                    func.layout.remove_inst(inst);
                    func.layout.insert_inst(inst, brif_inst);
                }
                let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(brif_inst);
                let mut selected = Vec::with_capacity(t_args.len());
                for (&a, &b) in t_args.iter().zip(&f_args) {
                    selected.push(BlockArg::Value(pos.ins().select(cond, a, b)));
                }
                pos.ins().jump(t_dest_blk, &selected);
                func.layout.remove_inst(brif_inst);
                func.layout.remove_inst(t_term);
                func.layout.remove_inst(f_term);
                func.layout.remove_block(t_blk);
                func.layout.remove_block(f_blk);
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
        cfg.compute(func);
    }
}

fn eval_const_int_op(func: &ir::Function, inst: Inst, env: &HashMap<Value, i64>) -> Option<i64> {
    let val_of = |v: Value| -> Option<i64> {
        let v = func.dfg.resolve_aliases(v);
        env.get(&v).copied().or_else(|| constant(func, v, 2))
    };
    let intcc = |cc: cranelift_codegen::ir::condcodes::IntCC, a: i64, b: i64| -> i64 {
        use cranelift_codegen::ir::condcodes::IntCC;
        let res = match cc {
            IntCC::Equal => a == b,
            IntCC::NotEqual => a != b,
            IntCC::SignedLessThan => a < b,
            IntCC::SignedGreaterThanOrEqual => a >= b,
            IntCC::SignedGreaterThan => a > b,
            IntCC::SignedLessThanOrEqual => a <= b,
            IntCC::UnsignedLessThan => (a as u64) < (b as u64),
            IntCC::UnsignedGreaterThanOrEqual => (a as u64) >= (b as u64),
            IntCC::UnsignedGreaterThan => (a as u64) > (b as u64),
            IntCC::UnsignedLessThanOrEqual => (a as u64) <= (b as u64),
        };
        i64::from(res)
    };
    match func.dfg.insts[inst] {
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => Some(imm.bits()),
        InstructionData::Unary {
            opcode: Opcode::Uextend | Opcode::Sextend | Opcode::Ireduce,
            arg,
        } => {
            let v = val_of(arg)?;
            let res_ty = func.dfg.value_type(func.dfg.first_result(inst));
            let arg_ty = func.dfg.value_type(arg);
            let masked = match (func.dfg.insts[inst].opcode(), arg_ty, res_ty) {
                (Opcode::Uextend, types::I8, _) => (v as u8) as i64,
                (Opcode::Uextend, types::I16, _) => (v as u16) as i64,
                (Opcode::Uextend, types::I32, _) => (v as u32) as i64,
                (Opcode::Sextend, types::I8, _) => (v as i8) as i64,
                (Opcode::Sextend, types::I16, _) => (v as i16) as i64,
                (Opcode::Sextend, types::I32, _) => (v as i32) as i64,
                (Opcode::Ireduce, _, types::I8) => (v as u8) as i8 as i64,
                (Opcode::Ireduce, _, types::I16) => (v as u16) as i16 as i64,
                (Opcode::Ireduce, _, types::I32) => (v as u32) as i32 as i64,
                _ => return None,
            };
            Some(masked)
        }
        InstructionData::IntCompareImm {
            opcode: Opcode::IcmpImm,
            cond,
            arg,
            imm,
        } => Some(intcc(cond, val_of(arg)?, imm.bits())),
        InstructionData::IntCompare {
            opcode: Opcode::Icmp,
            cond,
            args: [a, b],
        } => Some(intcc(cond, val_of(a)?, val_of(b)?)),
        InstructionData::Binary {
            opcode,
            args: [a, b],
        } => {
            let (av, bv) = (val_of(a)?, val_of(b)?);
            match opcode {
                Opcode::Band => Some(av & bv),
                Opcode::Bor => Some(av | bv),
                Opcode::Bxor => Some(av ^ bv),
                Opcode::Iadd => Some(av.wrapping_add(bv)),
                Opcode::Isub => Some(av.wrapping_sub(bv)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Single-hop jump threading for `Jump target(args...)` into blocks whose
/// `brif` folds to a constant under `args` and whose params/locals do not escape.
fn thread_constant_param_jumps(func: &mut ir::Function) {
    for _round in 0..4 {
        let mut val_users: HashMap<Value, HashSet<Block>> = HashMap::new();
        for b in func.layout.blocks() {
            for inst in func.layout.block_insts(b) {
                for v in values_of(func, inst) {
                    val_users
                        .entry(func.dfg.resolve_aliases(v))
                        .or_default()
                        .insert(b);
                }
            }
        }
        let jump_sites: Vec<(Block, Inst)> = func
            .layout
            .blocks()
            .filter_map(|b| {
                let last = func.layout.last_inst(b)?;
                matches!(
                    func.dfg.insts[last],
                    InstructionData::Jump {
                        opcode: Opcode::Jump,
                        ..
                    }
                )
                .then_some((b, last))
            })
            .collect();

        let mut changed = false;
        for (pred_blk, jump_inst) in jump_sites {
            let InstructionData::Jump {
                opcode: Opcode::Jump,
                destination,
            } = func.dfg.insts[jump_inst]
            else {
                continue;
            };
            let target = destination.block(&func.dfg.value_lists);
            if target == pred_blk {
                continue;
            }
            let init_args: Vec<Value> = destination
                .args(&func.dfg.value_lists)
                .filter_map(|a| a.as_value())
                .collect();
            let params: Vec<Value> = func.dfg.block_params(target).to_vec();
            if init_args.len() != params.len()
                || init_args.len() != destination.len(&func.dfg.value_lists)
            {
                continue;
            }
            let mut env: HashMap<Value, i64> = HashMap::new();
            let mut param_vals: HashMap<Value, Value> = HashMap::new();
            for (&p, &a) in params.iter().zip(&init_args) {
                let a = func.dfg.resolve_aliases(a);
                param_vals.insert(p, a);
                if let Some(c) = constant(func, a, 2) {
                    env.insert(p, c);
                }
            }
            if env.is_empty() {
                continue;
            }
            let body: Vec<Inst> = func.layout.block_insts(target).collect();
            let Some((&term, rest)) = body.split_last() else {
                continue;
            };
            let InstructionData::Brif {
                arg: cond,
                blocks: [t_call, f_call],
                ..
            } = func.dfg.insts[term]
            else {
                continue;
            };
            let mut local_defs: Vec<Value> = params.clone();
            let mut local_ok = true;
            for &i in rest {
                if let Some(c) = eval_const_int_op(func, i, &env) {
                    if let Some(&r) = func.dfg.inst_results(i).first() {
                        env.insert(r, c);
                        local_defs.push(r);
                    }
                } else {
                    local_ok = false;
                    break;
                }
            }
            if !local_ok {
                continue;
            }
            let cond = func.dfg.resolve_aliases(cond);
            let Some(&cond_val) = env.get(&cond) else {
                continue;
            };
            if local_defs.iter().any(|d| {
                val_users
                    .get(d)
                    .is_some_and(|bs| bs.iter().any(|&b| b != target))
            }) {
                continue;
            }
            let chosen = if cond_val != 0 { t_call } else { f_call };
            let next_blk = chosen.block(&func.dfg.value_lists);
            if next_blk == target || next_blk == pred_blk {
                continue;
            }
            let chosen_args: Vec<BlockArg> = chosen
                .args(&func.dfg.value_lists)
                .map(|a| match a {
                    BlockArg::Value(v) => {
                        let v = func.dfg.resolve_aliases(v);
                        BlockArg::Value(param_vals.get(&v).copied().unwrap_or(v))
                    }
                    other => other,
                })
                .collect();
            if chosen_args.iter().any(|a| {
                a.as_value()
                    .is_none_or(|v| local_defs.contains(&func.dfg.resolve_aliases(v)))
            }) {
                continue;
            }
            let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(jump_inst);
            pos.ins().jump(next_blk, &chosen_args);
            func.layout.remove_inst(jump_inst);
            changed = true;
        }
        if !changed {
            break;
        }
        remove_unreachable(func);
    }
}

/// Propagates known true/false branch conditions along single-predecessor edges
/// in the dominator tree to fold redundant `brif` and `select` instructions.
fn propagate_branch_conditions(func: &mut ir::Function) {
    let cfg = ControlFlowGraph::with_function(func);
    let domtree = DominatorTree::with_function(func, &cfg);
    let mut dom_children: HashMap<Block, Vec<Block>> = HashMap::new();
    for b in domtree.cfg_postorder().iter().rev().copied() {
        if let Some(idom) = domtree.idom(b)
            && idom != b
        {
            dom_children.entry(idom).or_default().push(b);
        }
    }
    let Some(entry) = func.layout.entry_block() else {
        return;
    };

    use cranelift_codegen::ir::condcodes::IntCC;

    type PropState = (Block, HashMap<Value, bool>, HashMap<Value, i64>);
    let mut stack: Vec<PropState> = vec![(entry, HashMap::new(), HashMap::new())];
    while let Some((block, mut known, mut known_consts)) = stack.pop() {
        let insts: Vec<Inst> = func.layout.block_insts(block).collect();
        for inst in insts {
            match func.dfg.insts[inst] {
                InstructionData::Ternary {
                    opcode: Opcode::Select,
                    args: [c, t_v, f_v],
                } => {
                    let c = func.dfg.resolve_aliases(c);
                    if let Some(&is_true) = known.get(&c) {
                        let chosen = if is_true { t_v } else { f_v };
                        let res = func.dfg.first_result(inst);
                        func.dfg.detach_inst_results(inst);
                        func.layout.remove_inst(inst);
                        func.dfg.change_to_alias(res, chosen);
                    }
                }
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    cond: cc,
                    arg,
                    imm,
                } => {
                    let arg = func.dfg.resolve_aliases(arg);
                    if let Some(&kc) = known_consts.get(&arg)
                        && let Some(b) = eval_icmp(cc, func.dfg.value_type(arg), kc, imm.bits())
                    {
                        let res = func.dfg.first_result(inst);
                        let res_ty = func.dfg.value_type(res);
                        func.dfg.replace(inst).iconst(res_ty, i64::from(b));
                        known.insert(res, b);
                    }
                }
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond: cc,
                    args: [lhs, rhs],
                } => {
                    let lhs = func.dfg.resolve_aliases(lhs);
                    let rhs = func.dfg.resolve_aliases(rhs);
                    let lv = known_consts
                        .get(&lhs)
                        .copied()
                        .or_else(|| constant(func, lhs, 1));
                    let rv = known_consts
                        .get(&rhs)
                        .copied()
                        .or_else(|| constant(func, rhs, 1));
                    if let (Some(a), Some(b)) = (lv, rv)
                        && let Some(res_bool) = eval_icmp(cc, func.dfg.value_type(lhs), a, b)
                    {
                        let res = func.dfg.first_result(inst);
                        let res_ty = func.dfg.value_type(res);
                        func.dfg.replace(inst).iconst(res_ty, i64::from(res_bool));
                        known.insert(res, res_bool);
                    }
                }
                _ => {}
            }
        }

        let mut edge_facts: Vec<(Block, Value, bool)> = Vec::new();
        let mut const_edge_facts: Vec<(Block, Value, i64)> = Vec::new();
        if let Some(last) = func.layout.last_inst(block)
            && let InstructionData::Brif {
                arg: cond,
                blocks: [t_call, f_call],
                ..
            } = func.dfg.insts[last]
        {
            let cond = func.dfg.resolve_aliases(cond);
            if let Some(&is_true) = known.get(&cond) {
                let chosen = if is_true { t_call } else { f_call };
                let target = chosen.block(&func.dfg.value_lists);
                let args: Vec<BlockArg> = chosen.args(&func.dfg.value_lists).collect();
                let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(last);
                pos.ins().jump(target, &args);
                func.layout.remove_inst(last);
            } else {
                let t_blk = t_call.block(&func.dfg.value_lists);
                let f_blk = f_call.block(&func.dfg.value_lists);
                if t_blk != f_blk {
                    let t_single = cfg.pred_iter(t_blk).count() == 1;
                    let f_single = cfg.pred_iter(f_blk).count() == 1;
                    if t_single {
                        edge_facts.push((t_blk, cond, true));
                    }
                    if f_single {
                        edge_facts.push((f_blk, cond, false));
                    }
                    if let Some(cmp_inst) = func.dfg.value_def(cond).inst() {
                        match func.dfg.insts[cmp_inst] {
                            InstructionData::IntCompareImm {
                                opcode: Opcode::IcmpImm,
                                cond: IntCC::Equal,
                                arg,
                                imm,
                            } if t_single => {
                                const_edge_facts.push((
                                    t_blk,
                                    func.dfg.resolve_aliases(arg),
                                    imm.bits(),
                                ));
                            }
                            InstructionData::IntCompareImm {
                                opcode: Opcode::IcmpImm,
                                cond: IntCC::NotEqual,
                                arg,
                                imm,
                            } if f_single => {
                                const_edge_facts.push((
                                    f_blk,
                                    func.dfg.resolve_aliases(arg),
                                    imm.bits(),
                                ));
                            }
                            InstructionData::IntCompare {
                                opcode: Opcode::Icmp,
                                cond: cc @ (IntCC::Equal | IntCC::NotEqual),
                                args: [lhs, rhs],
                            } => {
                                let eq_blk = if cc == IntCC::Equal && t_single {
                                    Some(t_blk)
                                } else if cc == IntCC::NotEqual && f_single {
                                    Some(f_blk)
                                } else {
                                    None
                                };
                                if let Some(target_blk) = eq_blk {
                                    if let Some(c) = constant(func, rhs, 1) {
                                        const_edge_facts.push((
                                            target_blk,
                                            func.dfg.resolve_aliases(lhs),
                                            c,
                                        ));
                                    } else if let Some(c) = constant(func, lhs, 1) {
                                        const_edge_facts.push((
                                            target_blk,
                                            func.dfg.resolve_aliases(rhs),
                                            c,
                                        ));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        if let Some(children) = dom_children.get(&block) {
            for (idx, &child) in children.iter().enumerate() {
                let (mut child_known, mut child_consts) = if idx + 1 == children.len() {
                    (
                        std::mem::take(&mut known),
                        std::mem::take(&mut known_consts),
                    )
                } else {
                    (known.clone(), known_consts.clone())
                };
                for &(eb, cond, val) in &edge_facts {
                    if eb == child {
                        child_known.insert(cond, val);
                    }
                }
                for &(eb, val, k) in &const_edge_facts {
                    if eb == child {
                        child_consts.insert(val, k);
                    }
                }
                stack.push((child, child_known, child_consts));
            }
        }
    }
}

fn resolve_base_offset(func: &ir::Function, mut addr: Value, mut off: i64) -> (Value, i64) {
    for _ in 0..4 {
        addr = func.dfg.resolve_aliases(addr);
        if let Some(inst) = func.dfg.value_def(addr).inst()
            && let InstructionData::BinaryImm64 {
                opcode: Opcode::IaddImm,
                arg,
                imm,
            } = func.dfg.insts[inst]
        {
            addr = arg;
            off += imm.bits();
            continue;
        }
        break;
    }
    (func.dfg.resolve_aliases(addr), off)
}

fn is_entry_param(func: &ir::Function, val: Value) -> bool {
    let val = func.dfg.resolve_aliases(val);
    matches!(
        (func.layout.entry_block(), func.dfg.value_def(val)),
        (Some(entry), ir::ValueDef::Param(b, _)) if b == entry
    )
}

const SEQ_LEN_OFFSET: i64 = 0;
const SEQ_DATA_OFFSET: i64 = 16;
const SEQ_WORD_BYTES: i64 = 8;
const MAX_SEQ_ADDR_DEPTH: u32 = 5;
const MAX_SEQ_ARM_SCAN_INSTS: usize = 16;
const MAX_DOM_REGION_BLOCKS: usize = 64;
const MAX_SEQ_HOIST_ROUNDS: usize = 2;
const MAX_SEQ_REUSE_ROUNDS: usize = 2;
const MIN_SHARED_DATA_LOADS: usize = 2;

fn seq_data_handle_from_elem_addr(
    func: &ir::Function,
    val: Value,
    extra_off: i64,
    depth: u32,
) -> Option<Value> {
    if depth == 0 {
        return None;
    }
    let val = func.dfg.resolve_aliases(val);
    let inst = func.dfg.value_def(val).inst()?;
    match func.dfg.insts[inst] {
        InstructionData::Load {
            opcode: Opcode::Load,
            arg: addr,
            offset,
            ..
        } if func.dfg.value_type(val) == types::I64 => {
            let (h, off) = resolve_base_offset(func, addr, i64::from(i32::from(offset)));
            (off == SEQ_DATA_OFFSET && extra_off >= 0).then_some(h)
        }
        InstructionData::BinaryImm64 {
            opcode: Opcode::IaddImm,
            arg,
            imm,
        } => seq_data_handle_from_elem_addr(
            func,
            arg,
            extra_off.saturating_add(imm.bits()),
            depth - 1,
        ),
        InstructionData::Binary {
            opcode: Opcode::Iadd,
            args: [lhs, rhs],
        } => seq_data_handle_from_elem_addr(func, lhs, 0, depth - 1)
            .or_else(|| seq_data_handle_from_elem_addr(func, rhs, 0, depth - 1)),
        _ => None,
    }
}

/// Collects `Value`s proven to be `SeqHandle`s by matching `TrySet` `plain_word`
/// stores (`store _, select(icmp ult(_, *(h + 0)), *(h + 16) + off, stack_addr _)`)
/// or `Brif` bounds-checked element loads/stores.
fn collect_verified_seq_handles(func: &ir::Function) -> HashSet<Value> {
    use cranelift_codegen::ir::condcodes::IntCC;
    let mut handles = HashSet::new();
    let is_stack = |v: Value| {
        let v = func.dfg.resolve_aliases(v);
        func.dfg.value_def(v).inst().is_some_and(|i| {
            matches!(
                func.dfg.insts[i],
                InstructionData::StackLoad {
                    opcode: Opcode::StackAddr,
                    ..
                }
            )
        })
    };
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            if let InstructionData::Brif {
                arg: cond,
                blocks: [t_call, f_call],
                ..
            } = func.dfg.insts[inst]
            {
                let cond = func.dfg.resolve_aliases(cond);
                if let Some(cmp_inst) = func.dfg.value_def(cond).inst()
                    && let InstructionData::IntCompare {
                        opcode: Opcode::Icmp,
                        cond: cc @ (IntCC::UnsignedLessThan | IntCC::UnsignedGreaterThanOrEqual),
                        args: [_, len_val],
                    } = func.dfg.insts[cmp_inst]
                    && let Some(len_inst) =
                        func.dfg.value_def(func.dfg.resolve_aliases(len_val)).inst()
                    && let InstructionData::Load {
                        opcode: Opcode::Load,
                        arg: len_addr,
                        offset: len_off,
                        ..
                    } = func.dfg.insts[len_inst]
                {
                    let (h_len, off_len) =
                        resolve_base_offset(func, len_addr, i64::from(i32::from(len_off)));
                    let elem_blk = if cc == IntCC::UnsignedLessThan {
                        t_call.block(&func.dfg.value_lists)
                    } else {
                        f_call.block(&func.dfg.value_lists)
                    };
                    if off_len == SEQ_LEN_OFFSET {
                        let matched = func
                            .layout
                            .block_insts(elem_blk)
                            .take(MAX_SEQ_ARM_SCAN_INSTS)
                            .any(|arm_inst| match func.dfg.insts[arm_inst] {
                                InstructionData::Load {
                                    opcode:
                                        Opcode::Load
                                        | Opcode::Uload8
                                        | Opcode::Sload8
                                        | Opcode::Uload16
                                        | Opcode::Sload16
                                        | Opcode::Uload32
                                        | Opcode::Sload32,
                                    arg: elem_ptr,
                                    offset: elem_off,
                                    ..
                                }
                                | InstructionData::Store {
                                    opcode:
                                        Opcode::Store
                                        | Opcode::Istore8
                                        | Opcode::Istore16
                                        | Opcode::Istore32,
                                    args: [_, elem_ptr],
                                    offset: elem_off,
                                    ..
                                } => {
                                    seq_data_handle_from_elem_addr(
                                        func,
                                        elem_ptr,
                                        i64::from(i32::from(elem_off)),
                                        MAX_SEQ_ADDR_DEPTH,
                                    ) == Some(h_len)
                                }
                                _ => false,
                            });
                        if matched {
                            handles.insert(h_len);
                        }
                    }
                }
                continue;
            }
            let InstructionData::Store {
                opcode: Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32,
                args: [_, ptr],
                offset,
                ..
            } = func.dfg.insts[inst]
            else {
                continue;
            };
            if i32::from(offset) != 0 {
                continue;
            }
            let ptr = func.dfg.resolve_aliases(ptr);
            let Some(sel_inst) = func.dfg.value_def(ptr).inst() else {
                continue;
            };
            let InstructionData::Ternary {
                opcode: Opcode::Select,
                args: [cond, t_arm, f_arm],
            } = func.dfg.insts[sel_inst]
            else {
                continue;
            };
            let elem_arm = if is_stack(f_arm) {
                t_arm
            } else if is_stack(t_arm) {
                f_arm
            } else {
                continue;
            };
            let cond = func.dfg.resolve_aliases(cond);
            let Some(cmp_inst) = func.dfg.value_def(cond).inst() else {
                continue;
            };
            let InstructionData::IntCompare {
                opcode: Opcode::Icmp,
                cond: IntCC::UnsignedLessThan,
                args: [_, len_val],
            } = func.dfg.insts[cmp_inst]
            else {
                continue;
            };
            let len_val = func.dfg.resolve_aliases(len_val);
            let Some(len_inst) = func.dfg.value_def(len_val).inst() else {
                continue;
            };
            let InstructionData::Load {
                opcode: Opcode::Load,
                arg: len_addr,
                offset: len_off,
                ..
            } = func.dfg.insts[len_inst]
            else {
                continue;
            };
            let (h_len, off_len) =
                resolve_base_offset(func, len_addr, i64::from(i32::from(len_off)));
            if off_len != SEQ_LEN_OFFSET {
                continue;
            }
            if let Some(h_data) =
                seq_data_handle_from_elem_addr(func, elem_arm, 0, MAX_SEQ_ADDR_DEPTH)
                && h_data == h_len
            {
                handles.insert(h_len);
            }
        }
    }
    handles
}

enum SeqStoreKind {
    SeqElem,
    ParamField(Value, i64),
    StackSlot,
    Unknown,
}

fn classify_seq_store(
    func: &ir::Function,
    inst: Inst,
    seq_handles: &HashSet<Value>,
) -> SeqStoreKind {
    let InstructionData::Store {
        opcode: Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32,
        args: [_, ptr],
        offset,
        ..
    } = func.dfg.insts[inst]
    else {
        return SeqStoreKind::Unknown;
    };
    let ptr = func.dfg.resolve_aliases(ptr);
    let off = i64::from(i32::from(offset));
    if off == 0
        && let Some(ptr_inst) = func.dfg.value_def(ptr).inst()
        && let InstructionData::Ternary {
            opcode: Opcode::Select,
            args: [_, t_arm, f_arm],
        } = func.dfg.insts[ptr_inst]
    {
        let is_stack = |v: Value| {
            let v = func.dfg.resolve_aliases(v);
            func.dfg.value_def(v).inst().is_some_and(|i| {
                matches!(
                    func.dfg.insts[i],
                    InstructionData::StackLoad {
                        opcode: Opcode::StackAddr,
                        ..
                    }
                )
            })
        };
        if (is_stack(f_arm)
            && seq_data_handle_from_elem_addr(func, t_arm, 0, MAX_SEQ_ADDR_DEPTH)
                .is_some_and(|h| seq_handles.contains(&h)))
            || (is_stack(t_arm)
                && seq_data_handle_from_elem_addr(func, f_arm, 0, MAX_SEQ_ADDR_DEPTH)
                    .is_some_and(|h| seq_handles.contains(&h)))
        {
            return SeqStoreKind::SeqElem;
        }
    }
    if seq_data_handle_from_elem_addr(func, ptr, off, MAX_SEQ_ADDR_DEPTH)
        .is_some_and(|h| seq_handles.contains(&h))
    {
        return SeqStoreKind::SeqElem;
    }
    let (base, total_off) = resolve_base_offset(func, ptr, off);
    if is_entry_param(func, base) && !seq_handles.contains(&base) {
        return SeqStoreKind::ParamField(base, total_off);
    }
    if func.dfg.value_def(base).inst().is_some_and(|i| {
        matches!(
            func.dfg.insts[i],
            InstructionData::StackLoad {
                opcode: Opcode::StackAddr,
                ..
            }
        )
    }) {
        return SeqStoreKind::StackSlot;
    }
    SeqStoreKind::Unknown
}

fn reuse_seq_header_loads(func: &mut ir::Function) {
    let seq_handles = collect_verified_seq_handles(func);

    let mut cfg = ControlFlowGraph::with_function(func);
    let mut domtree = DominatorTree::with_function(func, &cfg);
    let mut loops = LoopAnalysis::new();
    loops.compute(func, &cfg, &domtree);

    let all_loops: Vec<Loop> = if seq_handles.is_empty() {
        Vec::new()
    } else {
        loops.loops().collect()
    };
    let rpo_blocks: Vec<Block> = domtree.cfg_postorder().iter().rev().copied().collect();
    for lp in all_loops {
        let header = loops.loop_header(lp);
        let loop_blocks: HashSet<Block> = rpo_blocks
            .iter()
            .copied()
            .filter(|&b| loops.is_in_loop(b, lp))
            .collect();
        let outside_preds: Vec<Block> = cfg
            .pred_iter(header)
            .map(|p| p.block)
            .filter(|b| !loop_blocks.contains(b))
            .collect();
        if outside_preds.len() != 1 {
            continue;
        }
        let preheader = outside_preds[0];
        let Some(preheader_term) = func.layout.last_inst(preheader) else {
            continue;
        };
        if !matches!(
            func.dfg.insts[preheader_term],
            InstructionData::Jump {
                opcode: Opcode::Jump,
                ..
            }
        ) || !domtree.dominates(preheader, header, &func.layout)
        {
            continue;
        }

        let mut has_clobber = false;
        let mut written_param_fields: Vec<(Value, i64)> = Vec::new();
        'scan_loop: for &b in &loop_blocks {
            for inst in func.layout.block_insts(b) {
                match func.dfg.insts[inst].opcode() {
                    Opcode::Call | Opcode::CallIndirect => {
                        has_clobber = true;
                        break 'scan_loop;
                    }
                    Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32 => {
                        match classify_seq_store(func, inst, &seq_handles) {
                            SeqStoreKind::SeqElem | SeqStoreKind::StackSlot => {}
                            SeqStoreKind::ParamField(p, off) => written_param_fields.push((p, off)),
                            SeqStoreKind::Unknown => {
                                has_clobber = true;
                                break 'scan_loop;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if has_clobber {
            continue;
        }

        let is_outside_loop = |func: &ir::Function, v: Value| -> bool {
            let v = func.dfg.resolve_aliases(v);
            match func.dfg.value_def(v) {
                ir::ValueDef::Param(b, _) => !loop_blocks.contains(&b),
                ir::ValueDef::Result(i, _) => func
                    .layout
                    .inst_block(i)
                    .is_some_and(|b| !loop_blocks.contains(&b)),
                ir::ValueDef::Union(_, _) => false,
            }
        };

        let mut safe_bases: HashSet<Value> = HashSet::new();
        for &b in &rpo_blocks {
            if !domtree.dominates(b, preheader, &func.layout) {
                continue;
            }
            for inst in func.layout.block_insts(b) {
                if let InstructionData::Load {
                    opcode: Opcode::Load,
                    arg: addr,
                    offset,
                    ..
                } = func.dfg.insts[inst]
                {
                    let (base, _) = resolve_base_offset(func, addr, i64::from(i32::from(offset)));
                    safe_bases.insert(base);
                }
            }
        }
        for _ in 0..MAX_SEQ_HOIST_ROUNDS {
            let mut to_hoist: Vec<(Inst, Value, ir::MemFlags, Value, i32)> = Vec::new();
            for &b in &rpo_blocks {
                if !loop_blocks.contains(&b) {
                    continue;
                }
                for inst in func.layout.block_insts(b) {
                    let InstructionData::Load {
                        opcode: Opcode::Load,
                        arg: addr,
                        offset,
                        flags,
                    } = func.dfg.insts[inst]
                    else {
                        continue;
                    };
                    let Some(&res) = func.dfg.inst_results(inst).first() else {
                        continue;
                    };
                    if func.dfg.value_type(res) != types::I64 {
                        continue;
                    }
                    let (base, total_off) =
                        resolve_base_offset(func, addr, i64::from(i32::from(offset)));
                    if !flags.notrap() && !safe_bases.contains(&base) {
                        continue;
                    }
                    let Ok(off32) = i32::try_from(total_off) else {
                        continue;
                    };
                    if !is_outside_loop(func, base) {
                        continue;
                    }
                    let is_seq_hdr = seq_handles.contains(&base)
                        && (total_off == SEQ_LEN_OFFSET || total_off == SEQ_DATA_OFFSET);
                    let is_param_seq_field = is_entry_param(func, base)
                        && seq_handles.contains(&res)
                        && !written_param_fields
                            .iter()
                            .any(|&(p, po)| p == base && (po - total_off).abs() < SEQ_WORD_BYTES);
                    if is_seq_hdr || is_param_seq_field {
                        to_hoist.push((inst, res, flags, base, off32));
                    }
                }
            }
            if to_hoist.is_empty() {
                break;
            }
            for (inst, res, flags, base, off32) in to_hoist {
                let mut pos =
                    cranelift_codegen::cursor::FuncCursor::new(func).at_inst(preheader_term);
                let hoisted = pos.ins().load(types::I64, flags, base, off32);
                func.dfg.detach_inst_results(inst);
                func.layout.remove_inst(inst);
                func.dfg.change_to_alias(res, hoisted);
            }
        }
    }

    cfg.compute(func);
    domtree.compute(func, &cfg);

    let mut dom_children: HashMap<Block, Vec<Block>> = HashMap::new();
    for b in domtree.cfg_postorder().iter().rev().copied() {
        if let Some(idom) = domtree.idom(b)
            && idom != b
        {
            dom_children.entry(idom).or_default().push(b);
        }
    }

    let Some(entry) = func.layout.entry_block() else {
        return;
    };

    struct BlockClobber {
        clobber_all: bool,
        written_fields: Vec<(Value, i64)>,
    }

    for _ in 0..MAX_SEQ_REUSE_ROUNDS {
        let mut changed = false;
        let mut block_clobbers: HashMap<Block, BlockClobber> = HashMap::new();
        for b in func.layout.blocks() {
            let mut clobber_all = false;
            let mut written_fields = Vec::new();
            for inst in func.layout.block_insts(b) {
                match func.dfg.insts[inst].opcode() {
                    Opcode::Call | Opcode::CallIndirect => clobber_all = true,
                    Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32 => {
                        match classify_seq_store(func, inst, &seq_handles) {
                            SeqStoreKind::SeqElem | SeqStoreKind::StackSlot => {}
                            SeqStoreKind::ParamField(p, o) => written_fields.push((p, o)),
                            SeqStoreKind::Unknown => clobber_all = true,
                        }
                    }
                    _ => {}
                }
            }
            block_clobbers.insert(
                b,
                BlockClobber {
                    clobber_all,
                    written_fields,
                },
            );
        }

        type AvailLoads = HashMap<(Value, i64), Value>;
        type AvailLenInsts = HashMap<Value, (Inst, ir::MemFlags)>;
        type DataGroup = (Value, ir::MemFlags, Vec<(Inst, Value)>);
        let mut group_order: Vec<Inst> = Vec::new();
        let mut data_groups: HashMap<Inst, DataGroup> = HashMap::new();

        let mut stack: Vec<(Block, AvailLoads, AvailLenInsts)> =
            vec![(entry, AvailLoads::new(), AvailLenInsts::new())];
        while let Some((block, mut avail, mut avail_len)) = stack.pop() {
            if (!avail.is_empty() || !avail_len.is_empty()) && cfg.pred_iter(block).count() > 1 {
                let Some(idom) = domtree.idom(block) else {
                    avail.clear();
                    avail_len.clear();
                    continue;
                };
                let mut clobber_all = false;
                let mut region: Vec<Block> = Vec::new();
                let mut visited: HashSet<Block> = HashSet::new();
                for pred in cfg.pred_iter(block) {
                    if !domtree.is_reachable(pred.block) || pred.block == idom {
                        continue;
                    }
                    if visited.insert(pred.block) {
                        region.push(pred.block);
                    }
                }
                let mut cursor = 0;
                while cursor < region.len() {
                    if region.len() > MAX_DOM_REGION_BLOCKS {
                        clobber_all = true;
                        break;
                    }
                    let cur = region[cursor];
                    cursor += 1;
                    if !domtree.dominates(idom, cur, &func.layout)
                        || block_clobbers.get(&cur).is_some_and(|c| c.clobber_all)
                    {
                        clobber_all = true;
                        break;
                    }
                    for pred in cfg.pred_iter(cur) {
                        if !domtree.is_reachable(pred.block) || pred.block == idom {
                            continue;
                        }
                        if visited.insert(pred.block) {
                            region.push(pred.block);
                        }
                    }
                }
                if clobber_all {
                    avail.clear();
                    avail_len.clear();
                } else {
                    avail.retain(|&(b, o), _| {
                        seq_handles.contains(&b)
                            || region.iter().all(|r| {
                                block_clobbers.get(r).is_none_or(|c| {
                                    c.written_fields
                                        .iter()
                                        .all(|&(p, po)| b == p && (po - o).abs() >= SEQ_WORD_BYTES)
                                })
                            })
                    });
                }
            }

            let insts: Vec<Inst> = func.layout.block_insts(block).collect();
            for inst in insts {
                match func.dfg.insts[inst].opcode() {
                    Opcode::Call | Opcode::CallIndirect => {
                        avail.clear();
                        avail_len.clear();
                    }
                    Opcode::Store | Opcode::Istore8 | Opcode::Istore16 | Opcode::Istore32 => {
                        match classify_seq_store(func, inst, &seq_handles) {
                            SeqStoreKind::SeqElem | SeqStoreKind::StackSlot => {}
                            SeqStoreKind::ParamField(p, o) => {
                                avail.retain(|&(b, bo), _| {
                                    (b == p && (bo - o).abs() >= SEQ_WORD_BYTES)
                                        || seq_handles.contains(&b)
                                });
                                if let InstructionData::Store {
                                    opcode: Opcode::Store,
                                    args: [val, _],
                                    ..
                                } = func.dfg.insts[inst]
                                {
                                    let val = func.dfg.resolve_aliases(val);
                                    if func.dfg.value_type(val) == types::I64
                                        && seq_handles.contains(&val)
                                    {
                                        avail.insert((p, o), val);
                                    }
                                }
                            }
                            SeqStoreKind::Unknown => {
                                avail.clear();
                                avail_len.clear();
                            }
                        }
                    }
                    Opcode::Load => {
                        let InstructionData::Load {
                            opcode: Opcode::Load,
                            arg: addr,
                            offset,
                            flags,
                        } = func.dfg.insts[inst]
                        else {
                            continue;
                        };
                        let Some(&res) = func.dfg.inst_results(inst).first() else {
                            continue;
                        };
                        if func.dfg.value_type(res) != types::I64 {
                            continue;
                        };
                        let (base, total_off) =
                            resolve_base_offset(func, addr, i64::from(i32::from(offset)));
                        let is_seq_hdr = seq_handles.contains(&base)
                            && (total_off == SEQ_LEN_OFFSET || total_off == SEQ_DATA_OFFSET);
                        let is_param_seq_field = is_entry_param(func, base)
                            && (seq_handles.contains(&res)
                                || avail.contains_key(&(base, total_off)));
                        if is_seq_hdr || is_param_seq_field {
                            if let Some(&prev) = avail.get(&(base, total_off)) {
                                func.dfg.detach_inst_results(inst);
                                func.layout.remove_inst(inst);
                                func.dfg.change_to_alias(res, prev);
                                changed = true;
                            } else {
                                avail.insert((base, total_off), res);
                                if is_seq_hdr && total_off == SEQ_LEN_OFFSET {
                                    avail_len.insert(base, (inst, flags));
                                } else if is_seq_hdr
                                    && total_off == SEQ_DATA_OFFSET
                                    && let Some(&(len_inst, len_flags)) = avail_len.get(&base)
                                {
                                    if func.layout.inst_block(len_inst) == Some(block) {
                                        avail_len.remove(&base);
                                    } else {
                                        let entry =
                                            data_groups.entry(len_inst).or_insert_with(|| {
                                                group_order.push(len_inst);
                                                (base, len_flags, Vec::new())
                                            });
                                        entry.2.push((inst, res));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            if let Some(children) = dom_children.get(&block) {
                for (idx, &child) in children.iter().enumerate() {
                    let (child_avail, child_len) = if idx + 1 == children.len() {
                        (std::mem::take(&mut avail), std::mem::take(&mut avail_len))
                    } else {
                        (avail.clone(), avail_len.clone())
                    };
                    stack.push((child, child_avail, child_len));
                }
            }
        }

        for len_inst in group_order {
            let Some((base, len_flags, loads)) = data_groups.remove(&len_inst) else {
                continue;
            };
            if loads.len() < MIN_SHARED_DATA_LOADS {
                continue;
            }
            let Some(next_inst) = func.layout.next_inst(len_inst) else {
                continue;
            };
            let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(next_inst);
            let hoisted = pos
                .ins()
                .load(types::I64, len_flags, base, SEQ_DATA_OFFSET as i32);
            for (inst, res) in loads {
                func.dfg.detach_inst_results(inst);
                func.layout.remove_inst(inst);
                func.dfg.change_to_alias(res, hoisted);
            }
            changed = true;
        }

        if !changed {
            break;
        }
    }
}

const MAX_IMM_CANON_ROUNDS: usize = 3;

/// Canonicalizes integer `Binary` and `Icmp` instructions with a constant
/// operand into `BinaryImm64` / `IcmpImm`, and folds associative `BinaryImm64`
/// chains (`iadd_imm`, `imul_imm`, `band_imm`, `bor_imm`, `bxor_imm`).
fn canonicalize_imm_ops(func: &mut ir::Function) {
    use cranelift_codegen::ir::condcodes::CondCode as _;
    let sign_extend_imm = |ty: Type, c: i64| -> Option<i64> {
        let bits = ty.bits();
        if !matches!(bits, 8 | 16 | 32 | 64) {
            return None;
        }
        let shift = 64 - bits;
        Some(((c as u64) << shift) as i64 >> shift)
    };
    for _ in 0..MAX_IMM_CANON_ROUNDS {
        let mut changed = false;
        for (_, inst) in insts(func) {
            match func.dfg.insts[inst] {
                InstructionData::Binary {
                    opcode: op,
                    args: [lhs, rhs],
                } if matches!(
                    op,
                    Opcode::Iadd
                        | Opcode::Imul
                        | Opcode::Band
                        | Opcode::Bor
                        | Opcode::Bxor
                        | Opcode::Ishl
                        | Opcode::Ushr
                        | Opcode::Sshr
                ) =>
                {
                    let lhs = func.dfg.resolve_aliases(lhs);
                    let rhs = func.dfg.resolve_aliases(rhs);
                    let dest = func.dfg.first_result(inst);
                    let ty = func.dfg.value_type(dest);
                    if !matches!(ty, types::I8 | types::I16 | types::I32 | types::I64) {
                        continue;
                    }
                    let rc = constant(func, rhs, 1);
                    let lc = if matches!(
                        op,
                        Opcode::Iadd | Opcode::Imul | Opcode::Band | Opcode::Bor | Opcode::Bxor
                    ) {
                        constant(func, lhs, 1)
                    } else {
                        None
                    };
                    let Some((var, c)) = rc.map(|c| (lhs, c)).or_else(|| lc.map(|c| (rhs, c)))
                    else {
                        continue;
                    };
                    match op {
                        Opcode::Iadd => {
                            if let Some(sc) = sign_extend_imm(ty, c) {
                                func.dfg.replace(inst).iadd_imm(var, sc);
                                changed = true;
                            }
                        }
                        Opcode::Imul => {
                            if let Some(sc) = sign_extend_imm(ty, c) {
                                func.dfg.replace(inst).imul_imm(var, sc);
                                changed = true;
                            }
                        }
                        Opcode::Band => {
                            if let Some(sc) = sign_extend_imm(ty, c) {
                                func.dfg.replace(inst).band_imm(var, sc);
                                changed = true;
                            }
                        }
                        Opcode::Bor => {
                            if let Some(sc) = sign_extend_imm(ty, c) {
                                func.dfg.replace(inst).bor_imm(var, sc);
                                changed = true;
                            }
                        }
                        Opcode::Bxor => {
                            if let Some(sc) = sign_extend_imm(ty, c) {
                                func.dfg.replace(inst).bxor_imm(var, sc);
                                changed = true;
                            }
                        }
                        Opcode::Ishl => {
                            func.dfg.replace(inst).ishl_imm(var, c);
                            changed = true;
                        }
                        Opcode::Ushr => {
                            func.dfg.replace(inst).ushr_imm(var, c);
                            changed = true;
                        }
                        Opcode::Sshr => {
                            func.dfg.replace(inst).sshr_imm(var, c);
                            changed = true;
                        }
                        _ => {}
                    }
                }
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond,
                    args: [lhs, rhs],
                } if matches!(
                    cond,
                    ir::condcodes::IntCC::Equal | ir::condcodes::IntCC::NotEqual
                ) =>
                {
                    let lhs = func.dfg.resolve_aliases(lhs);
                    let rhs = func.dfg.resolve_aliases(rhs);
                    let ty = func.dfg.value_type(lhs);
                    if let Some(rc) = constant(func, rhs, 1)
                        && rc != 0
                        && let Some(sc) = sign_extend_imm(ty, rc)
                    {
                        func.dfg.replace(inst).icmp_imm(cond, lhs, sc);
                        changed = true;
                    } else if let Some(lc) = constant(func, lhs, 1)
                        && lc != 0
                        && let Some(sc) = sign_extend_imm(ty, lc)
                    {
                        func.dfg.replace(inst).icmp_imm(cond.swap_args(), rhs, sc);
                        changed = true;
                    }
                }
                InstructionData::BinaryImm64 {
                    opcode: op,
                    arg,
                    imm,
                } if matches!(
                    op,
                    Opcode::IaddImm
                        | Opcode::ImulImm
                        | Opcode::BandImm
                        | Opcode::BorImm
                        | Opcode::BxorImm
                ) =>
                {
                    let arg = func.dfg.resolve_aliases(arg);
                    let dest = func.dfg.first_result(inst);
                    let ty = func.dfg.value_type(dest);
                    let ir::ValueDef::Result(def, 0) = func.dfg.value_def(arg) else {
                        continue;
                    };
                    let InstructionData::BinaryImm64 {
                        opcode: inner_op,
                        arg: inner_arg,
                        imm: inner_imm,
                    } = func.dfg.insts[def]
                    else {
                        continue;
                    };
                    if inner_op != op {
                        continue;
                    }
                    let inner_arg = func.dfg.resolve_aliases(inner_arg);
                    let c1 = inner_imm.bits();
                    let c2 = imm.bits();
                    let combined = match op {
                        Opcode::IaddImm => c1.wrapping_add(c2),
                        Opcode::ImulImm => c1.wrapping_mul(c2),
                        Opcode::BandImm => c1 & c2,
                        Opcode::BorImm => c1 | c2,
                        Opcode::BxorImm => c1 ^ c2,
                        _ => continue,
                    };
                    let Some(sc) = sign_extend_imm(ty, combined) else {
                        continue;
                    };
                    match op {
                        Opcode::IaddImm => {
                            func.dfg.replace(inst).iadd_imm(inner_arg, sc);
                        }
                        Opcode::ImulImm => {
                            func.dfg.replace(inst).imul_imm(inner_arg, sc);
                        }
                        Opcode::BandImm => {
                            func.dfg.replace(inst).band_imm(inner_arg, sc);
                        }
                        Opcode::BorImm => {
                            func.dfg.replace(inst).bor_imm(inner_arg, sc);
                        }
                        Opcode::BxorImm => {
                            func.dfg.replace(inst).bxor_imm(inner_arg, sc);
                        }
                        _ => {}
                    }
                    changed = true;
                }
                _ => {}
            }
        }
        if !changed {
            break;
        }
    }
}

/// Converts dense linear chains of `icmp_imm eq key, k; brif` blocks (>= 4
/// cases, >= 50% density, span <= 4096, zero-arg branch targets) into a
/// Cranelift `br_table` jump table.
fn dense_icmp_chains_to_br_table(func: &mut ir::Function) {
    use cranelift_codegen::ir::condcodes::IntCC;

    let norm_imm = |ty: Type, bits: i64| -> Option<i64> {
        match ty {
            types::I8 => Some(i64::from(bits as u8)),
            types::I16 => Some(i64::from(bits as u16)),
            types::I32 => Some(i64::from(bits as i32)),
            types::I64 => Some(bits),
            _ => None,
        }
    };

    let cfg = ControlFlowGraph::with_function(func);
    let mut use_counts: HashMap<Value, usize> = HashMap::new();
    for (_, inst) in insts(func) {
        for v in values_of(func, inst) {
            *use_counts.entry(func.dfg.resolve_aliases(v)).or_default() += 1;
        }
    }

    let match_brif_eq =
        |func: &ir::Function, blk: Block| -> Option<(Inst, Inst, Value, Type, i64, Block, Block)> {
            let brif_inst = func.layout.last_inst(blk)?;
            let InstructionData::Brif {
                arg: cond,
                blocks: [t_call, f_call],
                ..
            } = func.dfg.insts[brif_inst]
            else {
                return None;
            };
            if t_call.len(&func.dfg.value_lists) != 0 || f_call.len(&func.dfg.value_lists) != 0 {
                return None;
            }
            let cond = func.dfg.resolve_aliases(cond);
            let cmp_inst = func.dfg.value_def(cond).inst()?;
            if func.layout.inst_block(cmp_inst) != Some(blk) {
                return None;
            }
            let (arg, raw_k, is_eq) = match func.dfg.insts[cmp_inst] {
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    cond: IntCC::Equal,
                    arg,
                    imm,
                } => (arg, imm.bits(), true),
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    cond: IntCC::NotEqual,
                    arg,
                    imm,
                } => (arg, imm.bits(), false),
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond,
                    args: [lhs, rhs],
                } if matches!(cond, IntCC::Equal | IntCC::NotEqual) => {
                    let eq = cond == IntCC::Equal;
                    if let Some(c) = constant(func, rhs, 2) {
                        (lhs, c, eq)
                    } else {
                        let c = constant(func, lhs, 2)?;
                        (rhs, c, eq)
                    }
                }
                _ => return None,
            };
            let key = func.dfg.resolve_aliases(arg);
            let key_ty = func.dfg.value_type(key);
            let k = norm_imm(key_ty, raw_k)?;
            let t_blk = t_call.block(&func.dfg.value_lists);
            let f_blk = f_call.block(&func.dfg.value_lists);
            if t_blk == f_blk || t_blk == blk || f_blk == blk {
                return None;
            }
            let (arm_blk, next_blk) = if is_eq {
                (t_blk, f_blk)
            } else {
                (f_blk, t_blk)
            };
            Some((brif_inst, cmp_inst, key, key_ty, k, arm_blk, next_blk))
        };

    let blocks: Vec<Block> = func.layout.blocks().collect();
    let mut removed: HashSet<Block> = HashSet::new();
    for head in blocks {
        if removed.contains(&head) || !func.layout.is_block_inserted(head) {
            continue;
        }
        let Some((head_brif, head_cmp, key, key_ty, k0, arm0, mut cur)) = match_brif_eq(func, head)
        else {
            continue;
        };
        let mut cases: Vec<(i64, Block)> = vec![(k0, arm0)];
        let mut chain_nodes: Vec<Block> = Vec::new();
        let mut seen: HashSet<Block> = HashSet::from([head]);

        while !seen.contains(&cur)
            && !removed.contains(&cur)
            && func.layout.is_block_inserted(cur)
            && cfg.pred_iter(cur).count() == 1
            && func.dfg.block_params(cur).is_empty()
        {
            let Some((brif_inst, cmp_inst, k_val, _, ki, arm_i, next_i)) = match_brif_eq(func, cur)
            else {
                break;
            };
            if k_val != key
                || use_counts
                    .get(&func.dfg.first_result(cmp_inst))
                    .copied()
                    .unwrap_or(0)
                    != 1
            {
                break;
            }
            let only_cmp_and_consts = func.layout.block_insts(cur).all(|i| {
                i == brif_inst
                    || i == cmp_inst
                    || (func.dfg.insts[i].opcode() == Opcode::Iconst
                        && use_counts
                            .get(&func.dfg.first_result(i))
                            .copied()
                            .unwrap_or(0)
                            == 1)
            });
            if !only_cmp_and_consts {
                break;
            }
            seen.insert(cur);
            cases.push((ki, arm_i));
            chain_nodes.push(cur);
            cur = next_i;
        }

        if cases.len() < 4 || seen.contains(&cur) || removed.contains(&cur) {
            continue;
        }
        if cases
            .iter()
            .any(|&(_, a)| seen.contains(&a) || removed.contains(&a))
        {
            continue;
        }
        let default_blk = cur;

        let mut dedup: BTreeMap<i64, Block> = BTreeMap::new();
        for (k, arm) in cases {
            dedup.entry(k).or_insert(arm);
        }
        if dedup.len() < 4 {
            continue;
        }
        let min_k = *dedup.keys().next().unwrap();
        let max_k = *dedup.keys().next_back().unwrap();
        let span_i128 = i128::from(max_k) - i128::from(min_k) + 1;
        if !(4..=4096).contains(&span_i128) || dedup.len() * 2 < span_i128 as usize {
            continue;
        }
        let span = span_i128 as usize;
        let Ok(neg_min) = i64::try_from(-i128::from(min_k)) else {
            continue;
        };
        if (key_ty == types::I8 || key_ty == types::I16) && min_k < 0 {
            continue;
        }

        let mut table = Vec::with_capacity(span);
        for off in 0..span {
            let k = min_k + off as i64;
            let dst = dedup.get(&k).copied().unwrap_or(default_blk);
            table.push(BlockCall::new(dst, [], &mut func.dfg.value_lists));
        }

        let jt_data = JumpTableData::new(
            BlockCall::new(default_blk, [], &mut func.dfg.value_lists),
            &table,
        );
        let mut pos = cranelift_codegen::cursor::FuncCursor::new(func).at_inst(head_brif);
        match key_ty {
            types::I64 => {
                let shifted = if min_k == 0 {
                    key
                } else {
                    pos.ins().iadd_imm(key, neg_min)
                };
                let oob =
                    pos.ins()
                        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, shifted, span as i64);
                let table_blk = pos.func.dfg.make_block();
                pos.func.layout.insert_block_after(table_blk, head);
                pos.ins().brif(oob, default_blk, &[], table_blk, &[]);
                pos.goto_bottom(table_blk);
                let idx32 = pos.ins().ireduce(types::I32, shifted);
                let jt = pos.func.create_jump_table(jt_data);
                pos.ins().br_table(idx32, jt);
            }
            types::I32 => {
                let shifted = if min_k == 0 {
                    key
                } else {
                    pos.ins().iadd_imm(key, neg_min)
                };
                let jt = pos.func.create_jump_table(jt_data);
                pos.ins().br_table(shifted, jt);
            }
            types::I8 | types::I16 => {
                let ext = pos.ins().uextend(types::I32, key);
                let shifted = if min_k == 0 {
                    ext
                } else {
                    pos.ins().iadd_imm(ext, neg_min)
                };
                let jt = pos.func.create_jump_table(jt_data);
                pos.ins().br_table(shifted, jt);
            }
            _ => continue,
        }

        func.layout.remove_inst(head_brif);
        if use_counts
            .get(&func.dfg.first_result(head_cmp))
            .copied()
            .unwrap_or(0)
            == 1
        {
            func.layout.remove_inst(head_cmp);
        }
        for blk in chain_nodes {
            while let Some(inst) = func.layout.first_inst(blk) {
                func.layout.remove_inst(inst);
            }
            func.layout.remove_block(blk);
            removed.insert(blk);
        }
    }
}
