//! §23.5 Stage 5 constant-time SSA taint verifier over real Cranelift IR.
//!
//! The verifier takes a `cranelift_codegen::ir::Function` (the workspace's
//! Cranelift 0.125) and a [`CtPolicy`] naming which inputs are secret, because
//! the source language cannot yet spell `SecU64` parameters or
//! `eff [ConstTime]` on a Cranelift function. It computes a monotone
//! forward taint fixpoint over the whole function, including block parameters
//! fed by `jump`, `brif` and `br_table` arguments around loops, then reports
//! every instruction whose timing or address trace could depend on a secret:
//!
//! | rule | rejected shape |
//! | --- | --- |
//! | `secret-branch` | `brif` on a secret condition |
//! | `secret-branch-table` | `br_table` on a secret index |
//! | `secret-load-address` | load whose address is secret-derived |
//! | `secret-store-address` | store whose address is secret-derived |
//! | `variable-latency-div` | `udiv`/`sdiv`/`urem`/`srem` with a secret operand |
//! | `secret-call-unknown` | call passing a secret (or a pointer to secret memory) to a callee the policy does not trust |
//! | `secret-call-target` | `call_indirect` through a secret callee address |
//! | `secret-trap` | any other trapping instruction (`trapz`, `trapnz`, `*_overflow_trap`, trapping float conversions) with a secret operand |
//! | `unsupported-secret-operand` | an instruction format the verifier does not model receiving a secret |
//! | `invalid-policy` | the policy names a parameter the function does not have |
//!
//! `select`, `select_spectre_guard`, `bitselect`, bitwise, shift, rotate,
//! add, sub and mul propagate taint and are accepted. A direct call to a
//! function the policy lists in [`CtPolicy::declassify`] (by default
//! [`DECLASSIFY_MARKER`]) is the explicit, auditable declassification
//! boundary: its results are public.
//!
//! # Memory model
//!
//! Every value carries two facts: whether it is secret, and which tracked
//! memory it may point into (a policy-named secret buffer, a Cranelift stack
//! slot, or untracked memory). A load yields a secret when its address may
//! point into a secret buffer, into a stack slot that ever received a secret,
//! into untracked memory after any secret was stored to untracked memory, or
//! when its `MemFlags` alias region is listed in [`CtPolicy::secret_regions`].
//! Memory facts are flow-insensitive (once secret, always secret), which is
//! sound for the function body. Pointers reloaded from memory lose their
//! provenance and are treated as untracked, so secret buffers must be passed
//! as policy-named parameters. A callee that receives a secret (or access to
//! one) is assumed to write secrets through every pointer argument it gets,
//! since callees carry no memory-effect summaries.
//!
//! # What this does and does not establish
//!
//! Acceptance means the analysed Cranelift IR has no secret-dependent
//! control flow, no secret-dependent addresses, no secret operand to an
//! integer divider and no secret passed to an untrusted callee. It says
//! nothing about the machine code a backend emits from it (for example
//! whether `select` lowers to `cmov`/`csel`), about callees the policy trusts,
//! or about microarchitectural channels beyond those rules. Multiplication and
//! variable shifts are treated as constant time, which holds on the x86_64 and
//! AArch64 cores Vesper targets but not on every CPU.

use std::collections::BTreeSet;

use cranelift_codegen::entity::EntityRef;

use cranelift_codegen::ir::{
    AliasRegion, BlockArg, Function, Inst, InstructionData, Opcode, Value, instructions::CallInfo,
};

/// Contract-wide finding shape shared by the wave-five backend verifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierFinding {
    /// Cranelift function name as displayed by `Function::name`.
    pub function: String,
    /// Cranelift instruction index (`Inst::as_u32`) when the finding has one.
    pub inst: Option<u32>,
    /// Stable rule identifier, for example `secret-branch`.
    pub rule: &'static str,
    /// Human-readable explanation.
    pub message: String,
}

/// Name (as displayed by Cranelift with function parameters) of the default
/// explicit declassification marker. A test case function declared as
/// `ExternalName::testcase("vesper_ct_declassify")` displays as this string.
pub const DECLASSIFY_MARKER: &str = "%vesper_ct_declassify";

