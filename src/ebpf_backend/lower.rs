//! Place IR -> `vesper_ebpf::Asm`.
//!
//! Every IR value and local lives in an 8-byte stack slot below `r10`
//! (an `Option` takes two: tag, then payload); constants and field-less
//! enum constructors are immediates. Values used only inside their
//! defining block share a per-block pool of slots. Registers `r0`-`r5`
//! are scratch, `r9` holds the context for the whole function (it is
//! callee-saved across helper and BPF-to-BPF calls), and every
//! subprogram receives the context in `r1` and its scalar parameters in
//! `r2`-`r5`.
use super::surface::{EffectOp, MapOp, PacketOp, Scalar, Surface};
use super::{EbpfBackendError, Options, rules, unsupported};
use crate::{
    ast::{Literal, Pass, Pattern, PatternKind, Span, Type},
    check::World,
    ir::{self, FunctionIr, Op, Terminator, ValueId},
};
use std::collections::{BTreeMap, BTreeSet};
use vesper_ebpf::{
    Asm, BPF_MAX_STACK_BYTES, MapKind, Program, Reg,
    insn::{AluOp, AtomicOp, Endian, JmpOp, Size},
    program::Label,
};

/// Largest packet offset the kernel accepts in pointer arithmetic
/// (`MAX_PACKET_OFF`).
const MAX_PACKET_OFF: i32 = 0xffff;
/// Parameter registers after `r1` (the context).
const ARG_REGS: [Reg; 4] = [Reg::R2, Reg::R3, Reg::R4, Reg::R5];
/// `bpf_map_update_elem` flags.
const BPF_ANY: i32 = 0;
const BPF_NOEXIST: i32 = 1;

pub(crate) struct Lowered {
    pub program: Program,
    pub frames: Vec<(String, u32)>,
    pub loops: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Repr {
    Unit,
    Scalar(Scalar),
    /// A field-less enum: its tag.
    Enum,
    /// `Option[scalar]`: tag slot, then payload slot.
    Opt(Scalar),
}

impl Repr {
    fn slots(self) -> u32 {
        match self {
            Repr::Unit => 0,
            Repr::Scalar(_) | Repr::Enum => 1,
            Repr::Opt(_) => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    None,
    Imm(i64),
    Slot(u32),
}

fn repr(
    world: &World,
    t: &Type,
    function: &str,
    span: Option<&Span>,
) -> Result<Repr, EbpfBackendError> {
    let t = crate::numeric::default_type(t);
    if let Some(s) = Scalar::of(&t) {
        return Ok(Repr::Scalar(s));
    }
    match &t {
        Type::Tuple(a) if a.is_empty() => return Ok(Repr::Unit),
        Type::Named(n, a) if a.is_empty() && matches!(n.as_str(), "Unit" | "Never") => {
            return Ok(Repr::Unit);
        }
        Type::Named(n, a) if n == "Option" && a.len() == 1 => {
            if let Some(s) = Scalar::of(&a[0]) {
                return Ok(Repr::Opt(s));
            }
        }
        Type::Named(n, a) if a.is_empty() => {
            if let Some(adt) = world.adts.get(n)
                && adt.is_enum
                && adt.generics.is_empty()
                && adt.variants.iter().all(|v| v.fields.is_empty())
            {
                return Ok(Repr::Enum);
            }
        }
        _ => {}
    }
    Err(unsupported(
        rules::TYPE,
        function,
        span,
        format!(
            "type `{t}` is outside the eBPF subset (u8, u32, u64, i64, bool, Unit, field-less enums, Option of a scalar)"
        ),
    ))
}

/// Tag of a field-less enum variant (`XdpAction::Drop` or `Drop`).
fn variant_tag(world: &World, ty: &Type, name: &str) -> Option<i64> {
    let adt = world.adts.get(crate::numeric::default_type(ty).name())?;
    let v = name.rsplit("::").next()?;
    adt.variants
        .iter()
        .position(|x| x.name == v)
        .map(|p| p as i64)
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::Constant(_) => "constant",
        Op::Unit => "unit",
        Op::Read { .. } => "read",
        Op::Bind { .. } => "bind",
        Op::Write { .. } => "write",
        Op::Unary(..) => "unary operator",
        Op::Binary(..) => "binary operator",
        Op::Tuple(_) => "tuple",
        Op::Vector { .. } => "vector operation",
        Op::Data(..) => "struct or variant value",
        Op::Project(..) => "field projection",
        Op::Index(..) => "indexing",
        Op::Call { .. } => "call",
        Op::Indirect { .. } => "indirect call",
        Op::Try(_) => "`?`",
        Op::Closure { .. } => "closure",
        Op::Handle { .. } => "effect handler",
    }
}

fn op_uses(op: &Op) -> Vec<ValueId> {
    match op {
        Op::Bind { value, .. } | Op::Write { value, .. } | Op::Unary(_, value) | Op::Try(value) => {
            vec![*value]
        }
        Op::Binary(_, a, b) | Op::Index(a, b) => vec![*a, *b],
        Op::Tuple(v) | Op::Vector { args: v, .. } => v.clone(),
        Op::Data(_, f) => f.iter().map(|(_, v)| *v).collect(),
        Op::Project(v, _) => vec![*v],
        Op::Call { args, .. } => args.iter().map(|a| a.value).collect(),
        Op::Indirect { callee, args } => {
            let mut v = vec![*callee];
            v.extend(args.iter().map(|a| a.value));
            v
        }
        Op::Handle { state_value, .. } => state_value.iter().copied().collect(),
        _ => vec![],
    }
}

fn term_uses(t: &Terminator) -> Vec<ValueId> {
    match t {
        Terminator::Return(v) => vec![*v],
        Terminator::Jump { args, .. } => args.clone(),
        Terminator::Branch { condition, .. } => vec![*condition],
        Terminator::Match { value, .. } => vec![*value],
        Terminator::Unreachable => vec![],
    }
}

/// Whether `value`, produced at the end of block `b`, flows unchanged to
/// the function's return (through parameter-only forwarding blocks).
fn returns_directly(f: &FunctionIr, b: usize, value: ValueId) -> bool {
    let mut term = &f.blocks[b].terminator;
    let mut v = value;
    for _ in 0..=f.blocks.len() {
        match term {
            Terminator::Return(r) => return *r == v,
            Terminator::Jump { block, args } if args.as_slice() == [v] => {
                let t = &f.blocks[*block];
                if !t.instructions.is_empty() || t.params.len() != 1 {
                    return false;
                }
                v = t.params[0];
                term = &t.terminator;
            }
            _ => return false,
        }
    }
    false
}

/// Dests of tail self-calls in `f`: a call of `f` that is the last
/// instruction of its block and whose result is returned unchanged.
fn tail_self_calls(f: &FunctionIr) -> Result<BTreeSet<ValueId>, EbpfBackendError> {
    let mut out = BTreeSet::new();
    for (b, block) in f.blocks.iter().enumerate() {
        for (k, i) in block.instructions.iter().enumerate() {
            if let Op::Call { callee, .. } = &i.op
                && *callee == f.name
            {
                if k + 1 == block.instructions.len() && returns_directly(f, b, i.dest) {
                    out.insert(i.dest);
                } else {
                    return Err(unsupported(
                        rules::RECURSION,
                        &f.name,
                        Some(&i.span),
                        "recursion is not allowed on the eBPF target; only a tail self-call (a bounded loop) is lowered",
                    ));
                }
            }
        }
    }
    Ok(out)
}

fn slot_off(slot: u32) -> i16 {
    -(8 * (slot as i16 + 1))
}

struct LineTable<'a> {
    source: &'a str,
    starts: Vec<usize>,
    file: &'a str,
}

impl<'a> LineTable<'a> {
    fn new(source: &'a str, file: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        Self {
            source,
            starts,
            file,
        }
    }

