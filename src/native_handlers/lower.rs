//! Lowering checked source functions to Cranelift under the handler ABI.
//!
//! Every value is one 64-bit word: integers, `bool` (0 or 1), unit (0),
//! capabilities (handler record pointers) and function values (pointers to
//! closure records `[code, captured words..]`). Signatures, all words:
//!
//! * source function: `(rt, caps.., params..)`, one capability per
//!   operation-carrying effect of its row, in row order;
//! * closure code: `(rt, record, caps.., params..)`, capabilities of the
//!   function type's row;
//! * handled body: `(rt, env, cap)`, its own handler's capability last;
//! * operation clause: `(rt, env, k, state, args..)`;
//! * return clause: `(rt, env, value, state)`.
//!
//! Handled bodies and clauses read the variables and capabilities they use
//! from an environment record the `handle` site builds. After every call
//! that can reach an operation, generated code tests `rt.unwinding` and
//! returns at once when it is set (cancellation of an unresumed
//! continuation, or a run-time fault).
use super::HandlerAbiError;
use super::runtime::{MAX_OPERATION_ARGS, descriptor};
use crate::ast::*;
use crate::check::World;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types::I64};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type Result<T> = std::result::Result<T, HandlerAbiError>;

fn unsupported<T>(what: impl Into<String>) -> Result<T> {
    Err(HandlerAbiError::Unsupported(what.into()))
}
fn invalid<T>(what: impl Into<String>) -> Result<T> {
    Err(HandlerAbiError::Invalid(what.into()))
}

/// The machine reading of a source type.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    I64,
    U64,
    U32,
    U8,
    Bool,
    Unit,
    Never,
    Function(Type),
    /// A value the slice passes through but never inspects (the payload of
    /// `Fail[ArithOverflow]`).
    Opaque(Type),
}
impl Kind {
    pub fn of(t: &Type) -> Kind {
        match t {
            Type::Named(n, a) if a.is_empty() => match n.as_str() {
                "i64" => Kind::I64,
                "u64" => Kind::U64,
                "u32" => Kind::U32,
                "u8" => Kind::U8,
                "bool" => Kind::Bool,
                "Never" => Kind::Never,
                _ => Kind::Opaque(t.clone()),
            },
            Type::Tuple(ts) if ts.is_empty() => Kind::Unit,
            Type::IntLiteral(_) => Kind::I64,
            Type::Function { .. } => Kind::Function(t.clone()),
            _ => Kind::Opaque(t.clone()),
        }
    }
    fn max(&self) -> Option<u64> {
        match self {
            Kind::U8 => Some(0xff),
            Kind::U32 => Some(0xffff_ffff),
            _ => None,
        }
    }
    fn integer(&self) -> bool {
        matches!(self, Kind::I64 | Kind::U64 | Kind::U32 | Kind::U8)
    }
}

/// The capability key of an effect instance: label, or name and arguments.
pub fn effect_key(e: &Effect) -> String {
    match source_label(&e.label) {
        Some(label) => format!("label:{label}"),
        None => {
            let args: Vec<String> = e.args.iter().map(ToString::to_string).collect();
            format!("{}[{}]", e.name, args.join(","))
        }
    }
}

/// The label a program wrote, ignoring the checker's internal labels
/// (`$latent:origin:label:Name` keeps its written label, `$fncap:` and
/// `$cap` name per-call bindings that resolve by effect instance).
fn source_label(label: &Option<String>) -> Option<String> {
    let label = label.as_ref()?;
    if let Some(rest) = label.strip_prefix("$latent:") {
        return rest
            .split(':')
            .nth(1)
            .filter(|l| !l.is_empty())
            .map(str::to_owned);
    }
    (!label.starts_with('$')).then(|| label.clone())
}

/// The effects of a row that are passed as capabilities, in canonical
/// order (capability-only effects such as `Realtime` have no operations
/// and are erased).
pub fn row(world: &World, effects: &Effects) -> Result<Vec<Effect>> {
    if effects.tail.is_some() {
        return unsupported("row-polymorphic effect rows");
    }
    let mut out = vec![];
    for e in &effects.instances {
        if crate::capability_effects::is_capability_effect(&e.name) {
            continue;
        }
        if !world.effects.contains_key(&e.name) {
            return unsupported(format!("effect {}", e.name));
        }
        out.push(e.clone());
    }
    // A closure's type is inferred where it is built and written where it
    // is called, and the two rows need not list effects in the same order:
    // pass capabilities in a canonical order (by instance, not label).
    out.sort_by_cached_key(|e| {
        let args: Vec<String> = e.args.iter().map(ToString::to_string).collect();
        (e.name.clone(), args)
    });
    Ok(out)
}

/// A function type's parameters, result and effects.
type FunctionParts<'t> = (&'t [(Pass, Type)], &'t Type, &'t Effects);

fn function_type(t: &Type) -> Option<FunctionParts<'_>> {
    match t {
        Type::Function {
            params,
            result,
            effects,
            ..
        } => Some((params, result, effects)),
        _ => None,
    }
}

/// A function body still to be defined.
enum Pending {
    Source(String),
    Adapter(String),
    Lambda {
        name: String,
        params: Vec<(String, Type)>,
        row: Vec<Effect>,
        captures: Vec<(String, Kind)>,
        body: Expr,
    },
    Body {
        name: String,
        captures: Vec<(String, Kind)>,
        caps: Vec<(String, Effect)>,
        own: Effect,
        body: Expr,
    },
    Clause {
        name: String,
        captures: Vec<(String, Kind)>,
        caps: Vec<(String, Effect)>,
        /// Operation parameter patterns.
        binds: Vec<(Pattern, Kind)>,
        state: Option<(Pattern, Kind)>,
        resume: Option<String>,
        body: Expr,
    },
    Ret {
        name: String,
        captures: Vec<(String, Kind)>,
        caps: Vec<(String, Effect)>,
        value: (Pattern, Kind),
        state: Option<(Pattern, Kind)>,
        body: Expr,
    },
}

struct Runtime {
    handle: FuncId,
    perform: FuncId,
    resume: FuncId,
    alloc: FuncId,
    fault: FuncId,
}

