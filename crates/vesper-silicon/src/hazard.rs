//! MMIO access-hazard checks against a [`DeviceModel`] (§26.1 item 4).
//!
//! Two inputs are checked with the same rules:
//!
//! * [`check_ops`]: a driver's register-operation trace ([`MmioOp`]).
//! * [`check_function`]: a Cranelift function whose loads and stores use
//!   constant addresses (an `iconst`, optionally plus `iadd_imm`/`iadd` of
//!   constants, plus the memory op's offset). Addresses that are not
//!   constant are not MMIO as far as this pass can tell and are skipped; the
//!   report counts them so a caller can see what was not analysed.
//!
//! Rules: read-modify-write of a register that has a W1C, W1T or W0C field;
//! a plain read of a register with a read-to-clear or read-side-effect
//! (`readAction=modify`) field outside an explicit consume; a write that sets bits of a read-only field (or any write to a
//! fully read-only register); a read of a write-only register; a value wider
//! than the register.

use std::collections::{BTreeMap, HashMap, HashSet};

use cranelift_codegen::ir::{Block, BlockArg, Function, InstructionData, Opcode, Value, ValueDef};

use crate::VerifierFinding;
use crate::svd::{DeviceModel, RegisterClass, RegisterModel};

pub const RULE_RMW_W1C: &str = "silicon.mmio.rmw_of_write_clear_register";
pub const RULE_RC_READ: &str = "silicon.mmio.read_to_clear_outside_consume";
pub const RULE_WRITE_RO: &str = "silicon.mmio.write_to_read_only";
pub const RULE_READ_WO: &str = "silicon.mmio.read_of_write_only";
pub const RULE_RANGE: &str = "silicon.mmio.value_out_of_range";
pub const RULE_UNKNOWN: &str = "silicon.mmio.unknown_register";

/// One register operation of a driver trace. `reg` is `PERIPHERAL.REGISTER`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MmioOp {
    Read {
        reg: String,
    },
    /// An explicit consuming read of a read-to-clear register.
    Consume {
        reg: String,
    },
    Write {
        reg: String,
        value: u64,
    },
    /// `reg = (reg & !clear) | set`.
    Modify {
        reg: String,
        clear: u64,
        set: u64,
    },
}

fn ro_mask(r: &RegisterModel) -> u64 {
    r.mask_where(|a| !a.writable())
}

