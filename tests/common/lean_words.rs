//! Lean extraction input as words (self-hosting SH5 lane 18, #1341): the
//! input of `@vesper/lean-extract`'s entries, shared by
//! tests/self_host_lean_extract.rs and the hybrid's `lean_extract` swap.
//!
//! [`lift`] lowers the world and lifts each closure the CFG extractor would
//! (`lean_ir::extract_closures`); [`cfg_input_full`] writes check-data's
//! names, types and world, the program section (tests/common/ir_words.rs)
//! of the lifted program, the function sections, then optionally the AST
//! section ([`Ast`]) and the effect and contract sections.

#![allow(dead_code)]

use super::{check_words, ir_words};
use std::collections::BTreeMap;
use vesper::ast::{Effects, ExprKind, Function, Mode, Param, Pass, Type};
use vesper::check::World;
use vesper::ir::{FunctionIr, Op, Program};
// ------------------------------------------------------------ part 2

/// A world and its lowered program with every closure the CFG extractor
/// would lift (`lean_ir::extract_closures`) added as its own function,
/// lowered the way Rust lowers it (in the world extended by the lifted
/// function), and the closure table: per closure instruction its owner,
/// destination, lifted name and captured slots.
pub struct Lifted<'a> {
    pub world: std::borrow::Cow<'a, World>,
    pub program: Program,
    pub closures: Vec<(String, u32, String, Vec<String>)>,
}

pub fn lift(world: &World) -> Lifted<'_> {
    let program = vesper::ir::lower_world(world);
    // The lifted functions are lowered in a copy of the world whose other
    // functions keep their signatures but lose their bodies, one round per
    // nesting depth: lowering a function reads only signatures, so each
    // lifted function's IR is the one Rust's extended world gives, without
    // lowering the whole world once per closure. Most worlds have no
    // closure to lift and are not copied at all.
    let first: Vec<Closure> = program
        .functions
        .values()
        .flat_map(|f| closures_of(world, f))
        .collect();
    if first.is_empty() {
        return Lifted {
            world: std::borrow::Cow::Borrowed(world),
            program,
            closures: vec![],
        };
    }
    let mut bodiless = world.clone();
    for f in bodiless.functions.values_mut() {
        f.body = None;
    }
    let mut lifted: BTreeMap<String, (Function, FunctionIr)> = BTreeMap::new();
    let mut round = first;
    while !round.is_empty() {
        let names: Vec<String> = round.iter().map(|c| c.name.clone()).collect();
        for c in round {
            bodiless.functions.insert(c.name, c.function);
        }
        let lowered = vesper::ir::lower_world(&bodiless);
        round = vec![];
        for name in names {
            if let Some(ir) = lowered.functions.get(&name) {
                round.extend(closures_of(&bodiless, ir));
                lifted.insert(
                    name.clone(),
                    (bodiless.functions[&name].clone(), ir.clone()),
                );
            }
        }
    }
    let mut out = Lifted {
        world: std::borrow::Cow::Owned(world.clone()),
        program: Program::default(),
        closures: vec![],
    };
    for f in program.functions.values() {
        add_closures(&bodiless, f, &lifted, &mut out);
    }
    let added = std::mem::take(&mut out.program.functions);
    out.program = program;
    out.program.functions.extend(added);
    out
}

/// A closure instruction of a function as its lifted function.
pub struct Closure {
    dest: u32,
    name: String,
    function: Function,
    captures: Vec<String>,
}

/// The closures `lean_ir::extract_closures` lifts out of `f`, in
/// instruction order, up to the first whose captures it cannot type.
pub fn closures_of(world: &World, f: &FunctionIr) -> Vec<Closure> {
    let mut out = vec![];
    for i in f.blocks.iter().flat_map(|b| &b.instructions) {
        let Op::Closure { expr, aliases, .. } = &i.op else {
            continue;
        };
        let ExprKind::Closure { params, body, .. } = &expr.kind else {
            continue;
        };
        let Type::Function {
            params: typed,
            result,
            effects,
            ..
        } = &i.ty
        else {
            continue;
        };
        let mut free = vesper::check::free_names(body);
        for p in params {
            free.remove(&p.name);
        }
        let mut captured = vec![];
        let mut captures = vec![];
        for n in free {
            if let Some(slot) = aliases.get(&n) {
                let Some(ty) = f.locals.get(slot) else {
                    return out;
                };
                captured.push(Param {
                    name: n,
                    ty: ty.clone(),
                    pass: Pass::Sink,
                    projections: vec![],
                    span: expr.span.clone(),
                });
                captures.push(slot.clone());
            }
        }
        let mut actual = params.clone();
        for (p, (pass, ty)) in actual.iter_mut().zip(typed) {
            p.pass = *pass;
            p.ty = ty.clone();
        }
        captured.extend(actual);
        let name = format!("$lambda_{}_{}", f.name, i.dest);
        let Some(generics) = world.functions.get(&f.name).map(|g| g.generics.clone()) else {
            return out;
        };
        out.push(Closure {
            dest: i.dest,
            name: name.clone(),
            function: Function {
                name,
                fip: false,
                params: captured,
                result: *result.clone(),
                effects: effects.clone(),
                generics,
                body: Some(*body.clone()),
                span: expr.span.clone(),
            },
            captures,
        });
    }
    out
}

