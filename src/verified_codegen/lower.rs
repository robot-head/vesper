//! Checked Place IR to the slot IR (`Lir`): plan the reachable functions,
//! flatten every value of the scalar subset into 64-bit words and reject
//! everything else with a typed error.
use super::machine::{ArithOp, Cond, FIRST_FREE_LABEL, Kind, Label, Lir, Slot, TrapKind, WrapOp};
use super::{Unsupported, VerifiedError};
use crate::ast::{Literal, Pass, Pattern, PatternKind, Type, substitute};
use crate::check::World;
use crate::ir::{Block, FunctionIr, Instruction, Op, Program, Terminator, ValueId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The flattened words of one value: `(field path, kind)` per word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape(pub Vec<(Vec<String>, Kind)>);
impl Shape {
    pub fn len(&self) -> u32 {
        self.0.len() as u32
    }
    /// The word range of the sub-value at `path`.
    fn range(&self, path: &[String]) -> Option<(u32, u32)> {
        let mut start = None;
        let mut end = 0;
        for (i, (p, _)) in self.0.iter().enumerate() {
            if p.starts_with(path) {
                if start.is_none() {
                    start = Some(i as u32);
                }
                end = i as u32 + 1;
            }
        }
        match start {
            Some(s) => Some((s, end - s)),
            // A zero-word sub-value (Unit, or an empty struct).
            None => Some((self.0.len() as u32, 0)),
        }
    }
}

/// The flattening of a type: integers and `bool` are one word, `Unit` none,
/// non-generic-after-substitution structs and tuples the concatenation of
/// their fields in declaration order.
pub fn shape(world: &World, ty: &Type, function: &str) -> Result<Shape, VerifiedError> {
    let mut out = vec![];
    flatten(world, ty, &mut vec![], &mut out, function, 0, &mut vec![])?;
    Ok(Shape(out))
}

/// Path of an enum's tag word.
pub const TAG: &str = "#tag";
/// Path prefix of an enum's payload words.
pub const PAYLOAD: &str = "#payload";

/// The generic substitution of `adt` instantiated at `args`.
fn adt_vars(adt: &crate::ast::Adt, args: &[Type]) -> BTreeMap<String, Type> {
    adt.generics
        .iter()
        .zip(args)
        .map(|(g, t)| (g.name.clone(), t.clone()))
        .collect()
}

/// One variant of an enum value's layout: its tag and, per field, the
/// field's name, its word offset from the start of the value (after the tag
/// word) and its substituted type.
pub struct VariantLayout {
    pub tag: u64,
    pub fields: Vec<(String, u32, Type)>,
}

/// The layout of variant `ctor` (a short or `Enum::Variant` name) of the
/// enum type `ty`, or `None` when `ty` is not an enum.
pub fn variant_layout(
    world: &World,
    ty: &Type,
    ctor: &str,
    function: &str,
) -> Option<Result<VariantLayout, VerifiedError>> {
    let ty = crate::numeric::default_type(ty);
    let Type::Named(n, args) = &ty else {
        return None;
    };
    let adt = world.adts.get(n).filter(|a| a.is_enum)?;
    let short = ctor.rsplit("::").next().unwrap_or(ctor);
    let Some((tag, variant)) = adt
        .variants
        .iter()
        .enumerate()
        .find(|(_, v)| v.name == short)
    else {
        return Some(Err(VerifiedError::Invalid(format!(
            "{ctor} is not a variant of {ty}"
        ))));
    };
    let vars = adt_vars(adt, args);
    let mut offset = 1;
    let mut fields = vec![];
    for f in &variant.fields {
        let t = substitute(&f.ty, &vars);
        let words = match shape(world, &t, function) {
            Ok(s) => s.len(),
            Err(e) => return Some(Err(e)),
        };
        fields.push((f.name.clone(), offset, t));
        offset += words;
    }
    Some(Ok(VariantLayout {
        tag: tag as u64,
        fields,
    }))
}

fn flatten(
    world: &World,
    ty: &Type,
    path: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, Kind)>,
    function: &str,
    depth: usize,
    active: &mut Vec<String>,
) -> Result<(), VerifiedError> {
    if depth > 32 {
        return Err(VerifiedError::Limit(format!(
            "type nesting deeper than 32 in '{function}'"
        )));
    }
    let ty = crate::numeric::default_type(ty);
    let kind = match ty.name() {
        "u8" => Some(Kind::U8),
        "u32" => Some(Kind::U32),
        "u64" => Some(Kind::U64),
        "i64" => Some(Kind::I64),
        "bool" => Some(Kind::Bool),
        _ => None,
    };
    if let Some(k) = kind {
        out.push((path.clone(), k));
        return Ok(());
    }
    match &ty {
        Type::Tuple(ts) => {
            for (i, t) in ts.iter().enumerate() {
                path.push(i.to_string());
                flatten(world, t, path, out, function, depth + 1, active)?;
                path.pop();
            }
            Ok(())
        }
        Type::Named(n, _) if n == "Unit" => Ok(()),
        Type::Named(n, _) if n == "f64" || n == "f32" => Err(VerifiedError::unsupported(
            Unsupported::Float,
            function,
            format!("type {ty}"),
        )),
        Type::Named(n, _) if n == "String" => Err(VerifiedError::unsupported(
            Unsupported::String,
            function,
            format!("type {ty}"),
        )),
        Type::Named(n, args) => {
            let Some(adt) = world.adts.get(n) else {
                return Err(VerifiedError::unsupported(
                    Unsupported::Type,
                    function,
                    format!("type {ty}"),
                ));
            };
            let vars = adt_vars(adt, args);
            if adt.is_enum {
                // A tag word, then a union payload as wide as the widest
                // variant. Payload words are untyped (`u64`): each variant
                // reads them at its own field kinds.
                if active.iter().any(|a| a == n) {
                    return Err(VerifiedError::unsupported(
                        Unsupported::Enum,
                        function,
                        format!("recursive enum {ty} (no heap in this backend)"),
                    ));
                }
                active.push(n.clone());
                let mut widest = 0;
                for v in &adt.variants {
                    let mut words = vec![];
                    for f in &v.fields {
                        flatten(
                            world,
                            &substitute(&f.ty, &vars),
                            &mut vec![],
                            &mut words,
                            function,
                            depth + 1,
                            active,
                        )?;
                    }
                    widest = widest.max(words.len());
                }
                active.pop();
                path.push(TAG.into());
                out.push((path.clone(), Kind::U64));
                path.pop();
                for i in 0..widest {
                    path.push(PAYLOAD.into());
                    path.push(i.to_string());
                    out.push((path.clone(), Kind::U64));
                    path.pop();
                    path.pop();
                }
                return Ok(());
            }
            for f in &adt.variants[0].fields {
                path.push(f.name.clone());
                flatten(
                    world,
                    &substitute(&f.ty, &vars),
                    path,
                    out,
                    function,
                    depth + 1,
                    active,
                )?;
                path.pop();
            }
            Ok(())
        }
        Type::Function { .. } => Err(VerifiedError::unsupported(
            Unsupported::FunctionValue,
            function,
            format!("type {ty}"),
        )),
        _ => Err(VerifiedError::unsupported(
            Unsupported::Type,
            function,
            format!("type {ty}"),
        )),
    }
}

