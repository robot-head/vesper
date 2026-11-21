//! The word stream of `@vesper/ir-inline` (self-hosting SH3 lane 11, #1250)
//! and its read-back.
//!
//! `ir_inline::inline` reads the world only through a handful of questions
//! about types and functions (a type's layout, whether it holds a sequence,
//! a `Box` or a function, a function's `fip` flag and the checker's type
//! arguments per call span). Those belong to other ports, so the encoder
//! answers them here, once per type or function, and sends the answers as
//! fact bits. Everything else, the cost model, the call graph and its
//! cycles, the callee and site rules, site folding, partial inlining, the
//! growth budgets and the splicing, runs in Vesper.
//!
//! - [`inline_input`]: names, types (structure and facts), effect rows, the
//!   budget, the program (every function in `Program::functions` order) and
//!   the type substitutions of the generic call sites.
//! - [`decode_report`]: the Vesper `Report`, as Rust's `ir_inline::Report`
//!   fields.
//! - [`dump`]: a canonical text of a program, the same text the package's
//!   `inline_dump` writes for its inlined program. Parts the inliner copies
//!   without reading (operators, sequence operations, handled expressions,
//!   literals) are written as indexes into [`InlineInput::opaque`].
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use vesper::ast::{Effects, ExprKind, Literal, Mode, Pass, Pattern, PatternKind, Type};
use vesper::check::{Place, World};
use vesper::ir::{Argument, Block, FunctionIr, Instruction, Op, Program, Terminator};
use vesper::ir_inline::{Budget, Refusal};

// Type facts (bits of a type's first word).
pub const F_SCALAR: u64 = 1;
pub const F_AGGREGATE: u64 = 2;
pub const F_SEQUENCE: u64 = 4;
pub const F_SLICE: u64 = 8;
pub const F_BOX: u64 = 16;
pub const F_FUNCTION: u64 = 32;
pub const F_PLAIN: u64 = 64;
pub const F_NEVER: u64 = 128;

// Function facts.
pub const W_WORLD: u64 = 1;
pub const W_FIP: u64 = 2;
pub const W_GENERIC: u64 = 4;
pub const W_DYN: u64 = 8;
pub const W_SLOTS: u64 = 16;
pub const W_CAPTURES: u64 = 32;
pub const W_BORROWED_MATCH: u64 = 64;

// Instruction flags.
pub const I_FALLIBLE: u64 = 1;
pub const I_GENERIC_CALL: u64 = 2;

pub fn pass_word(p: Pass) -> u64 {
    match p {
        Pass::In => 0,
        Pass::Inout => 1,
        Pass::Sink => 2,
    }
}

/// `native::contains_function` (crate-private): a function type anywhere in
/// `t`, through the fields of named types.
pub fn contains_function(world: &World, t: &Type) -> bool {
    fn visit(world: &World, t: &Type, seen: &mut BTreeSet<String>) -> bool {
        match t {
            Type::Function { .. } => true,
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if !seen.insert(n.clone()) {
                    return args.iter().any(|t| matches!(t, Type::Function { .. }));
                }
                let result = world.adts.get(n).is_some_and(|a| {
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    a.variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .any(|f| visit(world, &vesper::ast::substitute(&f.ty, &vars), seen))
                });
                seen.remove(n);
                result
            }
            _ => false,
        }
    }
    visit(world, t, &mut BTreeSet::new())
}

/// The facts the inliner asks of a type.
pub fn type_facts(world: &World, t: &Type) -> u64 {
    let layout = vesper::layout::Layout::of(world, t);
    let sequence = vesper::seq::holds_sequence(world, t);
    let boxed = vesper::fip::mentions_box(world, t) || vesper::fip::type_contains_box(world, t);
    let function = contains_function(world, t);
    let mut f = 0;
    if layout.as_ref().is_ok_and(|l| !l.aggregate()) && !sequence {
        f |= F_SCALAR;
    }
    if layout.as_ref().map(|l| l.aggregate()).unwrap_or(true) {
        f |= F_AGGREGATE;
    }
    if sequence {
        f |= F_SEQUENCE;
    }
    if vesper::seq::is_slice_type(t) {
        f |= F_SLICE;
    }
    if boxed {
        f |= F_BOX;
    }
    if function {
        f |= F_FUNCTION;
    }
    if !vesper::fip::may_need_drop(world, t) && !boxed && !function && layout.is_ok() {
        f |= F_PLAIN;
    }
    if t.name() == "Never" {
        f |= F_NEVER;
    }
    f
}

/// Names, types and rows interned as they are first written.
#[derive(Default)]
pub struct Tables {
    pub names: HashMap<String, u64>,
    pub name_list: Vec<String>,
    pub types: HashMap<String, u64>,
    pub type_list: Vec<Type>,
    /// Per type: kind, name, then the argument type indexes.
    type_words: Vec<Vec<u64>>,
    pub rows: HashMap<String, u64>,
    row_words: Vec<Vec<u64>>,
    /// Debug text of every copied-but-unread part -> index.
    pub opaque: HashMap<String, u64>,
    pub opaque_list: Vec<String>,
    /// Per opaque index, the first instruction with that key: what
    /// [`decode_program`] copies the unread parts from.
    pub templates: Vec<Option<Instruction>>,
    /// The literal behind each `lit` opaque index of a pattern.
    pub literals: HashMap<u64, Literal>,
}

