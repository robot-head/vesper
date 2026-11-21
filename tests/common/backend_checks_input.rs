//! The input of `@vesper/backend-checks` (SH4 lane 17, #1291): the emitted
//! Cranelift functions of one build, the world facts the verifiers read, the
//! capability facts, the policy and the source, as little-endian u64 words
//! in one `Str` (the layout is documented in
//! packages/vesper-backend-checks/src/input.ves).
//!
//! Everything Cranelift computes is input here: the instruction tables, the
//! opcode properties, the machine code the stack verifier measures (compiled
//! with the object ISA, exactly as `native_verifiers::measure_emitted` does)
//! and `verify_function`'s verdict. The verifiers' own logic is not.
#![allow(dead_code)]

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::{
    self, BlockArg, ExternalName, Function, InstructionData as D, Opcode, Value, ValueDef,
    instructions::CallInfo,
};
use cranelift_codegen::{Context, FinalizedRelocTarget};
use std::collections::{BTreeSet, HashMap};
use vesper::capability_effects::{self, Capability};
use vesper::check::World;
use vesper::native::{self, EmittedFunction};
use vesper::native_verifiers::{NativeVerifier, NativeVerifierPolicy};

/// Interned strings: index 0 is the empty string, as in `@vesper/check-data`'s
/// `Names`, and indices are the Vesper symbols.
#[derive(Default)]
pub struct Enc {
    strs: Vec<String>,
    map: HashMap<String, u64>,
    pub w: Vec<u64>,
}

impl Enc {
    pub fn new() -> Self {
        let mut e = Self::default();
        e.s("");
        e
    }
    pub fn s(&mut self, s: &str) -> u64 {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let i = self.strs.len() as u64;
        self.strs.push(s.to_string());
        self.map.insert(s.to_string(), i);
        i
    }
    fn put(&mut self, x: u64) {
        self.w.push(x);
    }
    fn str(&mut self, s: &str) {
        let i = self.s(s);
        self.put(i);
    }
    fn strs<'a>(&mut self, list: impl IntoIterator<Item = &'a String>) {
        let v: Vec<u64> = list.into_iter().map(|s| self.s(s)).collect();
        self.put(v.len() as u64);
        self.w.extend(v);
    }
    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.put(1);
                self.str(s);
            }
            None => {
                self.put(0);
                self.put(0);
            }
        }
    }
    fn list(&mut self, v: &[u64]) {
        self.put(v.len() as u64);
        self.w.extend_from_slice(v);
    }
    /// The string table, then the words.
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        let push = |out: &mut Vec<u8>, x: u64| out.extend_from_slice(&x.to_le_bytes());
        push(&mut out, self.strs.len() as u64);
        for s in &self.strs {
            push(&mut out, s.len() as u64);
            let mut b = s.as_bytes().to_vec();
            b.resize(s.len().div_ceil(8) * 8, 0);
            out.extend_from_slice(&b);
        }
        push(&mut out, self.w.len() as u64);
        for x in &self.w {
            push(&mut out, *x);
        }
        out
    }
}

pub fn verifier_mask(set: &BTreeSet<NativeVerifier>) -> u64 {
    set.iter()
        .map(|v| match v {
            NativeVerifier::ConstTime => 1,
            NativeVerifier::Stack => 2,
            NativeVerifier::Realtime => 4,
            NativeVerifier::DetFloat => 8,
        })
        .sum()
}

fn cap_mask(caps: &BTreeSet<Capability>) -> u64 {
    caps.iter()
        .map(|c| match c {
            Capability::ConstTime => 1,
            Capability::Realtime => 2,
            Capability::DetFloat => 4,
            Capability::ConstDenorm => 8,
        })
        .sum()
}

// ------------------------------------------------------------------ opcodes

