//! Place IR as words, both directions (self-hosting SH3 lane 1, #1240).
//!
//! Every SH3 IR lane reads an `ir::Program` in Vesper and lanes 9-13 write
//! one back. This module is the Rust half of `@vesper/place-ir`
//! (packages/vesper-place-ir):
//!
//! - [`program_input`]: the world and a program, as the `in Str` input of
//!   a `@vesper/place-ir` entry: `@vesper/check-data`'s names, types and
//!   world (`Enc` of tests/common/check_words.rs), then the program
//!   section. `None` when a type literal does not fit 64 bits.
//! - [`decode_program`]: the words `put_program` of the package writes
//!   (names, types, the program section) back into a `Program`, which is
//!   `Debug`-equal to the input of an identity pass.
//! - [`dump`]: the canonical text dump, line for line the package's
//!   `dump_program`, for readable diffs.
//!
//! Both directions go through [`Tables`], the same flat tables the Vesper
//! side keeps: functions, blocks and instructions as rows over one operand
//! pool of tagged words, a path table and a payload pool.
//!
//! **Payloads.** The operation enums of `Op::Seq`, `Op::Vector`,
//! `Op::Tiled` and `Op::Fip` and the expressions of `Op::Closure` and
//! `Op::Handle` stay Rust values in a [`Pool`]: the words carry the pool
//! index, the Vesper tables carry the index and (for the operation enums)
//! the `Debug` text as a name, so a pass can tell `Get` from `Push`. A
//! Vesper pass can copy, move and drop pool references but not make new
//! payloads; a lane that must (bounds hoisting makes `SeqOp::Hoisted`)
//! adds a structured pool kind.
//!
//! The program section (every name a name index, every type a type index
//! of the stream's own tables; `opt` is 0 for none, else index + 1):
//!
//! ```text
//! pool:   n, n x (kind, label name)          kind: 0 seq 1 vector 2 tiled 3 fip 4 expr
//! paths:  n, n x (k, k names)                path 0 is the empty path
//! fns:    n, n x function
//! function: name, header, blocks, trailer
//! header: np, np x (name, pass, nproj, nproj paths, type, span start, end),
//!         result type, effects (a Row type), nb, nb x (name, mode),
//!         nc, nc capture names, nv, nv x (value, type), nl, nl x (name, type)
//! blocks: n, n x (block header, ni, ni x instruction, terminator)
//! block header: entry token, ni, ni x (block, token), outgoing token,
//!         np, np param values
//! instruction: dest, type, span start, end, has token, token in, out,
//!         fallible, opcode, operands (see `OP_NAMES` and `Tables::op`)
//! terminator: 0 return v | 1 jump b, n, args | 2 branch c, yes, no |
//!         3 match v, n, n x (pattern, block) | 4 unreachable
//! trailer: the `fip` side tables (call slots, join slots, box drops)
//! ```
#![allow(dead_code)]

use super::check_words::{Enc, Words, mode_word, pass_word};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use vesper::ast::{Effects, Expr, Literal, Mode, Param, Pass, Pattern, PatternKind, Type};
use vesper::check::{Place, World};
use vesper::fip::{CallSlot, FipDrop, FipJoinSlot, FipOp};
use vesper::ir::{Argument, Block, FunctionIr, Instruction, Op, Program, Terminator};
use vesper::seq::SeqOp;
use vesper::tiled::TiledOp;
use vesper::vector::VectorOp;

// ------------------------------------------------------------------ tags

/// What an operand word is; the dump formats a word by its tag, and the
/// Vesper reader maps name and type indices to symbols and type ids.
pub const T_RAW: u8 = 0;
pub const T_NAME: u8 = 1;
pub const T_OPT_NAME: u8 = 2;
pub const T_TYPE: u8 = 3;
pub const T_PATH: u8 = 4;
pub const T_POOL: u8 = 5;
pub const T_VALUE: u8 = 6;
pub const T_BLOCK: u8 = 7;

/// Opcodes, in `Op` order; the names the dump prints.
pub const OP_NAMES: [&str; 22] = [
    "const", "unit", "read", "bind", "write", "unary", "binary", "tuple", "data", "project",
    "index", "call", "indirect", "try", "closure", "handle", "vector", "tiled", "seq", "fip",
    "loop", "exit",
];

pub const POOL_KINDS: [&str; 5] = ["seq", "vector", "tiled", "fip", "expr"];

// ------------------------------------------------------------------ pool

/// The Rust values the words refer to by index.
#[derive(Debug, Clone)]
pub enum Payload {
    Seq(SeqOp),
    Vector(VectorOp),
    Tiled(TiledOp),
    Fip(FipOp),
    Expr(Expr),
}

impl Payload {
    fn kind(&self) -> u64 {
        match self {
            Payload::Seq(_) => 0,
            Payload::Vector(_) => 1,
            Payload::Tiled(_) => 2,
            Payload::Fip(_) => 3,
            Payload::Expr(_) => 4,
        }
    }

    /// The label the Vesper side sees: the operation's `Debug` text, or
    /// nothing for an expression.
    fn label(&self) -> String {
        match self {
            Payload::Seq(x) => format!("{x:?}"),
            Payload::Vector(x) => format!("{x:?}"),
            Payload::Tiled(x) => format!("{x:?}"),
            Payload::Fip(x) => format!("{x:?}"),
            Payload::Expr(_) => String::new(),
        }
    }
}

/// The payload pool of one encoding. Operations are shared by `Debug`
/// text; every expression gets its own entry.
#[derive(Debug, Clone, Default)]
pub struct Pool {
    pub items: Vec<Payload>,
    ops: BTreeMap<(u64, String), u64>,
}