impl Tables {
    pub fn name(&mut self, s: &str) -> u64 {
        if let Some(i) = self.names.get(s) {
            return *i;
        }
        let i = self.name_list.len() as u64;
        self.names.insert(s.into(), i);
        self.name_list.push(s.into());
        i
    }

    fn opt_name(&mut self, s: &Option<String>) -> u64 {
        s.as_ref().map_or(0, |s| self.name(s) + 1)
    }

    /// The index of `t`; its parts are interned first.
    pub fn ty(&mut self, t: &Type) -> u64 {
        let key = format!("{t:?}");
        if let Some(i) = self.types.get(&key) {
            return *i;
        }
        let words = match t {
            Type::Named(n, args) => {
                let args: Vec<u64> = args.iter().map(|t| self.ty(t)).collect();
                let mut w = vec![1, self.name(n), args.len() as u64];
                w.extend(args);
                w
            }
            Type::Tuple(args) => {
                let args: Vec<u64> = args.iter().map(|t| self.ty(t)).collect();
                let mut w = vec![2, 0, args.len() as u64];
                w.extend(args);
                w
            }
            Type::Function { .. } => vec![3, 0, 0],
            _ => vec![0, 0, 0],
        };
        let i = self.type_list.len() as u64;
        self.types.insert(key, i);
        self.type_list.push(t.clone());
        self.type_words.push(words);
        i
    }

    /// The index of an effect row (rows are equal exactly when their
    /// indexes are).
    pub fn row(&mut self, e: &Effects) -> u64 {
        let key = format!("{e:?}");
        if let Some(i) = self.rows.get(&key) {
            return *i;
        }
        let mut words = vec![e.instances.len() as u64];
        for i in &e.instances {
            words.push(self.opt_name(&i.label));
            words.push(self.name(&i.name));
            words.push(i.args.len() as u64);
        }
        words.push(self.opt_name(&e.tail));
        let i = self.row_words.len() as u64;
        self.rows.insert(key, i);
        self.row_words.push(words);
        i
    }

    pub fn opaque(&mut self, key: String) -> u64 {
        if let Some(i) = self.opaque.get(&key) {
            return *i;
        }
        let i = self.opaque_list.len() as u64;
        self.opaque.insert(key.clone(), i);
        self.opaque_list.push(key);
        i
    }

    /// The opaque index of `key` if it was interned, else a marker that
    /// never equals an index.
    pub fn opaque_of(&self, key: &str) -> String {
        match self.opaque.get(key) {
            Some(i) => i.to_string(),
            None => format!("?{key}"),
        }
    }
}

/// What the inliner copies from an instruction without reading it.
pub fn opaque_key(i: &Instruction) -> String {
    match &i.op {
        Op::Constant(l) => format!("lit {l:?}"),
        Op::Read {
            pass, projections, ..
        } => format!("read {pass:?} {projections:?}"),
        Op::Write { op, .. } => format!("write {op}"),
        Op::Unary(op, _) => format!("unary {op}"),
        Op::Binary(op, _, _) => format!("binary {op}"),
        Op::Project(_, field) => format!("project {field}"),
        Op::Data(name, fields) => format!(
            "data {name} {:?}",
            fields.iter().map(|(f, _)| f).collect::<Vec<_>>()
        ),
        Op::Call {
            types, capability, ..
        } => format!("call {types:?} {capability:?}"),
        Op::Vector { op, .. } => format!("vector {op:?}"),
        Op::Tiled { op, capability, .. } => format!("tiled {op:?} {capability:?}"),
        Op::Seq { op, .. } => format!("seq {op:?}"),
        Op::Fip { op, .. } => format!("fip {op:?}"),
        Op::Closure { .. } => format!("closure {:?}", i.op),
        Op::Handle { expr, .. } => format!("handle {expr:?}"),
        Op::Exit { owner, .. } => format!("exit {owner}"),
        Op::Unit => "unit".into(),
        Op::Loop => "loop".into(),
        Op::Bind { .. } => "bind".into(),
        Op::Index(..) => "index".into(),
        Op::Try(_) => "try".into(),
        Op::Tuple(_) => "tuple".into(),
        Op::Indirect { .. } => "indirect".into(),
    }
}

/// `ir::nested_name` of every graph a handle lowers.
fn handler_graphs(op: &Op) -> Vec<String> {
    let mut out = vec![];
    if let Op::Handle { expr, .. } = op
        && let ExprKind::Handle { body, ret, ops, .. } = &expr.kind
    {
        out.push(vesper::ir::nested_name("handler_body", &body.span));
        out.push(vesper::ir::nested_name("handler_clause", &ret.body.span));
        for op in ops {
            out.push(vesper::ir::nested_name("handler_clause", &op.body.span));
        }
    }
    out
}

fn place_of(written: &str) -> Place {
    let mut path = written.split('.');
    Place {
        root: path.next().unwrap_or_default().to_owned(),
        path: path.map(str::to_owned).collect(),
    }
}

fn is_generic(world: &World, callee: &str) -> bool {
    world
        .functions
        .get(callee)
        .is_some_and(|s| !s.generics.is_empty())
}

/// The encoder: [`Tables`] plus the output words.
pub struct Enc<'w> {
    pub world: &'w World,
    pub t: Tables,
    out: Vec<u64>,
}

impl<'w> Enc<'w> {
    fn new(world: &'w World) -> Self {
        Self {
            world,
            t: Tables::default(),
            out: vec![],
        }
    }

    fn put(&mut self, x: u64) {
        self.out.push(x);
    }

