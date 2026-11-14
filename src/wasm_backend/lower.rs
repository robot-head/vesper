//! Place IR to core Wasm.
//!
//! * Each reachable IR function becomes one Wasm function whose parameters
//!   and result are `i64` (see [`super::layout`]). `inout` parameters and the
//!   captured places of a handler body are *cells*: addresses of one 8-byte
//!   slot. Callers pass a fresh cell holding the current value and copy it
//!   back after the call, on every exit including an abort.
//! * The acyclic block graph is emitted in topological order inside nested
//!   `block`s, so every forward edge is a `br` to the end of the block that
//!   precedes its target. Loops are recursion; a direct call in tail position
//!   is a `return_call`.
//! * Aborts (`Fail`, clauses that drop their continuation) set the `abort`
//!   global to the target handler frame and return; every call site checks it.
//! * Handlers are frames in linear memory. Each dynamic evidence key
//!   (`$effect:E`, `$effect:E[args]`, `$label:l`) is a global holding the
//!   innermost frame; installing a handler saves all of them in its frame.
//!   An operation calls the frame's dispatch function through the table.
use super::{
    HandlerMode, Unsupported, WasmError,
    analysis::{Handler, Plan, Role},
    layout::{self, Scalar, Shape},
    module::Builder,
};
use crate::{
    ast::*,
    check::World,
    ir::{Argument, FunctionIr, Op, Program, Terminator, ValueId},
    numeric::IntegerKind,
};
use std::collections::{BTreeMap, BTreeSet};
use wasm_encoder::{
    BlockType, Function, Handle, HeapType, Instruction as I, MemArg, RefType, ValType,
};

const HEAP_START: i64 = 16;

/// Temporary `inout` cells and the places they are copied back to.
type Writebacks = Vec<(u32, crate::check::Place)>;

// Handler frame layout (byte offsets).
const F_DISPATCH: u64 = 0;
const F_STATE: u64 = 8;
const F_ABORT_OP: u64 = 16;
const F_ARGS: u64 = 24;

pub(crate) struct FxTypes {
    pub ct: u32,
    pub tag: u32,
    pub on_block: u32,
}

pub(crate) struct HandlerFns {
    pub dispatch_slot: u32,
    pub dispatch: u32,
    pub ret: u32,
    pub abort: u32,
    pub body_entry: Option<u32>,
    pub run: Option<u32>,
    pub fx_op: Option<u32>,
    pub frame_size: u64,
    pub env_offset: u64,
    pub capture_offset: BTreeMap<String, u64>,
    pub body_offset: u64,
}

pub(crate) struct Lowerer<'a> {
    pub world: &'a World,
    pub program: &'a Program,
    pub plan: &'a Plan,
    pub b: Builder,
    pub funcs: BTreeMap<String, u32>,
    pub heap: u32,
    pub abort: u32,
    pub env: BTreeMap<String, u32>,
    pub alloc: u32,
    pub dispatch_ty: u32,
    pub fx: Option<FxTypes>,
    pub hfns: Vec<HandlerFns>,
    eq: BTreeMap<String, u32>,
    eq_pending: Vec<(u32, Type)>,
    pub arity: usize,
    pub instructions: usize,
    pub max_instructions: usize,
}

fn mem(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 3,
        memory_index: 0,
    }
}

fn integer(s: Scalar) -> Option<IntegerKind> {
    Some(match s {
        Scalar::I64 => IntegerKind::I64,
        Scalar::U64 => IntegerKind::U64,
        Scalar::U32 => IntegerKind::U32,
        Scalar::U8 => IntegerKind::U8,
        _ => return None,
    })
}

impl<'a> Lowerer<'a> {
    pub(crate) fn new(
        world: &'a World,
        program: &'a Program,
        plan: &'a Plan,
        stack_switching: bool,
        max_instructions: usize,
    ) -> Result<Self, WasmError> {
        let mut b = Builder::default();
        let heap = b.global(HEAP_START);
        let abort = b.global(0);
        let env = plan.keys.iter().map(|k| (k.clone(), b.global(0))).collect();
        let alloc = b.declare("$alloc", &[ValType::I64], &[ValType::I64]);
        let arity = plan.max_arity;
        let dispatch_ty = b.func_type(&vec![ValType::I64; 2 + arity], &[ValType::I64]);
        let fx = if stack_switching && plan.handlers.iter().any(|h| h.stack_switching) {
            let ct_func = b.func_type(&[ValType::I64], &[ValType::I64]);
            let ct = b.cont_type(ct_func);
            let tag_ty = b.func_type(&vec![ValType::I64; 2 + arity], &[ValType::I64]);
            let tag = b.tag(tag_ty);
            let mut results = vec![ValType::I64; 2 + arity];
            results.push(ValType::Ref(RefType {
                nullable: false,
                heap_type: HeapType::Concrete(ct),
            }));
            let on_block = b.func_type(&[], &results);
            Some(FxTypes { ct, tag, on_block })
        } else {
            None
        };
        Ok(Self {
            world,
            program,
            plan,
            b,
            funcs: BTreeMap::new(),
            heap,
            abort,
            env,
            alloc,
            dispatch_ty,
            fx,
            hfns: vec![],
            eq: BTreeMap::new(),
            eq_pending: vec![],
            arity,
            instructions: 0,
            max_instructions,
        })
    }

    fn k_type(&self) -> ValType {
        ValType::Ref(RefType {
            nullable: true,
            heap_type: HeapType::Concrete(self.fx.as_ref().map(|f| f.ct).unwrap_or(0)),
        })
    }

    /// Wasm parameter list of an IR function under its role.
    fn signature(&self, name: &str) -> Vec<ValType> {
        let f = &self.program.functions[name];
        let mut params = vec![];
        let (skip, hidden) = match &self.plan.roles[name] {
            Role::OpClause { handler, op } => {
                let c = self.plan.handlers[*handler]
                    .ops
                    .iter()
                    .find(|c| c.op == *op)
                    .expect("planned clause");
                let hidden = match c.mode {
                    HandlerMode::Abort => vec![],
                    HandlerMode::Tail => vec![ValType::I64],
                    HandlerMode::StackSwitching => vec![ValType::I64, self.k_type()],
                };
                (c.resume.clone(), hidden)
            }
            _ => (None, vec![]),
        };
        for p in &f.params {
            if Some(&p.name) != skip.as_ref() {
                params.push(ValType::I64);
            }
        }
        params.extend(hidden);
        params
    }

    pub(crate) fn declare_all(&mut self) -> Result<(), WasmError> {
        for name in &self.plan.functions {
            let params = self.signature(name);
            let idx = self.b.declare(name, &params, &[ValType::I64]);
            self.funcs.insert(name.clone(), idx);
        }
        for (i, h) in self.plan.handlers.iter().enumerate() {
            let a = vec![ValType::I64; 2 + self.arity];
            let dispatch = self
                .b
                .declare(&format!("$dispatch{i}"), &a, &[ValType::I64]);
            let dispatch_slot = self.b.table_slot(dispatch);
            let ret = self
                .b
                .declare(&format!("$ret{i}"), &[ValType::I64; 2], &[ValType::I64]);
            let abort = self
                .b
                .declare(&format!("$abort{i}"), &[ValType::I64], &[ValType::I64]);
            let (body_entry, run, fx_op) = if h.stack_switching {
                let entry = self
                    .b
                    .declare(&format!("$body{i}"), &[ValType::I64], &[ValType::I64]);
                self.b.declare_ref(entry);
                let k = self.k_type();
                let run = self.b.declare(
                    &format!("$run{i}"),
                    &[ValType::I64, k, ValType::I64],
                    &[ValType::I64],
                );
                let mut p = vec![ValType::I64; 2 + self.arity];
                p.push(k);
                let fx_op = self.b.declare(&format!("$fxop{i}"), &p, &[ValType::I64]);
                (Some(entry), Some(run), Some(fx_op))
            } else {
                (None, None, None)
            };
            let env_offset = F_ARGS + 8 * self.arity as u64;
            let mut offset = env_offset + 8 * self.env.len() as u64;
            let mut capture_offset = BTreeMap::new();
            for name in h
                .ret_captures
                .iter()
                .chain(h.ops.iter().flat_map(|c| c.captures.iter()))
            {
                if !capture_offset.contains_key(name) {
                    capture_offset.insert(name.clone(), offset);
                    offset += 8;
                }
            }
            let body_offset = offset;
            offset += 8 * self.program.functions[&h.body].params.len() as u64;
            self.hfns.push(HandlerFns {
                dispatch_slot,
                dispatch,
                ret,
                abort,
                body_entry,
                run,
                fx_op,
                frame_size: offset,
                env_offset,
                capture_offset,
                body_offset,
            });
        }
        Ok(())
    }

    pub(crate) fn define_all(&mut self) -> Result<(), WasmError> {
        self.define_alloc();
        for name in self.plan.functions.clone() {
            let body = Emit::function(self, &name)?;
            let idx = self.funcs[&name];
            self.b.define(idx, body);
        }
        for i in 0..self.plan.handlers.len() {
            self.define_handler(i)?;
        }
        self.flush_helpers()
    }

    pub(crate) fn flush_helpers(&mut self) -> Result<(), WasmError> {
        while let Some((idx, ty)) = self.eq_pending.pop() {
            let body = self.eq_body(&ty)?;
            self.b.define(idx, body);
        }
        Ok(())
    }

