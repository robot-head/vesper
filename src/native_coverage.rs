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
//! Limits: native code runs without a fuel bound, so a target that does not
//! terminate hangs the campaign (the evaluator cross-check never starts);
//! effects other than unlabeled `Fail[..]` are rejected; instrumentation of
//! an object file is for code-generation validation only, because the
//! buffer address is a host address.
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

const PREV: usize = 0;
const CMP_COUNT: usize = 1;
const ARGS: usize = 2;
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
    /// Clear edges, `prev` and the comparison log before a run.
    pub fn reset(&self) {
        self.words[PREV].store(0, Ordering::Relaxed);
        self.words[CMP_COUNT].store(0, Ordering::Relaxed);
        for w in &self.words[EDGES..] {
            w.store(0, Ordering::Relaxed);
        }
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

/// What one instrumented comparison site compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmpSite {
    /// Instance key of the function containing the comparison.
    pub function: String,
    pub kind: CmpKind,
    pub signed: bool,
    /// Operand width in bits.
    pub bits: u32,
}

/// Counts of what [`Instrumenter`] inserted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstrumentStats {
    pub functions: usize,
    pub edges: usize,
    pub comparisons: usize,
    pub stubs: usize,
}

/// The rewrite applied through [`crate::native::Instrument`].
pub struct Instrumenter {
    base: i64,
    /// Argument stub key -> argument index.
    stubs: BTreeMap<String, usize>,
    sites: Vec<CmpSite>,
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
            sites: Vec::new(),
            stats: InstrumentStats::default(),
        }
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
        self.stats.functions += 1;
        let blocks: Vec<ir::Block> = func.layout.blocks().collect();
        let mut compares = Vec::new();
        for &block in &blocks {
            for inst in func.layout.block_insts(block) {
                compares.push(inst);
            }
        }
        let mut pos = FuncCursor::new(func);
        for inst in compares {
            let (cond, lhs, rhs_imm, rhs) = match pos.func.dfg.insts[inst] {
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    args,
                    cond,
                } => (cond, args[0], None, Some(args[1])),
                InstructionData::IntCompareImm {
                    opcode: Opcode::IcmpImm,
                    arg,
                    cond,
                    imm,
                } => (cond, arg, Some(imm.bits()), None),
                _ => continue,
            };
            let ty = pos.func.dfg.value_type(lhs);
            if !ty.is_int() || ty.bits() > 64 {
                continue;
            }
            let site = u32::try_from(self.sites.len()).map_err(|_| "too many comparison sites")?;
            let (kind, signed) = cmp_kind(cond);
            self.sites.push(CmpSite {
                function: key.to_string(),
                kind,
                signed,
                bits: ty.bits(),
            });
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
        for (index, block) in blocks.into_iter().enumerate() {
            let cur = block_label(key, index);
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
        }
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
}

/// A fuzz target compiled once with coverage instrumentation.
pub struct NativeCoverage {
    // Field order: the JIT is dropped before the buffer it writes.
    jit: InstrumentedJit,
    buffer: Box<CoverageBuffer>,
    params: Vec<NativeParam>,
    fails: Vec<String>,
    sites: Vec<CmpSite>,
    stats: InstrumentStats,
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
    /// `Fail[..]` effects.
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
        let call = (0..params.len())
            .map(|i| format!("{STUB_PREFIX}arg_{i}()"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut body = format!("{{\n    let _result = {target}({call});\n    0\n}}");
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
        let mut hook = |key: &str, func: &mut ir::Function| instrumenter.rewrite(key, func);
        // SAFETY: the instrumentation only loads and stores whole words
        // inside `buffer` (fixed offsets below `WORDS`, edge slots masked
        // below `EDGE_SLOTS`, log slots clamped to `MAX_CMP_LOG`). The boxed
        // buffer never moves, is owned by the returned value, is never
        // handed out mutably, and is dropped after `jit` (field order). Its
        // words are atomics, so the host only touches them atomically.
        let jit = unsafe { InstrumentedJit::compile(&wrapped, CASE, &mut hook) }
            .map_err(|e| format!("--native-coverage: '{target}' does not compile natively: {e}"))?;
        if instrumenter.stats().stubs != params.len() {
            return Err("--native-coverage: an argument stub was not emitted".into());
        }
        Ok(Self {
            jit,
            buffer,
            params,
            fails,
            sites: instrumenter.sites().to_vec(),
            stats: instrumenter.stats(),
        })
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
        let code = self.jit.run();
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
    /// sign-extended operands (site label with the top bit set).
    pub fn feed(&self, probe: &mut Probe) {
        for (slot, hits) in self.buffer.edges() {
            for _ in 0..hits.min(128) {
                probe.edge(slot);
            }
        }
        let zigzag = self.params.contains(&NativeParam::I64);
        for (site, lhs, rhs) in self.buffer.comparisons() {
            let Some(info) = self.sites.get(site as usize) else {
                continue;
            };
            probe.cmp(site, info.kind, lhs, rhs);
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
                    site | 0x8000_0000,
                    kind,
                    encode(extend(lhs)),
                    encode(extend(rhs)),
                );
            }
        }
    }
}
