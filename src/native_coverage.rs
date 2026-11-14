//! Native coverage instrumentation for Vesper Crucible (§29.1.1, §7 Stage 5;
//! wave-six lane 23, issue #265).
//!
//! When `vesper fuzz --native-coverage` asks for it, the Cranelift functions
//! [`crate::native`] emits are rewritten through the backend's
//! [`crate::native::Instrument`] hook before machine code is generated:
//!
//! * **Edge counters.** Every basic block starts with an AFL-style edge
//!   update: `map[cur ^ prev] += 1; prev = cur >> 1`, where `cur` is a
//!   stable 13-bit label of `(instance key, block index)`. The map, the
//!   `prev` slot and the comparison log live in one host-owned buffer whose
//!   absolute address is an immediate in the emitted code.
//! * **Comparison-operand logging.** Before every scalar `icmp`/`icmp_imm`
//!   of at most 64 bits, the site label and both operands (zero-extended to
//!   64 bits) are appended to a bounded log, branch-free: once the log is
//!   full, records go to one scratch slot and only the count grows.
//!
//! Builds that do not pass the hook emit identical code; the
//! instrumented-vs-plain identity test compiles every example for x86_64
//! and AArch64 and compares object bytes.
//!
//! The fuzz target is called through a synthesized zero-argument wrapper
//! that handles each of the target's `Fail[..]` instances, returning `0` on
//! return and `k + 1` when instance `k` is raised. Its arguments come from
//! argument stubs (`__vesper_fuzz_arg_<i>`) whose bodies this module
//! replaces with a load from the buffer, so one compiled module serves the
//! whole campaign. Edge counts become Crucible [`Probe`] edges (corpus
//! selection) and logged operands become [`Probe::cmp`] records (the
//! input-to-state solver), translated to the tape encodings of the target's
//! parameters.
//!
//! Running instrumented code is only reachable through [`NativeCoverage`],
//! which owns the buffer; [`object_with_coverage`] emits object bytes that
//! are never executed in-process.
//!
//! * **Fuel and stack bound** (lane 23, #346). Every instrumented block
//!   also decrements a fuel word, and every function entry compares the
//!   stack pointer with a host-chosen limit. When either runs out the
//!   function stores an "exhausted" flag and returns zeros; every block it
//!   reaches afterwards does the same, so the run ends after at most the
//!   rest of the current block, and [`NativeCoverage::run`] reports
//!   [`NativeOutcome::Exhausted`] instead of hanging or overflowing the
//!   stack. Results of an exhausted run are discarded.
//! * **`select` and `br_table`** conditions and indices are logged as
//!   comparison records too, because they hide branches from edge coverage.
//! * **Source locations.** Comparison and edge sites carry the source byte
//!   offset the backend attached to their instruction
//!   ([`crate::native::source_offset`]).
//!
//! Limits: effects other than unlabeled `Fail[..]` are rejected;
//! instrumentation of an object file is for code-generation validation
//! only, because the buffer address is a host address.
use crate::{
    ast::{Function, Type},
    check::{self, World},
    native::{Instrument, InstrumentedJit},
    parser,
};
use cranelift_codegen::{
    cursor::{Cursor, FuncCursor},
    ir::{self, InstBuilder, InstructionData, MemFlags, Opcode, condcodes::IntCC, types},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use vesper_crucible::{CmpKind, Probe};

/// Edge map slots (a power of two).
pub const EDGE_SLOTS: usize = 1 << 13;
/// Comparison records kept per run; later comparisons are counted only.
pub const MAX_CMP_LOG: usize = 1024;
/// Most target parameters.
pub const MAX_ARGS: usize = 32;

/// Default fuel: instrumented block entries per run.
pub const DEFAULT_FUEL: u64 = 1_000_000;
/// Default stack budget per run, below the stack pointer at the call.
pub const DEFAULT_STACK_BUDGET: u64 = 512 * 1024;

const PREV: usize = 0;
const CMP_COUNT: usize = 1;
const FUEL: usize = 2;
const EXHAUSTED: usize = 3;
const STACK_HIT: usize = 4;
const STACK_LIMIT: usize = 5;
const RESULT: usize = 6;
const ARGS: usize = 7;
const CMP_LOG: usize = ARGS + MAX_ARGS;
/// Words per comparison record: site, lhs, rhs.
const RECORD: usize = 3;
const EDGES: usize = CMP_LOG + RECORD * (MAX_CMP_LOG + 1);
const WORDS: usize = EDGES + EDGE_SLOTS;

/// Prefix of the synthesized wrapper and argument stubs.
pub const STUB_PREFIX: &str = "__vesper_fuzz_";
const CASE: &str = "__vesper_fuzz_case";

/// The host-owned buffer the instrumented code writes. Words are atomics,
/// so the host reads and resets them between runs through shared
/// references while compiled code stores to the same addresses during a
/// run (which only happens inside `NativeCoverage::run`).
pub struct CoverageBuffer {
    words: Box<[AtomicU64]>,
}

impl Default for CoverageBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl CoverageBuffer {
    pub fn new() -> Self {
        Self {
            words: (0..WORDS).map(|_| AtomicU64::new(0)).collect(),
        }
    }
    /// Absolute address of word 0 (stable: the words are boxed).
    pub fn base(&self) -> i64 {
        self.words.as_ptr() as i64
    }
    /// Clear edges, `prev`, the comparison log, the exhaustion flags and
    /// the result before a run.
    pub fn reset(&self) {
        for w in [PREV, CMP_COUNT, EXHAUSTED, STACK_HIT, RESULT] {
            self.words[w].store(0, Ordering::Relaxed);
        }
        for w in &self.words[EDGES..] {
            w.store(0, Ordering::Relaxed);
        }
    }
    /// Arm the fuel bound and the lowest stack address code may run at.
    fn arm(&self, fuel: u64, stack_limit: u64) {
        self.words[FUEL].store(fuel, Ordering::Relaxed);
        self.words[STACK_LIMIT].store(stack_limit, Ordering::Relaxed);
    }
    /// Fuel left after the last run.
    pub fn fuel_left(&self) -> u64 {
        self.words[FUEL].load(Ordering::Relaxed)
    }
    /// Whether the last run ran out of fuel or stack.
    pub fn exhausted(&self) -> bool {
        self.words[EXHAUSTED].load(Ordering::Relaxed) != 0
    }
    fn stack_hit(&self) -> bool {
        self.words[STACK_HIT].load(Ordering::Relaxed) != 0
    }
    fn result(&self) -> u64 {
        self.words[RESULT].load(Ordering::Relaxed)
    }
    fn set_arg(&self, i: usize, value: u64) {
        self.words[ARGS + i].store(value, Ordering::Relaxed);
    }
    /// Non-zero edge slots and their hit counts.
    pub fn edges(&self) -> Vec<(u32, u64)> {
        self.words[EDGES..]
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                let hits = w.load(Ordering::Relaxed);
                (hits != 0).then_some((i as u32, hits))
            })
            .collect()
    }
    /// Comparisons executed in the last run (including unlogged ones).
    pub fn comparisons_executed(&self) -> u64 {
        self.words[CMP_COUNT].load(Ordering::Relaxed)
    }
    /// Logged `(site, lhs, rhs)` records of the last run.
    pub fn comparisons(&self) -> Vec<(u32, u64, u64)> {
        let n = (self.comparisons_executed() as usize).min(MAX_CMP_LOG);
        (0..n)
            .map(|i| {
                let at = CMP_LOG + RECORD * i;
                (
                    self.words[at].load(Ordering::Relaxed) as u32,
                    self.words[at + 1].load(Ordering::Relaxed),
                    self.words[at + 2].load(Ordering::Relaxed),
                )
            })
            .collect()
    }
}

