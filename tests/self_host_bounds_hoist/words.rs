//! The input words of `@vesper/bounds-hoist` (self-hosting SH3 lane 12,
//! #1251): one step of `bounds_hoist::hoist_function` and the function it
//! decides on, as little-endian `u64` words in one `Str`, which any JIT
//! entry can take. `packages/vesper-bounds-hoist/src/input.ves` reads them.
//!
//! The pass reads every part of a function's Place IR that this encodes and
//! nothing else, so its analyses decide exactly as Rust does. Types cross as
//! flags (the queries the pass makes: `u64`, `usize`, sequence, slice, text,
//! `Never`) and as interned names (their `Debug` text, which a header hoist
//! reports); the full type tables are lane 1's `@vesper/place-ir` (#1240),
//! which this format moves onto once it lands.
//!
//! Layout: names (count, then each a byte length and the bytes padded to
//! eight), then the step (kind, header, the enclosing loop's blocks, the
//! headers tried), then the function: parameters, value types, `@fip` slots,
//! box-drop places, blocks.
#![allow(dead_code)]

use std::collections::BTreeMap;
use vesper::ast::{Literal, Pass, Pattern, PatternKind, Type};
use vesper::bounds_hoist::StepKind;
use vesper::check::Place;
use vesper::fip::FipDrop;
use vesper::ir::{Argument, FunctionIr, Op, Terminator};
use vesper::seq::{Access, SeqOp};
use vesper::strings::StrOp;

/// The step an input asks the port to decide.
pub enum Ask<'a> {
    Step(&'a vesper::bounds_hoist::Step),
    /// `bounds_hoist::stable_views`.
    StableViews,
    /// `bounds_hoist::header_hoists`.
    HeaderHoists,
}

pub fn kind_word(k: StepKind) -> u64 {
    match k {
        StepKind::Compared => 0,
        StepKind::Order => 1,
        StepKind::Counted => 2,
        StepKind::ComparedLoop => 3,
        StepKind::Dominated => 4,
    }
}

pub const STABLE_VIEWS: u64 = 5;
pub const HEADER_HOISTS: u64 = 6;

// Type flags.
pub const T_U64: u64 = 1;
pub const T_USIZE: u64 = 2;
pub const T_SEQ: u64 = 4;
pub const T_SLICE: u64 = 8;
pub const T_TEXT: u64 = 16;
pub const T_NEVER: u64 = 32;

/// The flags of a type: of its default type (`numeric::default_type`) for
/// the integer, sequence, slice and text queries, and of the type itself
/// for `Never`.
pub fn type_flags(t: &Type) -> u64 {
    let d = vesper::numeric::default_type(t);
    let mut x = 0;
    if d == Type::named("u64") {
        x |= T_U64;
    }
    if d == Type::named("usize") {
        x |= T_USIZE;
    }
    if vesper::seq::is_seq_type(&d) {
        x |= T_SEQ;
    }
    if vesper::seq::is_slice_type(&d) {
        x |= T_SLICE;
    }
    if vesper::strings::is_text(&d) {
        x |= T_TEXT;
    }
    if *t == Type::named("Never") {
        x |= T_NEVER;
    }
    x
}

/// The sequence operation classes the pass tells apart.
pub fn seq_word(op: SeqOp) -> u64 {
    match op {
        SeqOp::New | SeqOp::WithCapacity | SeqOp::Swap | SeqOp::Replace => 1,
        SeqOp::Get => 2,
        SeqOp::SliceGet => 3,
        SeqOp::Set => 4,
        SeqOp::SliceSet => 5,
        SeqOp::TryGet => 6,
        SeqOp::TrySet => 7,
        SeqOp::Len => 8,
        SeqOp::SliceLen => 9,
        SeqOp::AsSlice => 10,
        SeqOp::SubSlice => 11,
        SeqOp::Str(StrOp::AsBytes) => 12,
        SeqOp::Str(StrOp::Len) => 13,
        SeqOp::Hoisted(a) => 14 + access_word(a),
        SeqOp::BoundsGuard { .. } => 18,
        // Every other operation has a receiver and may change its length.
        _ => 0,
    }
}

pub fn access_word(a: Access) -> u64 {
    match a {
        Access::Get => 0,
        Access::Set => 1,
        Access::TryGet => 2,
        Access::TrySet => 3,
    }
}

fn pass_word(p: Pass) -> u64 {
    match p {
        Pass::In => 0,
        Pass::Inout => 1,
        Pass::Sink => 2,
    }
}

#[derive(Default)]
struct Enc {
    names: BTreeMap<String, u64>,
    list: Vec<String>,
    out: Vec<u64>,
}