/// The opcodes the verifiers distinguish; code = position + 1 (0: any other).
/// Mirrored by the `OP_*` constants of clif.ves.
pub const OPS: &[Opcode] = &[
    Opcode::Brif,
    Opcode::BrTable,
    Opcode::Udiv,
    Opcode::Sdiv,
    Opcode::Urem,
    Opcode::Srem,
    Opcode::StackAddr,
    Opcode::StackLoad,
    Opcode::StackStore,
    Opcode::AtomicRmw,
    Opcode::AtomicCas,
    Opcode::Fma,
    Opcode::Fadd,
    Opcode::Fsub,
    Opcode::Fmul,
    Opcode::Fdiv,
    Opcode::Sqrt,
    Opcode::Fmin,
    Opcode::Fmax,
    Opcode::Ceil,
    Opcode::Floor,
    Opcode::Trunc,
    Opcode::Nearest,
    Opcode::Fpromote,
    Opcode::Fdemote,
    Opcode::Fvdemote,
    Opcode::FvpromoteLow,
    Opcode::Extractlane,
    Opcode::Select,
    Opcode::SelectSpectreGuard,
    Opcode::Bitselect,
    Opcode::Splat,
    Opcode::ScalarToVector,
    Opcode::Vconst,
    Opcode::Bitcast,
    Opcode::Store,
    Opcode::AtomicStore,
    Opcode::Return,
];

fn op_code(op: Opcode) -> u64 {
    OPS.iter()
        .position(|o| *o == op)
        .map_or(0, |i| i as u64 + 1)
}

/// Instruction formats the verifiers match on (mirrored by `FMT_*`).
fn format_code(d: &D) -> u64 {
    match d {
        D::Load { .. } => 1,
        D::LoadNoOffset { .. } => 2,
        D::Store { .. } => 3,
        D::StoreNoOffset { .. } => 4,
        D::StackLoad { .. } => 5,
        D::StackStore { .. } => 6,
        D::DynamicStackLoad { .. } => 7,
        D::DynamicStackStore { .. } => 8,
        D::Call { .. } => 9,
        D::CallIndirect { .. } => 10,
        D::TryCall { .. } => 11,
        D::TryCallIndirect { .. } => 12,
        D::BinaryImm8 { .. } => 13,
        D::Binary { .. } => 14,
        D::UnaryIeee32 { .. } => 15,
        D::UnaryIeee64 { .. } => 16,
        D::Unary { .. } => 17,
        D::UnaryConst { .. } => 18,
        D::FloatCompare { .. } => 19,
        _ => 0,
    }
}

fn type_word(t: ir::Type) -> u64 {
    let lane = t.lane_type();
    u64::from(lane.bits()) | (u64::from(t.lane_count()) << 16) | (u64::from(lane.is_float()) << 40)
}

fn sig_has_float(sig: &ir::Signature) -> bool {
    sig.params
        .iter()
        .chain(&sig.returns)
        .any(|p| p.value_type.lane_type().is_float())
}

fn ext_name(e: &mut Enc, func: &Function, name: &ExternalName) {
    let display = name.display(Some(&func.params)).to_string();
    match name {
        ExternalName::User(r) => {
            let u = &func.params.user_named_funcs()[*r];
            e.w.extend([0, u64::from(u.namespace), u64::from(u.index)]);
            e.str(&display);
            e.str("");
        }
        ExternalName::TestCase(t) => {
            e.w.extend([1, 0, 0]);
            e.str(&display);
            e.str(&t.to_string());
        }
        ExternalName::LibCall(l) => {
            e.w.extend([2, 0, 0]);
            e.str(&display);
            e.str(&l.to_string());
        }
        ExternalName::KnownSymbol(k) => {
            e.w.extend([3, 0, 0]);
            e.str(&display);
            e.str(&format!("{k}"));
            e.str(&format!("{k:?}"));
            return;
        }
    }
    e.str("");
}