/// The secret annotations the source language cannot yet express.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtPolicy {
    /// Entry-block parameter indices whose values are secret scalars.
    pub secret_params: Vec<usize>,
    /// Entry-block parameter indices that are public pointers into secret
    /// memory (a key schedule, a tag buffer): loads through addresses derived
    /// from them produce secrets.
    pub secret_pointer_params: Vec<usize>,
    /// Loads whose `MemFlags` alias region is listed here produce secrets.
    pub secret_regions: Vec<AliasRegion>,
    /// Callees (Cranelift display names, such as `%vesper_ct_declassify` or
    /// `u0:7`) whose results are explicitly declassified.
    pub declassify: Vec<String>,
    /// Callees trusted to be constant time: they may receive secrets, and
    /// their results are secret when any argument is.
    pub constant_time_callees: Vec<String>,
}

impl Default for CtPolicy {
    fn default() -> Self {
        Self {
            secret_params: Vec::new(),
            secret_pointer_params: Vec::new(),
            secret_regions: Vec::new(),
            declassify: vec![DECLASSIFY_MARKER.to_string()],
            constant_time_callees: Vec::new(),
        }
    }
}

impl CtPolicy {
    /// A policy marking the given entry parameters secret, with the default
    /// declassification marker.
    pub fn secret_params(params: impl IntoIterator<Item = usize>) -> Self {
        Self {
            secret_params: params.into_iter().collect(),
            ..Self::default()
        }
    }
}

const PTS_UNTRACKED: u8 = 1;
const PTS_SECRET_BUF: u8 = 2;
/// Points into a stack slot whose index does not fit in the 64-bit slot mask.
const PTS_HIGH_SLOT: u8 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Abs {
    secret: bool,
    pts: u8,
    slots: u64,
}

impl Abs {
    const PUBLIC: Abs = Abs {
        secret: false,
        pts: 0,
        slots: 0,
    };

    fn join(self, other: Abs) -> Abs {
        Abs {
            secret: self.secret || other.secret,
            pts: self.pts | other.pts,
            slots: self.slots | other.slots,
        }
    }

    /// May this value, used as an address, point into memory holding secrets?
    fn points_to_secret_memory(self, mem: &Memory) -> bool {
        (self.pts & PTS_SECRET_BUF) != 0
            || (self.slots & mem.secret_slots) != 0
            || ((self.pts & PTS_HIGH_SLOT) != 0 && mem.secret_high_slot)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Memory {
    untracked_secret: bool,
    secret_slots: u64,
    secret_high_slot: bool,
}

struct Analysis<'a> {
    func: &'a Function,
    policy: &'a CtPolicy,
    vals: Vec<Abs>,
    mem: Memory,
}

/// Run the constant-time verifier. An empty result means the function is
/// accepted under the rules in the module documentation.
pub fn verify_constant_time(func: &Function, policy: &CtPolicy) -> Vec<VerifierFinding> {
    let fname = func.name.to_string();
    let Some(entry) = func.layout.entry_block() else {
        return Vec::new();
    };
    let params = func.dfg.block_params(entry);
    let mut findings = Vec::new();
    for (what, list) in [
        ("secret_params", &policy.secret_params),
        ("secret_pointer_params", &policy.secret_pointer_params),
    ] {
        for &i in list {
            if i >= params.len() {
                findings.push(VerifierFinding {
                    function: fname.clone(),
                    inst: None,
                    rule: "invalid-policy",
                    message: format!(
                        "{what} names parameter {i}, but the entry block has {} parameters",
                        params.len()
                    ),
                });
            }
        }
    }
    if !findings.is_empty() {
        return findings;
    }

    let mut an = Analysis {
        func,
        policy,
        vals: vec![Abs::PUBLIC; func.dfg.num_values()],
        mem: Memory::default(),
    };
    for &i in &policy.secret_params {
        let v = params[i];
        an.vals[v.index()].secret = true;
    }
    for &i in &policy.secret_pointer_params {
        let v = params[i];
        an.vals[v.index()].pts |= PTS_SECRET_BUF;
    }
    an.fixpoint();
    an.check(&fname, &mut findings);
    findings
}

impl Analysis<'_> {
    fn get(&self, v: Value) -> Abs {
        let v = self.func.dfg.resolve_aliases(v);
        self.vals[v.index()]
    }