    fn name(&mut self, s: &str) {
        let n = self.t.name(s);
        self.put(n);
    }

    fn ty(&mut self, t: &Type) {
        let n = self.t.ty(t);
        self.put(n);
    }

    fn path(&mut self, path: &[String]) {
        self.put(path.len() as u64);
        for p in path {
            self.name(p);
        }
    }

    fn literal(&mut self, l: &Literal) {
        match l {
            Literal::Int(n) => {
                self.put(0);
                self.put(*n as u64);
                self.put((*n >> 64) as u64);
            }
            Literal::Bool(b) => {
                self.put(1);
                self.put(*b as u64);
                self.put(0);
            }
            _ => {
                self.put(2);
                self.put(0);
                self.put(0);
            }
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Wild => self.put(0),
            PatternKind::Bind(n) => {
                self.put(1);
                self.name(n);
            }
            PatternKind::Tuple(ps) => {
                self.put(2);
                self.put(ps.len() as u64);
                for p in ps {
                    self.pattern(p);
                }
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                self.put(3);
                self.name(name);
                self.put(*named as u64);
                self.put(fields.len() as u64);
                for (f, p) in fields {
                    self.name(f);
                    self.pattern(p);
                }
            }
            PatternKind::Literal(l) => {
                self.put(4);
                self.literal(l);
                let o = self.t.opaque(format!("lit {l:?}"));
                self.t.literals.entry(o).or_insert_with(|| l.clone());
                self.put(o);
            }
        }
        self.put(p.span.start as u64);
        self.put(p.span.end as u64);
    }

    fn args(&mut self, args: &[Argument]) {
        self.put(args.len() as u64);
        for a in args {
            self.put(a.value as u64);
            self.put(pass_word(a.pass));
            match &a.place {
                Some(p) => {
                    self.put(1);
                    self.name(&p.root);
                    self.path(&p.path);
                }
                None => self.put(0),
            }
        }
    }

    fn instruction(&mut self, i: &Instruction) {
        self.put(i.dest as u64);
        self.ty(&i.ty);
        self.put(i.span.start as u64);
        self.put(i.span.end as u64);
        let generic_call = self
            .world
            .calls
            .get(&(i.span.start, i.span.end))
            .is_some_and(|vars| !vars.is_empty());
        self.put(i.fallible as u64 | if generic_call { I_GENERIC_CALL } else { 0 });
        let o = self.t.opaque(opaque_key(i));
        let slot = o as usize;
        if self.t.templates.len() <= slot {
            self.t.templates.resize(slot + 1, None);
        }
        if self.t.templates[slot].is_none() {
            self.t.templates[slot] = Some(i.clone());
        }
        self.put(o);
        match &i.op {
            Op::Constant(l) => {
                self.put(0);
                self.literal(l);
            }
            Op::Unit => self.put(1),
            Op::Loop => self.put(2),
            Op::Read { root, path, .. } => {
                self.put(3);
                self.name(root);
                self.path(path);
            }
            Op::Bind { pattern, value } => {
                self.put(4);
                self.put(*value as u64);
                self.pattern(pattern);
            }
            Op::Write { place, value, .. } => {
                self.put(5);
                self.put(*value as u64);
                let p = place_of(place);
                self.name(&p.root);
                self.path(&p.path);
            }
            Op::Unary(op, v) => {
                self.put(6);
                self.put((op == "!") as u64);
                self.put(*v as u64);
            }
            Op::Binary(op, a, b) => {
                self.put(7);
                let code = match op.as_str() {
                    "==" => 1,
                    "!=" => 2,
                    "<" => 3,
                    "<=" => 4,
                    ">" => 5,
                    ">=" => 6,
                    _ => 0,
                };
                self.put(code);
                self.put(*a as u64);
                self.put(*b as u64);
            }
            Op::Index(a, b) => {
                self.put(8);
                self.put(*a as u64);
                self.put(*b as u64);
            }
            Op::Project(v, _) => {
                self.put(9);
                self.put(*v as u64);
            }
            Op::Try(v) => {
                self.put(10);
                self.put(*v as u64);
            }
            Op::Tuple(vs) | Op::Vector { args: vs, .. } => {
                self.put(if matches!(i.op, Op::Tuple(_)) { 11 } else { 12 });
                self.put(vs.len() as u64);
                for v in vs {
                    self.put(*v as u64);
                }
            }
            Op::Data(name, fields) => {
                self.put(13);
                self.name(name);
                self.put(fields.len() as u64);
                for (_, v) in fields {
                    self.put(*v as u64);
                }
            }
            Op::Call {
                callee,
                args,
                types,
                capability,
            } => {
                self.put(14);
                self.name(callee);
                self.put(!types.is_empty() as u64 | (capability.is_some() as u64) << 1);
                self.args(args);
            }
            Op::Tiled { args, .. } => {
                self.put(15);
                self.args(args);
            }
            Op::Seq { args, .. } => {
                self.put(16);
                self.args(args);
            }
            Op::Fip { args, .. } => {
                self.put(17);
                self.args(args);
            }
            Op::Indirect { callee, args } => {
                self.put(18);
                self.put(*callee as u64);
                self.args(args);
            }
            Op::Exit { value, owner } => {
                self.put(19);
                self.name(owner);
                self.put(*value as u64);
            }
            Op::Handle {
                state_value,
                aliases,
                moves,
                ..
            } => {
                self.put(20);
                self.put(state_value.is_some() as u64);
                self.put(state_value.unwrap_or(0) as u64);
                self.put(aliases.len() as u64);
                for (k, slot) in aliases {
                    self.name(k);
                    self.name(slot);
                }
                self.put(moves.len() as u64);
                for m in moves {
                    self.name(&m.root);
                    self.path(&m.path);
                }
                let graphs = handler_graphs(&i.op);
                self.put(graphs.len() as u64);
                for g in &graphs {
                    self.name(g);
                }
            }
            Op::Closure { .. } => self.put(21),
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
                self.put(args.len() as u64);
                for v in args {
                    self.put(*v as u64);
                }
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
                self.put(arms.len() as u64);
                for (p, b) in arms {
                    self.pattern(p);
                    self.put(*b as u64);
                }
            }
            Terminator::Unreachable => self.put(4),
        }
    }

    fn function(&mut self, f: &FunctionIr) {
        let w = self.world;
        self.name(&f.name);
        let mut flags = 0;
        if let Some(s) = w.functions.get(&f.name) {
            flags |= W_WORLD;
            if s.fip {
                flags |= W_FIP;
            }
            if !s.generics.is_empty() {
                flags |= W_GENERIC;
            }
        }
        if w.dyn_objects.params.contains_key(&f.name) {
            flags |= W_DYN;
        }
        if !f.fip_call_slots.is_empty()
            || !f.fip_join_slots.is_empty()
            || !f.box_drops_before_inst.is_empty()
            || !f.box_drops_after_inst.is_empty()
            || !f.box_drops_before_term.is_empty()
        {
            flags |= W_SLOTS;
        }
        if !f.external_captures.is_empty() {
            flags |= W_CAPTURES;
        }
        if vesper::fip::borrowed_scrutinees(f).iter().any(|v| {
            f.values
                .get(v)
                .is_some_and(|t| w.mode(t, &f.bounds) != Mode::Copy)
        }) {
            flags |= W_BORROWED_MATCH;
        }
        self.put(flags);
        self.ty(&f.result);
        let r = self.t.row(&f.effects);
        self.put(r);
        self.put(f.params.len() as u64);
        for p in &f.params {
            self.name(&p.name);
            self.put(pass_word(p.pass));
            self.ty(&p.ty);
        }
        self.put(f.bounds.len() as u64);
        for n in f.bounds.keys() {
            self.name(n);
        }
        self.put(f.locals.len() as u64);
        for (n, t) in &f.locals {
            self.name(n);
            self.ty(t);
        }
        self.put(f.values.len() as u64);
        for (v, t) in &f.values {
            self.put(*v as u64);
            self.ty(t);
        }
        self.put(f.blocks.len() as u64);
        for b in &f.blocks {
            self.put(b.params.len() as u64);
            for p in &b.params {
                self.put(*p as u64);
            }
            self.put(b.instructions.len() as u64);
            for i in &b.instructions {
                self.instruction(i);
            }
            self.terminator(&b.terminator);
        }
    }
}