impl Pool {
    fn add(&mut self, p: Payload) -> u64 {
        if matches!(p, Payload::Expr(_)) {
            self.items.push(p);
            return self.items.len() as u64 - 1;
        }
        let key = (p.kind(), p.label());
        if let Some(i) = self.ops.get(&key) {
            return *i;
        }
        let i = self.items.len() as u64;
        self.items.push(p);
        self.ops.insert(key, i);
        i
    }
}

// ------------------------------------------------------------------ tables

#[derive(Debug, Clone, Copy, Default)]
pub struct FnRow {
    pub name: u64,
    /// Header words: ops[ha..ha + hn].
    pub ha: usize,
    pub hn: usize,
    /// Blocks: blocks[b0..b0 + bc].
    pub b0: usize,
    pub bc: usize,
    /// Trailer words (the fip side tables): ops[ta..ta + tn].
    pub ta: usize,
    pub tn: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BlockRow {
    pub ha: usize,
    pub hn: usize,
    pub i0: usize,
    pub ic: usize,
    pub term: u64,
    pub ta: usize,
    pub tn: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InstRow {
    pub dest: u64,
    pub ty: u64,
    pub s0: u64,
    pub s1: u64,
    pub has_token: u64,
    pub tin: u64,
    pub tout: u64,
    pub fallible: u64,
    pub op: u64,
    pub a: usize,
    pub n: usize,
}

/// A program as the flat tables both sides keep. Name words index
/// `enc.name_list`, type words index the encoder's types (`types` keeps
/// the ones the tables refer to), path words index `paths`.
#[derive(Default)]
pub struct Tables {
    pub enc: Enc,
    pub types: BTreeMap<u64, Type>,
    pub pool: Pool,
    pub paths: Vec<Vec<u64>>,
    path_ids: BTreeMap<Vec<String>, u64>,
    pub fns: Vec<FnRow>,
    pub blocks: Vec<BlockRow>,
    pub insts: Vec<InstRow>,
    pub ops: Vec<u64>,
    pub tags: Vec<u8>,
}

impl Tables {
    fn new() -> Self {
        let mut t = Self::default();
        t.paths.push(vec![]);
        t.path_ids.insert(vec![], 0);
        t
    }

    fn put(&mut self, x: u64, tag: u8) {
        self.ops.push(x);
        self.tags.push(tag);
    }

    fn raw(&mut self, x: u64) {
        self.put(x, T_RAW);
    }

    fn val(&mut self, v: u32) {
        self.put(v as u64, T_VALUE);
    }

    fn blk(&mut self, b: usize) {
        self.put(b as u64, T_BLOCK);
    }

    fn name(&mut self, s: &str) {
        let i = self.enc.name(s);
        self.put(i, T_NAME);
    }

    fn opt_name(&mut self, s: &Option<String>) {
        let i = self.enc.opt_name(s);
        self.put(i, T_OPT_NAME);
    }

    fn ty(&mut self, t: &Type) {
        let i = self.enc.ty(t);
        self.types.entry(i).or_insert_with(|| t.clone());
        self.put(i, T_TYPE);
    }

    fn path(&mut self, p: &[String]) {
        let i = if let Some(i) = self.path_ids.get(p) {
            *i
        } else {
            let names = p.iter().map(|s| self.enc.name(s)).collect();
            let i = self.paths.len() as u64;
            self.paths.push(names);
            self.path_ids.insert(p.to_vec(), i);
            i
        };
        self.put(i, T_PATH);
    }

    fn pool(&mut self, p: Payload) {
        let i = self.pool.add(p);
        self.put(i, T_POOL);
    }

    fn place(&mut self, p: &Place) {
        self.name(&p.root);
        self.path(&p.path);
    }

    fn literal(&mut self, l: &Literal) {
        match l {
            Literal::Int(n) => {
                self.raw(0);
                self.raw(*n as u64);
                self.raw((*n >> 64) as u64);
            }
            Literal::Float(x) => {
                self.raw(1);
                self.raw(x.to_bits());
                self.raw(0);
            }
            Literal::Bool(b) => {
                self.raw(2);
                self.raw(*b as u64);
                self.raw(0);
            }
            Literal::String(s) => {
                self.raw(3);
                self.name(s);
                self.raw(0);
            }
        }
    }

    /// A pattern in prefix order: kind, span, then its parts.
    fn pattern(&mut self, p: &Pattern) {
        let span = |t: &mut Self| {
            t.raw(p.span.start as u64);
            t.raw(p.span.end as u64);
        };
        match &p.kind {
            PatternKind::Wild => {
                self.raw(0);
                span(self);
            }
            PatternKind::Bind(n) => {
                self.raw(1);
                span(self);
                self.name(n);
            }
            PatternKind::Literal(l) => {
                self.raw(2);
                span(self);
                self.literal(l);
            }
            PatternKind::Tuple(ps) => {
                self.raw(3);
                span(self);
                self.raw(ps.len() as u64);
                for c in ps {
                    self.pattern(c);
                }
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                self.raw(4);
                span(self);
                self.name(name);
                self.raw(*named as u64);
                self.raw(fields.len() as u64);
                for (f, c) in fields {
                    self.name(f);
                    self.pattern(c);
                }
            }
        }
    }

    /// Arguments: n, then (pass, value, place root as an optional name,
    /// place path) each; fixed width, so a pass can index them.
    fn args(&mut self, args: &[Argument]) {
        self.raw(args.len() as u64);
        for a in args {
            self.raw(pass_word(a.pass));
            self.val(a.value);
            match &a.place {
                Some(p) => {
                    self.opt_name(&Some(p.root.clone()));
                    self.path(&p.path);
                }
                None => {
                    self.put(0, T_OPT_NAME);
                    self.put(0, T_PATH);
                }
            }
        }
    }

    fn values(&mut self, vs: &[u32]) {
        self.raw(vs.len() as u64);
        for v in vs {
            self.val(*v);
        }
    }

    fn aliases(&mut self, a: &BTreeMap<String, String>) {
        self.raw(a.len() as u64);
        for (k, v) in a {
            self.name(k);
            self.name(v);
        }
    }

    fn moves(&mut self, m: &[Place]) {
        self.raw(m.len() as u64);
        for p in m {
            self.place(p);
        }
    }

    /// The opcode and operands of `op`.
    pub fn op(&mut self, op: &Op) {
        match op {
            Op::Constant(l) => {
                self.raw(0);
                self.literal(l);
            }
            Op::Unit => self.raw(1),
            Op::Read {
                root,
                path,
                pass,
                projections,
            } => {
                self.raw(2);
                self.name(root);
                self.path(path);
                self.raw(pass_word(*pass));
                self.raw(projections.len() as u64);
                for p in projections {
                    self.path(p);
                }
            }
            Op::Bind { pattern, value } => {
                self.raw(3);
                self.val(*value);
                self.pattern(pattern);
            }
            Op::Write { place, op, value } => {
                self.raw(4);
                self.name(place);
                self.name(op);
                self.val(*value);
            }
            Op::Unary(o, v) => {
                self.raw(5);
                self.name(o);
                self.val(*v);
            }
            Op::Binary(o, a, b) => {
                self.raw(6);
                self.name(o);
                self.val(*a);
                self.val(*b);
            }
            Op::Tuple(vs) => {
                self.raw(7);
                self.values(vs);
            }
            Op::Data(n, fs) => {
                self.raw(8);
                self.name(n);
                self.raw(fs.len() as u64);
                for (f, v) in fs {
                    self.name(f);
                    self.val(*v);
                }
            }
            Op::Project(v, f) => {
                self.raw(9);
                self.val(*v);
                self.name(f);
            }
            Op::Index(a, b) => {
                self.raw(10);
                self.val(*a);
                self.val(*b);
            }
            Op::Call {
                callee,
                args,
                types,
                capability,
            } => {
                self.raw(11);
                self.name(callee);
                self.args(args);
                self.raw(types.len() as u64);
                for t in types {
                    self.ty(t);
                }
                self.opt_name(capability);
            }
            Op::Indirect { callee, args } => {
                self.raw(12);
                self.val(*callee);
                self.args(args);
            }
            Op::Try(v) => {
                self.raw(13);
                self.val(*v);
            }
            Op::Closure {
                expr,
                aliases,
                moves,
            } => {
                self.raw(14);
                self.pool(Payload::Expr(expr.clone()));
                self.aliases(aliases);
                self.moves(moves);
            }
            Op::Handle {
                state_value,
                expr,
                aliases,
                moves,
            } => {
                self.raw(15);
                self.raw(state_value.is_some() as u64);
                self.put(state_value.unwrap_or(0) as u64, T_VALUE);
                self.pool(Payload::Expr(expr.clone()));
                self.aliases(aliases);
                self.moves(moves);
            }
            Op::Vector { op, args } => {
                self.raw(16);
                self.pool(Payload::Vector(op.clone()));
                self.values(args);
            }
            Op::Tiled {
                op,
                args,
                capability,
            } => {
                self.raw(17);
                self.pool(Payload::Tiled(op.clone()));
                self.args(args);
                self.opt_name(capability);
            }
            Op::Seq { op, args } => {
                self.raw(18);
                self.pool(Payload::Seq(*op));
                self.args(args);
            }
            Op::Fip { op, args } => {
                self.raw(19);
                self.pool(Payload::Fip(op.clone()));
                self.args(args);
            }
            Op::Loop => self.raw(20),
            Op::Exit { value, owner } => {
                self.raw(21);
                self.val(*value);
                self.name(owner);
            }
        }
    }

    fn terminator(&mut self, t: &Terminator) -> u64 {
        match t {
            Terminator::Return(v) => {
                self.val(*v);
                0
            }
            Terminator::Jump { block, args } => {
                self.blk(*block);
                self.values(args);
                1
            }
            Terminator::Branch { condition, yes, no } => {
                self.val(*condition);
                self.blk(*yes);
                self.blk(*no);
                2
            }
            Terminator::Match { value, arms } => {
                self.val(*value);
                self.raw(arms.len() as u64);
                for (p, b) in arms {
                    self.pattern(p);
                    self.blk(*b);
                }
                3
            }
            Terminator::Unreachable => 4,
        }
    }

    fn header(&mut self, f: &FunctionIr) {
        self.raw(f.params.len() as u64);
        for p in &f.params {
            self.name(&p.name);
            self.raw(pass_word(p.pass));
            self.raw(p.projections.len() as u64);
            for q in &p.projections {
                self.path(q);
            }
            self.ty(&p.ty);
            self.raw(p.span.start as u64);
            self.raw(p.span.end as u64);
        }
        self.ty(&f.result);
        self.ty(&Type::Row(f.effects.clone()));
        self.raw(f.bounds.len() as u64);
        for (n, m) in &f.bounds {
            self.name(n);
            self.raw(mode_word(*m));
        }
        self.raw(f.external_captures.len() as u64);
        for n in &f.external_captures {
            self.name(n);
        }
        self.raw(f.values.len() as u64);
        for (v, t) in &f.values {
            self.val(*v);
            self.ty(t);
        }
        self.raw(f.locals.len() as u64);
        for (n, t) in &f.locals {
            self.name(n);
            self.ty(t);
        }
    }

    fn drop(&mut self, d: &FipDrop) {
        match d {
            FipDrop::Value(v, t) => {
                self.raw(0);
                self.val(*v);
                self.ty(t);
            }
            FipDrop::Place(p, t) => {
                self.raw(1);
                self.place(p);
                self.ty(t);
            }
            FipDrop::OtherFields { value, ty, keep } => {
                self.raw(2);
                self.val(*value);
                self.ty(ty);
                self.name(keep);
            }
        }
    }

    fn inst_drops(&mut self, m: &BTreeMap<usize, Vec<(usize, FipDrop)>>) {
        self.raw(m.len() as u64);
        for (b, ds) in m {
            self.blk(*b);
            self.raw(ds.len() as u64);
            for (i, d) in ds {
                self.raw(*i as u64);
                self.drop(d);
            }
        }
    }

    fn trailer(&mut self, f: &FunctionIr) {
        self.raw(f.fip_call_slots.len() as u64);
        for (v, s) in &f.fip_call_slots {
            self.val(*v);
            match s {
                CallSlot::CallerSlot => {
                    self.raw(0);
                    self.val(0);
                    self.raw(0);
                    self.raw(0);
                }
                CallSlot::ReuseSlot {
                    bind_dest,
                    slot_index,
                    size,
                } => {
                    self.raw(1);
                    self.val(*bind_dest);
                    self.raw(*slot_index as u64);
                    self.raw(*size as u64);
                }
            }
        }
        self.raw(f.fip_join_slots.len() as u64);
        for (b, slots) in &f.fip_join_slots {
            self.blk(*b);
            self.raw(slots.len() as u64);
            for s in slots {
                self.val(s.dest_slot.0);
                self.raw(s.dest_slot.1 as u64);
                self.raw(s.incoming.len() as u64);
                for (pb, v, k) in &s.incoming {
                    self.blk(*pb);
                    self.val(*v);
                    self.raw(*k as u64);
                }
            }
        }
        self.inst_drops(&f.box_drops_before_inst);
        self.inst_drops(&f.box_drops_after_inst);
        self.raw(f.box_drops_before_term.len() as u64);
        for (b, ds) in &f.box_drops_before_term {
            self.blk(*b);
            self.raw(ds.len() as u64);
            for d in ds {
                self.drop(d);
            }
        }
    }

    fn block(&mut self, b: &Block) {
        let ha = self.ops.len();
        self.raw(b.entry_token as u64);
        self.raw(b.incoming_tokens.len() as u64);
        for (s, t) in &b.incoming_tokens {
            self.blk(*s);
            self.raw(*t as u64);
        }
        self.raw(b.outgoing_token as u64);
        self.values(&b.params);
        let hn = self.ops.len() - ha;
        let i0 = self.insts.len();
        for i in &b.instructions {
            self.instruction(i);
        }
        let ta = self.ops.len();
        let term = self.terminator(&b.terminator);
        self.blocks.push(BlockRow {
            ha,
            hn,
            i0,
            ic: b.instructions.len(),
            term,
            ta,
            tn: self.ops.len() - ta,
        });
    }

    fn instruction(&mut self, i: &Instruction) {
        let ty = self.enc.ty(&i.ty);
        self.types.entry(ty).or_insert_with(|| i.ty.clone());
        let (tin, tout) = i.token.unwrap_or((0, 0));
        let a = self.ops.len();
        self.op(&i.op);
        let op = self.ops[a];
        self.insts.push(InstRow {
            dest: i.dest as u64,
            ty,
            s0: i.span.start as u64,
            s1: i.span.end as u64,
            has_token: i.token.is_some() as u64,
            tin: tin as u64,
            tout: tout as u64,
            fallible: i.fallible as u64,
            op,
            // The opcode word is not an operand.
            a: a + 1,
            n: self.ops.len() - a - 1,
        });
    }

    fn function(&mut self, f: &FunctionIr) {
        let name = self.enc.name(&f.name);
        let ha = self.ops.len();
        self.header(f);
        let hn = self.ops.len() - ha;
        let b0 = self.blocks.len();
        for b in &f.blocks {
            self.block(b);
        }
        let ta = self.ops.len();
        self.trailer(f);
        self.fns.push(FnRow {
            name,
            ha,
            hn,
            b0,
            bc: f.blocks.len(),
            ta,
            tn: self.ops.len() - ta,
        });
    }

    /// The tables of `program`.
    pub fn of(program: &Program) -> Self {
        let mut t = Self::new();
        for f in program.functions.values() {
            t.function(f);
        }
        t
    }

    /// The program section's words (see the module comment).
    pub fn section(&self) -> Vec<u64> {
        let mut out = vec![self.pool.items.len() as u64];
        for p in &self.pool.items {
            out.push(p.kind());
            // Every label was interned by `label_names`.
            out.push(self.enc.names[&p.label()]);
        }
        out.push(self.paths.len() as u64);
        for p in &self.paths {
            out.push(p.len() as u64);
            out.extend(p);
        }
        out.push(self.fns.len() as u64);
        for f in &self.fns {
            out.push(f.name);
            out.extend(&self.ops[f.ha..f.ha + f.hn]);
            out.push(f.bc as u64);
            for b in &self.blocks[f.b0..f.b0 + f.bc] {
                out.extend(&self.ops[b.ha..b.ha + b.hn]);
                out.push(b.ic as u64);
                for i in &self.insts[b.i0..b.i0 + b.ic] {
                    out.extend([
                        i.dest,
                        i.ty,
                        i.s0,
                        i.s1,
                        i.has_token,
                        i.tin,
                        i.tout,
                        i.fallible,
                        i.op,
                    ]);
                    out.extend(&self.ops[i.a..i.a + i.n]);
                }
                out.push(b.term);
                out.extend(&self.ops[b.ta..b.ta + b.tn]);
            }
            out.extend(&self.ops[f.ta..f.ta + f.tn]);
        }
        out
    }

    fn label_names(&mut self) {
        let labels: Vec<String> = self.pool.items.iter().map(Payload::label).collect();
        for l in labels {
            self.enc.name(&l);
        }
    }
}

// ------------------------------------------------------------------ input

/// Whether every integer literal type fits the decoder's sign and 64-bit
/// magnitude (128-bit literals are not in the check-data format yet).
pub fn literal_fits(t: &Type) -> bool {
    fn row(e: &Effects) -> bool {
        e.instances.iter().all(|i| i.args.iter().all(literal_fits))
    }
    match t {
        Type::IntLiteral(n) => n.unsigned_abs() <= u64::MAX as u128,
        Type::Named(_, a) | Type::Tuple(a) => a.iter().all(literal_fits),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => params.iter().all(|(_, t)| literal_fits(t)) && literal_fits(result) && row(effects),
        Type::Row(e) => row(e),
        Type::Infer => true,
    }
}

fn function_fits(f: &FunctionIr) -> bool {
    let drop_fits = |d: &FipDrop| match d {
        FipDrop::Value(_, t) | FipDrop::Place(_, t) | FipDrop::OtherFields { ty: t, .. } => {
            literal_fits(t)
        }
    };
    f.params.iter().all(|p| literal_fits(&p.ty))
        && literal_fits(&f.result)
        && literal_fits(&Type::Row(f.effects.clone()))
        && f.values.values().all(literal_fits)
        && f.locals.values().all(literal_fits)
        && f.blocks.iter().all(|b| {
            b.instructions.iter().all(|i| {
                literal_fits(&i.ty)
                    && match &i.op {
                        Op::Call { types, .. } => types.iter().all(literal_fits),
                        _ => true,
                    }
            })
        })
        && f.box_drops_before_inst
            .values()
            .chain(f.box_drops_after_inst.values())
            .all(|ds| ds.iter().all(|(_, d)| drop_fits(d)))
        && f.box_drops_before_term
            .values()
            .all(|ds| ds.iter().all(drop_fits))
}

/// Whether [`program_input`] can encode the world and the program.
pub fn encodable(w: &World, p: &Program) -> bool {
    w.adts.values().all(|a| {
        a.variants
            .iter()
            .all(|v| v.fields.iter().all(|f| literal_fits(&f.ty)))
    }) && w.types.values().all(literal_fits)
        && p.functions.values().all(function_fits)
}

/// The input of a `@vesper/place-ir` entry: check-data's names, types and
/// world, then the program section; and the payload pool the result
/// refers to. `None` when a literal cannot be encoded.
pub fn program_input(w: &World, program: &Program) -> Option<(Vec<u8>, Pool)> {
    if !encodable(w, program) {
        return None;
    }
    let mut t = Tables::of(program);
    t.label_names();
    let section = t.section();
    let world = t.enc.world(w);
    Some((t.enc.finish(&world, &section), t.pool))
}

/// The words `put_program` writes, made by Rust: names, types (no world),
/// then the program section. The Rust half of the round trip.
pub fn program_words(program: &Program) -> Option<(Vec<u64>, Pool)> {
    if !program.functions.values().all(function_fits) {
        return None;
    }
    let mut t = Tables::of(program);
    t.label_names();
    let section = t.section();
    let mut out = vec![t.enc.name_list.len() as u64];
    for n in &t.enc.name_list {
        out.push(n.len() as u64);
        let mut b = n.as_bytes().to_vec();
        while !b.len().is_multiple_of(8) {
            b.push(0);
        }
        for c in b.chunks(8) {
            out.push(u64::from_le_bytes(c.try_into().unwrap()));
        }
    }
    out.push(t.enc.type_count);
    out.extend(&t.enc.type_words);
    out.extend(section);
    Some((out, t.pool))
}

// ------------------------------------------------------------------ decoder

fn pass_of(x: u64) -> Pass {
    match x {
        1 => Pass::Inout,
        2 => Pass::Sink,
        _ => Pass::In,
    }
}

fn mode_of(x: u64) -> Mode {
    match x {
        1 => Mode::Affine,
        2 => Mode::Linear,
        _ => Mode::Copy,
    }
}

/// The program section reader: names, types and paths resolved.
struct Dec<'a> {
    r: Words<'a>,
    names: Vec<String>,
    types: Vec<Type>,
    paths: Vec<Vec<String>>,
    pool: &'a Pool,
    /// Pool indices of the stream, mapped to `pool` (the entries a Vesper
    /// pass kept, which it may have reordered).
    pool_map: Vec<usize>,
}

impl Dec<'_> {
    fn next(&mut self) -> u64 {
        self.r.next()
    }