impl Enc {
    fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.names.get(s) {
            return *i;
        }
        let i = self.list.len() as u64;
        self.names.insert(s.into(), i);
        self.list.push(s.into());
        i
    }

    fn put(&mut self, x: u64) {
        self.out.push(x);
    }

    fn put_name(&mut self, s: &str) {
        let n = self.name(s);
        self.put(n);
    }

    fn path(&mut self, path: &[String]) {
        self.put(path.len() as u64);
        for p in path {
            self.put_name(p);
        }
    }

    fn place(&mut self, p: &Place) {
        self.put_name(&p.root);
        self.path(&p.path);
    }

    fn type_name(&mut self, t: &Type) {
        let text = format!("{:?}", vesper::numeric::default_type(t));
        self.put_name(&text);
    }

    fn args(&mut self, args: &[Argument]) {
        self.put(args.len() as u64);
        for a in args {
            self.put(pass_word(a.pass));
            self.put(a.value as u64);
            match &a.place {
                Some(p) => {
                    self.put(1);
                    self.place(p);
                }
                None => self.put(0),
            }
        }
    }

    fn values(&mut self, vs: &[u32]) {
        self.put(vs.len() as u64);
        for v in vs {
            self.put(*v as u64);
        }
    }

    fn blocks(&mut self, bs: impl ExactSizeIterator<Item = usize>) {
        self.put(bs.len() as u64);
        for b in bs {
            self.put(b as u64);
        }
    }

    /// The names a pattern binds (`binds`), each once.
    fn pattern(&mut self, p: &Pattern) {
        fn names(p: &Pattern, out: &mut Vec<String>) {
            match &p.kind {
                PatternKind::Bind(n) => {
                    if !out.contains(n) {
                        out.push(n.clone());
                    }
                }
                PatternKind::Tuple(ps) => ps.iter().for_each(|p| names(p, out)),
                PatternKind::Constructor { fields, .. } => {
                    fields.iter().for_each(|(_, p)| names(p, out));
                }
                PatternKind::Wild | PatternKind::Literal(_) => {}
            }
        }
        // A whole-value binding (`let x = v`), else 0.
        match &p.kind {
            PatternKind::Bind(n) => {
                let n = self.name(n);
                self.put(n + 1);
            }
            _ => self.put(0),
        }
        let mut all = vec![];
        names(p, &mut all);
        self.put(all.len() as u64);
        for n in &all {
            self.put_name(n);
        }
    }

    fn op(&mut self, op: &Op) {
        match op {
            Op::Constant(Literal::Int(n)) => {
                self.put(0);
                let m = n.unsigned_abs();
                self.put((*n < 0) as u64);
                self.put(m as u64);
                self.put((m >> 64) as u64);
            }
            Op::Constant(_) => self.put(1),
            Op::Unit => self.put(16),
            Op::Loop => self.put(2),
            Op::Read {
                root, path, pass, ..
            } => {
                self.put(3);
                self.put_name(root);
                self.put(pass_word(*pass));
                self.path(path);
            }
            Op::Bind { pattern, value } => {
                self.put(4);
                self.put(*value as u64);
                self.pattern(pattern);
            }
            Op::Write { place, op, value } => {
                self.put(5);
                self.put(*value as u64);
                self.put(u64::from(op != "="));
                let mut parts = place.split('.');
                self.put_name(parts.next().unwrap_or_default());
                let rest: Vec<String> = parts.map(str::to_owned).collect();
                self.path(&rest);
            }
            Op::Unary(_, v) | Op::Try(v) | Op::Project(v, _) | Op::Exit { value: v, .. } => {
                self.put(6);
                self.put(*v as u64);
            }
            Op::Binary(o, a, b) => {
                self.put(7);
                self.put_name(o);
                self.put(*a as u64);
                self.put(*b as u64);
            }
            Op::Index(a, b) => {
                self.put(8);
                self.values(&[*a, *b]);
            }
            Op::Tuple(vs) | Op::Vector { args: vs, .. } => {
                self.put(8);
                self.values(vs);
            }
            Op::Data(_, fs) => {
                self.put(8);
                let vs: Vec<u32> = fs.iter().map(|(_, v)| *v).collect();
                self.values(&vs);
            }
            Op::Call { callee, args, .. } => {
                self.put(9);
                self.put_name(callee);
                self.args(args);
            }
            Op::Seq { op, args } => {
                self.put(10);
                self.put(seq_word(*op));
                self.args(args);
            }
            Op::Tiled { args, .. } => {
                self.put(11);
                self.args(args);
            }
            Op::Fip { args, .. } => {
                self.put(12);
                self.args(args);
            }
            Op::Indirect { callee, args } => {
                self.put(13);
                self.put(*callee as u64);
                self.args(args);
            }
            Op::Closure { .. } => self.put(14),
            Op::Handle { state_value, .. } => {
                self.put(15);
                self.put(state_value.is_some() as u64);
                self.put(state_value.unwrap_or(0) as u64);
            }
        }
    }

    fn terminator(&mut self, t: &Terminator) {
        match t {
            Terminator::Return(v) => {
                self.put(0);
                self.put(*v as u64);
            }
            Terminator::Jump { block, args } => {
                self.put(1);
                self.put(*block as u64);
                self.values(args);
            }
            Terminator::Branch { condition, yes, no } => {
                self.put(2);
                self.put(*condition as u64);
                self.put(*yes as u64);
                self.put(*no as u64);
            }
            Terminator::Match { value, arms } => {
                self.put(3);
                self.put(*value as u64);
                self.blocks(arms.iter().map(|(_, b)| *b));
            }
            Terminator::Unreachable => self.put(4),
        }
    }

    fn function(&mut self, f: &FunctionIr) {
        self.put(f.params.len() as u64);
        for p in &f.params {
            self.put_name(&p.name);
            self.put(pass_word(p.pass));
            self.put(type_flags(&p.ty));
            self.type_name(&p.ty);
        }
        self.put(f.values.len() as u64);
        for (v, t) in &f.values {
            self.put(*v as u64);
            self.put(type_flags(t));
        }
        self.blocks(f.fip_join_slots.keys().copied());
        let slots: Vec<u32> = f.fip_call_slots.keys().copied().collect();
        self.values(&slots);
        // The places whose boxes a block's drop bookkeeping reclaims.
        let mut drops: Vec<(usize, &Place)> = vec![];
        for m in [&f.box_drops_before_inst, &f.box_drops_after_inst] {
            for (b, ds) in m {
                for (_, d) in ds {
                    if let FipDrop::Place(p, _) = d {
                        drops.push((*b, p));
                    }
                }
            }
        }
        for (b, ds) in &f.box_drops_before_term {
            for d in ds {
                if let FipDrop::Place(p, _) = d {
                    drops.push((*b, p));
                }
            }
        }
        self.put(drops.len() as u64);
        for (b, p) in drops {
            self.put(b as u64);
            self.place(p);
        }
        self.put(f.blocks.len() as u64);
        for b in &f.blocks {
            self.values(&b.params);
            self.put(b.instructions.len() as u64);
            for i in &b.instructions {
                self.put(i.dest as u64);
                self.put(type_flags(&i.ty));
                self.type_name(&i.ty);
                self.put(i.fallible as u64);
                self.op(&i.op);
            }
            self.terminator(&b.terminator);
        }
    }

    fn finish(self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(8 * (self.out.len() + self.list.len() * 3 + 1));
        out.extend((self.list.len() as u64).to_le_bytes());
        for n in &self.list {
            out.extend((n.len() as u64).to_le_bytes());
            out.extend(n.as_bytes());
            while !out.len().is_multiple_of(8) {
                out.push(0);
            }
        }
        for x in &self.out {
            out.extend(x.to_le_bytes());
        }
        out
    }
}