/// The instruction a comparison site logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteOp {
    /// `icmp`/`icmp_imm`: both operands.
    Compare,
    /// `select c, x, y`: the condition against zero (`Ne`).
    Select,
    /// `br_table index, table`: the index against the table length (`Lt`);
    /// [`NativeCoverage::feed`] also reports `index == k` for each case.
    BranchTable { cases: u32 },
}

/// What one instrumented comparison site compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmpSite {
    /// Instance key of the function containing the comparison.
    pub function: String,
    pub kind: CmpKind,
    pub signed: bool,
    /// Operand width in bits.
    pub bits: u32,
    pub op: SiteOp,
    /// Source byte offset of the instruction, when the backend attached one.
    pub location: Option<usize>,
}

/// One instrumented block: its edge-map label and source location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeSite {
    /// Instance key of the function containing the block.
    pub function: String,
    /// Block index in layout order.
    pub block: usize,
    /// The block's 13-bit label (`cur` in the edge update).
    pub label: u32,
    /// Source byte offset of the block's first original instruction.
    pub location: Option<usize>,
}

/// Map the outcome of a signed ordering comparison onto
/// [`CmpKind`]'s unsigned evaluation. Operands are zero-extended from
/// `bits`; when their sign bits agree, signed and unsigned order agree, and
/// when they differ the unsigned order is reversed, so `Lt` becomes `Gt`
/// and `Le` becomes `Ge` (and back). The operands are unchanged, so the
/// input-to-state solver still sees the values the code compared.
pub fn unsigned_view(kind: CmpKind, signed: bool, bits: u32, lhs: u64, rhs: u64) -> CmpKind {
    if !signed || bits == 0 || bits > 64 {
        return kind;
    }
    let sign = 1u64 << (bits - 1);
    if (lhs & sign) == (rhs & sign) {
        return kind;
    }
    match kind {
        CmpKind::Lt => CmpKind::Gt,
        CmpKind::Le => CmpKind::Ge,
        CmpKind::Gt => CmpKind::Lt,
        CmpKind::Ge => CmpKind::Le,
        other => other,
    }
}