    fn count(&mut self) -> usize {
        self.r.count()
    }

    fn u32(&mut self) -> u32 {
        self.next() as u32
    }

    fn usize(&mut self) -> usize {
        self.next() as usize
    }

    fn name(&mut self) -> String {
        let i = self.next() as usize;
        self.names.get(i).cloned().unwrap_or_default()
    }

    fn opt_name(&mut self) -> Option<String> {
        let i = self.next() as usize;
        (i != 0).then(|| self.names.get(i - 1).cloned().unwrap_or_default())
    }

    fn ty(&mut self) -> Type {
        let i = self.next() as usize;
        self.types.get(i).cloned().unwrap_or(Type::Infer)
    }

    fn path(&mut self) -> Vec<String> {
        let i = self.next() as usize;
        self.paths.get(i).cloned().unwrap_or_default()
    }

    fn payload(&mut self) -> Payload {
        let i = self.next() as usize;
        let at = self.pool_map.get(i).copied().unwrap_or(usize::MAX);
        self.pool
            .items
            .get(at)
            .cloned()
            .unwrap_or(Payload::Fip(FipOp::BoxNew))
    }

    fn place(&mut self) -> Place {
        let root = self.name();
        let path = self.path();
        Place { root, path }
    }

    fn literal(&mut self) -> Literal {
        let tag = self.next();
        match tag {
            0 => {
                let lo = self.next() as u128;
                let hi = self.next() as u128;
                Literal::Int((hi << 64 | lo) as i128)
            }
            1 => {
                let x = f64::from_bits(self.next());
                self.next();
                Literal::Float(x)
            }
            2 => {
                let b = self.next() != 0;
                self.next();
                Literal::Bool(b)
            }
            _ => {
                let s = self.name();
                self.next();
                Literal::String(s)
            }
        }
    }