/// Adds `f`'s lifted closures to `out` depth first, each followed by its
/// own: the order Rust lifts them in.
pub fn add_closures(
    world: &World,
    f: &FunctionIr,
    lifted: &BTreeMap<String, (Function, FunctionIr)>,
    out: &mut Lifted,
) {
    for c in closures_of(world, f) {
        let Some((function, ir)) = lifted.get(&c.name) else {
            continue;
        };
        out.world
            .to_mut()
            .functions
            .insert(c.name.clone(), function.clone());
        out.program.functions.insert(c.name.clone(), ir.clone());
        out.closures
            .push((f.name.clone(), c.dest, c.name.clone(), c.captures));
        add_closures(world, ir, lifted, out);
    }
}

/// The input of `extract_cfg_functions`: check-data's names, types and
/// world, the program section (tests/common/ir_words.rs) of the lifted
/// program, then
///
/// ```text
/// sigs:     n, per function of the lifted world (name order): name,
///           source (1, or 0 for a lifted closure), ng, ng x (name, row),
///           np, np x (pass, k, k x (len, len names), type), effectful
/// calls:    n, per call span of the program: start, end, k, k x (name, type)
/// closures: n, per closure: owner, dest, lifted name, nc, nc slot names
/// ```
///
/// `None` when the program has a literal the format cannot hold.
pub fn cfg_input(world: &World) -> Option<Vec<u8>> {
    cfg_input_with(world, false)
}

/// [`cfg_input`], then with `structured` the AST section ([`Ast`]).
pub fn cfg_input_with(world: &World, structured: bool) -> Option<Vec<u8>> {
    cfg_input_full(world, structured, false)
}

/// [`cfg_input_with`], then with `effects` the head's effect section
/// ([`effect_task`]): the input of `extract_module`.
pub fn cfg_input_full(world: &World, structured: bool, effects: bool) -> Option<Vec<u8>> {
    let l = lift(world);
    if !ir_words::encodable(&l.world, &l.program) {
        return None;
    }
    let mut t = ir_words::Tables::of(&l.program);
    t.label_names();
    let mut tail = t.section();
    let enc = &mut t.enc;
    tail.push(l.world.functions.len() as u64);
    for f in l.world.functions.values() {
        tail.push(enc.name(&f.name));
        tail.push(world.functions.contains_key(&f.name) as u64);
        tail.push(f.generics.len() as u64);
        for g in &f.generics {
            tail.push(enc.name(&g.name));
            tail.push(g.row as u64);
        }
        tail.push(f.params.len() as u64);
        for p in &f.params {
            tail.push(check_words::pass_word(p.pass));
            tail.push(p.projections.len() as u64);
            for q in &p.projections {
                tail.push(q.len() as u64);
                for n in q {
                    tail.push(enc.name(n));
                }
            }
            tail.push(enc.ty(&p.ty));
        }
        tail.push((!f.effects.instances.is_empty() || f.effects.tail.is_some()) as u64);
    }
    let spans: std::collections::BTreeSet<(usize, usize)> = l
        .program
        .functions
        .values()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.instructions))
        .filter(|i| matches!(i.op, Op::Call { .. }))
        .map(|i| (i.span.start, i.span.end))
        .collect();
    let calls: Vec<_> = spans
        .iter()
        .filter_map(|s| l.world.calls.get(s).map(|v| (s, v)))
        .collect();
    tail.push(calls.len() as u64);
    for ((a, b), vars) in calls {
        tail.extend([*a as u64, *b as u64, vars.len() as u64]);
        for (n, ty) in vars {
            tail.push(enc.name(n));
            tail.push(enc.ty(ty));
        }
    }
    tail.push(l.closures.len() as u64);
    for (owner, dest, name, captures) in &l.closures {
        tail.push(enc.name(owner));
        tail.push(*dest as u64);
        tail.push(enc.name(name));
        tail.push(captures.len() as u64);
        for c in captures {
            tail.push(enc.name(c));
        }
    }
    if structured {
        let mut a = Ast {
            enc,
            world,
            pats: vec![],
            pat_count: 0,
            exprs: vec![],
            expr_count: 0,
        };
        tail.extend(a.section());
    }
    if effects {
        // `effect_task` over this encoder
        let enc = &mut t.enc;
        tail.push(world.effects.len() as u64);
        for e in world.effects.values() {
            tail.push(enc.name(&e.name));
            tail.push(e.generics.len() as u64);
            for g in &e.generics {
                tail.push(enc.name(&g.name));
            }
            tail.push(e.ops.len() as u64);
            for op in &e.ops {
                tail.push(enc.name(&op.name));
                tail.push(op.params.len() as u64);
                for p in &op.params {
                    tail.push(check_words::pass_word(p.pass));
                    tail.push(enc.ty(&p.ty));
                }
                tail.push(enc.ty(&op.result));
            }
        }
        // the contract section: per `f` with `ensures_f` (name order), `f`,
        // `requires_f` or NONE, `ensures_f`, each with its generics (name,
        // mode bound, row flag)
        let contracts: Vec<_> = world
            .functions
            .values()
            .filter_map(|f| {
                let ensures = world.functions.get(&format!("ensures_{}", f.name))?;
                Some((
                    f,
                    world.functions.get(&format!("requires_{}", f.name)),
                    ensures,
                ))
            })
            .collect();
        tail.push(contracts.len() as u64);
        for (f, requires, ensures) in contracts {
            tail.push(enc.name(&f.name));
            tail.push(requires.map_or(NONE, |r| enc.name(&r.name)));
            tail.push(enc.name(&ensures.name));
            for g in [Some(f), requires, Some(ensures)].into_iter().flatten() {
                tail.push(g.generics.len() as u64);
                for x in &g.generics {
                    tail.push(enc.name(&x.name));
                    tail.push(match x.bound {
                        vesper::ast::Mode::Copy => 0,
                        vesper::ast::Mode::Affine => 1,
                        vesper::ast::Mode::Linear => 2,
                    });
                    tail.push(x.row as u64);
                }
            }
        }
    }
    let enc = &mut t.enc;
    let w = enc.world(&l.world);
    Some(enc.finish(&w, &tail))
}