/// One planned function.
pub struct Planned {
    pub name: String,
    pub label: Label,
    pub params: Vec<(Pass, Shape)>,
    /// For each parameter, the word ranges `(start, len)` the caller copies
    /// back after the call: the declared projections of an `inout`
    /// parameter (`inout p.{x}`), or the whole value when it declares none.
    pub writeback: Vec<Vec<(u32, u32)>>,
    pub result: Shape,
}

/// The functions reachable from the entry, in discovery order.
pub struct Plan {
    pub functions: Vec<Planned>,
    pub index: BTreeMap<String, usize>,
}

fn effects_supported(world: &World, f: &FunctionIr) -> Result<(), VerifiedError> {
    if f.effects.tail.is_some() {
        return Err(VerifiedError::unsupported(
            Unsupported::Effect,
            &f.name,
            "open effect row",
        ));
    }
    for e in &f.effects.instances {
        let arg = e.args.first().map(|t| t.name()).unwrap_or("");
        let ok = e.name == "Fail"
            && e.label.is_none()
            && e.args.len() == 1
            && matches!(arg, "ArithOverflow" | "RuntimeError");
        if !ok {
            return Err(VerifiedError::unsupported(
                Unsupported::Effect,
                &f.name,
                format!(
                    "effect {}{} (only unhandled Fail[ArithOverflow] and Fail[RuntimeError] aborts)",
                    e.name,
                    if e.args.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "[{}]",
                            e.args
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ),
            ));
        }
    }
    let _ = world;
    Ok(())
}

pub fn plan(
    world: &World,
    program: &Program,
    entry: &str,
    max_functions: usize,
) -> Result<Plan, VerifiedError> {
    let mut functions = vec![];
    let mut index = BTreeMap::new();
    let mut queue = VecDeque::from([entry.to_owned()]);
    let mut seen = BTreeSet::from([entry.to_owned()]);
    let mut label = FIRST_FREE_LABEL;
    while let Some(name) = queue.pop_front() {
        if functions.len() >= max_functions {
            return Err(VerifiedError::Limit(format!(
                "more than {max_functions} reachable functions"
            )));
        }
        let decl = world.functions.get(&name).ok_or_else(|| {
            VerifiedError::unsupported(
                Unsupported::ExternalFunction,
                &name,
                "no such function in the checked world",
            )
        })?;
        if !decl.generics.is_empty() {
            return Err(VerifiedError::unsupported(
                Unsupported::Generic,
                &name,
                "generic functions (no monomorphization in this backend)",
            ));
        }
        let f = program.functions.get(&name).ok_or_else(|| {
            VerifiedError::unsupported(
                Unsupported::ExternalFunction,
                &name,
                "function without a body",
            )
        })?;
        effects_supported(world, f)?;
        let params = f
            .params
            .iter()
            .map(|p| Ok((p.pass, shape(world, &p.ty, &name)?)))
            .collect::<Result<Vec<_>, VerifiedError>>()?;
        let writeback = f
            .params
            .iter()
            .zip(&params)
            .map(|(p, (_, s))| {
                if p.projections.is_empty() {
                    return Ok(vec![(0, s.len())]);
                }
                p.projections
                    .iter()
                    .map(|path| {
                        s.range(path).ok_or_else(|| {
                            VerifiedError::Invalid(format!(
                                "projection {}.{{{}}} of {name}",
                                p.name,
                                path.join(".")
                            ))
                        })
                    })
                    .collect()
            })
            .collect::<Result<Vec<_>, VerifiedError>>()?;
        let result = shape(world, &f.result, &name)?;
        for b in &f.blocks {
            for i in &b.instructions {
                if let Op::Call { callee, .. } = &i.op
                    && crate::numeric::wrapping_intrinsic(callee).is_none()
                    && seen.insert(callee.clone())
                {
                    queue.push_back(callee.clone());
                }
            }
        }
        index.insert(name.clone(), functions.len());
        functions.push(Planned {
            name,
            label,
            params,
            writeback,
            result,
        });
        // Entry label and body label (after the prologue).
        label += 2;
    }
    Ok(Plan { functions, index })
}