fn write_findings(r: &RegisterModel, value: u64, out: &mut Vec<(&'static str, String)>) {
    if value & !r.width_mask() != 0 {
        out.push((
            RULE_RANGE,
            format!("value {value:#x} wider than {}-bit {}", r.size_bits, r.path),
        ));
    }
    if !r.fields.iter().any(|f| f.access.writable()) {
        out.push((
            RULE_WRITE_RO,
            format!("write to read-only register {}", r.path),
        ));
    } else if value & ro_mask(r) != 0 {
        out.push((
            RULE_WRITE_RO,
            format!(
                "write sets read-only bits {:#x} of {}",
                value & ro_mask(r),
                r.path
            ),
        ));
    }
}

fn rmw_finding(r: &RegisterModel) -> Option<(&'static str, String)> {
    let hazard: Vec<&str> = r
        .fields
        .iter()
        .filter(|f| f.access.rmw_hazard())
        .map(|f| f.name.as_str())
        .collect();
    (!hazard.is_empty()).then(|| {
        (
            RULE_RMW_W1C,
            format!(
                "read-modify-write of {} writes back pending bits of write-clear field(s) {}",
                r.path,
                hazard.join(", ")
            ),
        )
    })
}

fn read_findings(r: &RegisterModel, consume: bool, out: &mut Vec<(&'static str, String)>) {
    if r.class() == RegisterClass::WriteOnly {
        out.push((
            RULE_READ_WO,
            format!("read of write-only register {}", r.path),
        ));
    }
    if !consume {
        let consuming: Vec<&str> = r
            .fields
            .iter()
            .filter(|f| f.access.consuming_read())
            .map(|f| f.name.as_str())
            .collect();
        if !consuming.is_empty() {
            out.push((
                RULE_RC_READ,
                format!(
                    "plain read of {} whose read has a side effect on field(s) {}; use an explicit consume",
                    r.path,
                    consuming.join(", ")
                ),
            ));
        }
    }
}

/// Checks a register-operation trace. `inst` in findings is the op index.
pub fn check_ops(model: &DeviceModel, function: &str, ops: &[MmioOp]) -> Vec<VerifierFinding> {
    let mut findings = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let reg = match op {
            MmioOp::Read { reg }
            | MmioOp::Consume { reg }
            | MmioOp::Write { reg, .. }
            | MmioOp::Modify { reg, .. } => reg,
        };
        let mut out = Vec::new();
        match model.register(reg) {
            None => out.push((RULE_UNKNOWN, format!("no register {reg} in {}", model.name))),
            Some(r) => match op {
                MmioOp::Read { .. } => read_findings(r, false, &mut out),
                MmioOp::Consume { .. } => read_findings(r, true, &mut out),
                MmioOp::Write { value, .. } => write_findings(r, *value, &mut out),
                MmioOp::Modify { clear, set, .. } => {
                    read_findings(r, false, &mut out);
                    out.extend(rmw_finding(r));
                    if (clear | set) & !r.width_mask() != 0 {
                        out.push((RULE_RANGE, format!("mask wider than {}", r.path)));
                    }
                    if set & ro_mask(r) != 0 {
                        out.push((
                            RULE_WRITE_RO,
                            format!("modify sets read-only bits of {}", r.path),
                        ));
                    }
                }
            },
        }
        findings.extend(out.into_iter().map(|(rule, message)| VerifierFinding {
            function: function.to_string(),
            inst: u32::try_from(i).ok(),
            rule,
            message,
        }));
    }
    findings
}

/// Which functions may perform consuming reads of which registers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MmioPolicy {
    /// Register path -> functions allowed to read it as a consume.
    pub consumers: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MmioReport {
    pub findings: Vec<VerifierFinding>,
    /// Loads/stores whose address resolved to a modelled register.
    pub mmio_accesses: usize,
    /// Loads/stores whose address was not a constant (not analysed).
    pub non_constant_accesses: usize,
}

fn const_value(func: &Function, v: Value, depth: u32) -> Option<i64> {
    if depth > 16 {
        return None;
    }
    let ValueDef::Result(inst, _) = func.dfg.value_def(func.dfg.resolve_aliases(v)) else {
        return None;
    };
    match &func.dfg.insts[inst] {
        InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } => Some(imm.bits()),
        InstructionData::BinaryImm64 {
            opcode: Opcode::IaddImm,
            arg,
            imm,
        } => const_value(func, *arg, depth + 1)?.checked_add(imm.bits()),
        InstructionData::Binary {
            opcode: Opcode::Iadd,
            args,
        } => const_value(func, args[0], depth + 1)?.checked_add(const_value(
            func,
            args[1],
            depth + 1,
        )?),
        _ => None,
    }
}

/// Incoming branch arguments of every block parameter.
fn block_param_sources(func: &Function) -> HashMap<(Block, usize), Vec<Value>> {
    let mut map: HashMap<(Block, usize), Vec<Value>> = HashMap::new();
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            let dests = func.dfg.insts[inst]
                .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables);
            for call in dests {
                let target = call.block(&func.dfg.value_lists);
                for (i, arg) in call.args(&func.dfg.value_lists).enumerate() {
                    if let BlockArg::Value(v) = arg {
                        map.entry((target, i)).or_default().push(v);
                    }
                }
            }
        }
    }
    map
}

/// Registers whose loaded values flow into `v` (through arithmetic and
/// block parameters).
fn loaded_sources(
    func: &Function,
    v: Value,
    loads: &HashMap<Value, u64>,
    params: &HashMap<(Block, usize), Vec<Value>>,
    out: &mut HashSet<u64>,
) {
    let mut stack = vec![v];
    let mut seen = HashSet::new();
    while let Some(v) = stack.pop() {
        let v = func.dfg.resolve_aliases(v);
        if !seen.insert(v) {
            continue;
        }
        if let Some(&addr) = loads.get(&v) {
            out.insert(addr);
            continue;
        }
        match func.dfg.value_def(v) {
            ValueDef::Result(inst, _) if !func.dfg.insts[inst].opcode().can_load() => {
                stack.extend(func.dfg.inst_args(inst).iter().copied());
            }
            ValueDef::Param(block, i) => {
                if let Some(vs) = params.get(&(block, i)) {
                    stack.extend(vs.iter().copied());
                }
            }
            _ => {}
        }
    }
}