/// One function's CLIF tables (clif.ves documents the layout).
pub fn clif(e: &mut Enc, func: &Function) {
    let dfg = &func.dfg;
    e.str(&func.name.to_string());
    let nv = dfg.num_values();
    e.put(nv as u64);
    for i in 0..nv {
        let v = Value::new(i);
        let def = match dfg.value_def(v) {
            ValueDef::Result(inst, _) => inst.as_u32() as u64 + 1,
            _ => 0,
        };
        e.w.extend([
            dfg.resolve_aliases(v).as_u32() as u64,
            type_word(dfg.value_type(v)),
            def,
        ]);
    }
    let slots: Vec<u64> = func
        .sized_stack_slots
        .iter()
        .map(|(_, s)| u64::from(s.size))
        .collect();
    e.list(&slots);
    let blocks: Vec<ir::Block> = func.layout.blocks().collect();
    let pos: HashMap<ir::Block, u64> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (*b, i as u64))
        .collect();
    e.put(blocks.len() as u64);
    for b in &blocks {
        let params: Vec<u64> = dfg
            .block_params(*b)
            .iter()
            .map(|v| v.as_u32() as u64)
            .collect();
        e.list(&params);
        let insts: Vec<ir::Inst> = func.layout.block_insts(*b).collect();
        e.put(insts.len() as u64);
        for inst in insts {
            let d = &dfg.insts[inst];
            let op = d.opcode();
            let flags = u64::from(op.is_call())
                | u64::from(op.can_load()) << 1
                | u64::from(op.can_store()) << 2
                | u64::from(op.can_trap()) << 3;
            e.w.extend([inst.as_u32() as u64, op_code(op), flags, format_code(d)]);
            e.str(&op.to_string());
            let args: Vec<u64> = dfg
                .inst_args(inst)
                .iter()
                .map(|v| v.as_u32() as u64)
                .collect();
            e.list(&args);
            let results: Vec<u64> = dfg
                .inst_results(inst)
                .iter()
                .map(|v| v.as_u32() as u64)
                .collect();
            e.list(&results);
            // Extras: stack slot, immediate, float condition, constant bytes.
            let slot = match d {
                D::StackLoad { stack_slot, .. } | D::StackStore { stack_slot, .. } => {
                    stack_slot.as_u32() as u64
                }
                _ => 0,
            };
            let imm = match d {
                D::BinaryImm8 { imm, .. } => u64::from(*imm),
                D::UnaryIeee32 { imm, .. } => u64::from(imm.bits()),
                D::UnaryIeee64 { imm, .. } => imm.bits(),
                _ => 0,
            };
            let cond = match d {
                D::FloatCompare { cond, .. } => {
                    use cranelift_codegen::ir::condcodes::FloatCC;
                    match cond {
                        FloatCC::Unordered => 1,
                        FloatCC::NotEqual => 2,
                        FloatCC::Ordered => 3,
                        FloatCC::Equal => 4,
                        _ => 0,
                    }
                }
                _ => 0,
            };
            e.w.extend([slot, imm, cond]);
            match d {
                D::UnaryConst {
                    constant_handle, ..
                } => {
                    let bytes = dfg.constants.get(*constant_handle).as_slice();
                    let words: Vec<u64> = bytes
                        .chunks(8)
                        .map(|c| {
                            let mut b = [0u8; 8];
                            b[..c.len()].copy_from_slice(c);
                            u64::from_le_bytes(b)
                        })
                        .collect();
                    e.put(bytes.len() as u64);
                    e.list(&words);
                }
                _ => {
                    e.put(0);
                    e.put(0);
                }
            }
            match d.analyze_call(&dfg.value_lists, &dfg.exception_tables) {
                CallInfo::Direct(fr, cargs) | CallInfo::DirectWithSig(fr, _, cargs) => {
                    e.put(1);
                    let c: Vec<u64> = cargs.iter().map(|v| v.as_u32() as u64).collect();
                    e.list(&c);
                    let ext = &dfg.ext_funcs[fr];
                    e.put(u64::from(sig_has_float(&dfg.signatures[ext.signature])));
                    ext_name(e, func, &ext.name);
                }
                CallInfo::Indirect(sig, cargs) => {
                    e.put(2);
                    let c: Vec<u64> = cargs.iter().map(|v| v.as_u32() as u64).collect();
                    e.list(&c);
                    e.put(u64::from(sig_has_float(&dfg.signatures[sig])));
                }
                CallInfo::NotACall => e.put(0),
            }
            // Silicon reads the callee of Call/TryCall straight from the data.
            match d {
                D::Call { func_ref, .. } | D::TryCall { func_ref, .. } => {
                    e.put(1);
                    ext_name(e, func, &dfg.ext_funcs[*func_ref].name);
                }
                _ => e.put(0),
            }
            let dests = d.branch_destination(&dfg.jump_tables, &dfg.exception_tables);
            e.put(dests.len() as u64);
            for call in dests {
                e.put(pos[&call.block(&dfg.value_lists)]);
                let args: Vec<BlockArg> = call.args(&dfg.value_lists).collect();
                e.put(args.len() as u64);
                for a in args {
                    match a {
                        BlockArg::Value(v) => e.w.extend([0, v.as_u32() as u64]),
                        BlockArg::TryCallRet(_) => e.w.extend([1, 0]),
                        BlockArg::TryCallExn(_) => e.w.extend([2, 0]),
                    }
                }
            }
            match native::source_offset(func, inst) {
                Some(o) => e.w.extend([1, o as u64]),
                None => e.w.extend([0, 0]),
            }
        }
    }
    e.put(dfg.num_insts() as u64);
}