pub struct Lowering<'w, M: Module> {
    world: &'w World,
    pub module: M,
    call_conv: CallConv,
    ctx: cranelift_codegen::Context,
    fctx: FunctionBuilderContext,
    declared: BTreeMap<String, FuncId>,
    pending: VecDeque<Pending>,
    runtime: Runtime,
    fresh: usize,
    /// Bytes of machine code per defined function.
    pub sizes: BTreeMap<String, usize>,
}

impl<'w, M: Module> Lowering<'w, M> {
    pub fn new(world: &'w World, mut module: M) -> Result<Self> {
        let call_conv = module.isa().default_call_conv();
        let sig = |n: usize| {
            let mut s = ir::Signature::new(call_conv);
            s.params.extend((0..n).map(|_| AbiParam::new(I64)));
            s.returns.push(AbiParam::new(I64));
            s
        };
        let mut import = |name: &str, n: usize| {
            module
                .declare_function(name, Linkage::Import, &sig(n))
                .map_err(|e| HandlerAbiError::Invalid(e.to_string()))
        };
        let runtime = Runtime {
            handle: import("vesper_handlers_handle", 2)?,
            perform: import("vesper_handlers_perform", 5)?,
            resume: import("vesper_handlers_resume", 4)?,
            alloc: import("vesper_handlers_alloc", 2)?,
            fault: import("vesper_handlers_fault", 2)?,
        };
        Ok(Self {
            world,
            ctx: module.make_context(),
            module,
            call_conv,
            fctx: FunctionBuilderContext::new(),
            declared: BTreeMap::new(),
            pending: VecDeque::new(),
            runtime,
            fresh: 0,
            sizes: BTreeMap::new(),
        })
    }

    fn signature(&self, words: usize) -> ir::Signature {
        let mut s = ir::Signature::new(self.call_conv);
        s.params.extend((0..words).map(|_| AbiParam::new(I64)));
        s.returns.push(AbiParam::new(I64));
        s
    }
    fn declare(&mut self, name: &str, words: usize) -> Result<FuncId> {
        if let Some(id) = self.declared.get(name) {
            return Ok(*id);
        }
        let id = self
            .module
            .declare_function(name, Linkage::Local, &self.signature(words))
            .map_err(|e| HandlerAbiError::Invalid(e.to_string()))?;
        self.declared.insert(name.to_owned(), id);
        Ok(id)
    }
    fn fresh(&mut self, what: &str) -> String {
        self.fresh += 1;
        format!("{what}${}", self.fresh)
    }

    fn source(&self, name: &str) -> Result<&'w Function> {
        let f = self
            .world
            .functions
            .get(name)
            .ok_or_else(|| HandlerAbiError::Invalid(format!("unknown function {name}")))?;
        if !f.generics.is_empty() {
            return unsupported(format!("generic function {name}"));
        }
        if f.body.is_none() {
            return unsupported(format!("built-in function {name}"));
        }
        Ok(f)
    }
    /// Declare a source function (queued for definition).
    fn source_function(&mut self, name: &str) -> Result<FuncId> {
        if let Some(id) = self.declared.get(name) {
            return Ok(*id);
        }
        let f = self.source(name)?;
        let words = 1 + row(self.world, &f.effects)?.len() + f.params.len();
        let id = self.declare(name, words)?;
        self.pending.push_back(Pending::Source(name.to_owned()));
        Ok(id)
    }
    /// The closure-code adapter of a source function used as a value.
    fn adapter(&mut self, name: &str) -> Result<FuncId> {
        let key = format!("{name}$value");
        if let Some(id) = self.declared.get(&key) {
            return Ok(*id);
        }
        let f = self.source(name)?;
        self.source_function(name)?;
        let words = 2 + row(self.world, &f.effects)?.len() + f.params.len();
        let id = self.declare(&key, words)?;
        self.pending.push_back(Pending::Adapter(name.to_owned()));
        Ok(id)
    }

    /// Compile `entry` and everything it reaches.
    pub fn compile(&mut self, entry: &str) -> Result<FuncId> {
        let f = self.source(entry)?;
        if !f.params.is_empty() || !row(self.world, &f.effects)?.is_empty() {
            return unsupported("an entry with parameters or effects");
        }
        let id = self.source_function(entry)?;
        while let Some(p) = self.pending.pop_front() {
            self.define(p)?;
        }
        Ok(id)
    }

    fn define(&mut self, p: Pending) -> Result<()> {
        let world = self.world;
        let (name, words) = match &p {
            Pending::Source(n) => {
                let f = self.source(n)?;
                (
                    n.clone(),
                    1 + row(world, &f.effects)?.len() + f.params.len(),
                )
            }
            Pending::Adapter(n) => {
                let f = self.source(n)?;
                (
                    format!("{n}$value"),
                    2 + row(world, &f.effects)?.len() + f.params.len(),
                )
            }
            Pending::Lambda {
                name, params, row, ..
            } => (name.clone(), 2 + row.len() + params.len()),
            Pending::Body { name, .. } => (name.clone(), 3),
            Pending::Clause { name, binds, .. } => (name.clone(), 4 + binds.len()),
            Pending::Ret { name, .. } => (name.clone(), 4),
        };
        let id = self.declare(&name, words)?;
        self.ctx.func.signature = self.signature(words);
        self.ctx.func.name = ir::UserFuncName::user(0, id.as_u32());
        let mut fctx = std::mem::take(&mut self.fctx);
        let mut func = std::mem::replace(&mut self.ctx.func, ir::Function::new());
        let result = {
            let b = FunctionBuilder::new(&mut func, &mut fctx);
            let mut cx = FnCx::new(self, b);
            match cx.body(p) {
                Ok(v) => {
                    cx.finish(v);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        };
        self.fctx = fctx;
        self.ctx.func = func;
        if let Err(e) = result {
            self.ctx.clear();
            return Err(e);
        }
        self.module
            .define_function(id, &mut self.ctx)
            .map_err(|e| HandlerAbiError::Invalid(format!("{name}: {e:?}")))?;
        let size = self
            .ctx
            .compiled_code()
            .map(|c| c.code_buffer().len())
            .unwrap_or(0);
        self.sizes.insert(name, size);
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }
}

#[derive(Clone)]
struct Local {
    name: String,
    var: Variable,
    kind: Kind,
}

struct FnCx<'l, 'w, 'f, M: Module> {
    l: &'l mut Lowering<'w, M>,
    b: FunctionBuilder<'f>,
    rt: Variable,
    locals: Vec<Local>,
    /// Capabilities in scope, innermost last: (key, effect, variable).
    caps: Vec<(String, Effect, Variable)>,
    /// The `resume` of the clause being compiled: (name, k, stateful).
    resume: Option<(String, Variable, bool)>,
    /// Returns 0 at once (unwinding).
    bail: ir::Block,
}