// ---------------------------------------------------------------------------
// Part 3: the structured extractor (`Generator::function` over `Id`).

pub const NONE: u64 = u64::MAX;

// Expression kinds (`X_*` in packages/vesper-lean-extract/src/ast.ves).
pub const X_LITERAL: u64 = 1;
pub const X_NAME: u64 = 2;
pub const X_TUPLE: u64 = 3;
pub const X_STRUCT: u64 = 4;
pub const X_FIELD: u64 = 5;
pub const X_INDEX: u64 = 6;
pub const X_UNARY: u64 = 7;
pub const X_BINARY: u64 = 8;
pub const X_ASSIGN: u64 = 9;
pub const X_BLOCK: u64 = 10;
pub const X_CALL: u64 = 11;
pub const X_TRY: u64 = 12;
pub const X_IF: u64 = 13;
pub const X_MATCH: u64 = 14;
pub const X_LOOP: u64 = 15;
pub const X_CLOSURE: u64 = 21;
pub const X_HANDLE: u64 = 22;

/// The refusal `seq_lean::INOUT_SLICE`.
pub const INOUT_SLICE: &str =
    "Lean extraction does not model inout Slice arguments: they write through to their sequence";

/// The AST section of the structured extractor's input, after
/// [`cfg_input`]'s sections:
///
/// ```text
/// fip:       1 when the built-in Box is active (`fip::active`)
/// functions: n, per source function (name order): name, body or NONE,
///            np, np x (name, pass, type), result type, polymorphic
/// patterns:  n, n x (kind, start, end, box, payload), children first:
///            0 wild | 1 bind: name | 2 literal: literal |
///            3 tuple: n, ids | 4 constructor: name, named, n, (field, id)
/// exprs:     n, n x (kind, start, end, type, payload), children first
/// literal:   0 int: low, high | 1 float: bits, 0 | 2 bool: v, 0 |
///            3 string: name, 0
/// ```
///
/// An expression's type is `world.types` at its span after
/// `numeric::default_type`, or NONE. The payloads are in [`Ast::expr`].
/// What stays on the Rust side are the classification tables: which call
/// or index `seq::resolve` routes to a sequence operation (and the
/// refusals of `seq_op`), what `tiled::resolve_ir_call` resolves, the
/// numeric intrinsic name tables, `fip::is_box_pattern` and
/// `check::free_names` of a closure body.
pub struct Ast<'a> {
    enc: &'a mut check_words::Enc,
    world: &'a World,
    pats: Vec<u64>,
    pat_count: u64,
    exprs: Vec<u64>,
    expr_count: u64,
}

// Sequence operation codes (`Q_*` in ast.ves).
pub fn seq_code(op: vesper::seq::SeqOp) -> u64 {
    use vesper::seq::SeqOp as Q;
    match op {
        Q::New => 1,
        Q::WithCapacity => 2,
        Q::Len | Q::SliceLen => 3,
        Q::Push => 4,
        Q::Pop => 5,
        Q::Get | Q::SliceGet => 6,
        Q::Set => 7,
        Q::TryGet => 8,
        Q::Last => 9,
        Q::TrySet => 10,
        Q::Clear => 11,
        Q::Truncate => 12,
        Q::Reserve => 13,
        Q::Extend => 14,
        Q::Finish => 15,
        Q::AsSlice => 16,
        Q::SubSlice => 17,
        _ => 0,
    }
}