    fn at(&self, offset: usize) -> (u32, u32, &'a str) {
        let line = self.starts.partition_point(|&s| s <= offset).max(1) - 1;
        let start = self.starts[line];
        let end = self.source[start..]
            .find('\n')
            .map_or(self.source.len(), |e| start + e);
        let col = offset.saturating_sub(start) + 1;
        (line as u32 + 1, col as u32, self.source[start..end].trim())
    }
}

pub(crate) fn lower(
    world: &World,
    program: &ir::Program,
    surface: &Surface,
    source: Option<&str>,
    options: &Options,
) -> Result<Lowered, EbpfBackendError> {
    let mut asm = Asm::new();
    for def in &surface.map_defs {
        asm.map(def.clone())
            .map_err(|e| EbpfBackendError::Asm(e.to_string()))?;
    }
    let lines = source.map(|s| LineTable::new(s, &options.file));
    let entry = &surface.functions[0];

    // Call graph without tail self-calls; reject cycles.
    let mut tails = BTreeMap::new();
    let mut callees: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for name in &surface.functions {
        let f = &program.functions[name];
        let t = tail_self_calls(f)?;
        let mut cs = BTreeSet::new();
        for b in &f.blocks {
            for i in &b.instructions {
                if let Op::Call { callee, .. } = &i.op
                    && program.functions.contains_key(callee)
                    && !t.contains(&i.dest)
                {
                    cs.insert(callee.as_str());
                }
            }
        }
        callees.insert(name.as_str(), cs);
        tails.insert(name.as_str(), t);
    }
    fn cycle<'a>(
        n: &'a str,
        g: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<Vec<&'a str>> {
        if let Some(p) = path.iter().position(|x| *x == n) {
            let mut c = path[p..].to_vec();
            c.push(n);
            return Some(c);
        }
        if done.contains(n) {
            return None;
        }
        path.push(n);
        for c in g.get(n).into_iter().flatten() {
            if let Some(c) = cycle(c, g, path, done) {
                return Some(c);
            }
        }
        path.pop();
        done.insert(n);
        None
    }
    if let Some(c) = cycle(entry, &callees, &mut Vec::new(), &mut BTreeSet::new()) {
        return Err(unsupported(
            rules::RECURSION,
            c[0],
            world.functions.get(c[0]).map(|f| &f.span),
            format!(
                "recursion is not allowed on the eBPF target: {}",
                c.join(" -> ")
            ),
        ));
    }

    let labels: BTreeMap<&str, Label> = surface
        .functions
        .iter()
        .map(|n| (n.as_str(), asm.label()))
        .collect();
    let mut frames = Vec::new();
    let mut loops = Vec::new();
    for (k, name) in surface.functions.iter().enumerate() {
        let f = &program.functions[name];
        let tail = &tails[name.as_str()];
        if !tail.is_empty() {
            loops.push(name.clone());
        }
        asm.bind(labels[name.as_str()])
            .map_err(|e| EbpfBackendError::Asm(e.to_string()))?;
        if k > 0 {
            asm.function(name, labels[name.as_str()]);
        }
        let mut g = FnLower::new(
            world,
            surface,
            f,
            k == 0,
            &mut asm,
            &labels,
            tail,
            lines.as_ref(),
        )?;
        g.emit()?;
        frames.push((name.clone(), g.nslots * 8));
    }

    // Stack: each frame at most 512 bytes, and the worst call chain (the
    // kernel rounds each frame up to 32 bytes) too.
    let frame_of: BTreeMap<&str, u32> = frames.iter().map(|(n, b)| (n.as_str(), *b)).collect();
    for (n, b) in &frames {
        if *b > BPF_MAX_STACK_BYTES {
            return Err(unsupported(
                rules::STACK,
                n,
                world.functions.get(n).map(|f| &f.span),
                format!("frame of {b} bytes exceeds the {BPF_MAX_STACK_BYTES}-byte eBPF stack"),
            ));
        }
    }
    fn chain<'a>(
        n: &'a str,
        g: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        frame: &BTreeMap<&str, u32>,
    ) -> (u32, Vec<&'a str>) {
        let own = frame.get(n).copied().unwrap_or(0).div_ceil(32) * 32;
        let mut best = (0, Vec::new());
        for c in g.get(n).into_iter().flatten() {
            let r = chain(c, g, frame);
            if r.0 > best.0 {
                best = r;
            }
        }
        let mut path = vec![n];
        path.extend(best.1);
        (own + best.0, path)
    }
    let (worst, path) = chain(entry, &callees, &frame_of);
    if worst > BPF_MAX_STACK_BYTES {
        return Err(unsupported(
            rules::STACK,
            entry,
            None,
            format!(
                "call chain {} needs {worst} stack bytes, over the {BPF_MAX_STACK_BYTES}-byte eBPF limit",
                path.join(" -> ")
            ),
        ));
    }

    let mut name: String = entry
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(15)
        .collect();
    if name.is_empty() {
        name = "prog".into();
    }
    let program = asm
        .finish(&name, surface.action.prog_type())
        .map_err(|e| EbpfBackendError::Asm(e.to_string()))?;
    Ok(Lowered {
        program,
        frames,
        loops,
    })
}