/// The input of one decision on `f`.
pub fn input(ask: &Ask, f: &FunctionIr) -> Vec<u8> {
    let mut e = Enc::default();
    match ask {
        Ask::Step(s) => {
            e.put(kind_word(s.kind));
            e.put(s.header as u64);
            match &s.enclosing {
                Some(l) => {
                    e.put(1);
                    e.blocks(l.iter().copied());
                }
                None => e.put(0),
            }
            e.blocks(s.tried.iter().copied());
        }
        Ask::StableViews | Ask::HeaderHoists => {
            e.put(if matches!(ask, Ask::StableViews) {
                STABLE_VIEWS
            } else {
                HEADER_HOISTS
            });
            e.put(0);
            e.put(0);
            e.put(0);
        }
    }
    e.function(f);
    e.finish()
}

/// The text Rust's header hoists render as: one line each.
pub fn hoists_text(hs: &[vesper::bounds_hoist::HeaderHoist]) -> String {
    let mut o = String::new();
    for h in hs {
        let reads: Vec<String> = h.reads.iter().map(u32::to_string).collect();
        o.push_str(&format!(
            "pre={} place={} ty={:?} reads={} entry={}\n",
            h.preheader,
            h.place.display(),
            h.ty,
            reads.join(","),
            h.entry as u8
        ));
    }
    o
}