    fn pattern(&mut self, depth: usize) -> Pattern {
        let kind = self.next();
        let span = self.usize()..self.usize();
        if depth > 512 {
            return Pattern {
                kind: PatternKind::Wild,
                span,
            };
        }
        let kind = match kind {
            1 => PatternKind::Bind(self.name()),
            2 => PatternKind::Literal(self.literal()),
            3 => {
                let n = self.count();
                PatternKind::Tuple((0..n).map(|_| self.pattern(depth + 1)).collect())
            }
            4 => {
                let name = self.name();
                let named = self.next() != 0;
                let n = self.count();
                let fields = (0..n)
                    .map(|_| {
                        let f = self.name();
                        (f, self.pattern(depth + 1))
                    })
                    .collect();
                PatternKind::Constructor {
                    name,
                    fields,
                    named,
                }
            }
            _ => PatternKind::Wild,
        };
        Pattern { kind, span }
    }

    fn args(&mut self) -> Vec<Argument> {
        let n = self.count();
        (0..n)
            .map(|_| {
                let pass = pass_of(self.next());
                let value = self.u32();
                let root = self.opt_name();
                let path = self.path();
                Argument {
                    pass,
                    value,
                    place: root.map(|root| Place { root, path }),
                }
            })
            .collect()
    }