/// The lowered body of one function.
pub struct Lowered {
    pub rows: Vec<Lir>,
    /// Slot words, outgoing-area words.
    pub slots: u32,
    pub out_words: u32,
}

struct FnLower<'a> {
    world: &'a World,
    plan: &'a Plan,
    me: &'a Planned,
    f: &'a FunctionIr,
    rows: Vec<Lir>,
    next_slot: Slot,
    values: BTreeMap<ValueId, (Slot, Shape)>,
    types: BTreeMap<ValueId, Type>,
    /// The block being lowered.
    block: usize,
    /// Arm blocks of a `match` terminator, with the scrutinee: their leading
    /// `Bind` of that value was already tested by the dispatch.
    arm_values: BTreeMap<usize, ValueId>,
    locals: BTreeMap<String, (Slot, Shape)>,
    block_labels: Vec<Label>,
    out_words: u32,
    next_label: &'a mut Label,
    max_rows: usize,
}

fn kind_of(shape: &Shape, function: &str, what: &str) -> Result<Kind, VerifiedError> {
    match shape.0.as_slice() {
        [(_, k)] => Ok(*k),
        _ => Err(VerifiedError::unsupported(
            Unsupported::Operation,
            function,
            format!("{what} on a non-scalar value"),
        )),
    }
}