/// An encoded inliner input and what reading the output back needs.
pub struct InlineInput {
    pub bytes: Vec<u8>,
    pub tables: Tables,
}

fn mentions_any(t: &Type, names: &BTreeSet<&String>) -> bool {
    match t {
        Type::Named(n, args) => names.contains(n) || args.iter().any(|t| mentions_any(t, names)),
        Type::Tuple(ts) => ts.iter().any(|t| mentions_any(t, names)),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| mentions_any(t, names))
                || mentions_any(result, names)
                || effects
                    .instances
                    .iter()
                    .flat_map(|e| &e.args)
                    .any(|t| mentions_any(t, names))
        }
        _ => false,
    }
}

/// The inliner's whole input.
pub fn inline_input(world: &World, program: &Program, budget: &Budget) -> InlineInput {
    let mut e = Enc::new(world);
    let unit = e.t.ty(&Type::unit());
    let bind = e.t.opaque("bind".into());
    let mut body = vec![];
    for x in [
        budget.small_callee,
        budget.single_site_callee,
        budget.caller_growth,
        budget.program_growth,
        budget.callee_growth,
        budget.caller_handles,
    ] {
        body.push(x as u64);
    }
    body.push(program.functions.len() as u64);
    std::mem::swap(&mut body, &mut e.out);
    for f in program.functions.values() {
        e.function(f);
    }
    std::mem::swap(&mut body, &mut e.out);
    // The generic call sites: the checker's type arguments, as a map over
    // every type the table holds that mentions one of the callee's
    // parameters, and the substituted effect row.
    let mut sites = BTreeMap::<(String, usize, usize), ()>::new();
    for f in program.functions.values() {
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            if let Op::Call { callee, .. } = &i.op
                && is_generic(world, callee)
                && program.functions.contains_key(callee)
            {
                sites.insert((callee.clone(), i.span.start, i.span.end), ());
            }
        }
    }
    let mut inst = vec![sites.len() as u64];
    for (callee, start, end) in sites.keys() {
        inst.push(e.t.name(callee));
        inst.push(*start as u64);
        inst.push(*end as u64);
        let generics = &world.functions[callee].generics;
        let vars = world.calls.get(&(*start, *end));
        let Some(vars) = vars.filter(|v| generics.iter().all(|g| v.contains_key(&g.name))) else {
            inst.push(0);
            continue;
        };
        inst.push(1);
        let names = vars.keys().collect::<BTreeSet<_>>();
        let mut pairs = vec![];
        let known = e.t.type_list.len();
        for k in 0..known {
            let t = e.t.type_list[k].clone();
            if !mentions_any(&t, &names) {
                continue;
            }
            let s = vesper::ast::substitute(&t, vars);
            let to = e.t.ty(&s);
            if to != k as u64 {
                pairs.push((k as u64, to));
            }
        }
        inst.push(pairs.len() as u64);
        for (a, b) in pairs {
            inst.push(a);
            inst.push(b);
        }
        let effects = &program.functions[callee].effects;
        let row = e.t.row(&vesper::ast::substitute_effects(effects, vars));
        inst.push(row);
    }
    // Header: names, types with their facts, rows.
    let mut head = vec![];
    let facts: Vec<u64> = e.t.type_list.iter().map(|t| type_facts(world, t)).collect();
    head.push(e.t.type_list.len() as u64);
    for (k, words) in e.t.type_words.iter().enumerate() {
        head.push(facts[k]);
        head.extend(words);
    }
    head.push(e.t.row_words.len() as u64);
    for words in &e.t.row_words {
        head.extend(words);
    }
    head.push(unit);
    head.push(bind);
    let mut bytes = Vec::with_capacity(8 * (head.len() + body.len() + inst.len()) + 4096);
    let put = |out: &mut Vec<u8>, x: u64| out.extend(x.to_le_bytes());
    put(&mut bytes, e.t.name_list.len() as u64);
    for n in &e.t.name_list {
        put(&mut bytes, n.len() as u64);
        bytes.extend(n.as_bytes());
        while !bytes.len().is_multiple_of(8) {
            bytes.push(0);
        }
    }
    for x in head.iter().chain(&body).chain(&inst) {
        put(&mut bytes, *x);
    }
    InlineInput { bytes, tables: e.t }
}