    fn define_alloc(&mut self) {
        // $alloc(size) -> pointer: 8-byte aligned bump allocation that grows
        // memory on demand and traps when the configured maximum is reached.
        let mut f = Function::new([(2, ValType::I64)]);
        let (size, p, end) = (0, 1, 2);
        for ins in [
            I::GlobalGet(self.heap),
            I::LocalTee(p),
            I::LocalGet(size),
            I::I64Const(7),
            I::I64Add,
            I::I64Const(-8),
            I::I64And,
            I::I64Add,
            I::LocalTee(end),
            I::MemorySize(0),
            I::I64ExtendI32U,
            I::I64Const(16),
            I::I64Shl,
            I::I64GtU,
            I::If(BlockType::Empty),
            I::LocalGet(end),
            I::MemorySize(0),
            I::I64ExtendI32U,
            I::I64Const(16),
            I::I64Shl,
            I::I64Sub,
            I::I64Const(65535),
            I::I64Add,
            I::I64Const(16),
            I::I64ShrU,
            I::I32WrapI64,
            I::MemoryGrow(0),
            I::I32Const(-1),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::Unreachable,
            I::End,
            I::End,
            I::LocalGet(end),
            I::GlobalSet(self.heap),
            I::LocalGet(p),
            I::End,
        ] {
            f.instruction(&ins);
        }
        self.b.define(self.alloc, f);
    }

    /// The structural-equality helper for `ty`, generated on first use.
    pub(crate) fn eq_fn(&mut self, ty: &Type) -> Result<u32, WasmError> {
        let ty = crate::numeric::default_type(ty);
        let key = ty.to_string();
        if let Some(i) = self.eq.get(&key) {
            return Ok(*i);
        }
        match layout::shape(self.world, &ty)? {
            Shape::Opaque | Shape::Function => {
                return Err(WasmError::unsupported(
                    Unsupported::GenericOperation,
                    "",
                    format!("equality on {ty}"),
                ));
            }
            _ => {}
        }
        let idx = self
            .b
            .declare(&format!("$eq<{key}>"), &[ValType::I64; 2], &[ValType::I64]);
        self.eq.insert(key, idx);
        self.eq_pending.push((idx, ty));
        Ok(idx)
    }

    fn eq_body(&mut self, ty: &Type) -> Result<Function, WasmError> {
        let mut code: Vec<I<'static>> = vec![];
        let (a, b, tag) = (0, 1, 2);
        // Field i of both operands is equal (leaves an i64 0/1).
        let field = |l: &mut Self,
                     code: &mut Vec<I<'static>>,
                     off: u64,
                     t: &Type|
         -> Result<(), WasmError> {
            let f = l.eq_fn(t)?;
            code.extend([
                I::LocalGet(a),
                I::I32WrapI64,
                I::I64Load(mem(off)),
                I::LocalGet(b),
                I::I32WrapI64,
                I::I64Load(mem(off)),
                I::Call(f),
            ]);
            Ok(())
        };
        match layout::shape(self.world, ty)? {
            Shape::Scalar(Scalar::F64) => code.extend([
                I::LocalGet(a),
                I::F64ReinterpretI64,
                I::LocalGet(b),
                I::F64ReinterpretI64,
                I::F64Eq,
                I::I64ExtendI32U,
            ]),
            Shape::Scalar(_) => {
                code.extend([I::LocalGet(a), I::LocalGet(b), I::I64Eq, I::I64ExtendI32U])
            }
            Shape::Tuple(ts) => {
                code.push(I::I64Const(1));
                for (i, t) in ts.iter().enumerate() {
                    field(self, &mut code, 8 * i as u64, t)?;
                    code.push(I::I64And);
                }
            }
            Shape::Struct { fields, .. } => {
                code.push(I::I64Const(1));
                for (i, (_, t)) in fields.iter().enumerate() {
                    field(self, &mut code, 8 * i as u64, t)?;
                    code.push(I::I64And);
                }
            }
            Shape::Enum { variants, .. } => {
                code.extend([
                    I::LocalGet(a),
                    I::I32WrapI64,
                    I::I64Load(mem(0)),
                    I::LocalTee(tag),
                    I::LocalGet(b),
                    I::I32WrapI64,
                    I::I64Load(mem(0)),
                    I::I64Ne,
                    I::If(BlockType::Empty),
                    I::I64Const(0),
                    I::Return,
                    I::End,
                ]);
                for (vi, v) in variants.iter().enumerate() {
                    code.extend([
                        I::LocalGet(tag),
                        I::I64Const(vi as i64),
                        I::I64Eq,
                        I::If(BlockType::Empty),
                        I::I64Const(1),
                    ]);
                    for (fi, (_, t)) in v.fields.iter().enumerate() {
                        field(self, &mut code, layout::variant_offset(true, fi), t)?;
                        code.push(I::I64And);
                    }
                    code.extend([I::Return, I::End]);
                }
                code.push(I::Unreachable);
            }
            Shape::Opaque | Shape::Function => {
                return Err(WasmError::unsupported(
                    Unsupported::GenericOperation,
                    "",
                    format!("equality on {ty}"),
                ));
            }
        }
        code.push(I::End);
        let mut f = Function::new([(1, ValType::I64)]);
        for ins in &code {
            f.instruction(ins);
        }
        Ok(f)
    }

    // ---- handler support functions -------------------------------------

    fn define_handler(&mut self, i: usize) -> Result<(), WasmError> {
        let h = self.plan.handlers[i].clone();
        self.define_dispatch(i, &h)?;
        self.define_ret(i, &h)?;
        self.define_abort(i, &h)?;
        if h.stack_switching {
            self.define_body_entry(i, &h)?;
            self.define_run(i, &h)?;
            self.define_fx_op(i, &h)?;
        }
        Ok(())
    }