/// Counts of what [`Instrumenter`] inserted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstrumentStats {
    pub functions: usize,
    pub edges: usize,
    pub comparisons: usize,
    pub stubs: usize,
    /// Result sinks replaced (0 or 1).
    pub sinks: usize,
    /// `select` and `br_table` sites (also counted in `comparisons`).
    pub selects: usize,
    pub branch_tables: usize,
    /// Blocks guarded by the fuel check.
    pub fuel_checks: usize,
}

/// The rewrite applied through [`crate::native::Instrument`].
pub struct Instrumenter {
    base: i64,
    /// Argument stub key -> argument index.
    stubs: BTreeMap<String, usize>,
    /// Result sink key (its argument is stored in the result word).
    sink: Option<String>,
    sites: Vec<CmpSite>,
    edge_sites: Vec<EdgeSite>,
    stats: InstrumentStats,
}

fn cmp_kind(cond: IntCC) -> (CmpKind, bool) {
    match cond {
        IntCC::Equal => (CmpKind::Eq, false),
        IntCC::NotEqual => (CmpKind::Ne, false),
        IntCC::UnsignedLessThan => (CmpKind::Lt, false),
        IntCC::UnsignedLessThanOrEqual => (CmpKind::Le, false),
        IntCC::UnsignedGreaterThan => (CmpKind::Gt, false),
        IntCC::UnsignedGreaterThanOrEqual => (CmpKind::Ge, false),
        IntCC::SignedLessThan => (CmpKind::Lt, true),
        IntCC::SignedLessThanOrEqual => (CmpKind::Le, true),
        IntCC::SignedGreaterThan => (CmpKind::Gt, true),
        IntCC::SignedGreaterThanOrEqual => (CmpKind::Ge, true),
    }
}

/// Stable edge-map label of block `index` of instance `key`.
fn block_label(key: &str, index: usize) -> i64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(key.as_bytes());
    hasher.update(&(index as u64).to_le_bytes());
    let digest = hasher.finalize();
    let word = u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("8 bytes"));
    (word as usize & (EDGE_SLOTS - 1)) as i64
}

fn word(index: usize) -> i32 {
    (index * 8) as i32
}

impl Instrumenter {
    /// Instrument every function, writing to the buffer at `base`.
    pub fn new(base: i64) -> Self {
        Self {
            base,
            stubs: BTreeMap::new(),
            sink: None,
            sites: Vec::new(),
            edge_sites: Vec::new(),
            stats: InstrumentStats::default(),
        }
    }
    /// Replace the body of `key`, a `fn(T) -> i64` with one scalar
    /// parameter, with a store of that parameter to the result word.
    pub fn with_result_sink(mut self, key: &str) -> Self {
        self.sink = Some(key.to_string());
        self
    }
    pub fn edge_sites(&self) -> &[EdgeSite] {
        &self.edge_sites
    }
    /// Also replace the body of argument stub `key` with a load of argument
    /// `index` from the buffer.
    pub fn with_arg_stub(mut self, key: &str, index: usize) -> Self {
        self.stubs.insert(key.to_string(), index);
        self
    }
    pub fn sites(&self) -> &[CmpSite] {
        &self.sites
    }
    pub fn stats(&self) -> InstrumentStats {
        self.stats
    }