impl Ast<'_> {
    fn name(&mut self, s: &str) -> u64 {
        self.enc.name(s)
    }

    fn ty(&mut self, t: &Type) -> u64 {
        self.enc.ty(t)
    }

    fn type_of(&self, e: &vesper::ast::Expr) -> Option<Type> {
        self.world
            .types
            .get(&(e.span.start, e.span.end))
            .map(vesper::numeric::default_type)
    }

    fn literal(&mut self, l: &vesper::ast::Literal, out: &mut Vec<u64>) {
        use vesper::ast::Literal;
        match l {
            Literal::Int(n) => out.extend([0, *n as u64, (*n >> 64) as u64]),
            // A literal past `i128::MAX` (#1264): the same two words. No corpus
            // program has one.
            Literal::WideInt(n) => out.extend([0, *n as u64, (*n >> 64) as u64]),
            Literal::Float(x) => out.extend([1, x.to_bits(), 0]),
            Literal::Bool(b) => out.extend([2, *b as u64, 0]),
            Literal::String(s) => {
                let n = self.name(s);
                out.extend([3, n, 0]);
            }
        }
    }

    fn pattern(&mut self, p: &vesper::ast::Pattern) -> u64 {
        use vesper::ast::PatternKind;
        let mut w = vec![];
        let head = |kind: u64, w: &mut Vec<u64>| {
            w.extend([
                kind,
                p.span.start as u64,
                p.span.end as u64,
                vesper::fip::is_box_pattern(self.world, p) as u64,
            ])
        };
        match &p.kind {
            PatternKind::Wild => head(0, &mut w),
            PatternKind::Bind(n) => {
                head(1, &mut w);
                w.push(self.name(n));
            }
            PatternKind::Literal(l) => {
                head(2, &mut w);
                self.literal(l, &mut w);
            }
            PatternKind::Tuple(ps) => {
                let kids: Vec<u64> = ps.iter().map(|c| self.pattern(c)).collect();
                head(3, &mut w);
                w.push(kids.len() as u64);
                w.extend(kids);
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                let kids: Vec<(u64, u64)> = fields
                    .iter()
                    .map(|(f, c)| {
                        let c = self.pattern(c);
                        (self.name(f), c)
                    })
                    .collect();
                head(4, &mut w);
                w.push(self.name(name));
                w.push(*named as u64);
                w.push(kids.len() as u64);
                for (f, c) in kids {
                    w.extend([f, c]);
                }
            }
        }
        self.pats.extend(w);
        self.pat_count += 1;
        self.pat_count - 1
    }

    fn opt(&mut self, e: &Option<Box<vesper::ast::Expr>>) -> u64 {
        match e {
            Some(e) => self.expr(e),
            None => NONE,
        }
    }

    /// A refusal or operation of `seq_op`: 0 | 1 code | 2 message.
    fn seq_route(
        &mut self,
        op: vesper::seq::SeqOp,
        first: Option<&vesper::ast::Expr>,
        out: &mut Vec<u64>,
    ) {
        use vesper::seq::SeqOp as Q;
        let refusal = match op {
            Q::Str(s) => Some(vesper::strings::lean_refusal(s)),
            Q::CopyFrom { .. }
            | Q::CopyWithin { .. }
            | Q::Fill
            | Q::Resize
            | Q::FillRange { .. }
            | Q::ExtendFromWithin { .. }
            | Q::ExtendReversed
            | Q::Wide(_)
            | Q::Swap
            | Q::Replace
            | Q::Vector(_) => Some(format!(
                "the Lean model of sequences has no ChunkSeq::{} yet",
                op.name()
            )),
            _ if op.byte_scan() => Some(format!(
                "the Lean model of sequences has no ChunkSeq::{} yet",
                op.name()
            )),
            Q::Hoisted(_) | Q::BoundsGuard { .. } => Some(format!(
                "the Lean model has no native-only ChunkSeq::{}",
                op.name()
            )),
            Q::SubSlice
                if first
                    .and_then(|r| self.type_of(r))
                    .is_some_and(|t| vesper::strings::is_text(&t)) =>
            {
                Some(vesper::strings::lean_refusal(
                    vesper::strings::StrOp::SubStr,
                ))
            }
            Q::SliceSet => Some(INOUT_SLICE.into()),
            Q::TrySet
                if first
                    .and_then(|r| self.type_of(r))
                    .is_some_and(|t| vesper::seq::is_slice_type(&t)) =>
            {
                Some(INOUT_SLICE.into())
            }
            _ => None,
        };
        match refusal {
            Some(m) => {
                let m = self.name(&m);
                out.extend([2, m]);
            }
            None => {
                let c = seq_code(op);
                assert!(c != 0, "unrouted sequence operation {op:?}");
                out.extend([1, c]);
            }
        }
    }

    /// The tiled route of a call: op (0 none), width or field name,
    /// skip-first, place operand (0 none, 1 receiver, 2 first argument).
    fn tiled_route(
        &mut self,
        e: &vesper::ast::Expr,
        callee: &vesper::ast::Expr,
        types: &[Type],
        args: &[vesper::ast::Arg],
        out: &mut Vec<u64>,
    ) {
        use vesper::ast::ExprKind;
        use vesper::tiled::TiledOp as T;
        let route = match &callee.kind {
            ExprKind::Name(n) => Some((n.as_str(), None)),
            ExprKind::Field(base, method)
                if self
                    .type_of(base)
                    .as_ref()
                    .is_some_and(vesper::tiled::is_seq_type) =>
            {
                Some((method.as_str(), Some(base.as_ref())))
            }
            _ => None,
        }
        .and_then(|(name, receiver)| {
            vesper::lean::tiled_route(self.world, name, receiver, types, args, e)
                .map(|op| (op, receiver.is_some()))
        });
        let Some((op, receiver)) = route else {
            out.extend([0, 0, 0, 0]);
            return;
        };
        let (code, extra) = match &op {
            T::New { .. } => (1, 0),
            T::WithCapacity { .. } => (2, 0),
            T::WithRegion { .. } => (3, 0),
            T::Len => (4, 0),
            T::TileCount { tile_width } => (5, u64::from(*tile_width)),
            T::Get { .. } => (6, 0),
            T::ColGet { field, .. } => (7, self.name(field)),
            T::Push { .. } => (8, 0),
            T::Set { .. } => (9, 0),
            T::ColSet { field, .. } => (10, self.name(field)),
            T::LoadTileVec { .. } => (11, 0),
        };
        let creates = matches!(
            op,
            T::New { .. } | T::WithCapacity { .. } | T::WithRegion { .. }
        );
        let place = if receiver {
            1
        } else if !creates && !args.is_empty() {
            2
        } else {
            0
        };
        out.extend([
            code,
            extra,
            matches!(op, T::WithRegion { .. }) as u64,
            place,
        ]);
    }

    /// The name class of a plain callee: float refusal (name or NONE),
    /// integer intrinsic, wrapping operator (name or NONE), Box::new.
    fn name_class(&mut self, n: &str, out: &mut Vec<u64>) {
        let float = vesper::float_math::lean_refusal(n).map_or(NONE, |m| self.name(&m));
        let int = (vesper::numeric::conversion(n).is_some()
            || vesper::numeric::rotate_intrinsic(n).is_some()) as u64;
        let wrapping = vesper::numeric::wrapping_intrinsic(n).map_or(NONE, |op| self.name(op));
        let boxed = (n == vesper::fip::BOX_NEW || n == vesper::fip::BOX_NEW_IN_CALLER) as u64;
        out.extend([float, int, wrapping, boxed]);
    }

    /// An expression row (children first); returns its index. Payloads:
    ///
    /// - literal: literal; name: name; tuple: n, ids;
    /// - struct: name, n, n x (field, id); field: base, name;
    /// - index: base, index, route (0 none, 1 element, 3 interval, 4
    ///   refusal: message);
    /// - unary: op, operand; binary: op, lhs, rhs; assign: op, place, value;
    /// - block: n, n x statement (0 expr: id | 1 let: pattern, type or
    ///   NONE, fallback type or NONE, value), tail or NONE;
    /// - call: callee, n, n x (pass, id), m, m types, sequence route (kind,
    ///   word), tiled route (4 words), name class (4 words);
    /// - try: operand; if: condition, then, else or NONE;
    /// - match: value, n, n x (pattern, body);
    /// - loops, ranges, break, continue and return (kind 15): nothing;
    /// - closure: arrow type or NONE, n, n param names, body, k, k free
    ///   names (`check::free_names` without the parameters), the declared
    ///   effects (a row holder, [`Ast::row_holder`]);
    /// - handle: see [`Ast::handle`].
    ///
    /// Calls end with the `instance_key` of an `E::op` callee or NONE.
    fn expr(&mut self, e: &vesper::ast::Expr) -> u64 {
        use vesper::ast::{ExprKind, Statement};
        let mut w: Vec<u64> = vec![];
        let kind = match &e.kind {
            ExprKind::Literal(l) => {
                self.literal(l, &mut w);
                X_LITERAL
            }
            ExprKind::Name(n) => {
                w.push(self.name(n));
                X_NAME
            }
            ExprKind::Tuple(es) => {
                let kids: Vec<u64> = es.iter().map(|c| self.expr(c)).collect();
                w.push(kids.len() as u64);
                w.extend(kids);
                X_TUPLE
            }
            ExprKind::Struct { name, fields } => {
                let kids: Vec<(u64, u64)> = fields
                    .iter()
                    .map(|(f, c)| {
                        let c = self.expr(c);
                        (self.name(f), c)
                    })
                    .collect();
                w.push(self.name(name));
                w.push(kids.len() as u64);
                for (f, c) in kids {
                    w.extend([f, c]);
                }
                X_STRUCT
            }
            ExprKind::Field(base, f) => {
                let b = self.expr(base);
                w.extend([b, self.name(f)]);
                X_FIELD
            }
            ExprKind::Index(base, index) => {
                let b = self.expr(base);
                let i = self.expr(index);
                w.extend([b, i]);
                if vesper::seq::interval(e).is_some() {
                    let mut r = vec![];
                    self.seq_route(vesper::seq::SeqOp::SubSlice, Some(base), &mut r);
                    if r[0] == 1 {
                        w.push(3);
                    } else {
                        w.extend([4, r[1]]);
                    }
                } else {
                    match self.type_of(base) {
                        Some(t)
                            if vesper::seq::is_seq_type(&t) || vesper::seq::is_slice_type(&t) =>
                        {
                            w.push(1)
                        }
                        _ => w.push(0),
                    }
                }
                X_INDEX
            }
            ExprKind::Unary(op, v) => {
                let v = self.expr(v);
                w.extend([self.name(op), v]);
                X_UNARY
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.expr(a);
                let b = self.expr(b);
                w.extend([self.name(op), a, b]);
                X_BINARY
            }
            ExprKind::Assign(op, p, v) => {
                let p = self.expr(p);
                let v = self.expr(v);
                w.extend([self.name(op), p, v]);
                X_ASSIGN
            }
            ExprKind::Block { statements, tail } => {
                let mut ss = vec![];
                for s in statements {
                    match s {
                        Statement::Expr(x) => {
                            let x = self.expr(x);
                            ss.extend([0, x]);
                        }
                        Statement::Let { pat, ty, value, .. } => {
                            let p = self.pattern(pat);
                            let v = self.expr(value);
                            let t = if *ty == Type::Infer {
                                NONE
                            } else {
                                self.ty(ty)
                            };
                            let fallback = match &value.kind {
                                ExprKind::Struct { name, .. } => self.ty(&Type::named(name)),
                                ExprKind::Literal(l) => self.ty(&vesper::check::literal_type(l)),
                                _ => NONE,
                            };
                            ss.extend([1, p, t, fallback, v]);
                        }
                    }
                }
                let t = self.opt(tail);
                w.push(statements.len() as u64);
                w.extend(ss);
                w.push(t);
                X_BLOCK
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let c = self.expr(callee);
                let kids: Vec<(u64, u64)> = args
                    .iter()
                    .map(|a| (check_words::pass_word(a.pass), self.expr(&a.expr)))
                    .collect();
                let ts: Vec<u64> = types.iter().map(|t| self.enc.ty(t)).collect();
                w.extend([c, kids.len() as u64]);
                for (p, x) in kids {
                    w.extend([p, x]);
                }
                w.push(ts.len() as u64);
                w.extend(ts);
                // `seq_expr`'s call branch (the scope test is the port's).
                let seq = match &callee.kind {
                    ExprKind::Name(n) if !self.world.functions.contains_key(n) => {
                        vesper::seq::resolve(n, None).map(|op| (op, args.first().map(|a| &a.expr)))
                    }
                    ExprKind::Field(base, method) => self.type_of(base).and_then(|t| {
                        vesper::seq::resolve(method, Some(&t))
                            .map(|op| (op.with_arity(args.len()), Some(base.as_ref())))
                    }),
                    _ => None,
                };
                match seq {
                    Some((op, first)) => self.seq_route(op, first, &mut w),
                    None => w.extend([0, 0]),
                }
                self.tiled_route(e, callee, types, args, &mut w);
                match &callee.kind {
                    ExprKind::Name(n) => self.name_class(n, &mut w),
                    _ => w.extend([NONE, 0, NONE, 0]),
                }
                // `instance_key` of an unlabeled `E::op(...)` with explicit
                // types (the typed layer search of `Generator::call`).
                let key = match &callee.kind {
                    ExprKind::Name(n) => n
                        .split_once("::")
                        .filter(|(eff, _)| self.world.effects.contains_key(*eff))
                        .and_then(|(eff, _)| vesper::numeric::instance_key(eff, types)),
                    _ => None,
                };
                w.push(key.map_or(NONE, |k| self.name(&k)));
                X_CALL
            }
            ExprKind::Try(v) => {
                w.push(self.expr(v));
                X_TRY
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(condition);
                let t = self.expr(then);
                let o = self.opt(otherwise);
                w.extend([c, t, o]);
                X_IF
            }
            ExprKind::Match { value, arms } => {
                let v = self.expr(value);
                let kids: Vec<(u64, u64)> = arms
                    .iter()
                    .map(|(p, b)| {
                        let p = self.pattern(p);
                        (p, self.expr(b))
                    })
                    .collect();
                w.extend([v, kids.len() as u64]);
                for (p, b) in kids {
                    w.extend([p, b]);
                }
                X_MATCH
            }
            ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_) => X_LOOP,
            ExprKind::Closure { params, body, .. } => {
                let b = self.expr(body);
                let arrow = self
                    .world
                    .types
                    .get(&(e.span.start, e.span.end))
                    .cloned()
                    .map_or(NONE, |t| self.ty(&t));
                w.push(arrow);
                w.push(params.len() as u64);
                for p in params {
                    w.push(self.name(&p.name));
                }
                w.push(b);
                let mut free = vesper::check::free_names(body);
                for p in params {
                    free.remove(&p.name);
                }
                w.push(free.len() as u64);
                for n in &free {
                    w.push(self.name(n));
                }
                let declared = match &e.kind {
                    ExprKind::Closure { effects, .. } => effects.clone(),
                    _ => unreachable!(),
                };
                w.push(self.row_holder(&declared));
                X_CLOSURE
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                self.handle(effect, state, body, ret, ops, &mut w);
                X_HANDLE
            }
        };
        let ty = self.type_of(e).map_or(NONE, |t| self.ty(&t));
        self.exprs
            .extend([kind, e.span.start as u64, e.span.end as u64, ty]);
        self.exprs.extend(w);
        self.expr_count += 1;
        self.expr_count - 1
    }

    fn section(&mut self) -> Vec<u64> {
        let world = self.world;
        let mut out = vec![vesper::fip::active(world) as u64];
        out.push(world.functions.len() as u64);
        for f in world.functions.values() {
            out.push(self.name(&f.name));
            out.push(f.body.as_ref().map_or(NONE, |b| self.expr(b)));
            out.push(f.params.len() as u64);
            for p in &f.params {
                out.push(self.name(&p.name));
                out.push(check_words::pass_word(p.pass));
                out.push(self.ty(&p.ty));
            }
            out.push(self.ty(&f.result));
            let effectful = !f.effects.instances.is_empty() || f.effects.tail.is_some();
            let polymorphic = effectful
                || effectful_arrow(&f.result)
                || f.params.iter().any(|p| effectful_arrow(&p.ty));
            out.push(polymorphic as u64);
            out.push(self.row_holder(&f.effects));
            out.push(self.ty(&f.result));
        }
        out.push(self.pat_count);
        out.extend(&self.pats);
        out.push(self.expr_count);
        out.extend(&self.exprs);
        // The effect declarations (name order): name, linear, generics,
        // and per operation its name, parameters (pass, projections, type)
        // and result.
        out.push(world.effects.len() as u64);
        for e in world.effects.values() {
            out.push(self.name(&e.name));
            out.push(e.linear as u64);
            out.push(e.generics.len() as u64);
            for g in &e.generics {
                out.push(self.name(&g.name));
            }
            out.push(e.ops.len() as u64);
            for op in &e.ops {
                out.push(self.name(&op.name));
                out.push(op.params.len() as u64);
                for p in &op.params {
                    out.push(check_words::pass_word(p.pass));
                    out.push(p.projections.len() as u64);
                    for q in &p.projections {
                        out.push(q.len() as u64);
                        for n in q {
                            out.push(self.name(n));
                        }
                    }
                    out.push(self.ty(&p.ty));
                }
                out.push(self.ty(&op.result));
            }
        }
        out
    }

    /// A row as the arrow type `() -> Unit / effects`, whose row the port
    /// reads (the input has no row values of their own).
    fn row_holder(&mut self, effects: &Effects) -> u64 {
        self.ty(&Type::Function {
            params: vec![],
            result: Box::new(Type::unit()),
            effects: effects.clone(),
            mode: Mode::Affine,
        })
    }

    fn clause(&mut self, c: &vesper::ast::Clause, names: bool, w: &mut Vec<u64>) {
        let pats: Vec<u64> = c.pats.iter().map(|p| self.pattern(p)).collect();
        let body = self.expr(&c.body);
        w.push(self.name(&c.name));
        w.push(pats.len() as u64);
        w.extend(pats);
        w.push(body);
        let free = vesper::check::clause_free_names(c);
        w.push(free.len() as u64);
        for n in &free {
            w.push(self.name(n));
        }
        if names {
            // The names the body mentions with their spans
            // (`post_resume_access`).
            let mut found = vec![];
            vesper::lean::visit_exprs(&c.body, &mut |x| {
                if let ExprKind::Name(n) = &x.kind {
                    found.push((n.clone(), x.span.start, x.span.end));
                }
            });
            w.push(found.len() as u64);
            for (n, a, b) in found {
                w.extend([self.name(&n), a as u64, b as u64]);
            }
        }
    }

    /// A handler's payload: the effect (name, label or NONE, n, n types,
    /// `instance_key` or NONE), the state pattern and expression (or NONE
    /// twice), the body, the return clause and n operation clauses (name, n,
    /// n patterns, body, k, k free names; operation clauses then add m, m x
    /// (name, start, end) of the body's names), the body's written names,
    /// and what `handler` needs for `routes`: whether the body has a closure,
    /// then n call entries (1: a named callee, its function or NONE and
    /// whether its checked type is an effectful arrow; 2: a method callee,
    /// its receiver name or NONE and the functions named `*::method`; 3:
    /// any other callee).
    fn handle(
        &mut self,
        effect: &vesper::ast::Effect,
        state: &Option<(vesper::ast::Pattern, Box<vesper::ast::Expr>)>,
        body: &vesper::ast::Expr,
        ret: &vesper::ast::Clause,
        ops: &[vesper::ast::Clause],
        w: &mut Vec<u64>,
    ) {
        let (sp, se) = match state {
            Some((p, e)) => (self.pattern(p), self.expr(e)),
            None => (NONE, NONE),
        };
        let b = self.expr(body);
        w.push(self.name(&effect.name));
        w.push(effect.label.as_ref().map_or(NONE, |l| self.name(l)));
        w.push(effect.args.len() as u64);
        for t in &effect.args {
            let t = self.ty(t);
            w.push(t);
        }
        let key = vesper::numeric::instance_key(&effect.name, &effect.args);
        w.push(key.map_or(NONE, |k| self.name(&k)));
        w.extend([sp, se, b]);
        self.clause(ret, false, w);
        w.push(ops.len() as u64);
        for c in ops {
            self.clause(c, true, w);
        }
        let written = vesper::lean::handler_written_names(self.world, body);
        w.push(written.len() as u64);
        for n in &written {
            w.push(self.name(n));
        }
        let mut lexical = std::collections::BTreeSet::new();
        vesper::lean::visit_exprs(body, &mut |x| {
            if let ExprKind::Handle { effect, ops, .. } = &x.kind {
                lexical.extend(effect.label.clone());
                for c in ops {
                    if let Some(vesper::ast::Pattern {
                        kind: vesper::ast::PatternKind::Bind(k),
                        ..
                    }) = c.pats.last()
                    {
                        lexical.insert(k.clone());
                    }
                }
            }
        });
        let mut closure = false;
        let mut entries: Vec<(u64, Option<String>, Vec<String>, bool)> = vec![];
        let world = self.world;
        vesper::lean::visit_exprs(body, &mut |x| match &x.kind {
            ExprKind::Closure { .. } => closure = true,
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Name(n)
                    if lexical.contains(n)
                        || n.split_once("::")
                            .is_some_and(|(e, _)| world.effects.contains_key(e)) => {}
                ExprKind::Name(n) => {
                    let arrow = world
                        .types
                        .get(&(callee.span.start, callee.span.end))
                        .is_some_and(|t| matches!(t, Type::Function { effects, .. } if !effects.instances.is_empty() || effects.tail.is_some()));
                    let f = world.functions.contains_key(n).then(|| n.clone());
                    entries.push((1, f, vec![], arrow));
                }
                ExprKind::Field(base, m) => {
                    let label = match &base.kind {
                        ExprKind::Name(l) => {
                            if lexical.contains(l) || effect.label.as_deref() == Some(l.as_str()) {
                                return;
                            }
                            Some(l.clone())
                        }
                        _ => None,
                    };
                    let suffix = format!("::{m}");
                    let fs = world
                        .functions
                        .keys()
                        .filter(|n| n.ends_with(&suffix))
                        .cloned()
                        .collect();
                    entries.push((2, label, fs, false));
                }
                _ => entries.push((3, None, vec![], false)),
            },
            _ => {}
        });
        w.push(closure as u64);
        w.push(entries.len() as u64);
        for (kind, n, fs, arrow) in entries {
            w.push(kind);
            w.push(n.map_or(NONE, |n| self.name(&n)));
            if kind == 1 {
                w.push(arrow as u64);
            } else if kind == 2 {
                w.push(fs.len() as u64);
                for f in &fs {
                    w.push(self.name(f));
                }
            }
        }
    }
}

/// `lean_nodes::effectful_arrow`.
pub fn effectful_arrow(t: &Type) -> bool {
    match t {
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            !effects.instances.is_empty()
                || effects.tail.is_some()
                || params.iter().any(|(_, t)| effectful_arrow(t))
                || effectful_arrow(result)
        }
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(effectful_arrow),
        _ => false,
    }
}