/// A memory access: every load and store opcode, including extending loads
/// and truncating stores, with the width actually accessed.
struct Access {
    store: Option<Value>,
    address: Option<u64>,
    bits: u32,
}

fn access(func: &Function, inst: cranelift_codegen::ir::Inst) -> Option<Access> {
    let resolve = |base: Value, offset: i32| {
        const_value(func, base, 0)
            .and_then(|a| a.checked_add(i64::from(offset)))
            .map(|a| a as u64)
    };
    match &func.dfg.insts[inst] {
        InstructionData::Load {
            opcode,
            arg,
            offset,
            ..
        } => {
            let bits = match opcode {
                Opcode::Uload8 | Opcode::Sload8 => 8,
                Opcode::Uload16 | Opcode::Sload16 => 16,
                Opcode::Uload32 | Opcode::Sload32 => 32,
                _ => func.dfg.value_type(func.dfg.first_result(inst)).bits(),
            };
            Some(Access {
                store: None,
                address: resolve(*arg, i32::from(*offset)),
                bits,
            })
        }
        InstructionData::Store {
            opcode,
            args,
            offset,
            ..
        } => {
            let bits = match opcode {
                Opcode::Istore8 => 8,
                Opcode::Istore16 => 16,
                Opcode::Istore32 => 32,
                _ => func.dfg.value_type(args[0]).bits(),
            };
            Some(Access {
                store: Some(args[0]),
                address: resolve(args[1], i32::from(*offset)),
                bits,
            })
        }
        _ => None,
    }
}

/// Checks the constant-address loads and stores of one Cranelift function.
pub fn check_function(func: &Function, model: &DeviceModel, policy: &MmioPolicy) -> MmioReport {
    let name = crate::stack::symbol_of_user_func(&func.name);
    let mut report = MmioReport::default();
    let params = block_param_sources(func);
    // All constant-address loads first, so dependencies through blocks that
    // come later in the layout are seen too.
    let mut loads: HashMap<Value, u64> = HashMap::new();
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            if let Some(Access {
                store: None,
                address: Some(a),
                ..
            }) = access(func, inst)
            {
                loads.insert(func.dfg.first_result(inst), a);
            }
        }
    }
    for block in func.layout.blocks() {
        for inst in func.layout.block_insts(block) {
            let Some(acc) = access(func, inst) else {
                continue;
            };
            let idx = Some(inst.as_u32());
            let mut out = Vec::new();
            let Some(addr) = acc.address else {
                report.non_constant_accesses += 1;
                continue;
            };
            let what = if acc.store.is_some() {
                "store to"
            } else {
                "load from"
            };
            let Some(r) = model.register_at(addr) else {
                out.push((RULE_UNKNOWN, format!("{what} unmodelled address {addr:#x}")));
                push(&mut report, &name, idx, out);
                continue;
            };
            report.mmio_accesses += 1;
            match acc.store {
                None => {
                    let consumer = policy
                        .consumers
                        .get(&r.path)
                        .is_some_and(|fs| fs.contains(&name));
                    read_findings(r, consumer, &mut out);
                }
                Some(value) => {
                    let mut sources = HashSet::new();
                    loaded_sources(func, value, &loads, &params, &mut sources);
                    if sources.contains(&addr) {
                        out.extend(rmw_finding(r));
                    }
                    if let Some(v) = const_value(func, value, 0) {
                        let v = if acc.bits >= 64 {
                            v as u64
                        } else {
                            (v as u64) & ((1u64 << acc.bits) - 1)
                        };
                        write_findings(r, v, &mut out);
                    } else if !r.fields.iter().any(|f| f.access.writable()) {
                        out.push((
                            RULE_WRITE_RO,
                            format!("write to read-only register {}", r.path),
                        ));
                    }
                }
            }
            push(&mut report, &name, idx, out);
        }
    }
    report
}

fn push(report: &mut MmioReport, name: &str, inst: Option<u32>, out: Vec<(&'static str, String)>) {
    report
        .findings
        .extend(out.into_iter().map(|(rule, message)| VerifierFinding {
            function: name.to_string(),
            inst,
            rule,
            message,
        }));
}