    /// The [`crate::native::Instrument`] body.
    pub fn rewrite(&mut self, key: &str, func: &mut ir::Function) -> Result<(), String> {
        if let Some(&index) = self.stubs.get(key) {
            self.stats.stubs += 1;
            return self.replace_stub(func, index);
        }
        if self.sink.as_deref() == Some(key) {
            self.stats.sinks += 1;
            return self.replace_sink(func);
        }
        self.stats.functions += 1;
        let blocks: Vec<ir::Block> = func.layout.blocks().collect();
        let mut original = Vec::new();
        for &block in &blocks {
            for inst in func.layout.block_insts(block) {
                original.push(inst);
            }
        }
        let firsts: Vec<Option<ir::Inst>> =
            blocks.iter().map(|&b| func.layout.first_inst(b)).collect();
        let mut pos = FuncCursor::new(func);
        for inst in original {
            let location = crate::native::source_offset(pos.func, inst);
            // (kind, signed, op, lhs, rhs immediate, rhs value, width)
            let (kind, signed, op, lhs, rhs_imm, rhs, ty) = match pos.func.dfg.insts[inst] {
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    args,
                    cond,
                } => {
                    let (kind, signed) = cmp_kind(cond);
                    let ty = pos.func.dfg.value_type(args[0]);
                    (
                        kind,
                        signed,
                        SiteOp::Compare,
                        args[0],
                        None,
                        Some(args[1]),
                        ty,
                    )
                }
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    arg,
                    cond,
                    imm,
                } => {
                    let (kind, signed) = cmp_kind(cond);
                    let ty = pos.func.dfg.value_type(arg);
                    (
                        kind,
                        signed,
                        SiteOp::Compare,
                        arg,
                        Some(imm.bits()),
                        None,
                        ty,
                    )
                }
                InstructionData::Ternary {
                    opcode: Opcode::Select,
                    args,
                } => {
                    let ty = pos.func.dfg.value_type(args[0]);
                    (
                        CmpKind::Ne,
                        false,
                        SiteOp::Select,
                        args[0],
                        Some(0),
                        None,
                        ty,
                    )
                }
                InstructionData::BranchTable {
                    opcode: Opcode::BrTable,
                    arg,
                    table,
                } => {
                    let cases = pos.func.stencil.dfg.jump_tables[table].as_slice().len();
                    let cases = u32::try_from(cases).map_err(|_| "jump table too large")?;
                    let ty = pos.func.dfg.value_type(arg);
                    (
                        CmpKind::Lt,
                        false,
                        SiteOp::BranchTable { cases },
                        arg,
                        Some(i64::from(cases)),
                        None,
                        ty,
                    )
                }
                _ => continue,
            };
            if !ty.is_int() || ty.bits() > 64 {
                continue;
            }
            let site = u32::try_from(self.sites.len()).map_err(|_| "too many comparison sites")?;
            self.sites.push(CmpSite {
                function: key.to_string(),
                kind,
                signed,
                bits: ty.bits(),
                op,
                location,
            });
            match op {
                SiteOp::Compare => {}
                SiteOp::Select => self.stats.selects += 1,
                SiteOp::BranchTable { .. } => self.stats.branch_tables += 1,
            }
            pos.goto_inst(inst);
            let widen = |pos: &mut FuncCursor, v: ir::Value| {
                if ty == types::I64 {
                    v
                } else {
                    pos.ins().uextend(types::I64, v)
                }
            };
            let lhs64 = widen(&mut pos, lhs);
            let rhs64 = match (rhs, rhs_imm) {
                (Some(v), _) => widen(&mut pos, v),
                (None, Some(imm)) => {
                    let mask = if ty.bits() == 64 {
                        u64::MAX
                    } else {
                        (1u64 << ty.bits()) - 1
                    };
                    pos.ins().iconst(types::I64, (imm as u64 & mask) as i64)
                }
                (None, None) => unreachable!("comparison has a right operand"),
            };
            let flags = MemFlags::trusted();
            let base = pos.ins().iconst(types::I64, self.base);
            let count = pos.ins().load(types::I64, flags, base, word(CMP_COUNT));
            let cap = pos.ins().iconst(types::I64, MAX_CMP_LOG as i64);
            let slot = pos.ins().umin(count, cap);
            let offset = pos.ins().imul_imm(slot, (RECORD * 8) as i64);
            let record = pos.ins().iadd(base, offset);
            let label = pos.ins().iconst(types::I64, i64::from(site));
            pos.ins().store(flags, label, record, word(CMP_LOG));
            pos.ins().store(flags, lhs64, record, word(CMP_LOG + 1));
            pos.ins().store(flags, rhs64, record, word(CMP_LOG + 2));
            let next = pos.ins().iadd_imm(count, 1);
            pos.ins().store(flags, next, base, word(CMP_COUNT));
            self.stats.comparisons += 1;
        }
        let entry = blocks.first().copied();
        let mut exhausted = None;
        for (index, (block, first)) in blocks.into_iter().zip(firsts).enumerate() {
            let cur = block_label(key, index);
            self.edge_sites.push(EdgeSite {
                function: key.to_string(),
                block: index,
                label: cur as u32,
                location: first.and_then(|i| crate::native::source_offset(pos.func, i)),
            });
            pos.goto_first_insertion_point(block);
            let flags = MemFlags::trusted();
            let base = pos.ins().iconst(types::I64, self.base);
            let prev = pos.ins().load(types::I64, flags, base, word(PREV));
            let slot = pos.ins().bxor_imm(prev, cur);
            let offset = pos.ins().ishl_imm(slot, 3);
            let at = pos.ins().iadd(base, offset);
            let hits = pos.ins().load(types::I64, flags, at, word(EDGES));
            let more = pos.ins().iadd_imm(hits, 1);
            pos.ins().store(flags, more, at, word(EDGES));
            let shifted = pos.ins().iconst(types::I64, cur >> 1);
            pos.ins().store(flags, shifted, base, word(PREV));
            self.stats.edges += 1;
            // Fuel: `ok = fuel != 0`, and at a function entry also
            // `sp >= limit`; `fuel = ok ? fuel - 1 : 0`; continue only if ok.
            let Some(first) = first else { continue };
            let fuel = pos.ins().load(types::I64, flags, base, word(FUEL));
            let mut ok = pos.ins().icmp_imm(IntCC::NotEqual, fuel, 0);
            if Some(block) == entry {
                let sp = pos.ins().get_stack_pointer(types::I64);
                let limit = pos.ins().load(types::I64, flags, base, word(STACK_LIMIT));
                let deep = pos.ins().icmp(IntCC::UnsignedLessThan, sp, limit);
                let deep64 = pos.ins().uextend(types::I64, deep);
                let hit = pos.ins().load(types::I64, flags, base, word(STACK_HIT));
                let hit = pos.ins().bor(hit, deep64);
                pos.ins().store(flags, hit, base, word(STACK_HIT));
                let room = pos.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, sp, limit);
                ok = pos.ins().band(ok, room);
            }
            let less = pos.ins().iadd_imm(fuel, -1);
            let zero = pos.ins().iconst(types::I64, 0);
            let left = pos.ins().select(ok, less, zero);
            pos.ins().store(flags, left, base, word(FUEL));
            let rest = pos.func.dfg.make_block();
            pos.func.layout.split_block(rest, first);
            let out = match exhausted {
                Some(b) => b,
                None => {
                    let b = pos.func.dfg.make_block();
                    pos.func.layout.append_block(b);
                    exhausted = Some(b);
                    b
                }
            };
            pos.goto_bottom(block);
            pos.ins().brif(ok, rest, &[], out, &[]);
            self.stats.fuel_checks += 1;
        }
        if let Some(out) = exhausted {
            let returns: Vec<ir::Type> = pos
                .func
                .signature
                .returns
                .iter()
                .map(|r| r.value_type)
                .collect();
            pos.goto_bottom(out);
            let flags = MemFlags::trusted();
            let base = pos.ins().iconst(types::I64, self.base);
            let one = pos.ins().iconst(types::I64, 1);
            pos.ins().store(flags, one, base, word(EXHAUSTED));
            let mut values = Vec::new();
            for ty in returns {
                values.push(match ty {
                    types::F64 => pos.ins().f64const(0.0),
                    types::F32 => pos.ins().f32const(0.0),
                    t if t.is_int() && t.bits() <= 64 => pos.ins().iconst(t, 0),
                    t => {
                        return Err(format!("fuel exit cannot return a placeholder of type {t}"));
                    }
                });
            }
            pos.ins().return_(&values);
        }
        Ok(())
    }

    fn replace_sink(&self, func: &mut ir::Function) -> Result<(), String> {
        let signature = func.signature.clone();
        if signature.params.len() != 1
            || signature.params[0].value_type != types::I64
            || signature.returns.len() != 1
            || signature.returns[0].value_type != types::I64
        {
            return Err("result sink must be `fn(T) -> i64` with a scalar T".into());
        }
        let mut sink = ir::Function::with_name_signature(func.name.clone(), signature);
        let mut context = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut sink, &mut context);
            let block = b.create_block();
            b.append_block_params_for_function_params(block);
            b.switch_to_block(block);
            b.seal_block(block);
            let value = b.block_params(block)[0];
            let base = b.ins().iconst(types::I64, self.base);
            b.ins()
                .store(MemFlags::trusted(), value, base, word(RESULT));
            let zero = b.ins().iconst(types::I64, 0);
            b.ins().return_(&[zero]);
            b.finalize();
        }
        *func = sink;
        Ok(())
    }

    fn replace_stub(&self, func: &mut ir::Function, index: usize) -> Result<(), String> {
        if index >= MAX_ARGS {
            return Err(format!("argument stub index {index} exceeds {MAX_ARGS}"));
        }
        let signature = func.signature.clone();
        if !signature.params.is_empty()
            || signature.returns.len() != 1
            || signature.returns[0].value_type != types::I64
        {
            return Err("argument stub must be `fn() -> i64` at the native ABI".into());
        }
        let mut stub = ir::Function::with_name_signature(func.name.clone(), signature);
        let mut context = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut stub, &mut context);
            let block = b.create_block();
            b.switch_to_block(block);
            b.seal_block(block);
            let base = b.ins().iconst(types::I64, self.base);
            let value = b
                .ins()
                .load(types::I64, MemFlags::trusted(), base, word(ARGS + index));
            b.ins().return_(&[value]);
            b.finalize();
        }
        *func = stub;
        Ok(())
    }
}