impl FnLower<'_> {
    fn err(&self, construct: Unsupported, detail: impl Into<String>) -> VerifiedError {
        VerifiedError::unsupported(construct, &self.f.name, detail)
    }
    fn push(&mut self, row: Lir) -> Result<(), VerifiedError> {
        if self.rows.len() >= self.max_rows {
            return Err(VerifiedError::Limit(format!(
                "more than {} slot-IR rows",
                self.max_rows
            )));
        }
        self.rows.push(row);
        Ok(())
    }
    fn alloc(&mut self, words: u32) -> Result<Slot, VerifiedError> {
        let s = self.next_slot;
        self.next_slot = self
            .next_slot
            .checked_add(words)
            .filter(|n| *n <= super::MAX_FRAME_WORDS)
            .ok_or_else(|| {
                VerifiedError::Limit(format!(
                    "frame of '{}' exceeds {} words",
                    self.f.name,
                    super::MAX_FRAME_WORDS
                ))
            })?;
        Ok(s)
    }
    fn value_of(&mut self, v: ValueId, ty: &Type) -> Result<(Slot, Shape), VerifiedError> {
        if let Some(x) = self.values.get(&v) {
            return Ok(x.clone());
        }
        let shape = shape(self.world, ty, &self.f.name)?;
        let slot = self.alloc(shape.len())?;
        self.values.insert(v, (slot, shape.clone()));
        self.types.insert(v, crate::numeric::default_type(ty));
        Ok((slot, shape))
    }
    fn value(&self, v: ValueId) -> Result<(Slot, Shape), VerifiedError> {
        self.values.get(&v).cloned().ok_or_else(|| {
            VerifiedError::Invalid(format!(
                "value %{v} used before definition in '{}'",
                self.f.name
            ))
        })
    }
    fn local(&mut self, name: &str) -> Result<(Slot, Shape), VerifiedError> {
        if let Some(x) = self.locals.get(name) {
            return Ok(x.clone());
        }
        let ty = self.f.locals.get(name).cloned().ok_or_else(|| {
            VerifiedError::Invalid(format!("unknown local '{name}' in '{}'", self.f.name))
        })?;
        let shape = shape(self.world, &ty, &self.f.name)?;
        let slot = self.alloc(shape.len())?;
        self.locals.insert(name.to_owned(), (slot, shape.clone()));
        Ok((slot, shape))
    }
    fn copy(&mut self, dst: Slot, src: Slot, words: u32) -> Result<(), VerifiedError> {
        for i in 0..words {
            if dst + i != src + i {
                self.push(Lir::Move {
                    dst: dst + i,
                    src: src + i,
                })?;
            }
        }
        Ok(())
    }
    /// The slot range of `root.path`.
    fn place(&mut self, root: &str, path: &[String]) -> Result<(Slot, Shape), VerifiedError> {
        let (slot, shape) = self.local(root)?;
        let (start, len) = shape.range(path).ok_or_else(|| {
            VerifiedError::Invalid(format!("bad place {root}.{}", path.join(".")))
        })?;
        let sub = Shape(shape.0[start as usize..(start + len) as usize].to_vec());
        Ok((slot + start, sub))
    }

    /// The words of `ty`.
    fn words(&self, ty: &Type) -> Result<u32, VerifiedError> {
        Ok(shape(self.world, ty, &self.f.name)?.len())
    }

    /// The sub-values a non-leaf pattern destructures: `(sub-pattern, word
    /// offset, type)`, plus the enum tag the value must carry, if any.
    #[allow(clippy::type_complexity)]
    fn parts<'p>(
        &self,
        p: &'p Pattern,
        ty: &Type,
    ) -> Result<(Option<u64>, Vec<(&'p Pattern, u32, Type)>), VerifiedError> {
        let ty = crate::numeric::default_type(ty);
        match &p.kind {
            PatternKind::Tuple(ps) => {
                let Type::Tuple(ts) = &ty else {
                    return Err(VerifiedError::Invalid(format!("tuple pattern on {ty}")));
                };
                if ts.len() != ps.len() {
                    return Err(VerifiedError::Invalid("tuple pattern arity".into()));
                }
                let mut offset = 0;
                let mut out = vec![];
                for (p, t) in ps.iter().zip(ts) {
                    out.push((p, offset, t.clone()));
                    offset += self.words(t)?;
                }
                Ok((None, out))
            }
            PatternKind::Constructor { name, fields, .. } => {
                if let Some(layout) = variant_layout(self.world, &ty, name, &self.f.name) {
                    let layout = layout?;
                    let mut out = vec![];
                    for (field, p) in fields {
                        let (_, offset, t) = layout
                            .fields
                            .iter()
                            .find(|(n, _, _)| n == field)
                            .ok_or_else(|| {
                                VerifiedError::Invalid(format!("{name} has no field {field}"))
                            })?;
                        out.push((p, *offset, t.clone()));
                    }
                    return Ok((Some(layout.tag), out));
                }
                let Type::Named(n, args) = &ty else {
                    return Err(VerifiedError::Invalid(format!(
                        "constructor pattern on {ty}"
                    )));
                };
                let adt = self
                    .world
                    .adts
                    .get(n)
                    .filter(|a| !a.is_enum && a.variants.len() == 1)
                    .ok_or_else(|| VerifiedError::Invalid(format!("pattern {name} on {ty}")))?;
                let vars = adt_vars(adt, args);
                let mut offsets = BTreeMap::new();
                let mut offset = 0;
                for f in &adt.variants[0].fields {
                    let t = substitute(&f.ty, &vars);
                    let w = self.words(&t)?;
                    offsets.insert(f.name.clone(), (offset, t));
                    offset += w;
                }
                let mut out = vec![];
                for (field, p) in fields {
                    let (offset, t) = offsets.get(field).cloned().ok_or_else(|| {
                        VerifiedError::Invalid(format!("{name} has no field {field}"))
                    })?;
                    out.push((p, offset, t));
                }
                Ok((None, out))
            }
            _ => Ok((None, vec![])),
        }
    }

    /// Bind the names of `p` to the words of the value of type `ty` at
    /// `src`. A refutable part (an enum variant of a multi-variant enum, a
    /// literal) is accepted only when `tested`: a `match` dispatch already
    /// checked it.
    fn bind(
        &mut self,
        p: &Pattern,
        src: Slot,
        ty: &Type,
        tested: bool,
    ) -> Result<(), VerifiedError> {
        match &p.kind {
            PatternKind::Wild => Ok(()),
            PatternKind::Bind(name) => {
                let words = self.words(ty)?;
                let (dst, s) = self.local(name)?;
                if s.len() != words {
                    return Err(VerifiedError::Invalid(format!(
                        "binding '{name}' has {} words, value has {words}",
                        s.len(),
                    )));
                }
                self.copy(dst, src, words)
            }
            PatternKind::Literal(_) if tested => Ok(()),
            PatternKind::Literal(_) => {
                Err(self.err(Unsupported::Match, "refutable literal pattern in a binding"))
            }
            PatternKind::Tuple(_) | PatternKind::Constructor { .. } => {
                if let PatternKind::Constructor { name, .. } = &p.kind
                    && !tested
                    && let Some((adt, _)) = self.world.constructor(name)
                    && adt.is_enum
                    && adt.variants.len() > 1
                {
                    return Err(self.err(
                        Unsupported::Match,
                        format!("refutable constructor pattern {name} in a binding"),
                    ));
                }
                let (_, parts) = self.parts(p, ty)?;
                for (p, offset, t) in parts {
                    self.bind(p, src + offset, &t, tested)?;
                }
                Ok(())
            }
        }
    }

    /// Emit a test that jumps to `fail` unless the value of type `ty` at
    /// `src` matches `p` (first-match `match` dispatch).
    fn test(
        &mut self,
        p: &Pattern,
        src: Slot,
        ty: &Type,
        fail: Label,
    ) -> Result<(), VerifiedError> {
        let check = |this: &mut Self, kind: Kind, word: u64| -> Result<(), VerifiedError> {
            let tmp = this.alloc(2)?;
            this.push(Lir::Const { dst: tmp, word })?;
            this.push(Lir::Compare {
                cond: Cond::Eq,
                kind,
                dst: tmp + 1,
                a: src,
                b: tmp,
            })?;
            this.push(Lir::BranchZero {
                cond: tmp + 1,
                target: fail,
            })
        };
        match &p.kind {
            PatternKind::Wild | PatternKind::Bind(_) => Ok(()),
            PatternKind::Literal(Literal::Int(n)) => {
                let s = shape(self.world, ty, &self.f.name)?;
                let kind = kind_of(&s, &self.f.name, "integer pattern")?;
                let ik = crate::numeric::IntegerKind::from_type(&crate::numeric::default_type(ty))
                    .ok_or_else(|| VerifiedError::Invalid("integer pattern type".into()))?;
                if kind == Kind::Bool || !ik.fits(*n) {
                    // A literal outside the type never matches.
                    return self.push(Lir::Jump(fail));
                }
                check(self, kind, *n as u64)
            }
            PatternKind::Literal(Literal::Bool(b)) => check(self, Kind::Bool, *b as u64),
            PatternKind::Literal(Literal::Float(_)) => {
                Err(self.err(Unsupported::Float, "float pattern"))
            }
            PatternKind::Literal(Literal::String(_)) => {
                Err(self.err(Unsupported::String, "string pattern"))
            }
            #[allow(unreachable_patterns)]
            PatternKind::Literal(_) => Err(self.err(Unsupported::Match, "literal pattern")),
            PatternKind::Tuple(_) | PatternKind::Constructor { .. } => {
                let (tag, parts) = self.parts(p, ty)?;
                if let Some(tag) = tag {
                    check(self, Kind::U64, tag)?;
                }
                for (p, offset, t) in parts {
                    self.test(p, src + offset, &t, fail)?;
                }
                Ok(())
            }
        }
    }

    fn arith(&self, op: &str) -> Option<ArithOp> {
        Some(match op {
            "+" | "+=" => ArithOp::Add,
            "-" | "-=" => ArithOp::Sub,
            "*" | "*=" => ArithOp::Mul,
            "/" | "/=" => ArithOp::Div,
            "%" | "%=" => ArithOp::Rem,
            _ => return None,
        })
    }

    fn integer(&self, kind: Kind, what: &str) -> Result<Kind, VerifiedError> {
        if kind == Kind::Bool {
            Err(VerifiedError::Invalid(format!(
                "{what} on bool in '{}'",
                self.f.name
            )))
        } else {
            Ok(kind)
        }
    }

    fn instruction(&mut self, i: &Instruction) -> Result<(), VerifiedError> {
        match &i.op {
            Op::Constant(Literal::Int(n)) => {
                let (dst, shape) = self.value_of(i.dest, &i.ty)?;
                let kind = kind_of(&shape, &self.f.name, "integer literal")?;
                let ik =
                    crate::numeric::IntegerKind::from_type(&crate::numeric::default_type(&i.ty))
                        .ok_or_else(|| VerifiedError::Invalid("integer literal type".into()))?;
                if kind == Kind::Bool || !ik.fits(*n) {
                    return Err(VerifiedError::Invalid(format!(
                        "literal {n} does not fit {}",
                        i.ty
                    )));
                }
                // Two's complement for i64, zero extension for unsigned kinds.
                self.push(Lir::Const {
                    dst,
                    word: *n as u64,
                })
            }
            Op::Constant(Literal::Bool(b)) => {
                let (dst, _) = self.value_of(i.dest, &i.ty)?;
                self.push(Lir::Const {
                    dst,
                    word: *b as u64,
                })
            }
            Op::Constant(Literal::Float(_)) => Err(self.err(Unsupported::Float, "float literal")),
            Op::Constant(Literal::String(_)) => {
                Err(self.err(Unsupported::String, "string literal"))
            }
            Op::Unit => {
                self.value_of(i.dest, &Type::unit())?;
                Ok(())
            }
            Op::Read { root, path, .. } => {
                let (src, shape) = self.place(root, path)?;
                let (dst, dshape) = self.value_of(i.dest, &i.ty)?;
                if dshape.len() != shape.len() {
                    return Err(VerifiedError::Invalid(format!(
                        "read of {root} has {} words, expected {}",
                        shape.len(),
                        dshape.len()
                    )));
                }
                self.copy(dst, src, shape.len())
            }
            Op::Bind { pattern, value } => {
                let (src, _) = self.value(*value)?;
                let ty = self
                    .types
                    .get(value)
                    .cloned()
                    .ok_or_else(|| VerifiedError::Invalid(format!("untyped value %{value}")))?;
                self.value_of(i.dest, &Type::unit())?;
                let tested = self.arm_values.get(&self.block) == Some(value);
                self.bind(pattern, src, &ty, tested)
            }
            Op::Write { place, op, value } => {
                let mut parts = place.split('.');
                let root = parts.next().unwrap_or_default().to_owned();
                let path = parts.map(str::to_owned).collect::<Vec<_>>();
                if root.is_empty() {
                    return Err(VerifiedError::Invalid("write without a place".into()));
                }
                let (dst, shape) = self.place(&root, &path)?;
                let (src, vshape) = self.value(*value)?;
                self.value_of(i.dest, &Type::unit())?;
                if op == "=" {
                    if vshape.len() != shape.len() {
                        return Err(VerifiedError::Invalid("write shape mismatch".into()));
                    }
                    return self.copy(dst, src, shape.len());
                }
                let aop = self
                    .arith(op)
                    .ok_or_else(|| self.err(Unsupported::Operation, format!("assignment {op}")))?;
                let kind = kind_of(&shape, &self.f.name, op)?;
                let kind = self.integer(kind, op)?;
                // A failing compound assignment raises before its write.
                self.push(Lir::Arith {
                    op: aop,
                    kind,
                    dst,
                    a: dst,
                    b: src,
                })
            }
            Op::Unary(op, v) => {
                let (a, shape) = self.value(*v)?;
                let kind = kind_of(&shape, &self.f.name, op)?;
                let (dst, _) = self.value_of(i.dest, &i.ty)?;
                match op.as_str() {
                    "!" if kind == Kind::Bool => self.push(Lir::Not { dst, a }),
                    "-" if kind != Kind::Bool => self.push(Lir::Neg { kind, dst, a }),
                    _ => Err(self.err(Unsupported::Operation, format!("unary {op}"))),
                }
            }
            Op::Binary(op, x, y) => {
                let (a, ashape) = self.value(*x)?;
                let (b, bshape) = self.value(*y)?;
                // One word with an empty path is a scalar; anything else
                // (including a one-field struct) is an aggregate.
                if ashape.len() != 1 || bshape.len() != 1 || !ashape.0[0].0.is_empty() {
                    return Err(self.err(
                        Unsupported::StructuralEquality,
                        format!("operator {op} on aggregates"),
                    ));
                }
                let kind = ashape.0[0].1;
                if bshape.0[0].1 != kind {
                    return Err(VerifiedError::Invalid(format!(
                        "operator {op} on different kinds"
                    )));
                }
                let (dst, _) = self.value_of(i.dest, &i.ty)?;
                if let Some(aop) = self.arith(op).filter(|_| !op.ends_with('=')) {
                    let kind = self.integer(kind, op)?;
                    return self.push(Lir::Arith {
                        op: aop,
                        kind,
                        dst,
                        a,
                        b,
                    });
                }
                if let Some(w) = crate::numeric::wrapping_intrinsic(op) {
                    let kind = self.integer(kind, op)?;
                    let op = match w {
                        "+" => WrapOp::Add,
                        "-" => WrapOp::Sub,
                        _ => WrapOp::Mul,
                    };
                    return self.push(Lir::Wrapping {
                        op,
                        kind,
                        dst,
                        a,
                        b,
                    });
                }
                let cond = match op.as_str() {
                    "==" => Cond::Eq,
                    "!=" => Cond::Ne,
                    "<" => Cond::Lt,
                    "<=" => Cond::Le,
                    ">" => Cond::Gt,
                    ">=" => Cond::Ge,
                    _ => return Err(self.err(Unsupported::Operation, format!("operator {op}"))),
                };
                if kind == Kind::Bool && !matches!(cond, Cond::Eq | Cond::Ne) {
                    return Err(VerifiedError::Invalid(format!("operator {op} on bool")));
                }
                self.push(Lir::Compare {
                    cond,
                    kind,
                    dst,
                    a,
                    b,
                })
            }
            Op::Tuple(vs) => {
                let (dst, _) = self.value_of(i.dest, &i.ty)?;
                let mut offset = 0;
                for v in vs {
                    let (src, s) = self.value(*v)?;
                    self.copy(dst + offset, src, s.len())?;
                    offset += s.len();
                }
                Ok(())
            }
            Op::Data(name, fields) => {
                let Some((adt, variant)) = self.world.constructor(name) else {
                    return Err(VerifiedError::Invalid(format!(
                        "unknown constructor {name}"
                    )));
                };
                if adt.is_enum {
                    let (dst, shape) = self.value_of(i.dest, &i.ty)?;
                    let ty = self.types[&i.dest].clone();
                    let layout = variant_layout(self.world, &ty, name, &self.f.name).ok_or_else(
                        || VerifiedError::Invalid(format!("enum constructor {name} at {ty}")),
                    )??;
                    self.push(Lir::Const {
                        dst,
                        word: layout.tag,
                    })?;
                    let mut written = 1;
                    for (field, offset, _) in &layout.fields {
                        let Some((_, v)) = fields.iter().find(|(n, _)| n == field) else {
                            return Err(VerifiedError::Invalid(format!(
                                "constructor {name} misses field {field}"
                            )));
                        };
                        let (src, s) = self.value(*v)?;
                        self.copy(dst + offset, src, s.len())?;
                        written = written.max(offset + s.len());
                    }
                    // Unused payload words are zero, so results are
                    // deterministic.
                    for w in written..shape.len() {
                        self.push(Lir::Const {
                            dst: dst + w,
                            word: 0,
                        })?;
                    }
                    return Ok(());
                }
                let (dst, shape) = self.value_of(i.dest, &i.ty)?;
                for field in &variant.fields {
                    let Some((_, v)) = fields.iter().find(|(n, _)| n == &field.name) else {
                        return Err(VerifiedError::Invalid(format!(
                            "constructor {name} misses field {}",
                            field.name
                        )));
                    };
                    let (start, len) = shape
                        .range(std::slice::from_ref(&field.name))
                        .ok_or_else(|| VerifiedError::Invalid("struct layout".into()))?;
                    let (src, s) = self.value(*v)?;
                    if s.len() != len {
                        return Err(VerifiedError::Invalid("field shape mismatch".into()));
                    }
                    self.copy(dst + start, src, len)?;
                }
                Ok(())
            }
            Op::Project(v, field) => {
                let (src, shape) = self.value(*v)?;
                let (start, len) = shape
                    .range(std::slice::from_ref(field))
                    .ok_or_else(|| VerifiedError::Invalid(format!("projection .{field}")))?;
                let (dst, _) = self.value_of(i.dest, &i.ty)?;
                self.copy(dst, src + start, len)
            }
            Op::Call {
                callee,
                args,
                types,
                capability,
            } => {
                if capability.is_some() {
                    return Err(self.err(Unsupported::Effect, format!("effect operation {callee}")));
                }
                if !types.is_empty() {
                    return Err(self.err(Unsupported::Generic, format!("call {callee}[...]")));
                }
                let target = self
                    .plan
                    .index
                    .get(callee)
                    .map(|&n| &self.plan.functions[n])
                    .ok_or_else(|| {
                        self.err(
                            Unsupported::ExternalFunction,
                            format!("call to unplanned function {callee}"),
                        )
                    })?;
                let (label, nres, params, ranges) = (
                    target.label,
                    target.result.len(),
                    target.params.clone(),
                    target.writeback.clone(),
                );
                if params.len() != args.len() {
                    return Err(VerifiedError::Invalid(format!("arity of {callee}")));
                }
                let area = nres + params.iter().map(|(_, s)| s.len()).sum::<u32>();
                self.out_words = self.out_words.max(area);
                let mut index = nres;
                let mut writebacks = vec![];
                for ((arg, (pass, s)), ranges) in args.iter().zip(&params).zip(ranges) {
                    let (src, ashape) = self.value(arg.value)?;
                    if ashape.len() != s.len() {
                        return Err(VerifiedError::Invalid(format!(
                            "argument shape of {callee}"
                        )));
                    }
                    for w in 0..s.len() {
                        self.push(Lir::StoreOut {
                            index: index + w,
                            src: src + w,
                        })?;
                    }
                    if *pass == Pass::Inout {
                        let place = arg.place.clone().ok_or_else(|| {
                            VerifiedError::Invalid("inout argument without a place".into())
                        })?;
                        writebacks.push((place, index, ranges));
                    }
                    index += s.len();
                }
                self.push(Lir::Call { callee: label })?;
                let (dst, dshape) = self.value_of(i.dest, &i.ty)?;
                if dshape.len() != nres {
                    return Err(VerifiedError::Invalid(format!("result shape of {callee}")));
                }
                for w in 0..nres {
                    self.push(Lir::LoadOut {
                        dst: dst + w,
                        index: w,
                    })?;
                }
                // Copy back only the words the callee may write: two
                // `inout` arguments may name disjoint projections of one
                // place (`f(inout p, inout p)` with `p.{x}` and `q.{y}`).
                for (place, index, ranges) in writebacks {
                    let (slot, _) = self.place(&place.root, &place.path)?;
                    for (start, len) in ranges {
                        for w in start..start + len {
                            self.push(Lir::LoadOut {
                                dst: slot + w,
                                index: index + w,
                            })?;
                        }
                    }
                }
                Ok(())
            }
            Op::Index(..) => Err(self.err(Unsupported::Index, "dynamic indexing")),
            Op::Indirect { .. } | Op::Closure { .. } => {
                Err(self.err(Unsupported::FunctionValue, "closures and indirect calls"))
            }
            Op::Try(_) => Err(self.err(Unsupported::Effect, "`?`")),
            Op::Handle { .. } => Err(self.err(Unsupported::Effect, "effect handlers")),
            #[allow(unreachable_patterns)]
            _ => Err(self.err(
                Unsupported::Operation,
                "an IR operation this backend does not know",
            )),
        }
    }

    /// Whether `block` ends in a self call whose result is returned directly.
    fn tail_self_call(&self, block: &Block) -> bool {
        let Some(last) = block.instructions.last() else {
            return false;
        };
        let Op::Call { callee, .. } = &last.op else {
            return false;
        };
        if callee != &self.f.name || self.me.params.iter().any(|(p, _)| *p == Pass::Inout) {
            return false;
        }
        match &block.terminator {
            Terminator::Return(v) => *v == last.dest,
            Terminator::Jump {
                block: target,
                args,
            } => {
                let t = &self.f.blocks[*target];
                args.as_slice() == [last.dest]
                    && t.instructions.is_empty()
                    && t.params.len() == 1
                    && matches!(t.terminator, Terminator::Return(v) if v == t.params[0])
            }
            _ => false,
        }
    }

    fn function(mut self) -> Result<Lowered, VerifiedError> {
        let nres = self.me.result.len();
        // Give every block parameter and instruction result its words up
        // front, so the block order does not matter.
        for block in &self.f.blocks {
            for p in &block.params {
                let ty = self.f.values.get(p).cloned().ok_or_else(|| {
                    VerifiedError::Invalid(format!("untyped block parameter %{p}"))
                })?;
                self.value_of(*p, &ty)?;
            }
            for i in &block.instructions {
                let ty = if matches!(i.op, Op::Bind { .. } | Op::Write { .. } | Op::Unit) {
                    Type::unit()
                } else {
                    i.ty.clone()
                };
                self.value_of(i.dest, &ty)?;
            }
        }
        self.push(Lir::Label(self.me.label))?;
        let enter_at = self.rows.len();
        self.push(Lir::Enter { words: 0 })?;
        self.push(Lir::Label(self.me.label + 1))?;
        // Parameters: incoming area words after the results.
        let mut index = nres;
        let params = self.f.params.clone();
        for p in &params {
            let (slot, s) = self.local(&p.name)?;
            for w in 0..s.len() {
                self.push(Lir::LoadIn {
                    dst: slot + w,
                    index: index + w,
                })?;
            }
            index += s.len();
        }
        for block in &self.f.blocks {
            if let Terminator::Match { value, arms } = &block.terminator {
                for (_, target) in arms {
                    self.arm_values.insert(*target, *value);
                }
            }
        }
        for (b, block) in self.f.blocks.iter().enumerate() {
            self.block = b;
            if b > 0 {
                self.push(Lir::Label(self.block_labels[b]))?;
            }
            let tail = self.tail_self_call(block);
            let count = block.instructions.len();
            for (n, i) in block.instructions.iter().enumerate() {
                if tail && n + 1 == count {
                    // A self call in tail position is a loop: write the new
                    // arguments into the incoming area and jump to the body.
                    let Op::Call { args, .. } = &i.op else {
                        unreachable!()
                    };
                    let mut index = nres;
                    let mut staged = vec![];
                    for arg in args {
                        let (src, s) = self.value(arg.value)?;
                        staged.push((src, s.len(), index));
                        index += s.len();
                    }
                    for (src, len, index) in staged {
                        for w in 0..len {
                            self.push(Lir::StoreIn {
                                index: index + w,
                                src: src + w,
                            })?;
                        }
                    }
                    self.push(Lir::Jump(self.me.label + 1))?;
                    continue;
                }
                self.instruction(i)?;
            }
            if tail {
                continue;
            }
            match &block.terminator {
                Terminator::Return(v) => {
                    let (src, s) = self.value(*v)?;
                    if s.len() != nres {
                        return Err(VerifiedError::Invalid(format!(
                            "return shape of '{}'",
                            self.f.name
                        )));
                    }
                    for w in 0..nres {
                        self.push(Lir::StoreIn {
                            index: w,
                            src: src + w,
                        })?;
                    }
                    // `inout` parameters are written back on return.
                    let mut index = nres;
                    for (p, (pass, s)) in params.iter().zip(&self.me.params) {
                        if *pass == Pass::Inout {
                            let (slot, _) = self.local(&p.name)?;
                            for w in 0..s.len() {
                                self.push(Lir::StoreIn {
                                    index: index + w,
                                    src: slot + w,
                                })?;
                            }
                        }
                        index += s.len();
                    }
                    self.push(Lir::Return)?;
                }
                Terminator::Jump { block, args } => {
                    let target = &self.f.blocks[*block];
                    if target.params.len() != args.len() {
                        return Err(VerifiedError::Invalid("block arity".into()));
                    }
                    let mut moves = vec![];
                    for (param, arg) in target.params.iter().zip(args) {
                        let ty = self.f.values.get(param).cloned().ok_or_else(|| {
                            VerifiedError::Invalid(format!("untyped block parameter %{param}"))
                        })?;
                        let (dst, s) = self.value_of(*param, &ty)?;
                        let (src, a) = self.value(*arg)?;
                        if a.len() != s.len() {
                            return Err(VerifiedError::Invalid("jump argument shape".into()));
                        }
                        moves.push((dst, src, s.len()));
                    }
                    for (dst, src, len) in moves {
                        self.copy(dst, src, len)?;
                    }
                    self.push(Lir::Jump(self.block_labels[*block]))?;
                }
                Terminator::Branch { condition, yes, no } => {
                    let (c, s) = self.value(*condition)?;
                    if kind_of(&s, &self.f.name, "branch")? != Kind::Bool {
                        return Err(VerifiedError::Invalid("branch on a non-bool".into()));
                    }
                    self.push(Lir::BranchZero {
                        cond: c,
                        target: self.block_labels[*no],
                    })?;
                    self.push(Lir::Jump(self.block_labels[*yes]))?;
                }
                Terminator::Match { value, arms } => {
                    let (src, _) = self.value(*value)?;
                    let ty =
                        self.types.get(value).cloned().ok_or_else(|| {
                            VerifiedError::Invalid(format!("untyped value %{value}"))
                        })?;
                    for (p, target) in arms {
                        let next = *self.next_label;
                        *self.next_label += 1;
                        self.test(p, src, &ty, next)?;
                        self.push(Lir::Jump(self.block_labels[*target]))?;
                        self.push(Lir::Label(next))?;
                    }
                    // The checker proves the arms exhaustive.
                    self.push(Lir::Trap(TrapKind::Unreachable))?;
                }
                Terminator::Unreachable => self.push(Lir::Trap(TrapKind::Unreachable))?,
            }
        }
        let words = self
            .next_slot
            .checked_add(self.out_words)
            .filter(|n| *n <= super::MAX_FRAME_WORDS)
            .ok_or_else(|| VerifiedError::Limit(format!("frame of '{}'", self.f.name)))?;
        // Keep the stack 16-byte aligned.
        let words = words + (words & 1);
        self.rows[enter_at] = Lir::Enter { words };
        // Slots are addressed above the outgoing area.
        let out = self.out_words;
        for row in &mut self.rows {
            relocate(row, out);
        }
        Ok(Lowered {
            rows: self.rows,
            slots: self.next_slot,
            out_words: out,
        })
    }
}