    fn join_into(&mut self, v: Value, a: Abs) -> bool {
        let v = self.func.dfg.resolve_aliases(v);
        let slot = &mut self.vals[v.index()];
        let joined = slot.join(a);
        if joined != *slot {
            *slot = joined;
            true
        } else {
            false
        }
    }

    fn callee_name(&self, func_ref: cranelift_codegen::ir::FuncRef) -> String {
        self.func.dfg.ext_funcs[func_ref]
            .name
            .display(Some(&self.func.params))
            .to_string()
    }

    fn any_secret(&self, args: &[Value]) -> bool {
        args.iter().any(|&a| self.get(a).secret)
    }

    /// Does this argument hand a callee secret data or access to it?
    fn exposes_secret(&self, v: Value) -> bool {
        let a = self.get(v);
        a.secret
            || a.points_to_secret_memory(&self.mem)
            || (self.mem.untracked_secret && Self::untracked(a))
    }

    /// May this value address untracked memory? Plain integers count: once a
    /// secret is stored to untracked memory, any untracked pointer may reach it.
    fn untracked(a: Abs) -> bool {
        a.pts & PTS_UNTRACKED != 0 || (a.pts == 0 && a.slots == 0)
    }

    /// A callee that receives secrets may write them through any pointer it
    /// is given. Callees carry no memory-effect summaries, so every pointer
    /// argument's memory becomes secret.
    fn record_callee_writes(&mut self, cargs: &[Value]) -> bool {
        if !cargs.iter().any(|&a| self.exposes_secret(a)) {
            return false;
        }
        let secret = Abs {
            secret: true,
            pts: 0,
            slots: 0,
        };
        let mut changed = false;
        for &a in cargs {
            let addr = self.get(a);
            changed |= self.record_store(addr, secret);
        }
        changed
    }

    fn load_result(&self, data: &InstructionData, addr: Abs) -> Abs {
        let mut secret = addr.points_to_secret_memory(&self.mem);
        if Self::untracked(addr) && self.mem.untracked_secret {
            secret = true;
        }
        if let Some(flags) = data.memflags()
            && let Some(region) = flags.alias_region()
            && self.policy.secret_regions.contains(&region)
        {
            secret = true;
        }
        Abs {
            secret,
            pts: 0,
            slots: 0,
        }
    }

    fn record_store(&mut self, addr: Abs, val: Abs) -> bool {
        if !val.secret {
            return false;
        }
        let before = self.mem;
        if Self::untracked(addr) {
            self.mem.untracked_secret = true;
        }
        self.mem.secret_slots |= addr.slots;
        if addr.pts & PTS_HIGH_SLOT != 0 {
            self.mem.secret_high_slot = true;
        }
        before != self.mem
    }

    fn slot_abs(slot: cranelift_codegen::ir::StackSlot) -> Abs {
        let i = slot.as_u32();
        if i < 64 {
            Abs {
                secret: false,
                pts: 0,
                slots: 1u64 << i,
            }
        } else {
            Abs {
                secret: false,
                pts: PTS_HIGH_SLOT,
                slots: 0,
            }
        }
    }