    fn save_env(&self, code: &mut Vec<I<'static>>, first_local: u32) {
        for (n, g) in self.env.values().enumerate() {
            code.extend([I::GlobalGet(*g), I::LocalSet(first_local + n as u32)]);
        }
    }
    fn restore_env_locals(&self, code: &mut Vec<I<'static>>, first_local: u32) {
        for (n, g) in self.env.values().enumerate() {
            code.extend([I::LocalGet(first_local + n as u32), I::GlobalSet(*g)]);
        }
    }
    /// env := frame snapshot.
    pub(crate) fn restore_env_frame(&self, code: &mut Vec<I<'static>>, frame: u32, h: usize) {
        let base = self.hfns[h].env_offset;
        for (n, g) in self.env.values().enumerate() {
            code.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(base + 8 * n as u64)),
                I::GlobalSet(*g),
            ]);
        }
    }
    pub(crate) fn install_keys(&self, code: &mut Vec<I<'static>>, frame: u32, h: &Handler) {
        for k in &h.keys {
            code.extend([I::LocalGet(frame), I::GlobalSet(self.env[k])]);
        }
    }

    fn capture_values(&self, h: usize, names: &[String], frame: u32, out: &mut Vec<I<'static>>) {
        for n in names {
            out.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(self.hfns[h].capture_offset[n])),
            ]);
        }
    }

    fn finish_fn(code: Vec<I<'static>>, locals: Vec<ValType>) -> Function {
        let mut f = Function::new(locals.into_iter().map(|t| (1, t)));
        for ins in &code {
            f.instruction(ins);
        }
        f
    }

    /// Push the flattened parameters `pattern` binds in `value` (a local of
    /// type `ty`), in `ir::pattern_params` order, allocating temporaries.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn flatten(
        &mut self,
        code: &mut Vec<I<'static>>,
        locals: &mut Vec<ValType>,
        nparams: u32,
        p: &Pattern,
        value: u32,
        ty: &Type,
        count: &mut usize,
    ) -> Result<(), WasmError> {
        match &p.kind {
            PatternKind::Bind(_) => {
                code.push(I::LocalGet(value));
                *count += 1;
            }
            PatternKind::Tuple(ps) => {
                let Type::Tuple(ts) = crate::numeric::default_type(ty) else {
                    return Ok(());
                };
                for (i, (p, t)) in ps.iter().zip(&ts).enumerate() {
                    let tmp = nparams + locals.len() as u32;
                    locals.push(ValType::I64);
                    code.extend([
                        I::LocalGet(value),
                        I::I32WrapI64,
                        I::I64Load(mem(8 * i as u64)),
                        I::LocalSet(tmp),
                    ]);
                    self.flatten(code, locals, nparams, p, tmp, t, count)?;
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                let (_, v, is_enum) = layout::variant(self.world, ty, name)?;
                for (n, p) in fields {
                    if let Some(fi) = v.fields.iter().position(|(f, _)| f == n) {
                        let tmp = nparams + locals.len() as u32;
                        locals.push(ValType::I64);
                        code.extend([
                            I::LocalGet(value),
                            I::I32WrapI64,
                            I::I64Load(mem(layout::variant_offset(is_enum, fi))),
                            I::LocalSet(tmp),
                        ]);
                        let t = v.fields[fi].1.clone();
                        self.flatten(code, locals, nparams, p, tmp, &t, count)?;
                    }
                }
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
        Ok(())
    }

    fn expect_params(&self, function: &str, pushed: usize) -> Result<(), WasmError> {
        let want = self.signature(function).len();
        if pushed != want {
            return Err(WasmError::Invalid(format!(
                "handler call to {function} passes {pushed} values for {want} parameters"
            )));
        }
        Ok(())
    }

    /// $ret_i(frame, r): the return clause.
    fn define_ret(&mut self, i: usize, h: &Handler) -> Result<(), WasmError> {
        let (frame, r) = (0, 1);
        let mut code = vec![];
        let mut locals = vec![];
        self.capture_values(i, &h.ret_captures, frame, &mut code);
        let mut count = h.ret_captures.len();
        let (value, ty) = if let Some(state) = &h.state {
            let t = 2 + locals.len() as u32;
            locals.push(ValType::I64);
            code.extend([
                I::I64Const(16),
                I::Call(self.alloc),
                I::LocalTee(t),
                I::I32WrapI64,
                I::LocalGet(r),
                I::I64Store(mem(0)),
                I::LocalGet(t),
                I::I32WrapI64,
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(F_STATE)),
                I::I64Store(mem(8)),
            ]);
            (t, Type::Tuple(vec![h.body_type.clone(), state.clone()]))
        } else {
            (r, h.body_type.clone())
        };
        let pattern = h.ret_pattern.clone();
        self.flatten(&mut code, &mut locals, 2, &pattern, value, &ty, &mut count)?;
        self.expect_params(&h.ret, count)?;
        code.extend([I::Call(self.funcs[&h.ret]), I::End]);
        let idx = self.hfns[i].ret;
        self.b.define(idx, Self::finish_fn(code, locals));
        Ok(())
    }

    /// Push captures, flattened arguments (from `arg(j)` locals) and state
    /// for operation clause `c`, then call it with `hidden` extra values.
    #[allow(clippy::too_many_arguments)]
    fn call_clause(
        &mut self,
        code: &mut Vec<I<'static>>,
        locals: &mut Vec<ValType>,
        nparams: u32,
        i: usize,
        c: &super::analysis::OpClause,
        frame: u32,
        args: &[u32],
        hidden: &[u32],
    ) -> Result<(), WasmError> {
        let h = self.plan.handlers[i].clone();
        self.capture_values(i, &c.captures, frame, code);
        let mut count = c.captures.len();
        for (j, (p, t)) in c.arg_patterns.iter().zip(&c.arg_types).enumerate() {
            self.flatten(code, locals, nparams, p, args[j], t, &mut count)?;
        }
        if let (Some(p), Some(t)) = (&c.state_pattern, &h.state) {
            let s = nparams + locals.len() as u32;
            locals.push(ValType::I64);
            code.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(F_STATE)),
                I::LocalSet(s),
            ]);
            self.flatten(code, locals, nparams, p, s, t, &mut count)?;
        }
        for x in hidden {
            code.push(I::LocalGet(*x));
            count += 1;
        }
        self.expect_params(&c.function, count)?;
        code.push(I::Call(self.funcs[&c.function]));
        Ok(())
    }

    /// $dispatch_i(frame, op, a0..): perform an operation on this handler.
    fn define_dispatch(&mut self, i: usize, h: &Handler) -> Result<(), WasmError> {
        let (frame, op) = (0u32, 1u32);
        let nparams = 2 + self.arity as u32;
        let args: Vec<u32> = (0..self.arity as u32).map(|j| 2 + j).collect();
        let mut locals = vec![ValType::I64; self.env.len()];
        let env_save = nparams;
        let mut code = vec![];
        for c in &h.ops {
            code.extend([
                I::LocalGet(op),
                I::I64Const(c.op as i64),
                I::I64Eq,
                I::If(BlockType::Empty),
            ]);
            match c.mode {
                HandlerMode::Abort => {
                    code.extend([
                        I::LocalGet(frame),
                        I::I32WrapI64,
                        I::LocalGet(op),
                        I::I64Store(mem(F_ABORT_OP)),
                    ]);
                    for (j, a) in args.iter().enumerate() {
                        code.extend([
                            I::LocalGet(frame),
                            I::I32WrapI64,
                            I::LocalGet(*a),
                            I::I64Store(mem(F_ARGS + 8 * j as u64)),
                        ]);
                    }
                    code.extend([
                        I::LocalGet(frame),
                        I::GlobalSet(self.abort),
                        I::I64Const(0),
                        I::Return,
                    ]);
                }
                HandlerMode::Tail => {
                    self.save_env(&mut code, env_save);
                    self.restore_env_frame(&mut code, frame, i);
                    self.call_clause(
                        &mut code,
                        &mut locals,
                        nparams,
                        i,
                        c,
                        frame,
                        &args,
                        &[frame],
                    )?;
                    self.restore_env_locals(&mut code, env_save);
                    code.push(I::Return);
                }
                HandlerMode::StackSwitching => {
                    let fx = self.fx.as_ref().ok_or_else(|| {
                        WasmError::Invalid("stack switching types missing".into())
                    })?;
                    let tag = fx.tag;
                    self.save_env(&mut code, env_save);
                    code.extend([I::LocalGet(frame), I::LocalGet(op)]);
                    for a in &args {
                        code.push(I::LocalGet(*a));
                    }
                    code.push(I::Suspend(tag));
                    self.restore_env_locals(&mut code, env_save);
                    code.push(I::Return);
                }
            }
            code.push(I::End);
        }
        code.extend([I::Unreachable, I::End]);
        let idx = self.hfns[i].dispatch;
        self.b.define(idx, Self::finish_fn(code, locals));
        Ok(())
    }

    /// $abort_i(frame): run the aborting clause recorded in the frame.
    fn define_abort(&mut self, i: usize, h: &Handler) -> Result<(), WasmError> {
        let frame = 0u32;
        let nparams = 1;
        let mut locals = vec![ValType::I64; self.arity];
        let args: Vec<u32> = (0..self.arity as u32).map(|j| 1 + j).collect();
        let mut code = vec![];
        for (j, a) in args.iter().enumerate() {
            code.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(F_ARGS + 8 * j as u64)),
                I::LocalSet(*a),
            ]);
        }
        for c in h.ops.iter().filter(|c| c.mode != HandlerMode::Tail) {
            if c.mode == HandlerMode::StackSwitching {
                continue;
            }
            code.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(F_ABORT_OP)),
                I::I64Const(c.op as i64),
                I::I64Eq,
                I::If(BlockType::Empty),
            ]);
            self.call_clause(&mut code, &mut locals, nparams, i, c, frame, &args, &[])?;
            code.extend([I::Return, I::End]);
        }
        code.extend([I::Unreachable, I::End]);
        let idx = self.hfns[i].abort;
        self.b.define(idx, Self::finish_fn(code, locals));
        Ok(())
    }

    /// $body_i(frame): the body of a stack-switching handler, run as a
    /// continuation.
    fn define_body_entry(&mut self, i: usize, h: &Handler) -> Result<(), WasmError> {
        let frame = 0;
        let mut code = vec![];
        self.install_keys(&mut code, frame, h);
        let n = self.program.functions[&h.body].params.len();
        for j in 0..n {
            code.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::I64Load(mem(self.hfns[i].body_offset + 8 * j as u64)),
            ]);
        }
        code.extend([I::Call(self.funcs[&h.body]), I::End]);
        let idx = self.hfns[i].body_entry.expect("stack-switching handler");
        self.b.define(idx, Self::finish_fn(code, vec![]));
        Ok(())
    }

    /// $run_i(frame, k, v): resume `k` with `v` under this handler until the
    /// body returns, an operation of this handler suspends it, or it aborts.
    fn define_run(&mut self, i: usize, _h: &Handler) -> Result<(), WasmError> {
        let fx = self.fx.as_ref().expect("stack switching types");
        let (ct, tag, on_block) = (fx.ct, fx.tag, fx.on_block);
        let (frame, k, v) = (0u32, 1u32, 2u32);
        let nparams = 3;
        let mut locals = vec![ValType::I64; 2 + self.arity];
        let f2 = nparams;
        let op = nparams + 1;
        let args: Vec<u32> = (0..self.arity as u32).map(|j| nparams + 2 + j).collect();
        let r = nparams + locals.len() as u32;
        locals.push(ValType::I64);
        let k2 = nparams + locals.len() as u32;
        locals.push(self.k_type());
        let mut code = vec![
            I::Loop(BlockType::Empty),
            I::Block(BlockType::FunctionType(on_block)),
            I::LocalGet(v),
            I::LocalGet(k),
            I::Resume {
                cont_type_index: ct,
                resume_table: std::borrow::Cow::Owned(vec![Handle::OnLabel { tag, label: 0 }]),
            },
            I::LocalSet(r),
        ];
        // The body returned (normally or by abort).
        self.restore_env_frame(&mut code, frame, i);
        code.extend([
            I::GlobalGet(self.abort),
            I::LocalGet(frame),
            I::I64Eq,
            I::If(BlockType::Empty),
            I::I64Const(0),
            I::GlobalSet(self.abort),
            I::LocalGet(frame),
            I::ReturnCall(self.hfns[i].abort),
            I::End,
            I::GlobalGet(self.abort),
            I::I64Const(0),
            I::I64Ne,
            I::If(BlockType::Empty),
            I::I64Const(0),
            I::Return,
            I::End,
            I::LocalGet(frame),
            I::LocalGet(r),
            I::ReturnCall(self.hfns[i].ret),
            I::End,
        ]);
        // Suspended: [frame', op, args..., k'].
        code.push(I::LocalSet(k2));
        for a in args.iter().rev() {
            code.push(I::LocalSet(*a));
        }
        code.extend([I::LocalSet(op), I::LocalSet(f2)]);
        code.extend([
            I::LocalGet(f2),
            I::LocalGet(frame),
            I::I64Eq,
            I::If(BlockType::Empty),
        ]);
        self.restore_env_frame(&mut code, frame, i);
        code.extend([I::LocalGet(frame), I::LocalGet(op)]);
        for a in &args {
            code.push(I::LocalGet(*a));
        }
        code.extend([
            I::LocalGet(k2),
            I::ReturnCall(self.hfns[i].fx_op.expect("fx op")),
            I::End,
        ]);
        // Another handler's operation: forward it outward, then resume.
        code.extend([I::LocalGet(f2), I::LocalGet(op)]);
        for a in &args {
            code.push(I::LocalGet(*a));
        }
        code.extend([
            I::Suspend(tag),
            I::LocalSet(v),
            I::LocalGet(k2),
            I::LocalSet(k),
            I::Br(0),
            I::End,
            I::Unreachable,
            I::End,
        ]);
        let idx = self.hfns[i].run.expect("run");
        self.b.define(idx, Self::finish_fn(code, locals));
        Ok(())
    }

    /// $fxop_i(frame, op, a0.., k): run a stack-switching operation clause.
    fn define_fx_op(&mut self, i: usize, h: &Handler) -> Result<(), WasmError> {
        let frame = 0u32;
        let op = 1u32;
        let args: Vec<u32> = (0..self.arity as u32).map(|j| 2 + j).collect();
        let k = 2 + self.arity as u32;
        let nparams = k + 1;
        let mut locals = vec![];
        let mut code = vec![];
        for c in h
            .ops
            .iter()
            .filter(|c| c.mode == HandlerMode::StackSwitching)
        {
            code.extend([
                I::LocalGet(op),
                I::I64Const(c.op as i64),
                I::I64Eq,
                I::If(BlockType::Empty),
            ]);
            self.call_clause(
                &mut code,
                &mut locals,
                nparams,
                i,
                c,
                frame,
                &args,
                &[frame, k],
            )?;
            code.extend([I::Return, I::End]);
        }
        code.extend([I::Unreachable, I::End]);
        let idx = self.hfns[i].fx_op.expect("fx op");
        self.b.define(idx, Self::finish_fn(code, locals));
        Ok(())
    }
}