// ------------------------------------------------------------------ the case

/// What one run asks for.
pub enum Mode<'a> {
    /// `verify_emitted(world, functions, target, verifiers, policy)`.
    Emitted(&'a BTreeSet<NativeVerifier>, &'a NativeVerifierPolicy),
    /// `verify_native_with_capabilities`'s verification half: `requested`
    /// plus the capability verifiers under the derived policy over `base`.
    Capabilities(&'a BTreeSet<NativeVerifier>, &'a NativeVerifierPolicy),
}

fn policy(e: &mut Enc, p: &NativeVerifierPolicy) {
    e.put(p.const_time.len() as u64);
    for (k, c) in &p.const_time {
        e.str(k);
        e.strs(&c.secret);
        e.strs(&c.declassify);
        e.strs(&c.constant_time_callees);
    }
    e.put(p.stack.roots.len() as u64);
    for (k, b) in &p.stack.roots {
        e.str(k);
        e.put(u64::from(*b));
    }
    e.put(p.stack.external_bounds.len() as u64);
    for (k, b) in &p.stack.external_bounds {
        e.str(k);
        e.put(u64::from(*b));
    }
    let rt = &p.realtime;
    e.put(rt.roots.len() as u64);
    for (k, b) in &rt.roots {
        e.str(k);
        e.w.extend([u64::from(b.is_some()), b.unwrap_or(0)]);
    }
    match &rt.allowed_effects {
        Some(list) => {
            e.put(1);
            e.strs(list);
        }
        None => e.w.extend([0, 0]),
    }
    e.put(rt.symbols.len() as u64);
    for (k, s) in &rt.symbols {
        e.str(k);
        e.strs(&s.effects);
        e.put(s.frame_bytes);
    }
    e.put(rt.effects.len() as u64);
    for (k, list) in &rt.effects {
        e.str(k);
        e.strs(list);
    }
    e.put(rt.loop_bounds.len() as u64);
    for (k, b) in &rt.loop_bounds {
        e.str(k);
        e.put(*b);
    }
    e.w.extend([
        u64::from(rt.reject_unbounded_loops),
        u64::from(rt.max_loop_iterations.is_some()),
        rt.max_loop_iterations.unwrap_or(0),
    ]);
    let d = &p.det_float;
    match &d.functions {
        Some(list) => {
            e.put(1);
            e.strs(list);
        }
        None => e.w.extend([0, 0]),
    }
    e.put(u64::from(d.allow_fma));
    e.strs(&d.allowed_float_calls);
    e.opt_str(d.reduction_order.as_deref());
}