    fn values(&mut self) -> Vec<u32> {
        let n = self.count();
        (0..n).map(|_| self.u32()).collect()
    }

    fn aliases(&mut self) -> BTreeMap<String, String> {
        let n = self.count();
        (0..n)
            .map(|_| {
                let k = self.name();
                (k, self.name())
            })
            .collect()
    }

    fn moves(&mut self) -> Vec<Place> {
        let n = self.count();
        (0..n).map(|_| self.place()).collect()
    }

    fn expr(&mut self) -> Expr {
        match self.payload() {
            Payload::Expr(e) => e,
            _ => Expr {
                kind: vesper::ast::ExprKind::Tuple(vec![]),
                span: 0..0,
            },
        }
    }

    fn op(&mut self, code: u64) -> Op {
        match code {
            0 => Op::Constant(self.literal()),
            2 => {
                let root = self.name();
                let path = self.path();
                let pass = pass_of(self.next());
                let n = self.count();
                let projections = (0..n).map(|_| self.path()).collect();
                Op::Read {
                    root,
                    path,
                    pass,
                    projections,
                }
            }
            3 => {
                let value = self.u32();
                Op::Bind {
                    value,
                    pattern: self.pattern(0),
                }
            }
            4 => {
                let place = self.name();
                let op = self.name();
                Op::Write {
                    place,
                    op,
                    value: self.u32(),
                }
            }
            5 => {
                let o = self.name();
                Op::Unary(o, self.u32())
            }
            6 => {
                let o = self.name();
                let a = self.u32();
                Op::Binary(o, a, self.u32())
            }
            7 => Op::Tuple(self.values()),
            8 => {
                let n = self.name();
                let k = self.count();
                let fs = (0..k)
                    .map(|_| {
                        let f = self.name();
                        (f, self.u32())
                    })
                    .collect();
                Op::Data(n, fs)
            }
            9 => {
                let v = self.u32();
                Op::Project(v, self.name())
            }
            10 => {
                let a = self.u32();
                Op::Index(a, self.u32())
            }
            11 => {
                let callee = self.name();
                let args = self.args();
                let k = self.count();
                let types = (0..k).map(|_| self.ty()).collect();
                Op::Call {
                    callee,
                    args,
                    types,
                    capability: self.opt_name(),
                }
            }
            12 => {
                let callee = self.u32();
                Op::Indirect {
                    callee,
                    args: self.args(),
                }
            }
            13 => Op::Try(self.u32()),
            14 => {
                let expr = self.expr();
                let aliases = self.aliases();
                Op::Closure {
                    expr,
                    aliases,
                    moves: self.moves(),
                }
            }
            15 => {
                let has = self.next() != 0;
                let v = self.u32();
                let expr = self.expr();
                let aliases = self.aliases();
                Op::Handle {
                    state_value: has.then_some(v),
                    expr,
                    aliases,
                    moves: self.moves(),
                }
            }
            16 => {
                let op = match self.payload() {
                    Payload::Vector(op) => op,
                    _ => return Op::Unit,
                };
                Op::Vector {
                    op,
                    args: self.values(),
                }
            }
            17 => {
                let op = match self.payload() {
                    Payload::Tiled(op) => op,
                    _ => return Op::Unit,
                };
                let args = self.args();
                Op::Tiled {
                    op,
                    args,
                    capability: self.opt_name(),
                }
            }
            18 => {
                let op = match self.payload() {
                    Payload::Seq(op) => op,
                    _ => return Op::Unit,
                };
                Op::Seq {
                    op,
                    args: self.args(),
                }
            }
            19 => {
                let op = match self.payload() {
                    Payload::Fip(op) => op,
                    _ => return Op::Unit,
                };
                Op::Fip {
                    op,
                    args: self.args(),
                }
            }
            20 => Op::Loop,
            21 => {
                let value = self.u32();
                Op::Exit {
                    value,
                    owner: self.name(),
                }
            }
            _ => Op::Unit,
        }
    }

