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
//! Each step keeps the function valid CLIF on its own.
use cranelift_codegen::cursor::Cursor as _;
use cranelift_codegen::dominator_tree::DominatorTree;
use cranelift_codegen::flowgraph::ControlFlowGraph;
use cranelift_codegen::ir::{
    self, Block, BlockArg, Inst, InstBuilder, InstructionData, Opcode, StackSlot, StackSlotKind,
    Type, Value, types,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Run every step on `func`.
pub fn run(func: &mut ir::Function) {
    if std::env::var_os("VESPER_CLIF_OPT").is_some_and(|v| v == "off") {
        return;
    }
    remove_unreachable(func);
    merge_blocks(func);
    remove_constant_params(func);
    split_join_slots(func);
    promote_slots(func);
    remove_constant_params(func);
    sink_compares(func);
    thread_branches(func);
    remove_unreachable(func);
    remove_dead(func);
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
pub fn remove_constant_params(func: &mut ir::Function) {
    let entry = func.layout.entry_block();
    for _round in 0..16 {
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
                let mut only: Option<Value> = None;
                let mut ok = true;
                for &(inst, k) in incoming {
                    let call = func.dfg.insts[inst]
                        .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)[k];
                    match call.args(&func.dfg.value_lists).nth(i) {
                        Some(BlockArg::Value(v)) => {
                            let v = func.dfg.resolve_aliases(v);
                            if v == p {
                                continue;
                            }
                            match only {
                                None => only = Some(v),
                                Some(o) if o == v => {}
                                Some(_) => ok = false,
                            }
                        }
                        _ => ok = false,
                    }
                    if !ok {
                        break;
                    }
                }
                let (true, Some(a)) = (ok, only) else {
                    continue;
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
                func.dfg.change_to_alias(p, a);
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
        return;
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
            // Every edge passes a distinct whole slot, used only in its block.
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
                let only_here = used_in
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
            // The parameter: read and written at constant offsets only.
            let mut through: HashMap<Value, ((), i64)> = HashMap::new();
            through.insert(p, ((), 0));
            derive(func, &all, &mut through);
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
            // Each source block stores before it reads its slot.
            for &(inst, slot, _) in &sources {
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
            // The analysis above is stale for this block now; one
            // parameter per join per run.
            break;
        }
    }
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
    if depth > 8 {
        return None;
    }
    let v = func.dfg.resolve_aliases(v);
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
        } => constant(func, arg, depth + 1).map(mask),
        InstructionData::IntCompareImm {
            opcode: Opcode::IcmpImm,
            cond,
            arg,
            imm,
        } => {
            let a = constant(func, arg, depth + 1)?;
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
            let a = constant(func, args[0], depth + 1)?;
            let b = constant(func, args[1], depth + 1)?;
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