struct FnLower<'a, 'b> {
    world: &'a World,
    surface: &'a Surface,
    f: &'a FunctionIr,
    entry: bool,
    asm: &'b mut Asm,
    fn_labels: &'b BTreeMap<&'a str, Label>,
    tail: &'b BTreeSet<ValueId>,
    lines: Option<&'b LineTable<'b>>,
    blocks: Vec<Label>,
    loop_head: Label,
    locs: BTreeMap<ValueId, Loc>,
    reprs: BTreeMap<ValueId, Repr>,
    locals: BTreeMap<String, (u32, Repr)>,
    key_slot: u32,
    val_slot: u32,
    nslots: u32,
    last_line: u32,
}

impl<'a, 'b> FnLower<'a, 'b> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        world: &'a World,
        surface: &'a Surface,
        f: &'a FunctionIr,
        entry: bool,
        asm: &'b mut Asm,
        fn_labels: &'b BTreeMap<&'a str, Label>,
        tail: &'b BTreeSet<ValueId>,
        lines: Option<&'b LineTable<'b>>,
    ) -> Result<Self, EbpfBackendError> {
        let blocks = (0..f.blocks.len()).map(|_| asm.label()).collect();
        let loop_head = asm.label();
        let mut g = FnLower {
            world,
            surface,
            f,
            entry,
            asm,
            fn_labels,
            tail,
            lines,
            blocks,
            loop_head,
            locs: BTreeMap::new(),
            reprs: BTreeMap::new(),
            locals: BTreeMap::new(),
            key_slot: 0,
            val_slot: 0,
            nslots: 0,
            last_line: 0,
        };
        g.check_signature()?;
        g.allocate()?;
        Ok(g)
    }

    fn err(
        &self,
        rule: &'static str,
        span: Option<&Span>,
        message: impl Into<String>,
    ) -> EbpfBackendError {
        unsupported(rule, &self.f.name, span, message)
    }

    fn repr(&self, t: &Type, span: Option<&Span>) -> Result<Repr, EbpfBackendError> {
        repr(self.world, t, &self.f.name, span)
    }

    fn fn_span(&self) -> Option<&'a Span> {
        self.world.functions.get(&self.f.name).map(|f| &f.span)
    }

    /// Parameters and result fit the BPF calling convention.
    fn check_signature(&self) -> Result<(), EbpfBackendError> {
        let span = self.fn_span();
        let mut regs = 0;
        for p in &self.f.params {
            if p.pass == Pass::Inout {
                return Err(self.err(
                    rules::CALL,
                    Some(&p.span),
                    format!(
                        "inout parameter `{}` is not supported on the eBPF target",
                        p.name
                    ),
                ));
            }
            if !p.projections.is_empty() {
                return Err(self.err(
                    rules::CALL,
                    Some(&p.span),
                    "parameter projections are not supported",
                ));
            }
            match self.repr(&p.ty, Some(&p.span))? {
                Repr::Unit => {}
                Repr::Scalar(_) | Repr::Enum => regs += 1,
                Repr::Opt(_) => {
                    return Err(self.err(
                        rules::CALL,
                        Some(&p.span),
                        format!(
                            "parameter `{}`: Option parameters are not passed in registers",
                            p.name
                        ),
                    ));
                }
            }
        }
        if regs > ARG_REGS.len() {
            return Err(self.err(
                rules::CALL,
                span,
                format!(
                    "{regs} scalar parameters: a BPF subprogram takes at most {} besides the context",
                    ARG_REGS.len()
                ),
            ));
        }
        if let Repr::Opt(s) = self.repr(&self.f.result, span)?
            && !matches!(s, Scalar::U8 | Scalar::U32 | Scalar::Bool)
        {
            return Err(self.err(
                rules::CALL,
                span,
                "an Option result is returned packed in r0, so its payload must be at most 32 bits",
            ));
        }
        Ok(())
    }

    fn ty_of(&self, v: ValueId) -> Option<&'a Type> {
        self.f.values.get(&v)
    }

    fn allocate(&mut self) -> Result<(), EbpfBackendError> {
        let f = self.f;
        let mut next = 0u32;
        let mut dedicated = |n: u32| {
            let s = next;
            next += n;
            s
        };
        for (name, ty) in &f.locals {
            let r = self.repr(ty, self.fn_span())?;
            let slot = dedicated(r.slots());
            self.locals.insert(name.clone(), (slot, r));
        }
        let uses_map = f.blocks.iter().flat_map(|b| &b.instructions).any(|i| {
            matches!(&i.op, Op::Call { callee, .. }
                if matches!(self.surface.ops.get(callee), Some(EffectOp::Map { .. })))
        });
        if uses_map {
            self.key_slot = dedicated(1);
            self.val_slot = dedicated(1);
        }
        // Definition block and use blocks of every value.
        let mut def_block = BTreeMap::new();
        let mut used_in: BTreeMap<ValueId, BTreeSet<usize>> = BTreeMap::new();
        for (b, block) in f.blocks.iter().enumerate() {
            for p in &block.params {
                def_block.insert(*p, usize::MAX);
            }
            for i in &block.instructions {
                def_block.insert(i.dest, b);
                for u in op_uses(&i.op) {
                    used_in.entry(u).or_default().insert(b);
                }
            }
            for u in term_uses(&block.terminator) {
                used_in.entry(u).or_default().insert(b);
            }
        }
        // Reprs and immediates.
        for (b, block) in f.blocks.iter().enumerate() {
            for p in &block.params {
                let ty = self.ty_of(*p).ok_or_else(|| {
                    self.err(
                        rules::OPERATION,
                        self.fn_span(),
                        format!("block parameter v{p} has no type"),
                    )
                })?;
                let r = self.repr(ty, self.fn_span())?;
                self.reprs.insert(*p, r);
            }
            let _ = b;
            for i in &block.instructions {
                let r = self.repr(&i.ty, Some(&i.span))?;
                self.reprs.insert(i.dest, r);
                let imm = match (&i.op, r) {
                    (_, Repr::Unit) => Some(Loc::None),
                    (Op::Constant(Literal::Int(n)), Repr::Scalar(_)) => {
                        Some(Loc::Imm(*n as u64 as i64))
                    }
                    (Op::Constant(Literal::Bool(v)), Repr::Scalar(_)) => {
                        Some(Loc::Imm(i64::from(*v)))
                    }
                    (Op::Data(name, fields), Repr::Enum) if fields.is_empty() => {
                        let tag = variant_tag(self.world, &i.ty, name).ok_or_else(|| {
                            self.err(
                                rules::OPERATION,
                                Some(&i.span),
                                format!("unknown variant `{name}`"),
                            )
                        })?;
                        Some(Loc::Imm(tag))
                    }
                    _ => None,
                };
                if let Some(l) = imm {
                    self.locs.insert(i.dest, l);
                }
            }
        }
        // Dedicated slots: block parameters and values used outside their
        // defining block.
        for (v, b) in &def_block {
            if self.locs.contains_key(v) {
                continue;
            }
            let r = self.reprs[v];
            let cross =
                *b == usize::MAX || used_in.get(v).is_some_and(|s| s.iter().any(|u| u != b));
            if r == Repr::Unit {
                self.locs.insert(*v, Loc::None);
            } else if cross {
                self.locs.insert(*v, Loc::Slot(dedicated(r.slots())));
            }
        }
        // Block-local pool.
        let base = next;
        let mut pool_max = 0u32;
        for (b, block) in f.blocks.iter().enumerate() {
            let n = block.instructions.len();
            let mut last: BTreeMap<ValueId, usize> = BTreeMap::new();
            for (k, i) in block.instructions.iter().enumerate() {
                for u in op_uses(&i.op) {
                    last.insert(u, k);
                }
            }
            for u in term_uses(&block.terminator) {
                last.insert(u, n);
            }
            let mut free: Vec<(u32, u32)> = Vec::new();
            let mut top = 0u32;
            for (k, i) in block.instructions.iter().enumerate() {
                if !self.locs.contains_key(&i.dest) {
                    let size = self.reprs[&i.dest].slots();
                    let slot = if let Some(p) = free.iter().position(|(_, s)| *s == size) {
                        free.swap_remove(p).0
                    } else {
                        let s = top;
                        top += size;
                        s
                    };
                    pool_max = pool_max.max(top);
                    self.locs.insert(i.dest, Loc::Slot(base + slot));
                    if !last.contains_key(&i.dest) {
                        free.push((slot, size));
                    }
                }
                let mut operands = op_uses(&i.op);
                operands.sort_unstable();
                operands.dedup();
                for u in operands {
                    if last.get(&u) == Some(&k)
                        && def_block.get(&u) == Some(&b)
                        && let Some(Loc::Slot(s)) = self.locs.get(&u)
                        && *s >= base
                    {
                        free.push((s - base, self.reprs[&u].slots()));
                    }
                }
            }
        }
        self.nslots = base + pool_max;
        Ok(())
    }

    // ------------------------------------------------------------ emit --

    fn mov_imm(&mut self, r: Reg, v: i64) {
        if let Ok(i) = i32::try_from(v) {
            self.asm.mov64(r, i);
        } else {
            self.asm.ld_imm64(r, v as u64);
        }
    }

    fn st(&mut self, slot: u32, r: Reg) {
        self.asm.stx(Size::DW, Reg::R10, slot_off(slot), r);
    }

    fn st_imm(&mut self, slot: u32, v: i32) {
        self.asm.st(Size::DW, Reg::R10, slot_off(slot), v);
    }

    fn loc(&self, v: ValueId, span: &Span) -> Result<Loc, EbpfBackendError> {
        self.locs.get(&v).copied().ok_or_else(|| {
            self.err(
                rules::OPERATION,
                Some(span),
                format!("value v{v} is not defined"),
            )
        })
    }

    /// Load part `part` (0: scalar or tag, 1: payload) of `v` into `r`.
    fn ld(&mut self, r: Reg, v: ValueId, part: u32, span: &Span) -> Result<(), EbpfBackendError> {
        match self.loc(v, span)? {
            Loc::None => {
                self.asm.mov64(r, 0);
            }
            Loc::Imm(x) => self.mov_imm(r, if part == 0 { x } else { 0 }),
            Loc::Slot(s) => {
                self.asm.ldx(Size::DW, r, Reg::R10, slot_off(s + part));
            }
        }
        Ok(())
    }

    fn dest_slot(&self, v: ValueId, span: &Span) -> Result<Option<u32>, EbpfBackendError> {
        Ok(match self.loc(v, span)? {
            Loc::Slot(s) => Some(s),
            _ => None,
        })
    }

    /// Copy every part of value `v` into slots starting at `dst`.
    fn copy_to(
        &mut self,
        dst: u32,
        v: ValueId,
        parts: u32,
        span: &Span,
    ) -> Result<(), EbpfBackendError> {
        for p in 0..parts {
            self.ld(Reg::R1, v, p, span)?;
            self.st(dst + p, Reg::R1);
        }
        Ok(())
    }

    fn line(&mut self, span: &Span, force: bool) {
        if let Some(t) = self.lines {
            let (line, col, text) = t.at(span.start);
            if force || line != self.last_line {
                self.asm.line(t.file, line, col, text);
                self.last_line = line;
            }
        }
    }

    fn emit(&mut self) -> Result<(), EbpfBackendError> {
        if let Some(s) = self.fn_span() {
            self.line(s, true);
        }
        self.asm.mov64(Reg::R9, Reg::R1);
        if !self.entry {
            let mut k = 0;
            for p in &self.f.params {
                let (slot, r) = self.locals[&p.name];
                if r != Repr::Unit {
                    self.st(slot, ARG_REGS[k]);
                    k += 1;
                }
            }
        }
        self.asm
            .bind(self.loop_head)
            .map_err(|e| EbpfBackendError::Asm(e.to_string()))?;
        for b in 0..self.f.blocks.len() {
            self.asm
                .bind(self.blocks[b])
                .map_err(|e| EbpfBackendError::Asm(e.to_string()))?;
            let block = &self.f.blocks[b];
            let mut ended = false;
            for i in &block.instructions {
                self.line(&i.span, false);
                if self.instruction(i)? {
                    ended = true;
                    break;
                }
            }
            if !ended {
                self.terminator(b)?;
            }
        }
        Ok(())
    }

    /// Lower one instruction; `true` when it ended the block (a tail
    /// self-call became a back edge).
    fn instruction(&mut self, i: &'a ir::Instruction) -> Result<bool, EbpfBackendError> {
        let span = &i.span;
        if i.fallible {
            return Err(self.err(
                rules::FALLIBLE,
                Some(span),
                format!(
                    "this {} may raise Fail at run time: use wrapping_add/wrapping_sub/wrapping_mul, a nonzero constant divisor, or a guard the checker can see",
                    op_name(&i.op)
                ),
            ));
        }
        match &i.op {
            Op::Constant(_) | Op::Unit => {}
            Op::Data(name, fields) => match self.reprs[&i.dest] {
                Repr::Enum | Repr::Unit => {}
                Repr::Opt(_) => {
                    let Some(d) = self.dest_slot(i.dest, span)? else {
                        return Ok(false);
                    };
                    match (name.rsplit("::").next(), fields.as_slice()) {
                        (Some("Some"), [(_, v)]) => {
                            self.ld(Reg::R1, *v, 0, span)?;
                            self.st(d + 1, Reg::R1);
                            self.st_imm(d, 1);
                        }
                        (Some("None"), []) => {
                            self.st_imm(d, 0);
                            self.st_imm(d + 1, 0);
                        }
                        _ => {
                            return Err(self.err(
                                rules::OPERATION,
                                Some(span),
                                format!("constructor `{name}`"),
                            ));
                        }
                    }
                }
                Repr::Scalar(_) => {
                    return Err(self.err(
                        rules::OPERATION,
                        Some(span),
                        "struct values are not supported at run time on the eBPF target",
                    ));
                }
            },
            Op::Read { root, path, .. } => {
                if !path.is_empty() {
                    return Err(self.err(
                        rules::OPERATION,
                        Some(span),
                        "field projections are not supported",
                    ));
                }
                let Some(&(slot, r)) = self.locals.get(root) else {
                    return Err(self.err(
                        rules::OPERATION,
                        Some(span),
                        format!(
                            "`{root}` is not a local scalar (function values are not supported)"
                        ),
                    ));
                };
                if let Some(d) = self.dest_slot(i.dest, span)? {
                    for p in 0..r.slots() {
                        self.asm
                            .ldx(Size::DW, Reg::R1, Reg::R10, slot_off(slot + p));
                        self.st(d + p, Reg::R1);
                    }
                }
            }
            Op::Bind { pattern, value } => self.bind(pattern, *value, span)?,
            Op::Write { place, op, value } => {
                let Some(&(slot, r)) = self.locals.get(place.as_str()) else {
                    return Err(self.err(
                        rules::OPERATION,
                        Some(span),
                        format!("assignment to `{place}`: only whole scalar locals are writable"),
                    ));
                };
                match op.as_str() {
                    "=" => self.copy_to(slot, *value, r.slots(), span)?,
                    compound => {
                        let bin = compound.strip_suffix('=').unwrap_or(compound);
                        let Repr::Scalar(s) = r else {
                            return Err(self.err(
                                rules::OPERATION,
                                Some(span),
                                format!("`{compound}` on a non-scalar"),
                            ));
                        };
                        self.asm.ldx(Size::DW, Reg::R1, Reg::R10, slot_off(slot));
                        self.ld(Reg::R2, *value, 0, span)?;
                        self.arith(bin, s, span)?;
                        self.st(slot, Reg::R1);
                    }
                }
            }
            Op::Unary(op, v) => {
                let d = self.dest_slot(i.dest, span)?;
                self.ld(Reg::R1, *v, 0, span)?;
                match (op.as_str(), self.reprs[&i.dest]) {
                    ("!", Repr::Scalar(Scalar::Bool)) => {
                        self.asm.alu64(AluOp::Xor, Reg::R1, 1);
                    }
                    ("-", Repr::Scalar(Scalar::I64)) => {
                        self.asm.alu64(AluOp::Neg, Reg::R1, 0);
                    }
                    _ => {
                        return Err(self.err(
                            rules::OPERATION,
                            Some(span),
                            format!("unary `{op}`"),
                        ));
                    }
                }
                if let Some(d) = d {
                    self.st(d, Reg::R1);
                }
            }
            Op::Binary(op, a, b) => {
                let d = self.dest_slot(i.dest, span)?;
                let operand = self
                    .ty_of(*a)
                    .map(|t| self.repr(t, Some(span)))
                    .transpose()?
                    .unwrap_or(self.reprs[&i.dest]);
                self.ld(Reg::R1, *a, 0, span)?;
                self.ld(Reg::R2, *b, 0, span)?;
                if let (Repr::Opt(_), "==" | "!=") = (operand, op.as_str()) {
                    // Structural equality: equal tags, and equal payloads
                    // when both are Some.
                    let done = self.asm.label();
                    let equal = self.asm.label();
                    self.ld(Reg::R3, *a, 1, span)?;
                    self.ld(Reg::R4, *b, 1, span)?;
                    self.asm.mov64(Reg::R0, 0);
                    self.asm.jmp(JmpOp::Ne, Reg::R1, Reg::R2, done);
                    self.asm.jmp(JmpOp::Eq, Reg::R1, 0, equal);
                    self.asm.jmp(JmpOp::Ne, Reg::R3, Reg::R4, done);
                    self.bind_label(equal)?;
                    self.asm.mov64(Reg::R0, 1);
                    self.bind_label(done)?;
                    if op == "!=" {
                        self.asm.alu64(AluOp::Xor, Reg::R0, 1);
                    }
                    if let Some(d) = d {
                        self.st(d, Reg::R0);
                    }
                } else if let (Repr::Opt(_), _) = (operand, op.as_str()) {
                    return Err(self.err(
                        rules::OPERATION,
                        Some(span),
                        format!("`{op}` on an Option"),
                    ));
                } else if let Some(cmp) = compare(op, operand) {
                    let done = self.asm.label();
                    self.asm.mov64(Reg::R0, 1);
                    self.asm.jmp(cmp, Reg::R1, Reg::R2, done);
                    self.asm.mov64(Reg::R0, 0);
                    self.bind_label(done)?;
                    if let Some(d) = d {
                        self.st(d, Reg::R0);
                    }
                } else {
                    let s = match operand {
                        Repr::Scalar(s) => s,
                        _ => {
                            return Err(self.err(
                                rules::OPERATION,
                                Some(span),
                                format!("`{op}` on a non-scalar"),
                            ));
                        }
                    };
                    self.arith(op, s, span)?;
                    if let Some(d) = d {
                        self.st(d, Reg::R1);
                    }
                }
            }
            Op::Call { callee, args, .. } => {
                if let Some(e) = self.surface.ops.get(callee).copied() {
                    self.effect(e, i, args)?;
                } else if self.tail.contains(&i.dest) {
                    // Tail self-call: assign the parameters, jump back.
                    // The arguments already live in value slots, so no
                    // parameter is overwritten before it is read.
                    for (p, a) in self.f.params.iter().zip(args) {
                        let (slot, r) = self.locals[&p.name];
                        if r != Repr::Unit {
                            self.ld(Reg::R1, a.value, 0, span)?;
                            self.st(slot, Reg::R1);
                        }
                    }
                    self.asm.ja(self.loop_head);
                    return Ok(true);
                } else {
                    let Some(&label) = self.fn_labels.get(callee.as_str()) else {
                        return Err(self.err(
                            rules::OPERATION,
                            Some(span),
                            format!("call of `{callee}`"),
                        ));
                    };
                    let callee_f = &self.world.functions[callee];
                    let mut k = 0;
                    for (p, a) in callee_f.params.iter().zip(args) {
                        if a.pass == Pass::Inout {
                            return Err(self.err(
                                rules::CALL,
                                Some(span),
                                "inout arguments are not supported",
                            ));
                        }
                        if self.repr(&p.ty, Some(span))? != Repr::Unit {
                            let Some(&r) = ARG_REGS.get(k) else {
                                return Err(unsupported(
                                    rules::CALL,
                                    callee,
                                    Some(&callee_f.span),
                                    format!(
                                        "a BPF subprogram takes at most {} scalar parameters besides the context",
                                        ARG_REGS.len()
                                    ),
                                ));
                            };
                            self.ld(r, a.value, 0, span)?;
                            k += 1;
                        }
                    }
                    self.asm.mov64(Reg::R1, Reg::R9);
                    self.asm.call_local(label);
                    if let Some(d) = self.dest_slot(i.dest, span)? {
                        match self.reprs[&i.dest] {
                            Repr::Opt(_) => {
                                // payload in the low 32 bits, tag above.
                                self.asm.mov64(Reg::R1, Reg::R0);
                                self.asm.alu64(AluOp::Rsh, Reg::R1, 32);
                                self.st(d, Reg::R1);
                                self.asm.alu32(AluOp::Mov, Reg::R0, Reg::R0);
                                self.st(d + 1, Reg::R0);
                            }
                            _ => self.st(d, Reg::R0),
                        }
                    }
                }
            }
            other => {
                return Err(self.err(
                    rules::OPERATION,
                    Some(span),
                    format!("{} is not supported on the eBPF target", op_name(other)),
                ));
            }
        }
        Ok(false)
    }

    fn bind_label(&mut self, l: Label) -> Result<(), EbpfBackendError> {
        self.asm
            .bind(l)
            .map_err(|e| EbpfBackendError::Asm(e.to_string()))
    }

    /// `r1 = r1 <op> r2` with wrapping semantics at the width of `s`.
    fn arith(&mut self, op: &str, s: Scalar, span: &Span) -> Result<(), EbpfBackendError> {
        let alu = match op {
            "+" | "wrapping_add" => AluOp::Add,
            "-" | "wrapping_sub" => AluOp::Sub,
            "*" | "wrapping_mul" => AluOp::Mul,
            "/" => AluOp::Div,
            "%" => AluOp::Mod,
            "&&" => AluOp::And,
            "||" => AluOp::Or,
            _ => {
                return Err(self.err(rules::OPERATION, Some(span), format!("operator `{op}`")));
            }
        };
        if matches!(alu, AluOp::Div | AluOp::Mod) && s == Scalar::I64 {
            return Err(self.err(
                rules::OPERATION,
                Some(span),
                "signed division needs eBPF v4 sdiv/smod, which the target does not use",
            ));
        }
        match s {
            Scalar::U32 => {
                self.asm.alu32(alu, Reg::R1, Reg::R2);
            }
            Scalar::U8 => {
                self.asm.alu64(alu, Reg::R1, Reg::R2);
                self.asm.alu64(AluOp::And, Reg::R1, 0xff);
            }
            Scalar::U64 | Scalar::I64 | Scalar::Bool => {
                self.asm.alu64(alu, Reg::R1, Reg::R2);
            }
        }
        Ok(())
    }

    fn bind(
        &mut self,
        pattern: &Pattern,
        value: ValueId,
        span: &Span,
    ) -> Result<(), EbpfBackendError> {
        match &pattern.kind {
            PatternKind::Wild | PatternKind::Literal(_) => Ok(()),
            PatternKind::Bind(name) => {
                let Some(&(slot, r)) = self.locals.get(name) else {
                    return Err(self.err(
                        rules::OPERATION,
                        Some(span),
                        format!("binding `{name}`"),
                    ));
                };
                self.copy_to(slot, value, r.slots(), span)
            }
            PatternKind::Constructor { name, fields, .. } => {
                match (
                    self.reprs.get(&value),
                    name.rsplit("::").next(),
                    fields.as_slice(),
                ) {
                    (Some(Repr::Opt(_)), Some("Some"), [(_, p)]) => match &p.kind {
                        PatternKind::Bind(n) => {
                            let Some(&(slot, _)) = self.locals.get(n) else {
                                return Err(self.err(
                                    rules::OPERATION,
                                    Some(span),
                                    format!("binding `{n}`"),
                                ));
                            };
                            self.ld(Reg::R1, value, 1, span)?;
                            self.st(slot, Reg::R1);
                            Ok(())
                        }
                        PatternKind::Wild | PatternKind::Literal(_) => Ok(()),
                        _ => Err(self.err(rules::OPERATION, Some(span), "nested pattern")),
                    },
                    (_, _, []) => Ok(()),
                    _ => Err(self.err(rules::OPERATION, Some(span), format!("pattern `{name}`"))),
                }
            }
            PatternKind::Tuple(_) => Err(self.err(rules::OPERATION, Some(span), "tuple patterns")),
        }
    }

    fn jump_to(&mut self, from: usize, to: usize) {
        if to != from + 1 {
            self.asm.ja(self.blocks[to]);
        }
    }

    fn terminator(&mut self, b: usize) -> Result<(), EbpfBackendError> {
        let block = &self.f.blocks[b];
        let span = self.fn_span().cloned().unwrap_or(Span { start: 0, end: 0 });
        match &block.terminator {
            Terminator::Return(v) => {
                match self.reprs.get(v).copied().unwrap_or(Repr::Unit) {
                    Repr::Opt(_) => {
                        self.ld(Reg::R0, *v, 1, &span)?;
                        self.ld(Reg::R1, *v, 0, &span)?;
                        self.asm.alu64(AluOp::Lsh, Reg::R1, 32);
                        self.asm.alu64(AluOp::Or, Reg::R0, Reg::R1);
                    }
                    _ => self.ld(Reg::R0, *v, 0, &span)?,
                }
                self.asm.exit();
            }
            Terminator::Jump { block: t, args } => {
                let params = self.f.blocks[*t].params.clone();
                for (p, a) in params.iter().zip(args) {
                    if let Loc::Slot(s) = self.locs[p] {
                        let parts = self.reprs[p].slots();
                        self.copy_to(s, *a, parts, &span)?;
                    }
                }
                self.jump_to(b, *t);
            }
            Terminator::Branch { condition, yes, no } => {
                self.ld(Reg::R1, *condition, 0, &span)?;
                self.asm.jmp(JmpOp::Ne, Reg::R1, 0, self.blocks[*yes]);
                self.jump_to(b, *no);
            }
            Terminator::Match { value, arms } => {
                let r = self.reprs.get(value).copied().unwrap_or(Repr::Unit);
                let ty = self.ty_of(*value).cloned().unwrap_or(Type::Infer);
                self.ld(Reg::R1, *value, 0, &span)?;
                for (k, (pat, target)) in arms.iter().enumerate() {
                    let last = k + 1 == arms.len();
                    let target = self.blocks[*target];
                    match (&pat.kind, r) {
                        (PatternKind::Wild | PatternKind::Bind(_), _) => {
                            self.asm.ja(target);
                            break;
                        }
                        (PatternKind::Constructor { name, fields, .. }, Repr::Opt(_)) => {
                            let some = name.rsplit("::").next() == Some("Some");
                            let lit = match fields.first().map(|(_, p)| &p.kind) {
                                Some(PatternKind::Literal(l)) => Some(literal(l)),
                                _ => None,
                            };
                            match (some, lit) {
                                (true, Some(l)) => {
                                    let next = self.asm.label();
                                    self.asm.jmp(JmpOp::Ne, Reg::R1, 1, next);
                                    self.ld(Reg::R2, *value, 1, &span)?;
                                    self.mov_imm(Reg::R3, l);
                                    self.asm.jmp(JmpOp::Eq, Reg::R2, Reg::R3, target);
                                    self.bind_label(next)?;
                                }
                                (true, None) => {
                                    self.asm.jmp(JmpOp::Eq, Reg::R1, 1, target);
                                }
                                (false, _) => {
                                    self.asm.jmp(JmpOp::Eq, Reg::R1, 0, target);
                                }
                            }
                        }
                        (PatternKind::Constructor { name, .. }, Repr::Enum) => {
                            let tag = variant_tag(self.world, &ty, name).ok_or_else(|| {
                                self.err(
                                    rules::OPERATION,
                                    Some(&pat.span),
                                    format!("unknown variant `{name}`"),
                                )
                            })?;
                            self.mov_imm(Reg::R2, tag);
                            self.asm.jmp(JmpOp::Eq, Reg::R1, Reg::R2, target);
                        }
                        (PatternKind::Literal(l), Repr::Scalar(_)) => {
                            self.mov_imm(Reg::R2, literal(l));
                            self.asm.jmp(JmpOp::Eq, Reg::R1, Reg::R2, target);
                        }
                        _ => {
                            return Err(self.err(
                                rules::OPERATION,
                                Some(&pat.span),
                                "match pattern",
                            ));
                        }
                    }
                    if last {
                        // Exhaustive by checking; keep the fall-through
                        // edge out of the next block's code.
                        self.asm.ja(target);
                    }
                }
            }
            Terminator::Unreachable => {
                self.asm.mov64(Reg::R0, 0);
                self.asm.exit();
            }
        }
        Ok(())
    }

    fn key_ptr(&mut self, r: Reg) {
        self.asm.mov64(r, Reg::R10);
        self.asm.add64(r, i32::from(slot_off(self.key_slot)));
    }

    fn val_ptr(&mut self, r: Reg) {
        self.asm.mov64(r, Reg::R10);
        self.asm.add64(r, i32::from(slot_off(self.val_slot)));
    }

    fn effect(
        &mut self,
        e: EffectOp,
        i: &ir::Instruction,
        args: &[ir::Argument],
    ) -> Result<(), EbpfBackendError> {
        let span = &i.span;
        let d = self.dest_slot(i.dest, span)?;
        match e {
            EffectOp::Packet(op) => {
                let Some(d) = d else { return Ok(()) };
                let (data, data_end) = self.surface.action.packet_fields();
                let none = self.asm.label();
                let done = self.asm.label();
                self.asm.ldx(Size::W, Reg::R2, Reg::R9, data);
                self.asm.ldx(Size::W, Reg::R3, Reg::R9, data_end);
                match self.loc(args[0].value, span)? {
                    Loc::Imm(c) if (0..=i64::from(MAX_PACKET_OFF)).contains(&c) => {
                        self.asm.add64(Reg::R2, c as i32);
                    }
                    _ => {
                        self.ld(Reg::R1, args[0].value, 0, span)?;
                        self.asm.jmp(JmpOp::Gt, Reg::R1, MAX_PACKET_OFF, none);
                        self.asm.add64(Reg::R2, Reg::R1);
                    }
                }
                // Verifier-canonical guard: cursor + size > data_end -> none.
                self.asm.mov64(Reg::R4, Reg::R2);
                self.asm.add64(Reg::R4, op.bytes());
                self.asm.jmp(JmpOp::Gt, Reg::R4, Reg::R3, none);
                let size = match op {
                    PacketOp::U8 => Size::B,
                    PacketOp::U16Be => Size::H,
                    PacketOp::U32Be => Size::W,
                };
                self.asm.ldx(size, Reg::R0, Reg::R2, 0);
                match op {
                    PacketOp::U8 => {}
                    PacketOp::U16Be => {
                        self.asm.endian(Endian::ToBe, 16, Reg::R0);
                    }
                    PacketOp::U32Be => {
                        self.asm.endian(Endian::ToBe, 32, Reg::R0);
                    }
                }
                self.st(d + 1, Reg::R0);
                self.st_imm(d, 1);
                self.asm.ja(done);
                self.bind_label(none)?;
                self.st_imm(d, 0);
                self.st_imm(d + 1, 0);
                self.bind_label(done)?;
            }
            EffectOp::Helper(id) => {
                self.asm.call(id);
                if let Some(d) = d {
                    if matches!(self.reprs[&i.dest], Repr::Scalar(Scalar::U32)) {
                        self.asm.alu32(AluOp::Mov, Reg::R0, Reg::R0);
                    }
                    self.st(d, Reg::R0);
                }
            }
            EffectOp::Map {
                map,
                op,
                value,
                kind,
                ..
            } => {
                self.ld(Reg::R1, args[0].value, 0, span)?;
                self.st(self.key_slot, Reg::R1);
                let vsize = Size::from_bytes(value.map_bytes().unwrap_or(8)).unwrap_or(Size::DW);
                match op {
                    MapOp::Lookup => {
                        let none = self.asm.label();
                        let done = self.asm.label();
                        self.asm.ld_map(Reg::R1, map);
                        self.key_ptr(Reg::R2);
                        self.asm.call(vesper_ebpf::helpers::MAP_LOOKUP_ELEM);
                        if let Some(d) = d {
                            self.asm.jmp(JmpOp::Eq, Reg::R0, 0, none);
                            self.asm.ldx(vsize, Reg::R1, Reg::R0, 0);
                            self.st(d + 1, Reg::R1);
                            self.st_imm(d, 1);
                            self.asm.ja(done);
                            self.bind_label(none)?;
                            self.st_imm(d, 0);
                            self.st_imm(d + 1, 0);
                            self.bind_label(done)?;
                        } else {
                            self.bind_label(none)?;
                            self.bind_label(done)?;
                        }
                    }
                    MapOp::Update => {
                        self.ld(Reg::R1, args[1].value, 0, span)?;
                        self.st(self.val_slot, Reg::R1);
                        self.update(map);
                    }
                    MapOp::Delete => {
                        self.asm.ld_map(Reg::R1, map);
                        self.key_ptr(Reg::R2);
                        self.asm.call(vesper_ebpf::helpers::MAP_DELETE_ELEM);
                    }
                    MapOp::Add => {
                        let miss = self.asm.label();
                        let done = self.asm.label();
                        self.asm.ld_map(Reg::R1, map);
                        self.key_ptr(Reg::R2);
                        self.asm.call(vesper_ebpf::helpers::MAP_LOOKUP_ELEM);
                        self.asm.jmp(JmpOp::Eq, Reg::R0, 0, miss);
                        self.ld(Reg::R1, args[1].value, 0, span)?;
                        self.asm
                            .atomic(vsize, AtomicOp::Add { fetch: false }, Reg::R0, 0, Reg::R1);
                        self.asm.ja(done);
                        self.bind_label(miss)?;
                        if !kind.is_array() {
                            // First sighting: insert the delta only if
                            // the key is still absent (BPF_NOEXIST). If
                            // another CPU inserted it meanwhile, look it up
                            // again and add atomically, so no count is lost.
                            self.ld(Reg::R1, args[1].value, 0, span)?;
                            self.st(self.val_slot, Reg::R1);
                            self.update_with(map, BPF_NOEXIST);
                            self.asm.jmp(JmpOp::Eq, Reg::R0, 0, done);
                            self.asm.ld_map(Reg::R1, map);
                            self.key_ptr(Reg::R2);
                            self.asm.call(vesper_ebpf::helpers::MAP_LOOKUP_ELEM);
                            self.asm.jmp(JmpOp::Eq, Reg::R0, 0, done);
                            self.ld(Reg::R1, args[1].value, 0, span)?;
                            self.asm.atomic(
                                vsize,
                                AtomicOp::Add { fetch: false },
                                Reg::R0,
                                0,
                                Reg::R1,
                            );
                        }
                        self.bind_label(done)?;
                    }
                }
                let _: MapKind = kind;
            }
        }
        Ok(())
    }

    fn update(&mut self, map: usize) {
        self.update_with(map, BPF_ANY);
    }

    fn update_with(&mut self, map: usize, flags: i32) {
        self.asm.ld_map(Reg::R1, map);
        self.key_ptr(Reg::R2);
        self.val_ptr(Reg::R3);
        self.asm.mov64(Reg::R4, flags);
        self.asm.call(vesper_ebpf::helpers::MAP_UPDATE_ELEM);
    }
}

fn literal(l: &Literal) -> i64 {
    match l {
        Literal::Int(n) => *n as u64 as i64,
        Literal::Bool(b) => i64::from(*b),
        _ => 0,
    }
}

fn compare(op: &str, operand: Repr) -> Option<JmpOp> {
    let signed = operand == Repr::Scalar(Scalar::I64);
    Some(match (op, signed) {
        ("==", _) => JmpOp::Eq,
        ("!=", _) => JmpOp::Ne,
        ("<", false) => JmpOp::Lt,
        ("<=", false) => JmpOp::Le,
        (">", false) => JmpOp::Gt,
        (">=", false) => JmpOp::Ge,
        ("<", true) => JmpOp::SLt,
        ("<=", true) => JmpOp::SLe,
        (">", true) => JmpOp::SGt,
        (">=", true) => JmpOp::SGe,
        _ => return None,
    })
}