/// One function body under construction.
struct Emit<'l, 'a> {
    l: &'l mut Lowerer<'a>,
    f: &'a FunctionIr,
    name: String,
    code: Vec<I<'static>>,
    locals: Vec<ValType>,
    nparams: u32,
    /// Named locals: (Wasm local, is a cell address).
    names: BTreeMap<String, (u32, bool)>,
    values: BTreeMap<ValueId, u32>,
    resume: Option<String>,
    resume_reads: BTreeSet<ValueId>,
    frame: Option<u32>,
    k: Option<u32>,
    clause_handler: Option<usize>,
    labels: BTreeSet<String>,
    generics: BTreeSet<String>,
}

impl<'l, 'a> Emit<'l, 'a> {
    fn function(l: &'l mut Lowerer<'a>, name: &str) -> Result<Function, WasmError> {
        let f = &l.program.functions[name];
        let role = l.plan.roles[name].clone();
        let mut e = Emit {
            f,
            name: name.to_owned(),
            code: vec![],
            locals: vec![],
            nparams: 0,
            names: BTreeMap::new(),
            values: BTreeMap::new(),
            resume: None,
            resume_reads: BTreeSet::new(),
            frame: None,
            k: None,
            clause_handler: None,
            labels: l.plan.labels.get(name).cloned().unwrap_or_default(),
            generics: l
                .world
                .functions
                .get(name)
                .map(|w| w.generics.iter().map(|g| g.name.clone()).collect())
                .unwrap_or_default(),
            l,
        };
        let mut mode = None;
        if let Role::OpClause { handler, op } = &role {
            let c = e.l.plan.handlers[*handler]
                .ops
                .iter()
                .find(|c| c.op == *op)
                .expect("planned clause")
                .clone();
            e.resume = c.resume.clone();
            e.clause_handler = Some(*handler);
            mode = Some(c.mode);
        }
        let mut index = 0;
        for p in &f.params {
            if Some(&p.name) == e.resume.as_ref() {
                continue;
            }
            let cell = p.pass == Pass::Inout || role == Role::HandlerBody;
            e.names.insert(p.name.clone(), (index, cell));
            index += 1;
        }
        match mode {
            Some(HandlerMode::Tail) => {
                e.frame = Some(index);
                index += 1;
            }
            Some(HandlerMode::StackSwitching) => {
                e.frame = Some(index);
                e.k = Some(index + 1);
                index += 2;
            }
            _ => {}
        }
        e.nparams = index;
        for name in f.locals.keys() {
            if !e.names.contains_key(name) && Some(name) != e.resume.as_ref() {
                let x = e.local();
                e.names.insert(name.clone(), (x, false));
            }
        }
        e.body()?;
        let mut func = Function::new(e.locals.iter().map(|t| (1, *t)));
        e.l.instructions += e.code.len();
        if e.l.instructions > e.l.max_instructions {
            return Err(WasmError::Limit(format!(
                "more than {} Wasm instructions",
                e.l.max_instructions
            )));
        }
        for ins in &e.code {
            func.instruction(ins);
        }
        Ok(func)
    }

    fn err(&self, construct: Unsupported, detail: impl Into<String>) -> WasmError {
        WasmError::unsupported(construct, &self.name, detail)
    }
    fn local(&mut self) -> u32 {
        self.locals.push(ValType::I64);
        self.nparams + self.locals.len() as u32 - 1
    }
    fn push(&mut self, i: I<'static>) {
        self.code.push(i);
    }
    fn get(&mut self, x: u32) {
        self.code.push(I::LocalGet(x));
    }
    fn value(&mut self, v: ValueId) -> Result<u32, WasmError> {
        self.values.get(&v).copied().ok_or_else(|| {
            WasmError::Invalid(format!(
                "{}: IR value %{v} used before definition",
                self.name
            ))
        })
    }
    fn dest(&mut self, v: ValueId) -> u32 {
        if let Some(x) = self.values.get(&v) {
            return *x;
        }
        let x = self.local();
        self.values.insert(v, x);
        x
    }
    fn vty(&self, v: ValueId) -> Type {
        crate::numeric::default_type(self.f.values.get(&v).unwrap_or(&Type::Infer))
    }
    /// Push the 8-byte slot at `ptr + off`.
    fn load(&mut self, ptr: u32, off: u64) {
        self.code
            .extend([I::LocalGet(ptr), I::I32WrapI64, I::I64Load(mem(off))]);
    }
    /// Store local `v` into the slot at `ptr + off`.
    fn store(&mut self, ptr: u32, off: u64, v: u32) {
        self.code.extend([
            I::LocalGet(ptr),
            I::I32WrapI64,
            I::LocalGet(v),
            I::I64Store(mem(off)),
        ]);
    }
    /// A new block of `bytes` bytes in local `x`.
    fn alloc(&mut self, bytes: u64) -> u32 {
        let x = self.local();
        self.code.extend([
            I::I64Const(bytes as i64),
            I::Call(self.l.alloc),
            I::LocalSet(x),
        ]);
        x
    }
    /// `if abort { return 0 }`.
    fn abort_check(&mut self) {
        self.code.extend([
            I::GlobalGet(self.l.abort),
            I::I64Const(0),
            I::I64Ne,
            I::If(BlockType::Empty),
            I::I64Const(0),
            I::Return,
            I::End,
        ]);
    }

    fn name_slot(&self, n: &str) -> Result<(u32, bool), WasmError> {
        self.names
            .get(n)
            .copied()
            .ok_or_else(|| WasmError::Invalid(format!("{}: unknown local '{n}'", self.name)))
    }
    fn local_type(&self, n: &str) -> Type {
        crate::numeric::default_type(self.f.locals.get(n).unwrap_or(&Type::Infer))
    }
    /// The current value of named local `n` into a fresh local.
    fn read_name(&mut self, n: &str) -> Result<u32, WasmError> {
        let (x, cell) = self.name_slot(n)?;
        let out = self.local();
        if cell {
            self.load(x, 0);
        } else {
            self.get(x);
        }
        self.push(I::LocalSet(out));
        Ok(out)
    }
    fn write_name(&mut self, n: &str, v: u32) -> Result<(), WasmError> {
        let (x, cell) = self.name_slot(n)?;
        if cell {
            self.store(x, 0, v);
        } else {
            self.code.extend([I::LocalGet(v), I::LocalSet(x)]);
        }
        Ok(())
    }
    /// Read `root.path`, returning (local, type).
    fn read_place(&mut self, root: &str, path: &[String]) -> Result<(u32, Type), WasmError> {
        let mut v = self.read_name(root)?;
        let mut ty = self.local_type(root);
        for seg in path {
            let (off, t) = layout::field(self.l.world, &ty, seg).map_err(|e| e.at(&self.name))?;
            let out = self.local();
            self.load(v, off);
            self.push(I::LocalSet(out));
            v = out;
            ty = t;
        }
        Ok((v, ty))
    }
    /// `root.path = v` with copy-on-write of every enclosing block.
    fn write_place(&mut self, root: &str, path: &[String], v: u32) -> Result<(), WasmError> {
        if path.is_empty() {
            return self.write_name(root, v);
        }
        let base = self.read_name(root)?;
        let ty = self.local_type(root);
        let new = self.cow(base, &ty, path, v)?;
        self.write_name(root, new)
    }
    fn cow(&mut self, base: u32, ty: &Type, path: &[String], v: u32) -> Result<u32, WasmError> {
        let n = layout::slots(self.l.world, ty).map_err(|e| e.at(&self.name))?;
        let (off, fty) = layout::field(self.l.world, ty, &path[0]).map_err(|e| e.at(&self.name))?;
        let copy = self.alloc(8 * n as u64);
        for i in 0..n as u64 {
            self.code.extend([I::LocalGet(copy), I::I32WrapI64]);
            self.load(base, 8 * i);
            self.push(I::I64Store(mem(8 * i)));
        }
        let inner = if path.len() == 1 {
            v
        } else {
            let old = self.local();
            self.load(base, off);
            self.push(I::LocalSet(old));
            self.cow(old, &fty, &path[1..], v)?
        };
        self.store(copy, off, inner);
        Ok(copy)
    }