impl<'l, 'w, 'f, M: Module> FnCx<'l, 'w, 'f, M> {
    fn new(l: &'l mut Lowering<'w, M>, mut b: FunctionBuilder<'f>) -> Self {
        let rt = b.declare_var(I64);
        let bail = b.create_block();
        Self {
            l,
            b,
            rt,
            locals: vec![],
            caps: vec![],
            resume: None,
            bail,
        }
    }
    fn var(&mut self, value: ir::Value) -> Variable {
        let v = self.b.declare_var(I64);
        self.b.def_var(v, value);
        v
    }
    fn bind(&mut self, name: &str, kind: Kind, value: ir::Value) {
        let var = self.var(value);
        self.locals.push(Local {
            name: name.to_owned(),
            var,
            kind,
        });
    }
    fn lookup(&self, name: &str) -> Option<&Local> {
        self.locals.iter().rev().find(|l| l.name == name)
    }
    fn cap(&self, key: &str) -> Option<Variable> {
        self.caps
            .iter()
            .rev()
            .find(|(k, _, _)| k == key)
            .map(|(_, _, v)| *v)
    }
    fn ty(&self, e: &Expr) -> Option<&'w Type> {
        self.l.world.types.get(&(e.span.start, e.span.end))
    }
    fn iconst(&mut self, v: i64) -> ir::Value {
        self.b.ins().iconst(I64, v)
    }
    fn call_runtime(&mut self, f: FuncId, args: &[ir::Value]) -> ir::Value {
        let fref = self.l.module.declare_func_in_func(f, self.b.func);
        let call = self.b.ins().call(fref, args);
        self.b.inst_results(call)[0]
    }
    fn func_ref(&mut self, f: FuncId) -> ir::FuncRef {
        let fref = self.l.module.declare_func_in_func(f, self.b.func);
        // JIT definitions are not guaranteed to be within ±2 GiB of each
        // other: use absolute references (as the native backend does).
        self.b.func.dfg.ext_funcs[fref].colocated = false;
        fref
    }
    fn func_addr(&mut self, f: FuncId) -> ir::Value {
        let fref = self.func_ref(f);
        self.b.ins().func_addr(I64, fref)
    }
    /// Return at once if `rt.unwinding` is set.
    fn check_unwind(&mut self) {
        let rt = self.b.use_var(self.rt);
        let flag = self.b.ins().load(I64, MemFlags::trusted(), rt, 0);
        let next = self.b.create_block();
        self.b.ins().brif(flag, self.bail, &[], next, &[]);
        self.b.switch_to_block(next);
        self.b.seal_block(next);
    }
    fn alloc(&mut self, words: usize) -> ir::Value {
        let rt = self.b.use_var(self.rt);
        let n = self.iconst(words as i64);
        self.call_runtime(self.l.runtime.alloc, &[rt, n])
    }
    fn store(&mut self, base: ir::Value, offset: i32, v: ir::Value) {
        self.b.ins().store(MemFlags::trusted(), v, base, offset);
    }
    fn load(&mut self, base: ir::Value, offset: i32) -> ir::Value {
        self.b.ins().load(I64, MemFlags::trusted(), base, offset)
    }

    /// Entry block with `n` word parameters; the first is `rt`.
    fn entry(&mut self, n: usize) -> Vec<ir::Value> {
        let block = self.b.create_block();
        for _ in 0..n {
            self.b.append_block_param(block, I64);
        }
        self.b.switch_to_block(block);
        self.b.seal_block(block);
        let params = self.b.block_params(block).to_vec();
        self.b.def_var(self.rt, params[0]);
        params
    }
    fn finish(mut self, value: ir::Value) {
        self.b.ins().return_(&[value]);
        self.b.switch_to_block(self.bail);
        self.b.seal_block(self.bail);
        let zero = self.iconst(0);
        self.b.ins().return_(&[zero]);
        self.b.finalize();
    }
    /// Bind captured variables and capabilities from an environment record.
    fn unpack(
        &mut self,
        env: ir::Value,
        captures: &[(String, Kind)],
        caps: &[(String, Effect)],
        first: i32,
    ) {
        let mut offset = first;
        for (name, kind) in captures {
            let v = self.load(env, offset);
            self.bind(name, kind.clone(), v);
            offset += 8;
        }
        for (key, effect) in caps {
            let v = self.load(env, offset);
            let var = self.var(v);
            self.caps.push((key.clone(), effect.clone(), var));
            offset += 8;
        }
    }
    fn bind_row(&mut self, row: &[Effect], values: &[ir::Value]) {
        for (e, v) in row.iter().zip(values) {
            let var = self.var(*v);
            self.caps.push((effect_key(e), e.clone(), var));
        }
    }

    fn body(&mut self, p: Pending) -> Result<ir::Value> {
        let world = self.l.world;
        let (value, done) = match p {
            Pending::Source(name) => {
                let f = self.l.source(&name)?;
                let row = row(world, &f.effects)?;
                let params = self.entry(1 + row.len() + f.params.len());
                self.bind_row(&row, &params[1..1 + row.len()]);
                for (p, v) in f.params.iter().zip(&params[1 + row.len()..]) {
                    if p.pass == Pass::Inout {
                        return unsupported("inout parameters");
                    }
                    self.bind(&p.name, Kind::of(&p.ty), *v);
                }
                let body = f.body.as_ref().expect("checked by source()");
                (self.expr(body, Some(&f.result))?, ())
            }
            Pending::Adapter(name) => {
                let f = self.l.source(&name)?;
                let n = row(world, &f.effects)?.len() + f.params.len();
                let params = self.entry(2 + n);
                let id = self.l.source_function(&name)?;
                let fref = self.func_ref(id);
                let mut args = vec![params[0]];
                args.extend(&params[2..]);
                let call = self.b.ins().call(fref, &args);
                let v = self.b.inst_results(call)[0];
                self.check_unwind();
                (v, ())
            }
            Pending::Lambda {
                params,
                row,
                captures,
                body,
                ..
            } => {
                let values = self.entry(2 + row.len() + params.len());
                self.unpack(values[1], &captures, &[], 8);
                self.bind_row(&row, &values[2..2 + row.len()]);
                for ((name, ty), v) in params.iter().zip(&values[2 + row.len()..]) {
                    self.bind(name, Kind::of(ty), *v);
                }
                (self.expr(&body, None)?, ())
            }
            Pending::Body {
                captures,
                caps,
                own,
                body,
                ..
            } => {
                let values = self.entry(3);
                self.unpack(values[1], &captures, &caps, 0);
                let var = self.var(values[2]);
                self.caps.push((effect_key(&own), own, var));
                (self.expr(&body, None)?, ())
            }
            Pending::Clause {
                captures,
                caps,
                binds,
                state,
                resume,
                body,
                ..
            } => {
                let values = self.entry(4 + binds.len());
                self.unpack(values[1], &captures, &caps, 0);
                let k = self.var(values[2]);
                for ((pat, kind), v) in binds.iter().zip(&values[4..]) {
                    self.bind_pattern(pat, kind.clone(), *v)?;
                }
                if let Some((pat, kind)) = &state {
                    self.bind_pattern(pat, kind.clone(), values[3])?;
                }
                if let Some(name) = resume {
                    self.resume = Some((name, k, state.is_some()));
                }
                (self.expr(&body, None)?, ())
            }
            Pending::Ret {
                captures,
                caps,
                value,
                state,
                body,
                ..
            } => {
                let values = self.entry(4);
                self.unpack(values[1], &captures, &caps, 0);
                self.bind_pattern(&value.0, value.1, values[2])?;
                if let Some((p, k)) = state {
                    self.bind_pattern(&p, k, values[3])?;
                }
                (self.expr(&body, None)?, ())
            }
        };
        let () = done;
        Ok(value)
    }
}