    fn terminator(&mut self, code: u64) -> Terminator {
        match code {
            0 => Terminator::Return(self.u32()),
            1 => {
                let block = self.usize();
                Terminator::Jump {
                    block,
                    args: self.values(),
                }
            }
            2 => {
                let condition = self.u32();
                let yes = self.usize();
                Terminator::Branch {
                    condition,
                    yes,
                    no: self.usize(),
                }
            }
            3 => {
                let value = self.u32();
                let n = self.count();
                let arms = (0..n)
                    .map(|_| {
                        let p = self.pattern(0);
                        (p, self.usize())
                    })
                    .collect();
                Terminator::Match { value, arms }
            }
            _ => Terminator::Unreachable,
        }
    }

    fn drop(&mut self) -> FipDrop {
        match self.next() {
            0 => {
                let v = self.u32();
                FipDrop::Value(v, self.ty())
            }
            1 => {
                let p = self.place();
                FipDrop::Place(p, self.ty())
            }
            _ => {
                let value = self.u32();
                let ty = self.ty();
                FipDrop::OtherFields {
                    value,
                    ty,
                    keep: self.name(),
                }
            }
        }
    }

    fn inst_drops(&mut self) -> BTreeMap<usize, Vec<(usize, FipDrop)>> {
        let n = self.count();
        (0..n)
            .map(|_| {
                let b = self.usize();
                let k = self.count();
                let ds = (0..k)
                    .map(|_| {
                        let i = self.usize();
                        (i, self.drop())
                    })
                    .collect();
                (b, ds)
            })
            .collect()
    }