    fn body(&mut self) -> Result<(), WasmError> {
        let f = self.f;
        if f.blocks.len() > 4096 {
            return Err(WasmError::Limit(format!(
                "{}: more than 4096 blocks",
                self.name
            )));
        }
        let order = topological(f).ok_or_else(|| {
            WasmError::Invalid(format!("{}: Place IR block graph is cyclic", self.name))
        })?;
        let position: BTreeMap<usize, usize> =
            order.iter().enumerate().map(|(p, b)| (*b, p)).collect();
        for _ in 1..order.len() {
            self.push(I::Block(BlockType::Empty));
        }
        for (pos, &bi) in order.iter().enumerate() {
            if pos > 0 {
                self.push(I::End);
            }
            let block = &f.blocks[bi];
            let tail_call = block.instructions.last().is_some_and(|last| {
                self.tail_callable(&last.op)
                    && super::analysis::flows_to_return(f, bi, last.dest, 0)
            });
            let n = block.instructions.len();
            for (ii, inst) in block.instructions.iter().enumerate() {
                if tail_call && ii + 1 == n {
                    let Op::Call { callee, args, .. } = &inst.op else {
                        unreachable!()
                    };
                    for a in args {
                        let x = self.value(a.value)?;
                        self.get(x);
                    }
                    self.push(I::ReturnCall(self.l.funcs[callee]));
                    continue;
                }
                self.instruction(inst)?;
            }
            if tail_call {
                continue;
            }
            let depth = |target: usize| -> Result<u32, WasmError> {
                let p = position[&target];
                if p <= pos {
                    return Err(WasmError::Invalid("backward Place IR edge".into()));
                }
                Ok((p - pos - 1) as u32)
            };
            match &block.terminator {
                Terminator::Return(v) => {
                    let x = self.value(*v)?;
                    self.get(x);
                    self.push(I::Return);
                }
                Terminator::Jump { block: t, args } => {
                    for (a, p) in args.iter().zip(&f.blocks[*t].params) {
                        let x = self.value(*a)?;
                        let y = self.dest(*p);
                        self.code.extend([I::LocalGet(x), I::LocalSet(y)]);
                    }
                    let d = depth(*t)?;
                    self.push(I::Br(d));
                }
                Terminator::Branch { condition, yes, no } => {
                    let c = self.value(*condition)?;
                    let (dy, dn) = (depth(*yes)?, depth(*no)?);
                    self.code
                        .extend([I::LocalGet(c), I::I32WrapI64, I::BrIf(dy), I::Br(dn)]);
                }
                Terminator::Match { value, arms } => {
                    let v = self.value(*value)?;
                    let ty = self.vty(*value);
                    for (p, t) in arms {
                        self.condition(p, v, &ty)?;
                        let d = depth(*t)?;
                        self.push(I::BrIf(d));
                    }
                    self.push(I::Unreachable);
                }
                Terminator::Unreachable => self.push(I::Unreachable),
            }
        }
        self.push(I::End);
        Ok(())
    }

    fn tail_callable(&self, op: &Op) -> bool {
        matches!(op, Op::Call { callee, args, .. }
            if self.l.funcs.contains_key(callee)
                && self.l.plan.roles.get(callee) == Some(&Role::Ordinary)
                && args.iter().all(|a| a.pass != Pass::Inout)
                && self.l.world.functions.get(callee).is_some_and(|w| w.params.iter().all(|p| p.pass != Pass::Inout)))
    }