// ------------------------------------------------------------------ report

/// The refusal reasons, in the package's numbering.
pub const REASONS: &[&str] = &[
    "recursive",
    "caller",
    "size",
    "growth",
    // Refusal::Callee
    "not a world function",
    "fip",
    "dyn parameters",
    "generic callee with a handler",
    "generic callee with explicit call types",
    "generic callee calls a generic function",
    "generic callee with operators or projections",
    "captures",
    "effect row",
    "fip slots or Box drops",
    "Never result",
    "borrowed match",
    "entry block",
    "Box type",
    "function type",
    "sink slice",
    "closure",
    "try",
    "indirect call",
    "tiled or fip op",
    "foreign exit",
    "return inside a handler",
    // Refusal::Site
    "explicit types or capability",
    "type arguments not recorded",
    "signature",
    "effect row differs from the caller's",
    "caller generics",
    "inout argument is not a local place",
    "borrowed sequence argument is not a local place",
    "handled callee borrows a field place",
    "callee re-binds a borrowed parameter",
];

/// The first reason code of `Refusal::Site`.
const FIRST_SITE: usize = 26;

fn refusal(code: u64) -> Refusal {
    let c = code as usize;
    match c {
        0 => Refusal::Recursive,
        1 => Refusal::Caller,
        2 => Refusal::Size,
        3 => Refusal::Growth,
        _ if c < FIRST_SITE => Refusal::Callee(REASONS[c]),
        _ => Refusal::Site(REASONS[c]),
    }
}

/// `ir_inline::Report`'s public fields, and each function's [`size`] and
/// [`cost`] after inlining (`vesper::ir_inline::{size, cost}`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decoded {
    pub sites: usize,
    pub callees: BTreeMap<String, usize>,
    pub added: usize,
    pub inlined_into: BTreeMap<String, BTreeSet<String>>,
    pub refused: BTreeMap<String, BTreeMap<Refusal, usize>>,
    pub partial: BTreeMap<String, usize>,
    pub folded: usize,
    pub growth: BTreeMap<String, usize>,
    pub constants: usize,
    pub sizes: BTreeMap<String, (usize, usize)>,
}

impl Decoded {
    /// Rust's report and the sizes of its inlined program.
    pub fn of_rust(world: &World, report: &vesper::ir_inline::Report, program: &Program) -> Self {
        Self {
            sites: report.sites,
            callees: report.callees.clone(),
            added: report.added,
            inlined_into: report.inlined_into.clone(),
            refused: report.refused.clone(),
            partial: report.partial.clone(),
            folded: report.folded,
            growth: report.growth.clone(),
            constants: report.constants,
            sizes: program
                .functions
                .iter()
                .map(|(n, f)| {
                    (
                        n.clone(),
                        (
                            vesper::ir_inline::size(f),
                            vesper::ir_inline::cost(world, f),
                        ),
                    )
                })
                .collect(),
        }
    }
}

/// Reads `inline_report_words`'s output (packages/vesper-ir-inline): the
/// counts, then each map as a count and its entries, names as indexes into
/// the input's name table (new names never appear in a report).
pub fn decode_report(words: &[u64], tables: &Tables) -> Decoded {
    let mut at = 0;
    let mut next = || {
        let x = words.get(at).copied().unwrap_or(0);
        at += 1;
        x
    };
    let name = |i: u64| tables.name_list[i as usize].clone();
    let mut d = Decoded {
        sites: next() as usize,
        added: next() as usize,
        folded: next() as usize,
        constants: next() as usize,
        ..Decoded::default()
    };
    for _ in 0..next() {
        let n = name(next());
        d.callees.insert(n, next() as usize);
    }
    for _ in 0..next() {
        let n = name(next());
        let set = d.inlined_into.entry(n).or_default();
        for _ in 0..next() {
            set.insert(name(next()));
        }
    }
    for _ in 0..next() {
        let n = name(next());
        let why = refusal(next());
        *d.refused.entry(n).or_default().entry(why).or_default() += next() as usize;
    }
    for _ in 0..next() {
        let n = name(next());
        d.partial.insert(n, next() as usize);
    }
    for _ in 0..next() {
        let n = name(next());
        d.growth.insert(n, next() as usize);
    }
    for _ in 0..next() {
        let n = name(next());
        let size = next() as usize;
        d.sizes.insert(n, (size, next() as usize));
    }
    d
}