/// Slot `s` lives at frame word `out + s` (the outgoing area is at the bottom
/// of the frame).
fn relocate(row: &mut Lir, out: u32) {
    match row {
        Lir::Const { dst, .. } | Lir::LoadOut { dst, .. } | Lir::LoadIn { dst, .. } => *dst += out,
        Lir::Move { dst, src } => {
            *dst += out;
            *src += out;
        }
        Lir::Arith { dst, a, b, .. }
        | Lir::Wrapping { dst, a, b, .. }
        | Lir::Compare { dst, a, b, .. } => {
            *dst += out;
            *a += out;
            *b += out;
        }
        Lir::Neg { dst, a, .. } | Lir::Not { dst, a } => {
            *dst += out;
            *a += out;
        }
        Lir::StoreOut { src, .. } | Lir::StoreIn { src, .. } => *src += out,
        Lir::BranchZero { cond, .. } => *cond += out,
        Lir::Call { .. }
        | Lir::Label(_)
        | Lir::Jump(_)
        | Lir::Enter { .. }
        | Lir::Return
        | Lir::Trap(_) => {}
    }
}

/// Lower one planned function to slot IR.
pub fn function(
    world: &World,
    program: &Program,
    plan: &Plan,
    index: usize,
    next_label: &mut Label,
    max_rows: usize,
) -> Result<Lowered, VerifiedError> {
    let me = &plan.functions[index];
    let f = &program.functions[&me.name];
    let block_labels = (0..f.blocks.len())
        .map(|_| {
            let l = *next_label;
            *next_label += 1;
            l
        })
        .collect();
    FnLower {
        world,
        plan,
        me,
        f,
        rows: vec![],
        next_slot: 0,
        values: BTreeMap::new(),
        types: BTreeMap::new(),
        block: 0,
        arm_values: BTreeMap::new(),
        locals: BTreeMap::new(),
        block_labels,
        out_words: 0,
        next_label,
        max_rows,
    }
    .function()
}