    /// Abstract results of one instruction under the current state, plus any
    /// memory update it causes.
    fn transfer(&mut self, inst: Inst) -> (Abs, bool) {
        let dfg = &self.func.dfg;
        let data = &dfg.insts[inst];
        let op = data.opcode();
        let args = dfg.inst_args(inst);
        let mut mem_changed = false;
        let res = match (op, data) {
            (Opcode::StackAddr, InstructionData::StackLoad { stack_slot, .. }) => {
                Self::slot_abs(*stack_slot)
            }
            (Opcode::StackLoad, InstructionData::StackLoad { stack_slot, .. }) => {
                self.load_result(data, Self::slot_abs(*stack_slot))
            }
            (
                Opcode::StackStore,
                InstructionData::StackStore {
                    arg, stack_slot, ..
                },
            ) => {
                let addr = Self::slot_abs(*stack_slot);
                let val = self.get(*arg);
                mem_changed = self.record_store(addr, val);
                Abs::PUBLIC
            }
            (_, InstructionData::Load { arg, .. } | InstructionData::LoadNoOffset { arg, .. })
                if op.can_load() =>
            {
                let addr = self.get(*arg);
                self.load_result(data, addr)
            }
            (
                _,
                InstructionData::Store { args, .. } | InstructionData::StoreNoOffset { args, .. },
            ) if op.can_store() => {
                let val = self.get(args[0]);
                let addr = self.get(args[1]);
                mem_changed = self.record_store(addr, val);
                Abs::PUBLIC
            }
            (Opcode::AtomicRmw | Opcode::AtomicCas, _) => {
                let addr = self.get(args[0]);
                let mut val = Abs::PUBLIC;
                for &a in &args[1..] {
                    val = val.join(self.get(a));
                }
                mem_changed = self.record_store(addr, val);
                let mut r = self.load_result(data, addr);
                r.secret |= val.secret;
                r
            }
            _ if op.is_call() => match data.analyze_call(&dfg.value_lists, &dfg.exception_tables) {
                CallInfo::Direct(fr, cargs) | CallInfo::DirectWithSig(fr, _, cargs) => {
                    let name = self.callee_name(fr);
                    if self.policy.declassify.contains(&name) {
                        Abs::PUBLIC
                    } else {
                        let secret = cargs.iter().any(|&a| self.exposes_secret(a));
                        mem_changed = self.record_callee_writes(cargs);
                        Abs {
                            secret,
                            pts: 0,
                            slots: 0,
                        }
                    }
                }
                CallInfo::Indirect(_, cargs) => {
                    let secret = cargs.iter().any(|&a| self.exposes_secret(a))
                        || self.any_secret(&args[..1]);
                    mem_changed = self.record_callee_writes(cargs);
                    Abs {
                        secret,
                        pts: 0,
                        slots: 0,
                    }
                }
                CallInfo::NotACall => {
                    let mut r = Abs::PUBLIC;
                    for &a in args {
                        r = r.join(self.get(a));
                    }
                    r
                }
            },
            _ => {
                // Pure data flow: secret if any input is, and pointer
                // provenance flows through address arithmetic.
                let mut r = Abs::PUBLIC;
                for &a in args {
                    r = r.join(self.get(a));
                }
                r
            }
        };
        (res, mem_changed)
    }