    fn function(&mut self) -> FunctionIr {
        let name = self.name();
        let np = self.count();
        let params = (0..np)
            .map(|_| {
                let name = self.name();
                let pass = pass_of(self.next());
                let k = self.count();
                let projections = (0..k).map(|_| self.path()).collect();
                let ty = self.ty();
                let span = self.usize()..self.usize();
                Param {
                    name,
                    pass,
                    projections,
                    ty,
                    span,
                }
            })
            .collect();
        let result = self.ty();
        let effects = match self.ty() {
            Type::Row(e) => e,
            _ => Effects::default(),
        };
        let n = self.count();
        let bounds = (0..n)
            .map(|_| {
                let k = self.name();
                (k, mode_of(self.next()))
            })
            .collect();
        let n = self.count();
        let external_captures: BTreeSet<String> = (0..n).map(|_| self.name()).collect();
        let n = self.count();
        let values = (0..n)
            .map(|_| {
                let v = self.u32();
                (v, self.ty())
            })
            .collect();
        let n = self.count();
        let locals = (0..n)
            .map(|_| {
                let k = self.name();
                (k, self.ty())
            })
            .collect();
        let nb = self.count();
        let blocks = (0..nb).map(|_| self.block()).collect();
        let n = self.count();
        let fip_call_slots = (0..n)
            .map(|_| {
                let v = self.u32();
                let kind = self.next();
                let bind_dest = self.u32();
                let slot_index = self.usize();
                let size = self.u32();
                let s = if kind == 0 {
                    CallSlot::CallerSlot
                } else {
                    CallSlot::ReuseSlot {
                        bind_dest,
                        slot_index,
                        size,
                    }
                };
                (v, s)
            })
            .collect();
        let n = self.count();
        let fip_join_slots = (0..n)
            .map(|_| {
                let b = self.usize();
                let k = self.count();
                let slots = (0..k)
                    .map(|_| {
                        let v = self.u32();
                        let s = self.usize();
                        let m = self.count();
                        let incoming = (0..m)
                            .map(|_| {
                                let pb = self.usize();
                                let pv = self.u32();
                                (pb, pv, self.usize())
                            })
                            .collect();
                        FipJoinSlot {
                            dest_slot: (v, s),
                            incoming,
                        }
                    })
                    .collect();
                (b, slots)
            })
            .collect();
        let box_drops_before_inst = self.inst_drops();
        let box_drops_after_inst = self.inst_drops();
        let n = self.count();
        let box_drops_before_term = (0..n)
            .map(|_| {
                let b = self.usize();
                let k = self.count();
                (b, (0..k).map(|_| self.drop()).collect())
            })
            .collect();
        FunctionIr {
            name,
            params,
            result,
            effects,
            bounds,
            external_captures,
            blocks,
            values,
            locals,
            fip_call_slots,
            fip_join_slots,
            box_drops_before_inst,
            box_drops_after_inst,
            box_drops_before_term,
        }
    }

    fn block(&mut self) -> Block {
        let entry_token = self.u32();
        let n = self.count();
        let incoming_tokens = (0..n)
            .map(|_| {
                let b = self.usize();
                (b, self.u32())
            })
            .collect();
        let outgoing_token = self.u32();
        let params = self.values();
        let n = self.count();
        let instructions = (0..n)
            .map(|_| {
                let dest = self.u32();
                let ty = self.ty();
                let span = self.usize()..self.usize();
                let has = self.next() != 0;
                let tin = self.u32();
                let tout = self.u32();
                let fallible = self.next() != 0;
                let code = self.next();
                let op = self.op(code);
                Instruction {
                    dest,
                    op,
                    ty,
                    span,
                    token: has.then_some((tin, tout)),
                    fallible,
                }
            })
            .collect();
        let code = self.next();
        let terminator = self.terminator(code);
        Block {
            entry_token,
            incoming_tokens,
            outgoing_token,
            params,
            instructions,
            terminator,
        }
    }
}

fn read_row(r: &mut Words, names: &[String], types: &[Type]) -> Effects {
    let name = |i: u64| names.get(i as usize).cloned().unwrap_or_default();
    let opt = |i: u64| (i != 0).then(|| name(i - 1));
    let ty = |i: u64| types.get(i as usize).cloned().unwrap_or(Type::Infer);
    let n = r.count();
    let mut instances = Vec::with_capacity(n);
    for _ in 0..n {
        let label = opt(r.next());
        let effect = name(r.next());
        let k = r.count();
        let args = (0..k).map(|_| ty(r.next())).collect();
        instances.push(vesper::ast::Effect {
            label,
            name: effect,
            args,
        });
    }
    let tail = opt(r.next());
    Effects { instances, tail }
}