// ----------------------------------------------------------------- program

/// Reads mode 3's output (`inline_entry` of packages/vesper-ir-inline):
/// the report's length and words, then `put_program_words`. Returns Rust's
/// `Report` (its public fields) and the inlined program.
///
/// The words carry each function's locals, value types and blocks; every
/// other field (parameters, bounds, captures, the `fip` side tables) is
/// `input`'s, since the inliner never changes them. An instruction's parts
/// the inliner copies without reading (operators, literals, sequence and
/// vector operations, call types, handled expressions) come from the first
/// input instruction with the same opaque index. State tokens are threaded
/// again with `ir::thread_tokens`, as `ir_inline::inline` does when it
/// changed anything.
pub fn decode_program(
    words: &[u64],
    tables: &Tables,
    input: &Program,
) -> (vesper::ir_inline::Report, Program) {
    let n = words.first().copied().unwrap_or(0) as usize;
    let d = decode_report(&words[1..1 + n], tables);
    let mut report = vesper::ir_inline::Report::default();
    report.sites = d.sites;
    report.callees = d.callees;
    report.added = d.added;
    report.inlined_into = d.inlined_into;
    report.refused = d.refused;
    report.partial = d.partial;
    report.folded = d.folded;
    report.growth = d.growth;
    report.constants = d.constants;
    if report.sites == 0 && report.constants == 0 {
        return (report, input.clone());
    }
    let mut r = Reader {
        w: &words[1 + n..],
        at: 0,
        names: vec![],
        tables,
    };
    for _ in 0..r.next() {
        let len = r.next() as usize;
        let mut bytes = Vec::with_capacity(len);
        for k in 0..len.div_ceil(8) {
            bytes.extend(r.next().to_le_bytes());
            if k == len.div_ceil(8) - 1 {
                bytes.truncate(len);
            }
        }
        r.names
            .push(String::from_utf8(bytes).expect("names are UTF-8"));
    }
    let mut program = Program::default();
    for _ in 0..r.next() {
        let name = r.name();
        let mut f = input.functions[&name].clone();
        f.locals = (0..r.next()).map(|_| (r.name(), r.ty())).collect();
        f.values = (0..r.next()).map(|_| (r.next() as u32, r.ty())).collect();
        f.blocks = (0..r.next()).map(|_| r.block()).collect();
        program.functions.insert(name, f);
    }
    vesper::ir::thread_tokens(&mut program);
    (report, program)
}

struct Reader<'a> {
    w: &'a [u64],
    at: usize,
    names: Vec<String>,
    tables: &'a Tables,
}