    /// Push an i32 truth value: does `p` match the value in local `v`?
    fn condition(&mut self, p: &Pattern, v: u32, ty: &Type) -> Result<(), WasmError> {
        match &p.kind {
            PatternKind::Wild | PatternKind::Bind(_) => self.push(I::I32Const(1)),
            PatternKind::Literal(l) => {
                let s = layout::shape(self.l.world, ty)?.scalar();
                self.get(v);
                match (l, s) {
                    (Literal::Float(x), _) => {
                        self.code
                            .extend([I::F64ReinterpretI64, I::F64Const((*x).into()), I::F64Eq])
                    }
                    (Literal::String(_), _) => {
                        return Err(self.err(Unsupported::String, "string literal pattern"));
                    }
                    _ => {
                        let c = self.constant_bits(l, ty)?;
                        self.code.extend([I::I64Const(c), I::I64Eq]);
                    }
                }
            }
            PatternKind::Tuple(ps) => {
                let Type::Tuple(ts) = crate::numeric::default_type(ty) else {
                    return Err(WasmError::Invalid(format!("tuple pattern on {ty}")));
                };
                self.push(I::I32Const(1));
                for (i, (p, t)) in ps.iter().zip(&ts).enumerate() {
                    let x = self.local();
                    self.load(v, 8 * i as u64);
                    self.push(I::LocalSet(x));
                    self.condition(p, x, t)?;
                    self.push(I::I32And);
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                let (index, variant, is_enum) = layout::variant(self.l.world, ty, name)?;
                if is_enum {
                    self.load(v, 0);
                    self.code.extend([
                        I::I64Const(index as i64),
                        I::I64Eq,
                        I::If(BlockType::Result(ValType::I32)),
                    ]);
                }
                self.push(I::I32Const(1));
                for (n, p) in fields {
                    let fi = variant
                        .fields
                        .iter()
                        .position(|(f, _)| f == n)
                        .ok_or_else(|| WasmError::Invalid(format!("{name} has no field {n}")))?;
                    let x = self.local();
                    self.load(v, layout::variant_offset(is_enum, fi));
                    self.push(I::LocalSet(x));
                    let t = variant.fields[fi].1.clone();
                    self.condition(p, x, &t)?;
                    self.push(I::I32And);
                }
                if is_enum {
                    self.code.extend([I::Else, I::I32Const(0), I::End]);
                }
            }
        }
        Ok(())
    }

    /// Destructure local `v` of type `ty` into the names `p` binds.
    fn bind(&mut self, p: &Pattern, v: u32, ty: &Type) -> Result<(), WasmError> {
        match &p.kind {
            PatternKind::Bind(n) => self.write_name(n, v)?,
            PatternKind::Tuple(ps) => {
                let Type::Tuple(ts) = crate::numeric::default_type(ty) else {
                    return Err(WasmError::Invalid(format!("tuple pattern on {ty}")));
                };
                for (i, (p, t)) in ps.iter().zip(&ts).enumerate() {
                    let x = self.local();
                    self.load(v, 8 * i as u64);
                    self.push(I::LocalSet(x));
                    self.bind(p, x, t)?;
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                let (_, variant, is_enum) = layout::variant(self.l.world, ty, name)?;
                for (n, p) in fields {
                    let fi = variant
                        .fields
                        .iter()
                        .position(|(f, _)| f == n)
                        .ok_or_else(|| WasmError::Invalid(format!("{name} has no field {n}")))?;
                    let x = self.local();
                    self.load(v, layout::variant_offset(is_enum, fi));
                    self.push(I::LocalSet(x));
                    let t = variant.fields[fi].1.clone();
                    self.bind(p, x, &t)?;
                }
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
        Ok(())
    }

    fn constant_bits(&self, l: &Literal, ty: &Type) -> Result<i64, WasmError> {
        Ok(match l {
            Literal::Int(n) => {
                let kind = IntegerKind::from_type(ty).unwrap_or(IntegerKind::default_for(*n));
                if !kind.fits(*n) {
                    return Err(WasmError::Invalid(format!("literal {n} does not fit {ty}")));
                }
                *n as i64
            }
            Literal::Bool(b) => i64::from(*b),
            Literal::Float(x) => x.to_bits() as i64,
            Literal::String(_) => return Err(self.err(Unsupported::String, "string literal")),
        })
    }

    fn scalar_of(&self, ty: &Type) -> Result<Scalar, WasmError> {
        match layout::shape(self.l.world, ty)? {
            Shape::Scalar(s) => Ok(s),
            _ => Err(self.err(
                Unsupported::GenericOperation,
                format!("primitive operation on {ty}"),
            )),
        }
    }

    fn instruction(&mut self, inst: &crate::ir::Instruction) -> Result<(), WasmError> {
        let ty = crate::numeric::default_type(&inst.ty);
        match &inst.op {
            Op::Constant(l) => {
                let c = self.constant_bits(l, &ty)?;
                let d = self.dest(inst.dest);
                self.code.extend([I::I64Const(c), I::LocalSet(d)]);
            }
            Op::Unit => {
                let d = self.dest(inst.dest);
                self.code.extend([I::I64Const(0), I::LocalSet(d)]);
            }
            Op::Read { root, path, .. } => {
                if Some(root) == self.resume.as_ref() {
                    self.resume_reads.insert(inst.dest);
                    return Ok(());
                }
                if !self.names.contains_key(root) {
                    return Err(if self.l.world.functions.contains_key(root) {
                        self.err(
                            Unsupported::FunctionValue,
                            format!("function '{root}' used as a value"),
                        )
                    } else {
                        WasmError::Invalid(format!("{}: unknown name '{root}'", self.name))
                    });
                }
                let (v, t) = self.read_place(root, path)?;
                if matches!(layout::shape(self.l.world, &t)?, Shape::Function) {
                    return Err(self.err(
                        Unsupported::FunctionValue,
                        format!("'{root}' has function type"),
                    ));
                }
                self.values.insert(inst.dest, v);
            }
            Op::Bind { pattern, value } => {
                let v = self.value(*value)?;
                let t = self.vty(*value);
                self.bind(pattern, v, &t)?;
                let d = self.dest(inst.dest);
                self.code.extend([I::I64Const(0), I::LocalSet(d)]);
            }
            Op::Write { place, op, value } => {
                let mut parts = place.split('.').map(str::to_owned);
                let root = parts.next().filter(|r| !r.is_empty()).ok_or_else(|| {
                    WasmError::Invalid(format!("{}: write without a place", self.name))
                })?;
                let path: Vec<String> = parts.collect();
                let v = self.value(*value)?;
                let new = if op == "=" {
                    v
                } else {
                    let base = op.strip_suffix('=').ok_or_else(|| {
                        self.err(
                            Unsupported::Operation,
                            format!("assignment operator '{op}'"),
                        )
                    })?;
                    let (old, t) = self.read_place(&root, &path)?;
                    let s = self.scalar_of(&t)?;
                    self.binary(base, old, v, s, s)?
                };
                self.write_place(&root, &path, new)?;
                let d = self.dest(inst.dest);
                self.code.extend([I::I64Const(0), I::LocalSet(d)]);
            }
            Op::Unary(op, v) => {
                let x = self.value(*v)?;
                let s = self.scalar_of(&self.vty(*v))?;
                let d = self.dest(inst.dest);
                match (op.as_str(), s) {
                    ("!", Scalar::Bool) => {
                        self.code
                            .extend([I::LocalGet(x), I::I64Eqz, I::I64ExtendI32U])
                    }
                    ("-", Scalar::F64) => self.code.extend([
                        I::LocalGet(x),
                        I::F64ReinterpretI64,
                        I::F64Neg,
                        I::I64ReinterpretF64,
                    ]),
                    ("-", Scalar::I64) => {
                        self.code.extend([
                            I::LocalGet(x),
                            I::I64Const(i64::MIN),
                            I::I64Eq,
                            I::If(BlockType::Empty),
                        ]);
                        self.raise_overflow()?;
                        self.push(I::End);
                        self.code
                            .extend([I::I64Const(0), I::LocalGet(x), I::I64Sub]);
                    }
                    ("-", Scalar::U64 | Scalar::U32 | Scalar::U8) => {
                        self.code.extend([
                            I::LocalGet(x),
                            I::I64Eqz,
                            I::I32Eqz,
                            I::If(BlockType::Empty),
                        ]);
                        self.raise_overflow()?;
                        self.push(I::End);
                        self.push(I::I64Const(0));
                    }
                    _ => {
                        return Err(
                            self.err(Unsupported::Operation, format!("unary '{op}' on {s:?}"))
                        );
                    }
                }
                self.push(I::LocalSet(d));
            }
            Op::Binary(op, a, b) => {
                let (x, y) = (self.value(*a)?, self.value(*b)?);
                let (sa, sb) = (self.vty(*a), self.vty(*b));
                let r = match (op.as_str(), layout::shape(self.l.world, &sa)?) {
                    ("==" | "!=", Shape::Tuple(_) | Shape::Struct { .. } | Shape::Enum { .. }) => {
                        let eq = self.l.eq_fn(&sa).map_err(|e| e.at(&self.name))?;
                        let d = self.local();
                        self.code
                            .extend([I::LocalGet(x), I::LocalGet(y), I::Call(eq)]);
                        if op == "!=" {
                            self.code.extend([I::I64Eqz, I::I64ExtendI32U]);
                        }
                        self.push(I::LocalSet(d));
                        d
                    }
                    _ => {
                        let s1 = self.scalar_of(&sa)?;
                        let s2 = self.scalar_of(&sb)?;
                        self.binary(op, x, y, s1, s2)?
                    }
                };
                self.values.insert(inst.dest, r);
            }
            Op::Tuple(ids) => {
                let block = self.alloc(8 * ids.len() as u64);
                for (i, id) in ids.iter().enumerate() {
                    let v = self.value(*id)?;
                    self.store(block, 8 * i as u64, v);
                }
                self.values.insert(inst.dest, block);
            }
            Op::Data(name, fields) => {
                let (index, variant, is_enum) =
                    layout::variant(self.l.world, &ty, name).map_err(|e| e.at(&self.name))?;
                let slots = variant.fields.len() as u64 + u64::from(is_enum);
                let block = if slots == 0 {
                    let x = self.local();
                    self.code.extend([I::I64Const(0), I::LocalSet(x)]);
                    x
                } else {
                    self.alloc(8 * slots)
                };
                if is_enum {
                    let t = self.local();
                    self.code
                        .extend([I::I64Const(index as i64), I::LocalSet(t)]);
                    self.store(block, 0, t);
                }
                for (n, id) in fields {
                    let fi = variant
                        .fields
                        .iter()
                        .position(|(f, _)| f == n)
                        .ok_or_else(|| WasmError::Invalid(format!("{name} has no field {n}")))?;
                    let v = self.value(*id)?;
                    self.store(block, layout::variant_offset(is_enum, fi), v);
                }
                self.values.insert(inst.dest, block);
            }
            Op::Project(base, field) => {
                let b = self.value(*base)?;
                let (off, _) = layout::field(self.l.world, &self.vty(*base), field)
                    .map_err(|e| e.at(&self.name))?;
                let d = self.dest(inst.dest);
                self.load(b, off);
                self.push(I::LocalSet(d));
            }
            Op::Index(base, index) => {
                let bt = self.vty(*base);
                let Type::Tuple(ts) = &bt else {
                    return Err(self.err(Unsupported::Index, format!("indexing {bt}")));
                };
                let b = self.value(*base)?;
                let i = self.value(*index)?;
                let n = ts.len() as i64;
                self.code.extend([
                    I::LocalGet(i),
                    I::I64Const(n),
                    I::I64GeU,
                    I::If(BlockType::Empty),
                ]);
                self.raise_runtime_error("IndexOutOfBounds")?;
                self.push(I::End);
                let d = self.dest(inst.dest);
                self.code.extend([
                    I::LocalGet(b),
                    I::LocalGet(i),
                    I::I64Const(3),
                    I::I64Shl,
                    I::I64Add,
                    I::I32WrapI64,
                    I::I64Load(mem(0)),
                    I::LocalSet(d),
                ]);
            }
            Op::Call {
                callee,
                args,
                types,
                capability,
            } => {
                if self.l.program.functions.contains_key(callee)
                    && self.l.funcs.contains_key(callee)
                {
                    self.direct_call(inst.dest, callee, args)?;
                } else if let Some((effect, op)) = callee.split_once("::")
                    && self.l.world.effects.contains_key(effect)
                {
                    let mut vals = vec![];
                    for a in args {
                        if a.pass == Pass::Inout {
                            return Err(self.err(Unsupported::InoutEffectArgument, callee.clone()));
                        }
                        vals.push(self.value(a.value)?);
                    }
                    let r = self.perform(effect, op, types, capability.as_deref(), &vals)?;
                    self.values.insert(inst.dest, r);
                } else {
                    return Err(
                        self.err(Unsupported::ExternalFunction, format!("call to '{callee}'"))
                    );
                }
            }
            Op::Indirect { callee, args } if self.resume_reads.contains(callee) => {
                self.resume_call(inst.dest, args)?;
            }
            Op::Try(v) => {
                let x = self.value(*v)?;
                let t = self.vty(*v);
                let err_ty = match &t {
                    Type::Named(n, a) if n == "Result" && a.len() == 2 => a[1].clone(),
                    _ => return Err(self.err(Unsupported::Operation, format!("'?' on {t}"))),
                };
                let d = self.dest(inst.dest);
                self.load(x, 0);
                self.code.extend([I::I64Eqz, I::If(BlockType::Empty)]);
                self.load(x, 8);
                self.code.extend([I::LocalSet(d), I::Else]);
                let e = self.local();
                self.load(x, 8);
                self.push(I::LocalSet(e));
                self.perform("Fail", "raise", &[err_ty], None, &[e])?;
                self.code.extend([I::Unreachable, I::End]);
            }
            Op::Handle { state_value, .. } => {
                let id = *self
                    .l
                    .plan
                    .handler_at
                    .get(&(self.name.clone(), inst.dest))
                    .ok_or_else(|| WasmError::Invalid("unplanned handler".into()))?;
                let r = self.handle(id, *state_value)?;
                self.values.insert(inst.dest, r);
            }
            Op::Closure { .. } => {
                return Err(self.err(Unsupported::Closure, "closure values"));
            }
            Op::Indirect { .. } => {
                return Err(self.err(Unsupported::Closure, "call through a function value"));
            }
            #[allow(unreachable_patterns)]
            _ => {
                return Err(self.err(
                    Unsupported::Operation,
                    "IR operation unknown to the Wasm backend",
                ));
            }
        }
        Ok(())
    }

    /// Arguments for a call: values, or cells for `inout` places.
    fn call_args(
        &mut self,
        args: &[Argument],
        params: &[Param],
    ) -> Result<(Vec<u32>, Writebacks), WasmError> {
        let mut out = vec![];
        let mut writeback = vec![];
        for (i, a) in args.iter().enumerate() {
            let inout =
                a.pass == Pass::Inout || params.get(i).is_some_and(|p| p.pass == Pass::Inout);
            if !inout {
                out.push(self.value(a.value)?);
                continue;
            }
            let place = a.place.clone().ok_or_else(|| {
                WasmError::Invalid(format!("{}: inout argument without a place", self.name))
            })?;
            if place.path.is_empty()
                && let Some((x, true)) = self.names.get(&place.root).copied()
            {
                out.push(x);
                continue;
            }
            let (v, _) = self.read_place(&place.root, &place.path)?;
            let cell = self.alloc(8);
            self.store(cell, 0, v);
            out.push(cell);
            writeback.push((cell, place));
        }
        Ok((out, writeback))
    }
    fn writeback(&mut self, cells: Vec<(u32, crate::check::Place)>) -> Result<(), WasmError> {
        for (cell, place) in cells {
            let v = self.local();
            self.load(cell, 0);
            self.push(I::LocalSet(v));
            self.write_place(&place.root, &place.path, v)?;
        }
        Ok(())
    }

    fn direct_call(
        &mut self,
        dest: ValueId,
        callee: &str,
        args: &[Argument],
    ) -> Result<(), WasmError> {
        let params = self
            .l
            .world
            .functions
            .get(callee)
            .map(|f| f.params.clone())
            .unwrap_or_default();
        let (vals, cells) = self.call_args(args, &params)?;
        for v in &vals {
            self.get(*v);
        }
        let d = self.dest(dest);
        self.code
            .extend([I::Call(self.l.funcs[callee]), I::LocalSet(d)]);
        self.writeback(cells)?;
        self.abort_check();
        Ok(())
    }

    /// Select the innermost frame for an operation: the exact instance key
    /// if installed, else the effect name (or the label).
    fn perform(
        &mut self,
        effect: &str,
        op: &str,
        instance: &[Type],
        label: Option<&str>,
        args: &[u32],
    ) -> Result<u32, WasmError> {
        let decl = &self.l.world.effects[effect];
        let index = decl
            .ops
            .iter()
            .position(|o| o.name == op)
            .ok_or_else(|| WasmError::Invalid(format!("unknown operation {effect}::{op}")))?;
        if args.len() > self.l.arity {
            return Err(WasmError::Invalid(format!("{effect}::{op} arity")));
        }
        let frame = self.local();
        if let Some(l) = label {
            if !self.labels.contains(l) {
                return Err(self.err(
                    Unsupported::LabelledEffectRow,
                    format!("label '{l}' is not bound by an enclosing handler of this function"),
                ));
            }
            let key = format!("$label:{l}");
            let g = *self
                .l
                .env
                .get(&key)
                .ok_or_else(|| WasmError::Invalid(format!("no handler installs {key}")))?;
            self.code.extend([I::GlobalGet(g), I::LocalSet(frame)]);
        } else {
            if instance.iter().any(|t| type_mentions(t, &self.generics)) {
                return Err(self.err(
                    Unsupported::GenericEffectInstance,
                    format!("{effect} with generic arguments"),
                ));
            }
            let exact = crate::numeric::instance_key(effect, instance)
                .and_then(|k| self.l.env.get(&k).copied());
            let by_name = self.l.env.get(&format!("$effect:{effect}")).copied();
            match (exact, by_name) {
                (Some(e), Some(n)) => self.code.extend([
                    I::GlobalGet(e),
                    I::LocalTee(frame),
                    I::I64Eqz,
                    I::If(BlockType::Empty),
                    I::GlobalGet(n),
                    I::LocalSet(frame),
                    I::End,
                ]),
                (Some(g), None) | (None, Some(g)) => {
                    self.code.extend([I::GlobalGet(g), I::LocalSet(frame)])
                }
                (None, None) => self.code.extend([I::I64Const(0), I::LocalSet(frame)]),
            }
        }
        self.code.extend([
            I::LocalGet(frame),
            I::I64Eqz,
            I::If(BlockType::Empty),
            I::Unreachable,
            I::End,
        ]);
        self.code
            .extend([I::LocalGet(frame), I::I64Const(index as i64)]);
        for j in 0..self.l.arity {
            match args.get(j) {
                Some(a) => self.get(*a),
                None => self.push(I::I64Const(0)),
            }
        }
        let d = self.local();
        self.load(frame, F_DISPATCH);
        self.code.extend([
            I::I32WrapI64,
            I::CallIndirect {
                type_index: self.l.dispatch_ty,
                table_index: 0,
            },
            I::LocalSet(d),
        ]);
        self.abort_check();
        Ok(d)
    }

    fn raise_overflow(&mut self) -> Result<(), WasmError> {
        let e = self.local();
        self.code.extend([I::I64Const(0), I::LocalSet(e)]);
        self.perform(
            "Fail",
            "raise",
            &[Type::named(crate::numeric::OVERFLOW_TYPE)],
            None,
            &[e],
        )?;
        self.push(I::Unreachable);
        Ok(())
    }
    fn raise_runtime_error(&mut self, variant: &str) -> Result<(), WasmError> {
        let ty = Type::named("RuntimeError");
        let (index, _, _) = layout::variant(self.l.world, &ty, variant)?;
        let e = self.alloc(8);
        let t = self.local();
        self.code
            .extend([I::I64Const(index as i64), I::LocalSet(t)]);
        self.store(e, 0, t);
        self.perform("Fail", "raise", &[ty], None, &[e])?;
        self.push(I::Unreachable);
        Ok(())
    }

    /// `vesper.arith.checked/1` binary operators; returns the result local.
    fn binary(
        &mut self,
        op: &str,
        x: u32,
        y: u32,
        s1: Scalar,
        s2: Scalar,
    ) -> Result<u32, WasmError> {
        let d = self.local();
        if s1 == Scalar::F64 || s2 == Scalar::F64 {
            let f = |c: &mut Vec<I<'static>>| {
                c.extend([
                    I::LocalGet(x),
                    I::F64ReinterpretI64,
                    I::LocalGet(y),
                    I::F64ReinterpretI64,
                ])
            };
            f(&mut self.code);
            let (ins, cmp) = match op {
                "+" => (I::F64Add, false),
                "-" => (I::F64Sub, false),
                "*" => (I::F64Mul, false),
                "/" => (I::F64Div, false),
                "==" => (I::F64Eq, true),
                "!=" => (I::F64Ne, true),
                "<" => (I::F64Lt, true),
                "<=" => (I::F64Le, true),
                ">" => (I::F64Gt, true),
                ">=" => (I::F64Ge, true),
                "%" => {
                    return Err(self.err(
                        Unsupported::FloatRemainder,
                        "f64 '%' (fmod) has no Wasm instruction",
                    ));
                }
                _ => return Err(self.err(Unsupported::Operation, format!("f64 '{op}'"))),
            };
            self.push(ins);
            self.push(if cmp {
                I::I64ExtendI32U
            } else {
                I::I64ReinterpretF64
            });
            self.push(I::LocalSet(d));
            return Ok(d);
        }
        if s1 == Scalar::Bool || s1 == Scalar::Unit {
            let ins = match op {
                "==" => I::I64Eq,
                "!=" => I::I64Ne,
                _ => return Err(self.err(Unsupported::Operation, format!("'{op}' on {s1:?}"))),
            };
            self.code.extend([
                I::LocalGet(x),
                I::LocalGet(y),
                ins,
                I::I64ExtendI32U,
                I::LocalSet(d),
            ]);
            return Ok(d);
        }
        let kind = match (integer(s1), integer(s2)) {
            (Some(a), Some(b)) => {
                if a.unsigned() {
                    a
                } else {
                    b
                }
            }
            _ => {
                return Err(self.err(
                    Unsupported::Operation,
                    format!("'{op}' on {s1:?} and {s2:?}"),
                ));
            }
        };
        let signed = kind == IntegerKind::I64;
        let cmp = |c: &mut Vec<I<'static>>, ins: I<'static>| {
            c.extend([I::LocalGet(x), I::LocalGet(y), ins, I::I64ExtendI32U])
        };
        match op {
            "==" => cmp(&mut self.code, I::I64Eq),
            "!=" => cmp(&mut self.code, I::I64Ne),
            "<" => cmp(&mut self.code, if signed { I::I64LtS } else { I::I64LtU }),
            "<=" => cmp(&mut self.code, if signed { I::I64LeS } else { I::I64LeU }),
            ">" => cmp(&mut self.code, if signed { I::I64GtS } else { I::I64GtU }),
            ">=" => cmp(&mut self.code, if signed { I::I64GeS } else { I::I64GeU }),
            "wrapping_add" | "wrapping_sub" | "wrapping_mul" => {
                let ins = match op {
                    "wrapping_add" => I::I64Add,
                    "wrapping_sub" => I::I64Sub,
                    _ => I::I64Mul,
                };
                self.code.extend([I::LocalGet(x), I::LocalGet(y), ins]);
                if kind.bits() < 64 {
                    self.code
                        .extend([I::I64Const(kind.mask() as i64), I::I64And]);
                }
            }
            "+" | "-" | "*" => {
                self.overflow_check(op, kind, x, y)?;
                let ins = match op {
                    "+" => I::I64Add,
                    "-" => I::I64Sub,
                    _ => I::I64Mul,
                };
                self.code.extend([I::LocalGet(x), I::LocalGet(y), ins]);
            }
            "/" | "%" => {
                self.code
                    .extend([I::LocalGet(y), I::I64Eqz, I::If(BlockType::Empty)]);
                self.raise_runtime_error("DivisionByZero")?;
                self.push(I::End);
                if signed && op == "/" {
                    self.code.extend([
                        I::LocalGet(x),
                        I::I64Const(i64::MIN),
                        I::I64Eq,
                        I::LocalGet(y),
                        I::I64Const(-1),
                        I::I64Eq,
                        I::I32And,
                        I::If(BlockType::Empty),
                    ]);
                    self.raise_overflow()?;
                    self.push(I::End);
                }
                if signed && op == "%" {
                    // i64::MIN % -1 is 0 (Wasm's rem_s agrees and does not trap).
                    self.code
                        .extend([I::LocalGet(x), I::LocalGet(y), I::I64RemS]);
                } else {
                    let ins = match (op, signed) {
                        ("/", true) => I::I64DivS,
                        ("/", false) => I::I64DivU,
                        (_, _) => I::I64RemU,
                    };
                    self.code.extend([I::LocalGet(x), I::LocalGet(y), ins]);
                }
            }
            _ => return Err(self.err(Unsupported::Operation, format!("integer '{op}'"))),
        }
        self.push(I::LocalSet(d));
        Ok(d)
    }

    fn overflow_check(
        &mut self,
        op: &str,
        kind: IntegerKind,
        x: u32,
        y: u32,
    ) -> Result<(), WasmError> {
        use IntegerKind as K;
        match (op, kind) {
            ("+" | "*", K::U8 | K::U32) => {
                let ins = if op == "+" { I::I64Add } else { I::I64Mul };
                self.code.extend([
                    I::LocalGet(x),
                    I::LocalGet(y),
                    ins,
                    I::I64Const(kind.mask() as i64),
                    I::I64GtU,
                ]);
            }
            ("-", K::U8 | K::U32 | K::U64) => {
                self.code
                    .extend([I::LocalGet(x), I::LocalGet(y), I::I64LtU])
            }
            ("+", K::U64) => self.code.extend([
                I::LocalGet(x),
                I::LocalGet(y),
                I::I64Add,
                I::LocalGet(x),
                I::I64LtU,
            ]),
            ("*", K::U64) => {
                // x != 0 && (x * y) / x != y
                self.code.extend([
                    I::LocalGet(x),
                    I::I64Eqz,
                    I::If(BlockType::Result(ValType::I32)),
                    I::I32Const(0),
                    I::Else,
                    I::LocalGet(x),
                    I::LocalGet(y),
                    I::I64Mul,
                    I::LocalGet(x),
                    I::I64DivU,
                    I::LocalGet(y),
                    I::I64Ne,
                    I::End,
                ]);
            }
            ("+" | "-", K::I64) => {
                let r = self.local();
                let ins = if op == "+" { I::I64Add } else { I::I64Sub };
                self.code
                    .extend([I::LocalGet(x), I::LocalGet(y), ins, I::LocalSet(r)]);
                // Sign of the result differs from both addends (+), or from
                // the minuend while the operand signs differ (-).
                self.code
                    .extend([I::LocalGet(x), I::LocalGet(r), I::I64Xor]);
                if op == "+" {
                    self.code
                        .extend([I::LocalGet(y), I::LocalGet(r), I::I64Xor]);
                } else {
                    self.code
                        .extend([I::LocalGet(x), I::LocalGet(y), I::I64Xor]);
                }
                self.code.extend([I::I64And, I::I64Const(0), I::I64LtS]);
            }
            ("*", K::I64) => {
                // x == -1: overflow iff y == MIN; else x != 0 && (x*y)/x != y.
                self.code.extend([
                    I::LocalGet(x),
                    I::I64Const(-1),
                    I::I64Eq,
                    I::If(BlockType::Result(ValType::I32)),
                    I::LocalGet(y),
                    I::I64Const(i64::MIN),
                    I::I64Eq,
                    I::Else,
                    I::LocalGet(x),
                    I::I64Eqz,
                    I::If(BlockType::Result(ValType::I32)),
                    I::I32Const(0),
                    I::Else,
                    I::LocalGet(x),
                    I::LocalGet(y),
                    I::I64Mul,
                    I::LocalGet(x),
                    I::I64DivS,
                    I::LocalGet(y),
                    I::I64Ne,
                    I::End,
                    I::End,
                ]);
            }
            _ => {
                return Err(WasmError::Invalid(format!(
                    "no overflow rule for {op} on {kind:?}"
                )));
            }
        }
        self.push(I::If(BlockType::Empty));
        self.raise_overflow()?;
        self.push(I::End);
        Ok(())
    }

    /// `resume(v[, s])` inside an operation clause.
    fn resume_call(&mut self, dest: ValueId, args: &[Argument]) -> Result<(), WasmError> {
        let h = self
            .clause_handler
            .ok_or_else(|| WasmError::Invalid("resume outside a clause".into()))?;
        let frame = self
            .frame
            .ok_or_else(|| WasmError::Invalid("resume without a frame".into()))?;
        let stateful = self.l.plan.handlers[h].state.is_some();
        let v = self.value(
            args.first()
                .ok_or_else(|| WasmError::Invalid("resume without a value".into()))?
                .value,
        )?;
        if stateful {
            let s = self.value(
                args.get(1)
                    .ok_or_else(|| WasmError::Invalid("resume without a state".into()))?
                    .value,
            )?;
            self.store(frame, F_STATE, s);
        }
        match self.k {
            None => {
                self.values.insert(dest, v);
            }
            Some(k) => {
                let run = self.l.hfns[h]
                    .run
                    .ok_or_else(|| WasmError::Invalid("stack switching runner".into()))?;
                let d = self.dest(dest);
                self.code.extend([
                    I::LocalGet(frame),
                    I::LocalGet(k),
                    I::LocalGet(v),
                    I::Call(run),
                    I::LocalSet(d),
                ]);
                self.abort_check();
            }
        }
        Ok(())
    }

    /// Install handler `id` around its body and return the handler's output.
    fn handle(&mut self, id: usize, state_value: Option<ValueId>) -> Result<u32, WasmError> {
        let h = self.l.plan.handlers[id].clone();
        let fns = &self.l.hfns[id];
        let (size, slot, env_offset, body_offset) = (
            fns.frame_size,
            fns.dispatch_slot,
            fns.env_offset,
            fns.body_offset,
        );
        let capture_offset = fns.capture_offset.clone();
        let frame = self.alloc(size);
        let t = self.local();
        self.code
            .extend([I::I64Const(i64::from(slot)), I::LocalSet(t)]);
        self.store(frame, F_DISPATCH, t);
        if let Some(sv) = state_value {
            let s = self.value(sv)?;
            self.store(frame, F_STATE, s);
        }
        let globals: Vec<u32> = self.l.env.values().copied().collect();
        for (n, g) in globals.iter().enumerate() {
            self.code.extend([
                I::LocalGet(frame),
                I::I32WrapI64,
                I::GlobalGet(*g),
                I::I64Store(mem(env_offset + 8 * n as u64)),
            ]);
        }
        let alias = |e: &Self, n: &str| -> Result<String, WasmError> {
            h.aliases.get(n).cloned().ok_or_else(|| {
                WasmError::Invalid(format!("{}: handler capture '{n}' has no slot", e.name))
            })
        };
        for (n, off) in &capture_offset {
            let slot = alias(self, n)?;
            let v = self.read_name(&slot)?;
            self.store(frame, *off, v);
        }
        // Body captures are cells.
        let body_params = self.l.program.functions[&h.body].params.clone();
        let mut cells = vec![];
        let mut body_args = vec![];
        for p in &body_params {
            let slot = alias(self, &p.name)?;
            if let Some((x, true)) = self.names.get(&slot).copied() {
                body_args.push(x);
                continue;
            }
            let v = self.read_name(&slot)?;
            let cell = self.alloc(8);
            self.store(cell, 0, v);
            body_args.push(cell);
            cells.push((cell, slot));
        }
        let out = self.local();
        if h.stack_switching {
            let fx = self
                .l
                .fx
                .as_ref()
                .ok_or_else(|| WasmError::Invalid("stack switching types missing".into()))?;
            let ct = fx.ct;
            for (j, a) in body_args.iter().enumerate() {
                self.store(frame, body_offset + 8 * j as u64, *a);
            }
            let fns = &self.l.hfns[id];
            let (entry, run) = (fns.body_entry.expect("entry"), fns.run.expect("run"));
            self.code.extend([
                I::LocalGet(frame),
                I::RefFunc(entry),
                I::ContNew(ct),
                I::LocalGet(frame),
                I::Call(run),
                I::LocalSet(out),
            ]);
            for (cell, slot) in cells {
                let v = self.local();
                self.load(cell, 0);
                self.push(I::LocalSet(v));
                self.write_name(&slot, v)?;
            }
        } else {
            let mut code = vec![];
            self.l.install_keys(&mut code, frame, &h);
            self.code.extend(code);
            let r = self.local();
            for a in &body_args {
                self.get(*a);
            }
            self.code
                .extend([I::Call(self.l.funcs[&h.body]), I::LocalSet(r)]);
            let mut code = vec![];
            self.l.restore_env_frame(&mut code, frame, id);
            self.code.extend(code);
            for (cell, slot) in cells {
                let v = self.local();
                self.load(cell, 0);
                self.push(I::LocalSet(v));
                self.write_name(&slot, v)?;
            }
            let fns = &self.l.hfns[id];
            let (abort_fn, ret_fn) = (fns.abort, fns.ret);
            self.code.extend([
                I::GlobalGet(self.l.abort),
                I::LocalGet(frame),
                I::I64Eq,
                I::If(BlockType::Result(ValType::I64)),
                I::I64Const(0),
                I::GlobalSet(self.l.abort),
                I::LocalGet(frame),
                I::Call(abort_fn),
                I::Else,
                I::GlobalGet(self.l.abort),
                I::I64Const(0),
                I::I64Ne,
                I::If(BlockType::Empty),
                I::I64Const(0),
                I::Return,
                I::End,
                I::LocalGet(frame),
                I::LocalGet(r),
                I::Call(ret_fn),
                I::End,
                I::LocalSet(out),
            ]);
        }
        self.abort_check();
        Ok(out)
    }
}

fn type_mentions(t: &Type, generics: &BTreeSet<String>) -> bool {
    match t {
        Type::Named(n, args) => {
            generics.contains(n) || args.iter().any(|a| type_mentions(a, generics))
        }
        Type::Tuple(ts) => ts.iter().any(|a| type_mentions(a, generics)),
        _ => false,
    }
}

/// Reachable blocks from 0 in a topological order, or `None` if cyclic.
fn topological(f: &FunctionIr) -> Option<Vec<usize>> {
    fn succ(t: &Terminator) -> Vec<usize> {
        match t {
            Terminator::Jump { block, .. } => vec![*block],
            Terminator::Branch { yes, no, .. } => vec![*yes, *no],
            Terminator::Match { arms, .. } => arms.iter().map(|(_, b)| *b).collect(),
            _ => vec![],
        }
    }
    let n = f.blocks.len();
    let mut state = vec![0u8; n];
    let mut post = vec![];
    // Iterative DFS: (block, next successor index).
    let mut stack = vec![(0usize, 0usize)];
    state[0] = 1;
    while let Some((b, i)) = stack.pop() {
        let s = succ(&f.blocks[b].terminator);
        if i < s.len() {
            stack.push((b, i + 1));
            let t = *s.get(i)?;
            if t >= n {
                return None;
            }
            match state[t] {
                0 => {
                    state[t] = 1;
                    stack.push((t, 0));
                }
                1 => return None,
                _ => {}
            }
        } else {
            state[b] = 2;
            post.push(b);
        }
    }
    post.reverse();
    Some(post)
}