/// Object bytes and CLIF of `entry` for `target` with every function
/// instrumented against a buffer at `base` (code-generation validation;
/// the address is meaningless outside this process).
pub fn object_with_coverage(
    world: &World,
    entry: &str,
    target: Option<&str>,
    optimize: bool,
    base: i64,
) -> Result<(Vec<u8>, String, InstrumentStats), String> {
    let mut instrumenter = Instrumenter::new(base);
    let mut hook = |key: &str, f: &mut ir::Function| instrumenter.rewrite(key, f);
    let hook: Instrument<'_> = &mut hook;
    let (bytes, clif) =
        crate::native::object_instrumented(world, entry, target, optimize, None, Some(hook))?;
    Ok((bytes, clif, instrumenter.stats()))
}

/// Native carrier of one fuzz-target parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeParam {
    I64,
    U64,
    U32,
    U8,
    Bool,
}

impl NativeParam {
    fn of(ty: &Type) -> Option<Self> {
        Some(match ty.name() {
            "i64" => Self::I64,
            "u64" => Self::U64,
            "u32" => Self::U32,
            "u8" => Self::U8,
            "bool" => Self::Bool,
            _ => return None,
        })
    }
    fn source_type(self) -> &'static str {
        match self {
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::U32 => "u32",
            Self::U8 => "u8",
            Self::Bool => "bool",
        }
    }
    fn stub_body(self) -> &'static str {
        match self {
            Self::Bool => "false",
            _ => "0",
        }
    }
}