/// Encodes one verification run (input.ves documents the sections).
pub fn encode(
    world: &World,
    functions: &[EmittedFunction],
    target: Option<&str>,
    mode: &Mode,
    file: &str,
    source: &str,
) -> Vec<u8> {
    let mut e = Enc::new();
    let (kind, requested, base) = match mode {
        Mode::Emitted(v, p) => (0, *v, *p),
        Mode::Capabilities(v, p) => (1, *v, *p),
    };
    e.put(kind);
    e.put(verifier_mask(requested));
    e.str(file);
    e.str(source);
    e.put(u64::from(native::needs_nan_canonicalization(world)));
    // The world's functions, in `world.functions` order.
    e.put(world.functions.len() as u64);
    for f in world.functions.values() {
        e.str(&f.name);
        e.w.extend([f.span.start as u64, f.span.end as u64]);
        e.put(cap_mask(&capability_effects::capabilities(&f.effects)));
        e.put(cap_mask(&native::closure_capabilities(f)));
        // `native_verifiers::not_emitted`'s "specialized into its callers".
        let specialized = f.effects.tail.is_some()
            || f.effects
                .instances
                .iter()
                .any(|x| !capability_effects::is_capability_effect(&x.name))
            || f.params
                .iter()
                .any(|p| matches!(p.ty, vesper::ast::Type::Function { .. }))
            || matches!(f.result, vesper::ast::Type::Function { .. });
        e.put(u64::from(specialized));
        e.put(u64::from(capability_effects::is_declassify_gate(&f.name)));
        let params: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
        e.strs(&params);
    }
    // Capability facts (`capability_policies`), for mode 1.
    if kind == 1 {
        let checked = vesper::check::Checked {
            world: world.clone(),
            diagnostics: Vec::new(),
            expression_types: Default::default(),
        };
        let caps = capability_effects::capability_policies(&checked).unwrap();
        e.put(caps.const_time.len() as u64);
        for c in &caps.const_time {
            e.str(&c.function);
            let idx: Vec<u64> = c.secret_params.iter().map(|&i| i as u64).collect();
            e.list(&idx);
            e.strs(&c.declassify_callees);
            e.strs(&c.constant_time_callees);
        }
        e.put(caps.realtime.functions.len() as u64);
        for r in &caps.realtime.functions {
            e.str(&r.function);
            e.w.extend([
                u64::from(r.stack_budget_bytes.is_some()),
                r.stack_budget_bytes.unwrap_or(0),
            ]);
        }
        e.put(u64::from(caps.realtime.reject_unbounded_loops));
        e.put(caps.det_float.len() as u64);
        for d in &caps.det_float {
            e.str(&d.function);
            e.strs(&d.allowed_float_calls);
            e.put(u64::from(d.allow_fma));
        }
    }
    policy(&mut e, base);
    // The ISA the stack verifier measures with.
    let optimized = functions.iter().any(|f| f.optimized);
    let isa = native::object_isa_optimized(world, target, optimized);
    match &isa {
        Ok(isa) => {
            e.put(0);
            match isa.triple().architecture {
                target_lexicon::Architecture::X86_64 => e.put(0),
                target_lexicon::Architecture::Aarch64(_) => e.put(1),
                _ => e.put(2),
            }
            e.str(&isa.triple().architecture.to_string());
        }
        Err(msg) => {
            e.put(1);
            e.put(0);
            e.str(msg);
        }
    }
    // The emitted functions.
    e.put(functions.len() as u64);
    for f in functions {
        e.str(&f.key);
        e.str(&f.source);
        e.str(&f.symbol);
        e.put(u64::from(f.func_id));
        e.put(u64::from(f.optimized));
        e.strs(&f.inlined);
        e.put(f.callees.len() as u64);
        for (id, sym) in &f.callees {
            e.put(u64::from(*id));
            e.str(sym);
        }
        let bound: Vec<String> = native::function_arguments(&f.vars).into_values().collect();
        e.strs(&bound);
        // The instance's parameter layouts (const-time).
        match f.instance(world) {
            Err(msg) => {
                e.put(1);
                e.str(&msg);
            }
            Ok(inst) => {
                e.put(0);
                match vesper::layout::Layout::of(world, &inst.result) {
                    Ok(l) => {
                        e.put(u64::from(l.aggregate()));
                        e.str("");
                    }
                    Err(msg) => {
                        e.put(2);
                        e.str(&msg);
                    }
                }
                e.put(inst.params.len() as u64);
                for p in &inst.params {
                    e.str(&p.name);
                    if p.pass == vesper::ast::Pass::Inout {
                        e.put(1);
                        e.str("");
                    } else {
                        match vesper::layout::Layout::of(world, &p.ty) {
                            Ok(l) => {
                                e.put(u64::from(l.aggregate()));
                                e.str("");
                            }
                            Err(msg) => {
                                e.put(2);
                                e.str(&msg);
                            }
                        }
                    }
                }
            }
        }
        // Cranelift's verdict on the function (Kinetic's audit runs it).
        let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
        match cranelift_codegen::verify_function(&f.function, &flags) {
            Ok(()) => {
                e.put(0);
                e.str("");
            }
            Err(err) => {
                e.put(1);
                e.str(&err.to_string());
            }
        }
        // The machine code the stack verifier measures.
        match &isa {
            Ok(isa) => {
                let mut ctx = Context::for_function(f.function.clone());
                match ctx.compile(&**isa, &mut ControlPlane::default()) {
                    Ok(compiled) => {
                        let code = compiled.code_buffer();
                        e.put(0);
                        e.put(code.len() as u64);
                        let words: Vec<u64> = code
                            .chunks(8)
                            .map(|c| {
                                let mut b = [0u8; 8];
                                b[..c.len()].copy_from_slice(c);
                                u64::from_le_bytes(b)
                            })
                            .collect();
                        e.list(&words);
                        e.put(u64::from(compiled.frame_size));
                        let mut libcalls = BTreeSet::new();
                        for reloc in compiled.buffer.relocs() {
                            if let FinalizedRelocTarget::ExternalName(ExternalName::LibCall(lc)) =
                                &reloc.target
                            {
                                libcalls.insert(format!("libcall:{lc}"));
                            }
                        }
                        e.strs(&libcalls);
                    }
                    Err(err) => {
                        e.put(1);
                        e.str(&format!("{:?}", err.inner));
                    }
                }
            }
            Err(_) => e.put(2),
        }
        clif(&mut e, &f.function);
    }
    e.finish()
}