impl Reader<'_> {
    fn next(&mut self) -> u64 {
        let x = self.w[self.at];
        self.at += 1;
        x
    }

    fn name(&mut self) -> String {
        let i = self.next() as usize;
        self.names[i].clone()
    }

    fn ty(&mut self) -> Type {
        let i = self.next() as usize;
        self.tables.type_list[i].clone()
    }

    fn path(&mut self) -> Vec<String> {
        (0..self.next()).map(|_| self.name()).collect()
    }

    fn pass(&mut self) -> Pass {
        match self.next() {
            0 => Pass::In,
            1 => Pass::Inout,
            _ => Pass::Sink,
        }
    }

    fn pattern(&mut self) -> Pattern {
        let kind = match self.next() {
            0 => PatternKind::Wild,
            1 => PatternKind::Bind(self.name()),
            2 => PatternKind::Tuple((0..self.next()).map(|_| self.pattern()).collect()),
            3 => {
                let name = self.name();
                let named = self.next() != 0;
                let fields = (0..self.next())
                    .map(|_| (self.name(), self.pattern()))
                    .collect();
                PatternKind::Constructor {
                    name,
                    fields,
                    named,
                }
            }
            _ => {
                self.at += 3;
                let o = self.next();
                PatternKind::Literal(self.tables.literals[&o].clone())
            }
        };
        let start = self.next() as usize;
        let end = self.next() as usize;
        Pattern {
            kind,
            span: start..end,
        }
    }

    fn args(&mut self) -> Vec<Argument> {
        (0..self.next())
            .map(|_| {
                let value = self.next() as u32;
                let pass = self.pass();
                let place = (self.next() != 0).then(|| Place {
                    root: self.name(),
                    path: self.path(),
                });
                Argument { pass, value, place }
            })
            .collect()
    }

    fn values(&mut self) -> Vec<u32> {
        (0..self.next()).map(|_| self.next() as u32).collect()
    }

    fn instruction(&mut self) -> Instruction {
        let dest = self.next() as u32;
        let ty = self.ty();
        let start = self.next() as usize;
        let end = self.next() as usize;
        let fallible = self.next() & I_FALLIBLE != 0;
        let opq = self.next();
        let template = self
            .tables
            .templates
            .get(opq as usize)
            .and_then(|t| t.as_ref())
            .map(|t| t.op.clone());
        let tag = self.next();
        let op = match (tag, template) {
            (0, t) => {
                self.at += 3;
                match t {
                    Some(op @ Op::Constant(_)) => op,
                    _ => Op::Constant(self.tables.literals[&opq].clone()),
                }
            }
            (1, _) => Op::Unit,
            (2, _) => Op::Loop,
            (
                3,
                Some(Op::Read {
                    pass, projections, ..
                }),
            ) => Op::Read {
                root: self.name(),
                path: self.path(),
                pass,
                projections,
            },
            (4, _) => {
                let value = self.next() as u32;
                Op::Bind {
                    value,
                    pattern: self.pattern(),
                }
            }
            (5, Some(Op::Write { op, .. })) => {
                let value = self.next() as u32;
                let place = std::iter::once(self.name())
                    .chain(self.path())
                    .collect::<Vec<_>>()
                    .join(".");
                Op::Write { place, op, value }
            }
            (6, Some(Op::Unary(op, _))) => {
                self.at += 1;
                Op::Unary(op, self.next() as u32)
            }
            (7, Some(Op::Binary(op, _, _))) => {
                self.at += 1;
                let a = self.next() as u32;
                Op::Binary(op, a, self.next() as u32)
            }
            (8, _) => {
                let a = self.next() as u32;
                Op::Index(a, self.next() as u32)
            }
            (9, Some(Op::Project(_, field))) => Op::Project(self.next() as u32, field),
            (10, _) => Op::Try(self.next() as u32),
            (11, _) => Op::Tuple(self.values()),
            (12, Some(Op::Vector { op, .. })) => Op::Vector {
                op,
                args: self.values(),
            },
            (13, Some(Op::Data(name, fields))) => {
                let _ = self.name();
                let values = self.values();
                assert_eq!(values.len(), fields.len(), "data fields");
                Op::Data(
                    name,
                    fields.into_iter().map(|(f, _)| f).zip(values).collect(),
                )
            }
            (
                14,
                Some(Op::Call {
                    types, capability, ..
                }),
            ) => {
                let callee = self.name();
                self.at += 1;
                Op::Call {
                    callee,
                    args: self.args(),
                    types,
                    capability,
                }
            }
            (15, Some(Op::Tiled { op, capability, .. })) => Op::Tiled {
                op,
                args: self.args(),
                capability,
            },
            (16, Some(Op::Seq { op, .. })) => Op::Seq {
                op,
                args: self.args(),
            },
            (17, Some(Op::Fip { op, .. })) => Op::Fip {
                op,
                args: self.args(),
            },
            (18, _) => {
                let callee = self.next() as u32;
                Op::Indirect {
                    callee,
                    args: self.args(),
                }
            }
            (19, _) => {
                let owner = self.name();
                Op::Exit {
                    value: self.next() as u32,
                    owner,
                }
            }
            (20, Some(Op::Handle { expr, .. })) => {
                let has = self.next() != 0;
                let state = self.next() as u32;
                let aliases = (0..self.next())
                    .map(|_| (self.name(), self.name()))
                    .collect();
                let moves = (0..self.next())
                    .map(|_| Place {
                        root: self.name(),
                        path: self.path(),
                    })
                    .collect();
                for _ in 0..self.next() {
                    self.at += 1;
                }
                Op::Handle {
                    state_value: has.then_some(state),
                    expr,
                    aliases,
                    moves,
                }
            }
            (21, Some(op @ Op::Closure { .. })) => op,
            (tag, t) => panic!("opcode {tag} with template {t:?}"),
        };
        Instruction {
            dest,
            op,
            ty,
            span: start..end,
            token: None,
            fallible,
        }
    }

    fn terminator(&mut self) -> Terminator {
        match self.next() {
            0 => Terminator::Return(self.next() as u32),
            1 => {
                let block = self.next() as usize;
                Terminator::Jump {
                    block,
                    args: self.values(),
                }
            }
            2 => {
                let condition = self.next() as u32;
                let yes = self.next() as usize;
                Terminator::Branch {
                    condition,
                    yes,
                    no: self.next() as usize,
                }
            }
            3 => {
                let value = self.next() as u32;
                let arms = (0..self.next())
                    .map(|_| (self.pattern(), self.next() as usize))
                    .collect();
                Terminator::Match { value, arms }
            }
            _ => Terminator::Unreachable,
        }
    }

    fn block(&mut self) -> Block {
        let params = self.values();
        let instructions = (0..self.next()).map(|_| self.instruction()).collect();
        Block {
            entry_token: 0,
            incoming_tokens: vec![],
            outgoing_token: 0,
            params,
            instructions,
            terminator: self.terminator(),
        }
    }
}

// -------------------------------------------------------------------- dump

fn dump_path(o: &mut String, root: &str, path: &[String]) {
    o.push_str(root);
    for p in path {
        o.push('.');
        o.push_str(p);
    }
}

fn dump_pattern(o: &mut String, t: &Tables, p: &Pattern) {
    match &p.kind {
        PatternKind::Wild => o.push('_'),
        PatternKind::Bind(n) => o.push_str(n),
        PatternKind::Tuple(ps) => {
            o.push('(');
            for p in ps {
                dump_pattern(o, t, p);
                o.push(',');
            }
            o.push(')');
        }
        PatternKind::Constructor {
            name,
            fields,
            named,
        } => {
            write!(o, "{name}{}{{", if *named { "!" } else { "" }).unwrap();
            for (f, p) in fields {
                write!(o, "{f}:").unwrap();
                dump_pattern(o, t, p);
                o.push(',');
            }
            o.push('}');
        }
        PatternKind::Literal(l) => {
            write!(o, "#{}", t.opaque_of(&format!("lit {l:?}"))).unwrap();
        }
    }
    write!(o, "@{}-{}", p.span.start, p.span.end).unwrap();
}