/// Outcome of one native run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeOutcome {
    Returned,
    /// The target raised `Fail` instance `effect` (for example
    /// `Fail[ArithOverflow]`).
    Failed {
        effect: String,
    },
    /// The run used up its fuel ([`NativeCoverage::set_fuel`]) or its
    /// stack budget ([`NativeCoverage::set_stack_budget`]) and was stopped;
    /// its result and coverage past that point are meaningless.
    Exhausted {
        /// The stack budget, not the fuel, ran out.
        stack: bool,
    },
}

/// Result sink of the wrapper: its argument is the target's scalar result.
const SINK: &str = "__vesper_fuzz_result";
/// Site-label bit of the per-case `br_table` records.
const CASE_SITE: u32 = 0x4000_0000;
/// Site-label bit of the zigzag records.
const ZIGZAG_SITE: u32 = 0x8000_0000;
/// Site-label bit and edge offset of a shadow target ([`NativeCoverage::feed_shadow`]).
const SHADOW_SITE: u32 = 0x2000_0000;

/// A fuzz target compiled once with coverage instrumentation.
pub struct NativeCoverage {
    // Field order: the JIT is dropped before the buffer it writes.
    jit: InstrumentedJit,
    buffer: Box<CoverageBuffer>,
    params: Vec<NativeParam>,
    result: Option<NativeParam>,
    fails: Vec<String>,
    sites: Vec<CmpSite>,
    edge_sites: Vec<EdgeSite>,
    stats: InstrumentStats,
    fuel: u64,
    stack_budget: u64,
}

fn check_source(source: &str) -> Result<World, String> {
    let parsed = parser::parse(source);
    if let Some(d) = parsed.diagnostics.first() {
        return Err(format!("{} {}", d.code, d.message));
    }
    let checked = check::check(&parsed.module);
    if let Some(d) = checked.diagnostics.first() {
        return Err(format!("{} {}", d.code, d.message));
    }
    Ok(checked.world)
}

fn effect_text(name: &str, args: &[Type]) -> String {
    if args.is_empty() {
        name.to_string()
    } else {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        format!("{name}[{}]", args.join(", "))
    }
}