// ------------------------------------------------------------------ reports

/// A report as the Vesper package prints it: `render`'s text, then one line
/// per report field (lib.ves `dump`).
pub fn dump(
    report: &vesper::native_verifiers::NativeVerifierReport,
    file: &str,
    source: &str,
) -> String {
    let mut out = vesper::native_verifiers::render(report, file, source);
    out.push_str("--\n");
    let names: Vec<&str> = report.verifiers.iter().map(|v| v.name()).collect();
    out.push_str(&format!("verifiers {}\n", names.join(",")));
    out.push_str(&format!("functions {}\n", report.functions));
    for n in &report.notes {
        out.push_str(&format!("note {n}\n"));
    }
    for f in &report.findings {
        let opt = |x: Option<usize>| x.map_or("-".to_string(), |v| v.to_string());
        out.push_str(&format!(
            "finding {} {} {} {} {} {} {}\n",
            f.verifier.name(),
            f.function,
            f.inst.map_or("-".to_string(), |v| v.to_string()),
            f.rule,
            f.span.map_or("-".to_string(), |(a, b)| format!("{a}..{b}")),
            opt(f.location),
            f.message
        ));
    }
    if let Some(s) = &report.stack {
        for r in &s.roots {
            out.push_str(&format!(
                "root {} {} {} {} {}\n",
                r.root,
                r.budget_bytes,
                r.worst_case_bytes
                    .map_or("-".to_string(), |v| v.to_string()),
                u8::from(r.within_budget),
                r.worst_path.join(" -> ")
            ));
        }
        for f in &s.findings {
            out.push_str(&format!(
                "stack {} {} {} {}\n",
                f.function,
                f.inst.map_or("-".to_string(), |v| v.to_string()),
                f.rule,
                f.message
            ));
        }
    }
    out
}

/// `verify_emitted` / the verification half of
/// `verify_native_with_capabilities`, as the dump or `error <msg>`.
pub fn rust_text(
    world: &World,
    functions: &[EmittedFunction],
    target: Option<&str>,
    mode: &Mode,
    file: &str,
    source: &str,
) -> String {
    use vesper::native_verifiers as nv;
    let result = match mode {
        Mode::Emitted(v, p) => nv::verify_emitted(world, functions, target, v, p),
        Mode::Capabilities(requested, base) => (|| {
            let mut verifiers = (*requested).clone();
            verifiers.extend(nv::capability_verifiers(world));
            if verifiers.is_empty() {
                return Ok(nv::NativeVerifierReport {
                    verifiers,
                    functions: 0,
                    findings: Vec::new(),
                    stack: None,
                    notes: Vec::new(),
                });
            }
            // `derive_policy` with `explicit_det_float_all`: an explicit
            // `--verify=detfloat` over a base without `det_float.functions`
            // audits every emitted function.
            let mut derived = nv::policy_from_capabilities(world, functions, base)?;
            if requested.contains(&NativeVerifier::DetFloat) && base.det_float.functions.is_none() {
                derived.policy.det_float.functions = None;
            }
            let mut report =
                nv::verify_emitted(world, functions, target, &verifiers, &derived.policy)?;
            report.notes.extend(derived.notes);
            report.findings.splice(0..0, derived.findings);
            Ok(report)
        })(),
    };
    match result {
        Ok(r) => dump(&r, file, source),
        Err(e) => format!("error {e}\n"),
    }
}