impl<'l, 'w, 'f, M: Module> FnCx<'l, 'w, 'f, M> {
    fn bind_pattern(&mut self, p: &Pattern, kind: Kind, v: ir::Value) -> Result<()> {
        match &p.kind {
            PatternKind::Bind(name) => {
                self.bind(name, kind, v);
                Ok(())
            }
            PatternKind::Wild => Ok(()),
            _ => unsupported("destructuring patterns"),
        }
    }

    /// The kind of an expression's value.
    fn kind(&self, e: &Expr) -> Kind {
        if let ExprKind::Name(n) = &e.kind
            && let Some(l) = self.lookup(n)
        {
            return l.kind.clone();
        }
        self.ty(e).map(Kind::of).unwrap_or(Kind::I64)
    }

    fn expr(&mut self, e: &Expr, expected: Option<&Type>) -> Result<ir::Value> {
        match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => {
                let kind = expected.map(Kind::of).unwrap_or_else(|| self.kind(e));
                if !kind.integer() && !matches!(kind, Kind::Opaque(_)) {
                    return invalid("integer literal of a non-integer type");
                }
                Ok(self.iconst(*n as i64))
            }
            ExprKind::Literal(Literal::Bool(b)) => Ok(self.iconst(i64::from(*b))),
            ExprKind::Literal(_) => unsupported("float and string literals"),
            ExprKind::Tuple(es) if es.is_empty() => Ok(self.iconst(0)),
            ExprKind::Tuple(_) | ExprKind::Struct { .. } => unsupported("aggregate values"),
            ExprKind::Field(..) | ExprKind::Index(..) | ExprKind::Try(_) => {
                unsupported("fields, indexing and `?`")
            }
            ExprKind::Name(n) => self.name(n),
            ExprKind::Block { statements, tail } => {
                self.block(statements, tail.as_deref(), expected)
            }
            ExprKind::Unary(op, x) => self.unary(e, op, x),
            ExprKind::Binary(op, a, b) => self.binary(e, op, a, b),
            ExprKind::Assign(op, place, value) => {
                let ExprKind::Name(n) = &place.kind else {
                    return unsupported("assignment to a field");
                };
                let Some(local) = self.lookup(n).cloned() else {
                    return unsupported(format!("assignment to captured variable {n}"));
                };
                let v = if op == "=" {
                    self.expr(value, None)?
                } else {
                    let arith = op.trim_end_matches('=');
                    let combined = Expr {
                        kind: ExprKind::Binary(arith.into(), place.clone(), value.clone()),
                        span: e.span.clone(),
                    };
                    self.binary(&combined, arith, place, value)?
                };
                self.b.def_var(local.var, v);
                Ok(self.iconst(0))
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(condition, None)?;
                let (yes, no, join) = (
                    self.b.create_block(),
                    self.b.create_block(),
                    self.b.create_block(),
                );
                self.b.append_block_param(join, I64);
                self.b.ins().brif(c, yes, &[], no, &[]);
                self.b.switch_to_block(yes);
                self.b.seal_block(yes);
                let depth = self.locals.len();
                let v = self.expr(then, expected)?;
                self.locals.truncate(depth);
                self.b.ins().jump(join, &[v.into()]);
                self.b.switch_to_block(no);
                self.b.seal_block(no);
                let v = match otherwise {
                    Some(e) => self.expr(e, expected)?,
                    None => self.iconst(0),
                };
                self.locals.truncate(depth);
                self.b.ins().jump(join, &[v.into()]);
                self.b.switch_to_block(join);
                self.b.seal_block(join);
                Ok(self.b.block_params(join)[0])
            }
            ExprKind::Match { value, arms } => self.matches(value, arms, expected),
            ExprKind::Call { callee, args, .. } => self.call(e, callee, args),
            ExprKind::Closure { params, body, .. } => self.closure(e, params, body),
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => self.handle(effect, state.as_ref(), body, ret, ops),
        }
    }

    fn name(&mut self, n: &str) -> Result<ir::Value> {
        if let Some(l) = self.lookup(n) {
            let var = l.var;
            return Ok(self.b.use_var(var));
        }
        if self.resume.as_ref().is_some_and(|(r, _, _)| r == n) {
            return unsupported("a continuation used as a value");
        }
        if self.l.world.functions.contains_key(n) {
            // A source function as a value: a closure record with no
            // captures whose code forwards to the function.
            let id = self.l.adapter(n)?;
            let record = self.alloc(1);
            let code = self.func_addr(id);
            self.store(record, 0, code);
            return Ok(record);
        }
        unsupported(format!("name {n}"))
    }

    fn block(
        &mut self,
        statements: &[Statement],
        tail: Option<&Expr>,
        expected: Option<&Type>,
    ) -> Result<ir::Value> {
        let depth = self.locals.len();
        for s in statements {
            match s {
                Statement::Let { pat, ty, value } => {
                    let declared = (!matches!(ty, Type::Infer)).then_some(ty);
                    let v = self.expr(value, declared)?;
                    let kind = match declared {
                        Some(t) => Kind::of(t),
                        None => self.kind(value),
                    };
                    self.bind_pattern(pat, kind, v)?;
                }
                Statement::Expr(e) => {
                    self.expr(e, None)?;
                }
            }
        }
        let v = match tail {
            Some(e) => self.expr(e, expected)?,
            None => self.iconst(0),
        };
        self.locals.truncate(depth);
        Ok(v)
    }

    fn matches(
        &mut self,
        value: &Expr,
        arms: &[(Pattern, Expr)],
        expected: Option<&Type>,
    ) -> Result<ir::Value> {
        let kind = self.kind(value);
        let v = self.expr(value, None)?;
        let join = self.b.create_block();
        self.b.append_block_param(join, I64);
        for (pat, body) in arms {
            let depth = self.locals.len();
            let next = self.b.create_block();
            match &pat.kind {
                PatternKind::Wild | PatternKind::Bind(_) => {
                    self.bind_pattern(pat, kind.clone(), v)?;
                    let r = self.expr(body, expected)?;
                    self.b.ins().jump(join, &[r.into()]);
                }
                PatternKind::Literal(lit) => {
                    let w = match lit {
                        Literal::Int(n) => *n as i64,
                        Literal::Bool(b) => i64::from(*b),
                        _ => return unsupported("float and string patterns"),
                    };
                    let hit = self.b.ins().icmp_imm(IntCC::Equal, v, w);
                    let yes = self.b.create_block();
                    self.b.ins().brif(hit, yes, &[], next, &[]);
                    self.b.switch_to_block(yes);
                    self.b.seal_block(yes);
                    let r = self.expr(body, expected)?;
                    self.b.ins().jump(join, &[r.into()]);
                }
                _ => return unsupported("constructor patterns"),
            }
            self.locals.truncate(depth);
            self.b.switch_to_block(next);
            self.b.seal_block(next);
        }
        // No arm matched: the checker proved this unreachable.
        let rt = self.b.use_var(self.rt);
        let code = self.iconst(3);
        self.call_runtime(self.l.runtime.fault, &[rt, code]);
        self.b.ins().jump(self.bail, &[]);
        self.b.switch_to_block(join);
        self.b.seal_block(join);
        Ok(self.b.block_params(join)[0])
    }

    fn overflow_checked(&self, e: &Expr) -> bool {
        self.l
            .world
            .overflow_ops
            .contains(&(e.span.start, e.span.end))
    }
    fn partial(&self, e: &Expr) -> bool {
        self.l
            .world
            .partial_ops
            .contains(&(e.span.start, e.span.end))
    }

    /// Branch to a `raise` of `Fail[ArithOverflow]` when `cond` is set.
    fn raise_if(&mut self, cond: ir::Value) -> Result<()> {
        let key = effect_key(&Effect {
            label: None,
            name: "Fail".into(),
            args: vec![Type::named("ArithOverflow")],
        });
        let Some(cap) = self.cap(&key) else {
            return invalid("overflow check without a Fail[ArithOverflow] capability");
        };
        let (raise, next) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(cond, raise, &[], next, &[]);
        self.b.switch_to_block(raise);
        self.b.seal_block(raise);
        let rt = self.b.use_var(self.rt);
        let cap = self.b.use_var(cap);
        let args = self.alloc(1);
        let op = self.iconst(0);
        let one = self.iconst(1);
        self.call_runtime(self.l.runtime.perform, &[rt, cap, op, args, one]);
        // `raise` never returns normally: its clause cannot resume.
        let rt = self.b.use_var(self.rt);
        let code = self.iconst(2);
        let flag = self.b.ins().load(I64, MemFlags::trusted(), rt, 0);
        let fault = self.b.create_block();
        self.b.ins().brif(flag, self.bail, &[], fault, &[]);
        self.b.switch_to_block(fault);
        self.b.seal_block(fault);
        self.call_runtime(self.l.runtime.fault, &[rt, code]);
        self.b.ins().jump(self.bail, &[]);
        self.b.switch_to_block(next);
        self.b.seal_block(next);
        Ok(())
    }

    fn unary(&mut self, e: &Expr, op: &str, x: &Expr) -> Result<ir::Value> {
        let kind = self.kind(x);
        let v = self.expr(x, self.ty(e))?;
        match (op, &kind) {
            ("!", Kind::Bool) => Ok(self.b.ins().bxor_imm(v, 1)),
            ("-", Kind::I64) => {
                if self.overflow_checked(e) {
                    let min = self.b.ins().icmp_imm(IntCC::Equal, v, i64::MIN);
                    self.raise_if(min)?;
                }
                Ok(self.b.ins().ineg(v))
            }
            ("-", k) if k.integer() => {
                // Negating an unsigned value overflows unless it is zero.
                if self.overflow_checked(e) {
                    let nonzero = self.b.ins().icmp_imm(IntCC::NotEqual, v, 0);
                    self.raise_if(nonzero)?;
                }
                Ok(self.iconst(0))
            }
            _ => unsupported(format!("unary {op} on {kind:?}")),
        }
    }

    fn operand_kind(&self, a: &Expr, b: &Expr) -> Kind {
        let ka = self.kind(a);
        if matches!(self.ty(a), Some(Type::IntLiteral(_))) && !matches!(a.kind, ExprKind::Name(_)) {
            let kb = self.kind(b);
            if kb.integer() {
                return kb;
            }
        }
        ka
    }

    fn binary(&mut self, e: &Expr, op: &str, a: &Expr, b: &Expr) -> Result<ir::Value> {
        if op == "&&" || op == "||" {
            let x = self.expr(a, None)?;
            let (rhs, join) = (self.b.create_block(), self.b.create_block());
            self.b.append_block_param(join, I64);
            if op == "&&" {
                self.b.ins().brif(x, rhs, &[], join, &[x.into()]);
            } else {
                self.b.ins().brif(x, join, &[x.into()], rhs, &[]);
            }
            self.b.switch_to_block(rhs);
            self.b.seal_block(rhs);
            let y = self.expr(b, None)?;
            self.b.ins().jump(join, &[y.into()]);
            self.b.switch_to_block(join);
            self.b.seal_block(join);
            return Ok(self.b.block_params(join)[0]);
        }
        let kind = self.operand_kind(a, b);
        let operand_ty = match &kind {
            Kind::I64 => Type::named("i64"),
            Kind::U64 => Type::named("u64"),
            Kind::U32 => Type::named("u32"),
            Kind::U8 => Type::named("u8"),
            Kind::Bool => Type::named("bool"),
            k => return unsupported(format!("operator {op} on {k:?}")),
        };
        let x = self.expr(a, Some(&operand_ty))?;
        let y = self.expr(b, Some(&operand_ty))?;
        let signed = kind == Kind::I64;
        let compare = |cc_signed: IntCC, cc_unsigned: IntCC| {
            if signed { cc_signed } else { cc_unsigned }
        };
        let cc = match op {
            "==" => Some(IntCC::Equal),
            "!=" => Some(IntCC::NotEqual),
            "<" => Some(compare(IntCC::SignedLessThan, IntCC::UnsignedLessThan)),
            "<=" => Some(compare(
                IntCC::SignedLessThanOrEqual,
                IntCC::UnsignedLessThanOrEqual,
            )),
            ">" => Some(compare(
                IntCC::SignedGreaterThan,
                IntCC::UnsignedGreaterThan,
            )),
            ">=" => Some(compare(
                IntCC::SignedGreaterThanOrEqual,
                IntCC::UnsignedGreaterThanOrEqual,
            )),
            _ => None,
        };
        if let Some(cc) = cc {
            let c = self.b.ins().icmp(cc, x, y);
            return Ok(self.b.ins().uextend(I64, c));
        }
        if !kind.integer() {
            return unsupported(format!("operator {op} on {kind:?}"));
        }
        let checked = self.overflow_checked(e);
        let max = kind.max();
        match op {
            "+" | "-" | "*" => {
                let (r, ovf) = match (op, &kind) {
                    ("+", Kind::I64) => self.b.ins().sadd_overflow(x, y),
                    ("-", Kind::I64) => self.b.ins().ssub_overflow(x, y),
                    ("*", Kind::I64) => self.b.ins().smul_overflow(x, y),
                    ("+", _) => self.b.ins().uadd_overflow(x, y),
                    ("-", _) => self.b.ins().usub_overflow(x, y),
                    _ => self.b.ins().umul_overflow(x, y),
                };
                if checked {
                    let mut ovf = self.b.ins().uextend(I64, ovf);
                    if let Some(max) = max {
                        let big = self
                            .b
                            .ins()
                            .icmp_imm(IntCC::UnsignedGreaterThan, r, max as i64);
                        let big = self.b.ins().uextend(I64, big);
                        ovf = self.b.ins().bor(ovf, big);
                    }
                    self.raise_if(ovf)?;
                }
                Ok(r)
            }
            "/" | "%" => {
                if self.partial(e) {
                    return unsupported("division whose divisor may be zero (Fail[RuntimeError])");
                }
                if signed {
                    // MIN / -1 overflows; MIN % -1 is 0. Keep the hardware
                    // from trapping by dividing by 1 instead.
                    let minus_one = self.b.ins().icmp_imm(IntCC::Equal, y, -1);
                    let min = self.b.ins().icmp_imm(IntCC::Equal, x, i64::MIN);
                    let both = self.b.ins().band(minus_one, min);
                    if op == "/" && checked {
                        let both64 = self.b.ins().uextend(I64, both);
                        self.raise_if(both64)?;
                    }
                    let one = self.iconst(1);
                    let safe = self.b.ins().select(both, one, y);
                    if op == "/" {
                        let q = self.b.ins().sdiv(x, safe);
                        let neg = self.b.ins().ineg(q);
                        Ok(self.b.ins().select(both, neg, q))
                    } else {
                        let r = self.b.ins().srem(x, safe);
                        let zero = self.iconst(0);
                        Ok(self.b.ins().select(both, zero, r))
                    }
                } else if op == "/" {
                    Ok(self.b.ins().udiv(x, y))
                } else {
                    Ok(self.b.ins().urem(x, y))
                }
            }
            _ => unsupported(format!("operator {op}")),
        }
    }

    /// The capabilities a callee with row `row` receives from this scope.
    fn pass_caps(&mut self, row: &[Effect]) -> Result<Vec<ir::Value>> {
        let mut out = vec![];
        for e in row {
            let key = effect_key(e);
            let Some(var) = self.cap(&key) else {
                return invalid(format!("no capability for {key} in scope"));
            };
            out.push(self.b.use_var(var));
        }
        Ok(out)
    }

    fn args(&mut self, args: &[Arg], types: &[Type]) -> Result<Vec<ir::Value>> {
        let mut out = vec![];
        for (i, a) in args.iter().enumerate() {
            if a.pass == Pass::Inout {
                return unsupported("inout arguments");
            }
            out.push(self.expr(&a.expr, types.get(i))?);
        }
        Ok(out)
    }

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Arg]) -> Result<ir::Value> {
        let world = self.l.world;
        if let ExprKind::Name(n) = &callee.kind
            && self.lookup(n).is_none()
        {
            if let Some((r, k, stateful)) = self.resume.clone()
                && &r == n
            {
                return self.resume_call(k, stateful, args);
            }
            if let Some(v) = self.builtin(e, n, args)? {
                return Ok(v);
            }
            if let Some((effect, op)) = n.split_once("::")
                && let Some(decl) = world.effects.get(effect)
            {
                return self.perform(decl, effect, op, args);
            }
            if world.functions.contains_key(n) {
                let f = self.l.source(n)?;
                let row = row(world, &f.effects)?;
                let types: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
                let mut values = vec![self.b.use_var(self.rt)];
                values.extend(self.pass_caps(&row)?);
                values.extend(self.args(args, &types)?);
                let id = self.l.source_function(n)?;
                let fref = self.func_ref(id);
                let call = self.b.ins().call(fref, &values);
                let v = self.b.inst_results(call)[0];
                if !row.is_empty() {
                    self.check_unwind();
                }
                return Ok(v);
            }
        }
        // A function value: call through its closure record.
        let Some(t) = self
            .ty(callee)
            .cloned()
            .or_else(|| match self.kind(callee) {
                Kind::Function(t) => Some(t),
                _ => None,
            })
        else {
            return invalid("callee without a type");
        };
        let Some((params, _, effects)) = function_type(&t) else {
            return unsupported(format!("calling a value of type {t}"));
        };
        let row = row(world, effects)?;
        let types: Vec<Type> = params.iter().map(|(_, t)| t.clone()).collect();
        let record = self.expr(callee, None)?;
        let mut values = vec![self.b.use_var(self.rt), record];
        values.extend(self.pass_caps(&row)?);
        values.extend(self.args(args, &types)?);
        let code = self.load(record, 0);
        let sig = self.l.signature(values.len());
        let sig = self.b.import_signature(sig);
        let call = self.b.ins().call_indirect(sig, code, &values);
        let v = self.b.inst_results(call)[0];
        if !row.is_empty() {
            self.check_unwind();
        }
        Ok(v)
    }

    fn builtin(&mut self, e: &Expr, n: &str, args: &[Arg]) -> Result<Option<ir::Value>> {
        let op = match n {
            "wrapping_add" => "+",
            "wrapping_sub" => "-",
            "wrapping_mul" => "*",
            _ => return Ok(None),
        };
        if self
            .l
            .world
            .functions
            .get(n)
            .is_some_and(|f| f.body.is_some())
        {
            return Ok(None);
        }
        let [a, b] = args else {
            return invalid(format!("{n} takes two arguments"));
        };
        let kind = self.operand_kind(&a.expr, &b.expr);
        let _ = e;
        let x = self.expr(&a.expr, None)?;
        let y = self.expr(&b.expr, None)?;
        let r = match op {
            "+" => self.b.ins().iadd(x, y),
            "-" => self.b.ins().isub(x, y),
            _ => self.b.ins().imul(x, y),
        };
        Ok(Some(match kind.max() {
            Some(max) => self.b.ins().band_imm(r, max as i64),
            None => r,
        }))
    }

    fn perform(
        &mut self,
        decl: &EffectDecl,
        effect: &str,
        op: &str,
        args: &[Arg],
    ) -> Result<ir::Value> {
        let Some(index) = decl.ops.iter().position(|o| o.name == op) else {
            return invalid(format!("unknown operation {effect}::{op}"));
        };
        if args.len() > MAX_OPERATION_ARGS {
            return unsupported("operations with more than four arguments");
        }
        let candidates: Vec<Variable> = self
            .caps
            .iter()
            .rev()
            .filter(|(key, e, _)| e.name == effect || key.starts_with(&format!("{effect}[")))
            .map(|(_, _, v)| *v)
            .collect();
        let Some(&cap) = candidates.first() else {
            return invalid(format!("no handler for {effect} in scope"));
        };
        let types: Vec<Type> = decl.ops[index]
            .params
            .iter()
            .map(|p| p.ty.clone())
            .collect();
        let values = self.args(args, &types)?;
        let buffer = self.alloc(values.len());
        for (i, v) in values.iter().enumerate() {
            self.store(buffer, 8 * i as i32, *v);
        }
        let rt = self.b.use_var(self.rt);
        let cap = self.b.use_var(cap);
        let op = self.iconst(index as i64);
        let count = self.iconst(values.len() as i64);
        let v = self.call_runtime(self.l.runtime.perform, &[rt, cap, op, buffer, count]);
        self.check_unwind();
        Ok(v)
    }

    fn resume_call(&mut self, k: Variable, stateful: bool, args: &[Arg]) -> Result<ir::Value> {
        let (value, state) = match (stateful, args) {
            (false, [v]) => (self.expr(&v.expr, None)?, self.iconst(0)),
            (true, [v, s]) => {
                let v = self.expr(&v.expr, None)?;
                (v, self.expr(&s.expr, None)?)
            }
            _ => return invalid("resume with the wrong number of arguments"),
        };
        let rt = self.b.use_var(self.rt);
        let k = self.b.use_var(k);
        let v = self.call_runtime(self.l.runtime.resume, &[rt, k, value, state]);
        self.check_unwind();
        Ok(v)
    }

    /// Variables of this scope that `exprs` use, with their kinds.
    fn captures(&self, used: &BTreeSet<String>) -> Result<Vec<(String, Kind)>> {
        let mut out = vec![];
        for name in used {
            if let Some(l) = self.lookup(name) {
                out.push((name.clone(), l.kind.clone()));
            } else if self.resume.as_ref().is_some_and(|(r, _, _)| r == name) {
                return unsupported("a continuation captured by a closure or handler");
            }
        }
        Ok(out)
    }

    fn closure(&mut self, e: &Expr, params: &[Param], body: &Expr) -> Result<ir::Value> {
        let world = self.l.world;
        let Some(t) = self.ty(e).cloned() else {
            return invalid("closure without a type");
        };
        let Some((types, _, effects)) = function_type(&t) else {
            return invalid("closure of a non-function type");
        };
        if types.len() != params.len() {
            return invalid("closure arity differs from its type");
        }
        let row = row(world, effects)?;
        let mut used = crate::check::free_names(body);
        for p in params {
            if p.pass == Pass::Inout {
                return unsupported("inout closure parameters");
            }
            used.remove(&p.name);
        }
        let captures = self.captures(&used)?;
        let name = self.l.fresh("closure");
        let words = 2 + row.len() + params.len();
        let id = self.l.declare(&name, words)?;
        let record = self.alloc(1 + captures.len());
        let code = self.func_addr(id);
        self.store(record, 0, code);
        for (i, (n, _)) in captures.iter().enumerate() {
            let var = self.lookup(n).expect("captured").var;
            let v = self.b.use_var(var);
            self.store(record, 8 * (i as i32 + 1), v);
        }
        self.l.pending.push_back(Pending::Lambda {
            name,
            // Unannotated parameters take their type from the closure's.
            params: params
                .iter()
                .zip(types)
                .map(|(p, (_, t))| {
                    let ty = if p.ty == Type::Infer { t } else { &p.ty };
                    (p.name.clone(), ty.clone())
                })
                .collect(),
            row,
            captures,
            body: body.clone(),
        });
        Ok(record)
    }

    fn handle(
        &mut self,
        effect: &Effect,
        state: Option<&(Pattern, Box<Expr>)>,
        body: &Expr,
        ret: &Clause,
        ops: &[Clause],
    ) -> Result<ir::Value> {
        let world = self.l.world;
        let Some(decl) = world.effects.get(&effect.name) else {
            return unsupported(format!("effect {}", effect.name));
        };
        let generics: BTreeMap<String, Type> = decl
            .generics
            .iter()
            .zip(&effect.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect();
        let state_value = match state {
            Some((_, init)) => Some(self.expr(init, None)?),
            None => None,
        };
        let state_kind = state.map(|(_, init)| self.kind(init));
        // Everything the body and clauses read from this scope.
        let mut used = crate::check::free_names(body);
        for c in std::iter::once(ret).chain(ops) {
            let mut names = crate::check::free_names(&c.body);
            for p in &c.pats {
                for b in bound(p) {
                    names.remove(&b);
                }
            }
            used.extend(names);
        }
        let captures = self.captures(&used)?;
        let caps: Vec<(String, Effect)> = {
            let mut seen = BTreeSet::new();
            let mut innermost: Vec<(String, Effect)> = self
                .caps
                .iter()
                .rev()
                .filter(|(k, _, _)| seen.insert(k.clone()))
                .map(|(k, e, _)| (k.clone(), e.clone()))
                .collect();
            innermost.reverse();
            innermost
        };
        let env = self.alloc(captures.len() + caps.len());
        let mut offset = 0;
        for (n, _) in &captures {
            let var = self.lookup(n).expect("captured").var;
            let v = self.b.use_var(var);
            self.store(env, offset, v);
            offset += 8;
        }
        for (key, _) in &caps {
            let var = self.cap(key).expect("in scope");
            let v = self.b.use_var(var);
            self.store(env, offset, v);
            offset += 8;
        }
        // Body.
        let own = effect.clone();
        let body_name = self.l.fresh("handled");
        let body_id = self.l.declare(&body_name, 3)?;
        self.l.pending.push_back(Pending::Body {
            name: body_name,
            captures: captures.clone(),
            caps: caps.clone(),
            own,
            body: body.clone(),
        });
        // Return clause.
        let (value_pat, ret_state) = match (&state, ret.pats.as_slice()) {
            (None, [p]) => (p.clone(), None),
            (Some(_), [p]) => match &p.kind {
                PatternKind::Tuple(ps) if ps.len() == 2 => (
                    ps[0].clone(),
                    Some((ps[1].clone(), state_kind.clone().expect("stateful"))),
                ),
                _ => return unsupported("this return clause pattern"),
            },
            _ => return invalid("return clause with the wrong number of patterns"),
        };
        let ret_name = self.l.fresh("return");
        let ret_id = self.l.declare(&ret_name, 4)?;
        let body_kind = self.kind(body);
        self.l.pending.push_back(Pending::Ret {
            name: ret_name,
            captures: captures.clone(),
            caps: caps.clone(),
            value: (value_pat, body_kind),
            state: ret_state,
            body: ret.body.clone(),
        });
        // Operation clauses, in declaration order.
        let mut clause_ids = vec![];
        for op in &decl.ops {
            let Some(clause) = ops.iter().find(|c| c.name == op.name) else {
                return invalid(format!("no clause for {}", op.name));
            };
            let arity = op.params.len();
            if arity > MAX_OPERATION_ARGS {
                return unsupported("operations with more than four arguments");
            }
            let stateful = state.is_some();
            let expected = arity + usize::from(stateful) + 1;
            if clause.pats.len() != expected {
                return invalid(format!(
                    "clause {} has the wrong number of patterns",
                    op.name
                ));
            }
            let binds = op
                .params
                .iter()
                .zip(&clause.pats)
                .map(|(p, pat)| (pat.clone(), Kind::of(&substitute(&p.ty, &generics))))
                .collect();
            let clause_state = stateful.then(|| {
                (
                    clause.pats[arity].clone(),
                    state_kind.clone().expect("stateful"),
                )
            });
            let resume = match &clause.pats[expected - 1].kind {
                PatternKind::Bind(n) => Some(n.clone()),
                _ => None,
            };
            let name = self.l.fresh(&format!("clause_{}", op.name));
            let id = self.l.declare(&name, 4 + arity)?;
            clause_ids.push(id);
            self.l.pending.push_back(Pending::Clause {
                name,
                captures: captures.clone(),
                caps: caps.clone(),
                binds,
                state: clause_state,
                resume,
                body: clause.body.clone(),
            });
        }
        // Descriptor.
        let desc = self.alloc(6 + clause_ids.len());
        let code = self.func_addr(body_id);
        self.store(desc, descriptor::BODY, code);
        self.store(desc, descriptor::ENV, env);
        let code = self.func_addr(ret_id);
        self.store(desc, descriptor::RET, code);
        let stateful = self.iconst(i64::from(state.is_some()));
        self.store(desc, descriptor::STATEFUL, stateful);
        let init = match state_value {
            Some(v) => v,
            None => self.iconst(0),
        };
        self.store(desc, descriptor::STATE, init);
        let count = self.iconst(clause_ids.len() as i64);
        self.store(desc, descriptor::CLAUSES, count);
        for (i, id) in clause_ids.into_iter().enumerate() {
            let code = self.func_addr(id);
            self.store(desc, descriptor::CLAUSE0 + 8 * i as i32, code);
        }
        let rt = self.b.use_var(self.rt);
        let v = self.call_runtime(self.l.runtime.handle, &[rt, desc]);
        self.check_unwind();
        Ok(v)
    }
}

fn bound(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatternKind::Bind(n) => vec![n.clone()],
        PatternKind::Tuple(ps) => ps.iter().flat_map(bound).collect(),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().flat_map(|(_, p)| bound(p)).collect()
        }
        _ => vec![],
    }
}