    fn fixpoint(&mut self) {
        let func = self.func;
        loop {
            let mut changed = false;
            for block in func.layout.blocks() {
                for inst in func.layout.block_insts(block) {
                    let (res, mem_changed) = self.transfer(inst);
                    changed |= mem_changed;
                    for &r in func.dfg.inst_results(inst) {
                        changed |= self.join_into(r, res);
                    }
                    let data = &func.dfg.insts[inst];
                    for call in
                        data.branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                    {
                        let dest = call.block(&func.dfg.value_lists);
                        let dest_params = func.dfg.block_params(dest).to_vec();
                        for (arg, param) in call.args(&func.dfg.value_lists).zip(dest_params) {
                            let a = match arg {
                                BlockArg::Value(v) => self.get(v),
                                // Normal returns of a try_call carry the call's
                                // result taint; exception payloads are runtime
                                // values the callee chose, treated as public.
                                BlockArg::TryCallRet(_) => res,
                                BlockArg::TryCallExn(_) => Abs::PUBLIC,
                            };
                            changed |= self.join_into(param, a);
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn check(&self, fname: &str, out: &mut Vec<VerifierFinding>) {
        let func = self.func;
        let dfg = &func.dfg;
        let mut seen: BTreeSet<(u32, &'static str)> = BTreeSet::new();
        let mut push = |inst: Inst, rule: &'static str, message: String| {
            if seen.insert((inst.as_u32(), rule)) {
                out.push(VerifierFinding {
                    function: fname.to_string(),
                    inst: Some(inst.as_u32()),
                    rule,
                    message,
                });
            }
        };
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                let data = &dfg.insts[inst];
                let op = data.opcode();
                let args = dfg.inst_args(inst);
                let show = |v: Value| format!("{v}");
                match (op, data) {
                    (Opcode::Brif, _) => {
                        if self.get(args[0]).secret {
                            push(
                                inst,
                                "secret-branch",
                                format!("brif on secret condition {}", show(args[0])),
                            );
                        }
                    }
                    (Opcode::BrTable, _) => {
                        if self.get(args[0]).secret {
                            push(
                                inst,
                                "secret-branch-table",
                                format!("br_table on secret index {}", show(args[0])),
                            );
                        }
                    }
                    (Opcode::Udiv | Opcode::Sdiv | Opcode::Urem | Opcode::Srem, _) => {
                        for &a in args {
                            if self.get(a).secret {
                                push(
                                    inst,
                                    "variable-latency-div",
                                    format!("{op} has secret operand {}", show(a)),
                                );
                            }
                        }
                    }
                    (Opcode::StackAddr | Opcode::StackLoad | Opcode::StackStore, _) => {}
                    (
                        _,
                        InstructionData::Load { arg, .. }
                        | InstructionData::LoadNoOffset { arg, .. },
                    ) if op.can_load() => {
                        if self.get(*arg).secret {
                            push(
                                inst,
                                "secret-load-address",
                                format!("{op} address {} is secret-derived", show(*arg)),
                            );
                        }
                    }
                    (
                        _,
                        InstructionData::Store { args, .. }
                        | InstructionData::StoreNoOffset { args, .. },
                    ) if op.can_store() => {
                        if self.get(args[1]).secret {
                            push(
                                inst,
                                "secret-store-address",
                                format!("{op} address {} is secret-derived", show(args[1])),
                            );
                        }
                    }
                    (Opcode::AtomicRmw | Opcode::AtomicCas, _) => {
                        if self.get(args[0]).secret {
                            push(
                                inst,
                                "secret-store-address",
                                format!("{op} address {} is secret-derived", show(args[0])),
                            );
                        }
                    }
                    _ if op.is_call() => {
                        match data.analyze_call(&dfg.value_lists, &dfg.exception_tables) {
                            CallInfo::Direct(fr, cargs) | CallInfo::DirectWithSig(fr, _, cargs) => {
                                let name = self.callee_name(fr);
                                let trusted = self.policy.declassify.contains(&name)
                                    || self.policy.constant_time_callees.contains(&name);
                                if !trusted {
                                    for &a in cargs {
                                        if self.exposes_secret(a) {
                                            push(
                                                inst,
                                                "secret-call-unknown",
                                                format!(
                                                    "{op} {name} receives secret {} but is not a trusted constant-time callee",
                                                    show(a)
                                                ),
                                            );
                                        }
                                    }
                                }
                            }
                            CallInfo::Indirect(_, cargs) => {
                                if self.get(args[0]).secret {
                                    push(
                                        inst,
                                        "secret-call-target",
                                        format!("{op} target {} is secret", show(args[0])),
                                    );
                                }
                                for &a in cargs {
                                    if self.exposes_secret(a) {
                                        push(
                                            inst,
                                            "secret-call-unknown",
                                            format!(
                                                "{op} passes secret {} to an unknown callee",
                                                show(a)
                                            ),
                                        );
                                    }
                                }
                            }
                            CallInfo::NotACall => {
                                if self.any_secret(args) {
                                    push(
                                        inst,
                                        "unsupported-secret-operand",
                                        format!("{op} is not modelled with secret operands"),
                                    );
                                }
                            }
                        }
                    }
                    _ if op.can_trap() => {
                        for &a in args {
                            if self.get(a).secret {
                                push(
                                    inst,
                                    "secret-trap",
                                    format!("{op} may trap depending on secret {}", show(a)),
                                );
                            }
                        }
                    }
                    (
                        _,
                        InstructionData::DynamicStackLoad { .. }
                        | InstructionData::DynamicStackStore { .. },
                    ) => {
                        if self.any_secret(args) {
                            push(
                                inst,
                                "unsupported-secret-operand",
                                format!("{op} is not modelled with secret operands"),
                            );
                        }
                    }
                    _ if (op.can_load() || op.can_store()) && self.any_secret(args) => {
                        push(
                            inst,
                            "unsupported-secret-operand",
                            format!("memory operation {op} is not modelled with secret operands"),
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}