impl NativeCoverage {
    /// Compile `target` of the checked module `source` with coverage
    /// instrumentation. The target must be non-generic, take only `i64`,
    /// `u64`, `u32`, `u8` or `bool` by value, and declare only unlabeled
    /// `Fail[..]` effects. When its result is one of those types too, the
    /// wrapper passes it to a result sink and [`Self::last_result`] returns
    /// it after a run.
    pub fn compile(source: &str, world: &World, target: &str) -> Result<Self, String> {
        let f: &Function = world
            .functions
            .get(target)
            .ok_or_else(|| format!("'{target}' is not a function of the module"))?;
        if target.starts_with(STUB_PREFIX)
            || world.functions.keys().any(|k| k.starts_with(STUB_PREFIX))
        {
            return Err(format!(
                "names starting with '{STUB_PREFIX}' are reserved for native coverage"
            ));
        }
        if !f.generics.is_empty() {
            return Err(format!("'{target}' is generic"));
        }
        if f.body.is_none() {
            return Err(format!("'{target}' has no body"));
        }
        if f.params.len() > MAX_ARGS {
            return Err(format!("'{target}' has more than {MAX_ARGS} parameters"));
        }
        let mut params = Vec::new();
        for p in &f.params {
            if p.pass != crate::ast::Pass::In {
                return Err(format!(
                    "'{target}' parameter '{}' is not passed by value",
                    p.name
                ));
            }
            params.push(NativeParam::of(&p.ty).ok_or_else(|| {
                format!(
                    "'{target}' parameter '{}' has type {}; supported: i64, u64, u32, u8, bool",
                    p.name,
                    p.ty.name()
                )
            })?);
        }
        if f.effects.tail.is_some() {
            return Err(format!(
                "--native-coverage: '{target}' has an open effect row; only Fail[..] effects are handled natively"
            ));
        }
        let mut fails = Vec::new();
        for e in &f.effects.instances {
            if e.name != "Fail" || e.label.is_some() || e.args.len() != 1 {
                return Err(format!(
                    "--native-coverage: '{target}' declares {}; only unlabeled Fail[..] effects are handled natively",
                    effect_text(&e.name, &e.args)
                ));
            }
            fails.push(effect_text(&e.name, &e.args));
        }
        let mut wrapper = source.trim_end().to_string();
        wrapper.push('\n');
        for (i, p) in params.iter().enumerate() {
            wrapper.push_str(&format!(
                "\nfn {STUB_PREFIX}arg_{i}() -> {} {{\n    {}\n}}\n",
                p.source_type(),
                p.stub_body()
            ));
        }
        let result = NativeParam::of(&f.result);
        if let Some(r) = result {
            wrapper.push_str(&format!(
                "\nfn {SINK}(value: {}) -> i64 {{\n    0\n}}\n",
                r.source_type()
            ));
        }
        let call = (0..params.len())
            .map(|i| format!("{STUB_PREFIX}arg_{i}()"))
            .collect::<Vec<_>>()
            .join(", ");
        let call = if result.is_some() {
            format!("{SINK}({target}({call}))")
        } else {
            format!("{target}({call})")
        };
        let mut body = format!("{{\n    let _result = {call};\n    0\n}}");
        for (k, fail) in fails.iter().enumerate() {
            body = format!(
                "{{\n    handle {fail} in {body} with {{\n        return value => value,\n        raise(error, resume) => {},\n    }}\n}}",
                k + 1
            );
        }
        wrapper.push_str(&format!("\nfn {CASE}() -> i64 {body}\n"));
        let wrapped = check_source(&wrapper).map_err(|e| {
            format!("--native-coverage: wrapper for '{target}' does not check: {e}")
        })?;
        let buffer = Box::new(CoverageBuffer::new());
        let mut instrumenter = Instrumenter::new(buffer.base());
        for i in 0..params.len() {
            instrumenter = instrumenter.with_arg_stub(&format!("{STUB_PREFIX}arg_{i}"), i);
        }
        if result.is_some() {
            instrumenter = instrumenter.with_result_sink(SINK);
        }
        let mut hook = |key: &str, func: &mut ir::Function| instrumenter.rewrite(key, func);
        // SAFETY: the instrumentation only loads and stores whole words
        // inside `buffer` (fixed offsets below `WORDS`, edge slots masked
        // below `EDGE_SLOTS`, log slots clamped to `MAX_CMP_LOG`). The boxed
        // buffer never moves, is owned by the returned value, is never
        // handed out mutably, and is dropped after `jit` (field order). Its
        // words are atomics, so the host only touches them atomically.
        let jit = unsafe { InstrumentedJit::compile(&wrapped, CASE, &mut hook) }
            .map_err(|e| format!("--native-coverage: '{target}' does not compile natively: {e}"))?;
        if instrumenter.stats().stubs != params.len()
            || instrumenter.stats().sinks != usize::from(result.is_some())
        {
            return Err("--native-coverage: an argument stub was not emitted".into());
        }
        Ok(Self {
            jit,
            buffer,
            params,
            result,
            fails,
            sites: instrumenter.sites().to_vec(),
            edge_sites: instrumenter.edge_sites().to_vec(),
            stats: instrumenter.stats(),
            fuel: DEFAULT_FUEL,
            stack_budget: DEFAULT_STACK_BUDGET,
        })
    }

    /// Instrumented block entries a run may execute ([`DEFAULT_FUEL`]).
    pub fn set_fuel(&mut self, fuel: u64) {
        self.fuel = fuel;
    }
    /// Bytes of stack below the caller's frame a run may use
    /// ([`DEFAULT_STACK_BUDGET`]). The caller must have at least this much
    /// stack left (plus the frame of one instrumented function).
    pub fn set_stack_budget(&mut self, bytes: u64) {
        self.stack_budget = bytes;
    }
    /// The target's result in the last run that returned, as a machine
    /// value ([`Self::run`]'s encoding), when its type is a supported
    /// scalar.
    pub fn last_result(&self) -> Option<u64> {
        self.result.map(|r| {
            let v = self.buffer.result();
            match r {
                NativeParam::Bool => u64::from(v != 0),
                NativeParam::U8 => v & 0xff,
                NativeParam::U32 => v & 0xffff_ffff,
                NativeParam::I64 | NativeParam::U64 => v,
            }
        })
    }
    /// Result type of the target, when [`Self::last_result`] reports it.
    pub fn result_type(&self) -> Option<NativeParam> {
        self.result
    }
    /// Blocks of every instrumented function with their labels and source
    /// locations.
    pub fn edge_sites(&self) -> &[EdgeSite] {
        &self.edge_sites
    }

