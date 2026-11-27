//! The cold outliner's extraction (#2634): the helper and the parent's stub
//! built from `@vesper/native-cold-outline`'s decisions, the
//! [`E_OUTLINE`] ops of [`crate::clif_words`].
//!
//! This is `apply_extraction` of src/native_cold_outline.rs (its
//! `build_extraction` and `Cloner`), copied here so Stage 2, which has no
//! Rust compiler, runs it through `vcl_outline_cold` (`seam.rs`) and the
//! Cranelift component. The compiler keeps its copy until Stage 2 retires
//! it; tests/self_host_native_cold_outline.rs compares the two. The root
//! crate's differential includes this file by path, so it uses only
//! `cranelift_codegen`, `cranelift_module` and `crate::clif_words`.

use std::collections::{BTreeMap, BTreeSet};

use crate::clif_words::{E_END, E_OUTLINE, E_SKIP, Script};
use cranelift_codegen::ir::instructions::InstructionData;
use cranelift_codegen::ir::{
    self, AbiParam, Block, BlockArg, BlockCall, ExtFuncData, ExternalName, GlobalValueData, Inst,
    JumpTableData, MemFlags, Opcode, StackSlot, TrapCode, Type, ValueList,
};
use cranelift_module::{FuncId, Module};

/// Every decision of one outlining (the analysis half of the extraction):
/// what `@vesper/native-cold-outline` answers with, and what
/// [`apply_extraction`] builds the helper and the parent's stub from.
pub struct Extraction {
    /// The region's root, which becomes the stub.
    pub root: ir::Block,
    /// The blocks `root` dominates; removed from the parent.
    pub owned: BTreeSet<ir::Block>,
    pub has_return: bool,
    pub first_trap: Option<TrapCode>,
    /// The helper's leading parameters, in order.
    pub live_ins: Vec<ir::Value>,
    /// Values the helper recomputes at its entry, with their definitions.
    pub remat: Vec<(ir::Value, ir::Inst)>,
    /// Slots the parent keeps and passes by address, then the slots moved.
    pub shared_slots: Vec<StackSlot>,
    pub exclusive_slots: Vec<StackSlot>,
    /// The region's other blocks, in reverse post-order from `root`.
    pub ordered_blocks: Vec<ir::Block>,
}

/// Builds and verifies outlining number `idx` of `func` (its helper is
/// `{parent_key}::cold#{idx}`) and puts the stub in `func`.
pub fn apply_extraction<M: Module + ?Sized>(
    m: &mut M,
    parent_key: &str,
    idx: usize,
    func: &mut ir::Function,
    plan: &Extraction,
) -> Result<(FuncId, String, ir::Function), String> {
    let cold_key = format!("{parent_key}::cold#{idx}");
    let (cold_id, cold_func) = build_extraction(m, func, plan)?;
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    if let Err(errors) = cranelift_codegen::verify_function(&cold_func, &flags) {
        return Err(format!(
            "cold outline '{cold_key}' verifier errors:\n{errors}\n{}",
            cold_func.display()
        ));
    }
    Ok((cold_id, cold_key, cold_func))
}

fn build_extraction<M: Module + ?Sized>(
    m: &mut M,
    func: &mut ir::Function,
    plan: &Extraction,
) -> Result<(FuncId, ir::Function), String> {
    let root = plan.root;
    let live_ins = plan.live_ins.clone();
    let shared_slots = plan.shared_slots.clone();
    let exclusive_slots = plan.exclusive_slots.clone();
    let ordered_blocks = plan.ordered_blocks.clone();
    let remat_insts = plan.remat.clone();
    let ptr_ty = m.target_config().pointer_type();
    let mut cold_sig = m.make_signature();
    cold_sig.call_conv = func.signature.call_conv;
    for &v in &live_ins {
        cold_sig.params.push(AbiParam::new(func.dfg.value_type(v)));
    }
    for _ in &shared_slots {
        cold_sig.params.push(AbiParam::new(ptr_ty));
    }
    if plan.has_return {
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

    if plan.has_return {
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
        let code = plan.first_trap.unwrap_or_else(|| TrapCode::unwrap_user(1));
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
    for &b in &plan.owned {
        if b == root {
            continue;
        }
        let insts: Vec<ir::Inst> = func.layout.block_insts(b).collect();
        for inst in insts {
            func.layout.remove_inst(inst);
        }
        func.layout.remove_block(b);
    }

    Ok((cold_id, cold_func))
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

/// Replays the cold outliner's script on `func`: each [`E_OUTLINE`] goes
/// through [`apply_extraction`]. Returns the helpers.
pub fn apply_outline<M: Module + ?Sized>(
    m: &mut M,
    parent_key: &str,
    func: &mut ir::Function,
    words: &[u64],
) -> Result<Vec<(FuncId, String, ir::Function)>, String> {
    let mut s = Script::new(words);
    let mut out = vec![];
    let blocks = |s: &mut Script| -> Result<Vec<Block>, String> {
        let n = s.word()?;
        (0..n).map(|_| s.block()).collect()
    };
    let slots = |s: &mut Script| -> Result<Vec<ir::StackSlot>, String> {
        let n = s.word()?;
        (0..n)
            .map(|_| Ok(ir::StackSlot::from_u32(s.word()? as u32)))
            .collect()
    };
    loop {
        match s.word()? {
            E_END => return Ok(out),
            E_SKIP => {}
            E_OUTLINE => {
                let idx = s.word()? as usize;
                let root = s.block()?;
                let owned = blocks(&mut s)?.into_iter().collect();
                let _shared = blocks(&mut s)?;
                let has_return = s.word()? != 0;
                let trap = s.word()?;
                let first_trap = std::num::NonZeroU8::new(trap as u8).map(ir::TrapCode::from_raw);
                let live_ins = s.values()?;
                let n = s.word()?;
                let mut remat = vec![];
                for _ in 0..n {
                    let v = s.value()?;
                    remat.push((v, Inst::from_u32(s.word()? as u32)));
                }
                let shared_slots = slots(&mut s)?;
                let exclusive_slots = slots(&mut s)?;
                let ordered_blocks = blocks(&mut s)?;
                let plan = Extraction {
                    root,
                    owned,
                    has_return,
                    first_trap,
                    live_ins,
                    remat,
                    shared_slots,
                    exclusive_slots,
                    ordered_blocks,
                };
                out.push(apply_extraction(m, parent_key, idx, func, &plan)?);
            }
            op => return Err(format!("outline op {op}")),
        }
    }
}