fn dump_args(o: &mut String, args: &[Argument]) {
    for a in args {
        write!(o, " {}:{}", a.value, pass_word(a.pass)).unwrap();
        if let Some(p) = &a.place {
            o.push('=');
            dump_path(o, &p.root, &p.path);
        }
    }
}

fn type_of(t: &Tables, ty: &Type) -> String {
    match t.types.get(&format!("{ty:?}")) {
        Some(i) => i.to_string(),
        None => format!("?{ty:?}"),
    }
}

/// The canonical text of `program` (what the package's `inline_dump`
/// writes): locals in name order, values in id order, no state tokens.
pub fn dump(t: &Tables, program: &Program) -> String {
    let mut o = String::new();
    for f in program.functions.values() {
        writeln!(o, "fn {} -> {}", f.name, type_of(t, &f.result)).unwrap();
        for p in &f.params {
            writeln!(
                o,
                "param {} {} {}",
                p.name,
                pass_word(p.pass),
                type_of(t, &p.ty)
            )
            .unwrap();
        }
        for (n, ty) in &f.locals {
            writeln!(o, "local {n} {}", type_of(t, ty)).unwrap();
        }
        for (v, ty) in &f.values {
            writeln!(o, "value {v} {}", type_of(t, ty)).unwrap();
        }
        for (k, b) in f.blocks.iter().enumerate() {
            write!(o, "block {k}").unwrap();
            for p in &b.params {
                write!(o, " {p}").unwrap();
            }
            o.push('\n');
            for i in &b.instructions {
                write!(
                    o,
                    " {} {} {}-{} {} #{}",
                    i.dest,
                    type_of(t, &i.ty),
                    i.span.start,
                    i.span.end,
                    i.fallible as u8,
                    t.opaque_of(&opaque_key(i))
                )
                .unwrap();
                match &i.op {
                    Op::Read { root, path, .. } => {
                        o.push(' ');
                        dump_path(&mut o, root, path);
                    }
                    Op::Bind { pattern, value } => {
                        write!(o, " {value} ").unwrap();
                        dump_pattern(&mut o, t, pattern);
                    }
                    Op::Write { place, value, .. } => write!(o, " {place} {value}").unwrap(),
                    Op::Unary(_, v) | Op::Project(v, _) | Op::Try(v) => write!(o, " {v}").unwrap(),
                    Op::Binary(_, a, b) | Op::Index(a, b) => write!(o, " {a} {b}").unwrap(),
                    Op::Tuple(vs) | Op::Vector { args: vs, .. } => {
                        for v in vs {
                            write!(o, " {v}").unwrap();
                        }
                    }
                    Op::Data(_, fields) => {
                        for (_, v) in fields {
                            write!(o, " {v}").unwrap();
                        }
                    }
                    Op::Call { callee, args, .. } => {
                        write!(o, " {callee}").unwrap();
                        dump_args(&mut o, args);
                    }
                    Op::Tiled { args, .. } | Op::Seq { args, .. } | Op::Fip { args, .. } => {
                        dump_args(&mut o, args)
                    }
                    Op::Indirect { callee, args } => {
                        write!(o, " {callee}").unwrap();
                        dump_args(&mut o, args);
                    }
                    Op::Exit { value, .. } => write!(o, " {value}").unwrap(),
                    Op::Handle {
                        state_value,
                        aliases,
                        moves,
                        ..
                    } => {
                        if let Some(v) = state_value {
                            write!(o, " state {v}").unwrap();
                        }
                        for (k, slot) in aliases {
                            write!(o, " {k}={slot}").unwrap();
                        }
                        for m in moves {
                            o.push_str(" move ");
                            dump_path(&mut o, &m.root, &m.path);
                        }
                    }
                    Op::Constant(_) | Op::Unit | Op::Loop | Op::Closure { .. } => {}
                }
                o.push('\n');
            }
            match &b.terminator {
                Terminator::Return(v) => writeln!(o, " return {v}").unwrap(),
                Terminator::Jump { block, args } => {
                    write!(o, " jump {block}").unwrap();
                    for v in args {
                        write!(o, " {v}").unwrap();
                    }
                    o.push('\n');
                }
                Terminator::Branch { condition, yes, no } => {
                    writeln!(o, " branch {condition} {yes} {no}").unwrap()
                }
                Terminator::Match { value, arms } => {
                    write!(o, " match {value}").unwrap();
                    for (p, b) in arms {
                        o.push(' ');
                        dump_pattern(&mut o, t, p);
                        write!(o, "=>{b}").unwrap();
                    }
                    o.push('\n');
                }
                Terminator::Unreachable => writeln!(o, " unreachable").unwrap(),
            }
        }
    }
    o
}

/// The native path's program before inlining, as `native::compile_entry`
/// builds it when `optimize` is on, without the crate-private bit-builtin
/// lowering (which only turns calls of reserved builtins into operators).
pub fn native_program(world: &World) -> Option<Program> {
    let mut program = vesper::ffi_lowering::drops::lower_world(world);
    vesper::ir_opt::optimize(world, &mut program).ok()?;
    Some(program)
}