    pub fn params(&self) -> &[NativeParam] {
        &self.params
    }
    pub fn sites(&self) -> &[CmpSite] {
        &self.sites
    }
    pub fn stats(&self) -> InstrumentStats {
        self.stats
    }
    pub fn buffer(&self) -> &CoverageBuffer {
        &self.buffer
    }

    /// Run the target on `args`, machine values: `i64` as its two's
    /// complement bits, unsigned values as they are, `bool` as 0 or 1.
    pub fn run(&mut self, args: &[u64]) -> Result<NativeOutcome, String> {
        if args.len() != self.params.len() {
            return Err(format!(
                "expected {} argument(s), got {}",
                self.params.len(),
                args.len()
            ));
        }
        for (i, (p, &a)) in self.params.iter().zip(args).enumerate() {
            let fits = match p {
                NativeParam::I64 | NativeParam::U64 => true,
                NativeParam::U32 => a <= u64::from(u32::MAX),
                NativeParam::U8 => a <= u64::from(u8::MAX),
                NativeParam::Bool => a <= 1,
            };
            if !fits {
                return Err(format!(
                    "argument {i} ({a}) is out of range for {}",
                    p.source_type()
                ));
            }
            self.buffer.set_arg(i, a);
        }
        self.buffer.reset();
        let marker = 0u8;
        let here = std::ptr::addr_of!(marker) as u64;
        self.buffer
            .arm(self.fuel, here.saturating_sub(self.stack_budget));
        let code = self.jit.run();
        std::hint::black_box(&marker);
        if self.buffer.exhausted() {
            return Ok(NativeOutcome::Exhausted {
                stack: self.buffer.stack_hit(),
            });
        }
        match code {
            0 => Ok(NativeOutcome::Returned),
            k if k >= 1 && (k as usize) <= self.fails.len() => Ok(NativeOutcome::Failed {
                effect: self.fails[k as usize - 1].clone(),
            }),
            other => Err(format!("native wrapper returned unknown status {other}")),
        }
    }

    /// Report the last run's coverage to Crucible: each edge slot as a
    /// probe edge with its hit count (capped at 128, the top hit-count
    /// bucket), and each logged comparison with its raw operands plus, when
    /// the target has an `i64` parameter, the zigzag tape encoding of the
    /// sign-extended operands (site label with the top bit set). Signed
    /// ordering comparisons are reported through [`unsigned_view`], so their
    /// taken bit is the one the code computed; a `br_table` index is also
    /// reported as `index == k` for each of its first 256 cases.
    pub fn feed(&self, probe: &mut Probe) {
        self.feed_labelled(probe, 0, 0);
    }

    /// [`Self::feed`] for a second target in the same campaign (the shadow
    /// of a differential run): its edges are offset past the edge map and
    /// its site labels carry a separate bit, so the two targets' features
    /// stay distinct.
    pub fn feed_shadow(&self, probe: &mut Probe) {
        self.feed_labelled(probe, EDGE_SLOTS as u32, SHADOW_SITE);
    }

    fn feed_labelled(&self, probe: &mut Probe, edge_offset: u32, site_bit: u32) {
        for (slot, hits) in self.buffer.edges() {
            for _ in 0..hits.min(128) {
                probe.edge(slot + edge_offset);
            }
        }
        let zigzag = self.params.contains(&NativeParam::I64);
        for (site, lhs, rhs) in self.buffer.comparisons() {
            let Some(info) = self.sites.get(site as usize) else {
                continue;
            };
            let label = site | site_bit;
            let kind = unsigned_view(info.kind, info.signed, info.bits, lhs, rhs);
            probe.cmp(label, kind, lhs, rhs);
            if let SiteOp::BranchTable { cases } = info.op {
                for k in 0..cases.min(256) {
                    probe.cmp(
                        CASE_SITE | site_bit | ((site & 0x3f_ffff) << 8) | k,
                        CmpKind::Eq,
                        lhs,
                        u64::from(k),
                    );
                }
            }
            if zigzag {
                let extend = |v: u64| {
                    if info.bits < 64 && info.signed {
                        let shift = 64 - info.bits;
                        ((v << shift) as i64) >> shift
                    } else {
                        v as i64
                    }
                };
                let encode = |n: i64| ((n << 1) ^ (n >> 63)) as u64;
                let kind = match info.kind {
                    CmpKind::Eq | CmpKind::Ne => info.kind,
                    _ => CmpKind::Eq,
                };
                probe.cmp(
                    label | ZIGZAG_SITE,
                    kind,
                    encode(extend(lhs)),
                    encode(extend(rhs)),
                );
            }
        }
    }
}