/// One type of check-data's type section (its parts are earlier).
fn read_type(r: &mut Words, names: &[String], types: &[Type]) -> Type {
    let name = |i: u64| names.get(i as usize).cloned().unwrap_or_default();
    let ty = |i: u64| types.get(i as usize).cloned().unwrap_or(Type::Infer);
    match r.next() {
        0 => {
            let negative = r.next() != 0;
            let magnitude = r.next() as i128;
            Type::IntLiteral(if negative { -magnitude } else { magnitude })
        }
        1 => {
            let n = name(r.next());
            let k = r.count();
            Type::Named(n, (0..k).map(|_| ty(r.next())).collect())
        }
        2 => {
            let k = r.count();
            Type::Tuple((0..k).map(|_| ty(r.next())).collect())
        }
        3 => {
            let k = r.count();
            let params = (0..k)
                .map(|_| {
                    let p = pass_of(r.next());
                    (p, ty(r.next()))
                })
                .collect();
            let result = Box::new(ty(r.next()));
            let effects = read_row(r, names, types);
            let mode = mode_of(r.next());
            Type::Function {
                params,
                result,
                effects,
                mode,
            }
        }
        4 => Type::Row(read_row(r, names, types)),
        _ => Type::Infer,
    }
}

/// Reads the words of `put_program` (or [`program_words`]) back into a
/// `Program`; `pool` is the payload pool of the input encoding. A corrupt
/// stream decodes to garbage the differential tests catch, never a panic.
pub fn decode_program(words: &[u64], pool: &Pool) -> Program {
    let mut r = Words::new(words);
    let n = r.count();
    let names: Vec<String> = (0..n).map(|_| r.text()).collect();
    let k = r.count();
    let mut types = Vec::with_capacity(k);
    for _ in 0..k {
        let t = read_type(&mut r, &names, &types);
        types.push(t);
    }
    let mut d = Dec {
        r,
        names,
        types,
        paths: vec![],
        pool,
        pool_map: vec![],
    };
    // The pool: each entry's kind and label, matched to the input pool by
    // position (a Vesper pass keeps the input's pool and appends nothing).
    let np = d.count();
    for i in 0..np {
        d.next();
        d.next();
        d.pool_map.push(i);
    }
    let n = d.count();
    for _ in 0..n {
        let k = d.count();
        let p: Vec<String> = (0..k).map(|_| d.name()).collect();
        d.paths.push(p);
    }
    let nf = d.count();
    let mut functions = BTreeMap::new();
    for _ in 0..nf {
        let f = d.function();
        functions.insert(f.name.clone(), f);
    }
    Program { functions }
}

// ------------------------------------------------------------------ dump

/// A name as the dump writes it: quoted, with `\`, `"`, and control bytes
/// escaped (`\n`, `\xHH`); other bytes as they are.
pub fn push_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                write!(out, "\\x{:02x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Tables {
    fn word(&self, out: &mut String, x: u64, tag: u8) {
        let names = &self.enc.name_list;
        match tag {
            T_NAME => push_quoted(out, names.get(x as usize).map_or("", |s| s)),
            T_OPT_NAME if x == 0 => out.push('-'),
            T_OPT_NAME => push_quoted(out, names.get(x as usize - 1).map_or("", |s| s)),
            T_TYPE => write!(out, "{:?}", self.types[&x]).unwrap(),
            T_PATH => write!(out, "p{x}").unwrap(),
            T_POOL => write!(out, "#{x}").unwrap(),
            T_VALUE => write!(out, "%{x}").unwrap(),
            T_BLOCK => write!(out, "b{x}").unwrap(),
            _ => write!(out, "{x}").unwrap(),
        }
    }

    fn words(&self, out: &mut String, a: usize, n: usize) {
        for i in a..a + n {
            out.push(' ');
            self.word(out, self.ops[i], self.tags[i]);
        }
    }

    /// The canonical dump (`dump_program` of packages/vesper-place-ir).
    pub fn dump(&self) -> String {
        let mut o = String::new();
        for (i, p) in self.pool.items.iter().enumerate() {
            write!(o, "pool #{i} {} ", POOL_KINDS[p.kind() as usize]).unwrap();
            push_quoted(&mut o, &p.label());
            o.push('\n');
        }
        for (i, p) in self.paths.iter().enumerate() {
            write!(o, "path p{i}").unwrap();
            for n in p {
                o.push(' ');
                self.word(&mut o, *n, T_NAME);
            }
            o.push('\n');
        }
        for f in &self.fns {
            o.push_str("fn ");
            self.word(&mut o, f.name, T_NAME);
            o.push('\n');
            o.push_str("  header");
            self.words(&mut o, f.ha, f.hn);
            o.push('\n');
            for (bi, b) in self.blocks[f.b0..f.b0 + f.bc].iter().enumerate() {
                write!(o, "  b{bi}").unwrap();
                self.words(&mut o, b.ha, b.hn);
                o.push('\n');
                for i in &self.insts[b.i0..b.i0 + b.ic] {
                    write!(o, "    %{} ", i.dest).unwrap();
                    self.word(&mut o, i.ty, T_TYPE);
                    write!(
                        o,
                        " = {}",
                        OP_NAMES.get(i.op as usize).copied().unwrap_or("?")
                    )
                    .unwrap();
                    self.words(&mut o, i.a, i.n);
                    write!(o, " @{}..{}", i.s0, i.s1).unwrap();
                    if i.has_token != 0 {
                        write!(o, " tok {}->{}", i.tin, i.tout).unwrap();
                    }
                    if i.fallible != 0 {
                        o.push_str(" fallible");
                    }
                    o.push('\n');
                }
                write!(o, "    term {}", b.term).unwrap();
                self.words(&mut o, b.ta, b.tn);
                o.push('\n');
            }
            o.push_str("  trailer");
            self.words(&mut o, f.ta, f.tn);
            o.push('\n');
        }
        o
    }
}

/// The canonical dump of `program`.
pub fn dump(program: &Program) -> String {
    let mut t = Tables::of(program);
    t.label_names();
    t.dump()
}
