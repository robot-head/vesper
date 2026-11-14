//! Checked source to native one-shot suspension.
//!
//! An embedding names a checked entry function whose effect row declares
//! linear external effects, and binds every operation of those effects to a
//! fixed park operation of fiber ABI v2. The entry then compiles to an ABI v2
//! fiber entry (`extern "C" fn(ctx, arg) -> status`) in which every bound
//! operation lowers to `clif::emit_park` with typed value, error and
//! cancellation edges. Nothing here changes the shared AST, checker, Place IR
//! or fiber ABI: operation bindings, evidence depths and the data-frame
//! layout are backend-private, and the reference evaluator is never used.
//!
//! # Supported slice
//!
//! * The entry takes no parameters, has no generics and a concrete effect
//!   row. Each row instance is either an unlabeled `Fail[E]` (raised failures
//!   exit the fiber with [`exit::FAILED`]) or a bound, unlabeled,
//!   non-generic `linear effect`. Every operation of a bound effect must be
//!   bound exactly once.
//! * [`Operation::Yield`]: `fn op() -> R`. Parks with `OP_YIELD`.
//! * [`Operation::Read`]: `fn op(inout buffer: B) -> R`, where `B` is a
//!   *word buffer*: `u64`, `i64`, or a struct/tuple whose fields are word
//!   buffers, at most [`MAX_READ`] bytes. Parks with `OP_READ(len, offset)`
//!   on a staging area of the data frame. Before the park the current buffer
//!   is copied into the staging area; on a value resumption carrying `n` the
//!   whole staging area is copied back, so the first `n` bytes are the bytes
//!   read and the rest keep their previous contents (POSIX `read` semantics).
//!   Error and cancellation resumptions leave the buffer unchanged. This is a
//!   fixed-size buffer, not a general byte-slice API.
//! * `R` is an enum. The binding names its value constructor (Yield: no
//!   field or one unit field; Read: one `u64` byte count), its error
//!   constructor (one `u32` or `u64` code) and its cancellation result: a
//!   distinct field-less constructor or the error constructor with a fixed
//!   code. A cancellation resumption therefore produces an ordinary typed
//!   value that source code handles, so cleanup (which may park again) is
//!   source-defined and no obligation is dropped.
//! * Repeated cancellation is abortive: a cancellation delivered to a
//!   cleanup park produces the cancellation result again without performing
//!   the operation. Source code still runs to its own exit.
//!
//! Data-frame layout: `[result | failure payload | read staging]`, each
//! 8-aligned. Aggregate results and failure payloads are written through a
//! data-frame base loaded after the last park. After every park the staging
//! copy uses `ParkResult::data_base`; [`check_data_base_freshness`] rejects a
//! generated function that uses a base derived before a park after it.
use super::{Codegen, InstanceMap, contains_function, specialized_function, visit};
use crate::{
    ast::*,
    check::World,
    layout::{Layout, LayoutKind},
};
use cranelift_codegen::ir::{self, InstBuilder, InstructionData, Opcode, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vesper_fiber_abi::{
    abi::{exit, offsets},
    clif,
    prototype::{OP_READ, OP_YIELD},
};

/// Largest read buffer, matching the scheduler adapter's limit.
pub const MAX_READ: u32 = 4096;
/// Error code conventionally used for a cancelled operation.
pub const ECANCELED: u32 = 125;
const PARK_IDENTITY: &str = "native-session-park:";
const FAIL_IDENTITY: &str = "native-session-fail:";

/// What a bound operation does at its park.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    /// `OP_YIELD`: resumed with `Value(0)` on the next scheduler turn.
    Yield,
    /// `OP_READ`: read into the operation's `inout` word buffer.
    Read,
}

/// The source value a cancellation resumption produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cancellation {
    /// A field-less constructor of the result enum, distinct from the value
    /// and error constructors.
    Constructor(String),
    /// The error constructor carrying this code.
    ErrorCode(u32),
}

/// Explicit binding of one checked effect operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationBinding {
    pub op: String,
    pub operation: Operation,
    /// Constructor built from a value resumption.
    pub value: String,
    /// Constructor built from an error resumption (errno-like code).
    pub error: String,
    pub cancel: Cancellation,
}

impl OperationBinding {
    pub fn new(
        op: &str,
        operation: Operation,
        value: &str,
        error: &str,
        cancel: Cancellation,
    ) -> Self {
        Self {
            op: op.into(),
            operation,
            value: value.into(),
            error: error.into(),
            cancel,
        }
    }
}

/// Explicit binding of one linear external effect of the entry row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectBinding {
    pub effect: String,
    pub ops: Vec<OperationBinding>,
}

/// Byte layout of a session's data frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLayout {
    pub result_offset: u32,
    pub result_size: u32,
    pub failure_offset: u32,
    pub failure_size: u32,
    pub staging_offset: u32,
    pub staging_size: u32,
    pub bytes: u32,
}

#[derive(Clone, Debug)]
pub(super) enum BoundCancel {
    Constructor(String),
    Error(u64),
}

#[derive(Clone, Debug)]
pub(super) struct BoundOp {
    operation: Operation,
    /// Read buffer type and byte size.
    buffer: Option<(Type, u32)>,
    result: Type,
    value: String,
    value_field: Option<Type>,
    error: String,
    error_field: Type,
    cancel: BoundCancel,
}

#[derive(Clone, Debug)]
pub(super) struct BoundEffect {
    name: String,
    ops: BTreeMap<String, BoundOp>,
}

/// A binding validated against the checked world.
#[derive(Clone, Debug)]
pub struct Plan {
    entry: String,
    effects: Vec<BoundEffect>,
    fails: Vec<Type>,
    result: Type,
    frame: FrameLayout,
}

impl Plan {
    pub fn entry(&self) -> &str {
        &self.entry
    }
    pub fn frame(&self) -> FrameLayout {
        self.frame
    }
    pub fn result_type(&self) -> &Type {
        &self.result
    }
    /// Error types of the entry's `Fail` instances, in row order. A raise
    /// of instance `i` exits with status `FAILED` and result word `i + 1`.
    pub fn failures(&self) -> &[Type] {
        &self.fails
    }
    /// Bound effects in evidence-depth order; the slot at depth `d` carries
    /// effect tag `d + 1`.
    pub fn effects(&self) -> Vec<&str> {
        self.effects.iter().map(|e| e.name.as_str()).collect()
    }
}

fn integer(ty: &Type, names: &[&str]) -> bool {
    matches!(ty, Type::Named(n, args) if args.is_empty() && names.contains(&n.as_str()))
}

/// `u64`/`i64` words, or products of word buffers: every byte image is a
/// valid value, so a read may overwrite any prefix.
fn word_buffer(world: &World, ty: &Type, depth: usize) -> Result<(), String> {
    if depth > 16 {
        return Err("read buffer nests too deeply".into());
    }
    let ty = crate::numeric::default_type(ty);
    if integer(&ty, &["u64", "i64"]) {
        return Ok(());
    }
    let layout = Layout::of(world, &ty)?;
    match &layout.kind {
        LayoutKind::Product(fields) if ty.name() != "String" && !fields.is_empty() => {
            if let Type::Named(n, _) = &ty
                && world.adts.get(n).is_some_and(|a| a.is_enum)
            {
                return Err(format!("read buffer {ty} is an enum"));
            }
            for f in fields {
                word_buffer(world, &f.ty, depth + 1)
                    .map_err(|e| format!("read buffer {ty}: field {}: {e}", f.name))?;
            }
            Ok(())
        }
        _ => Err(format!(
            "read buffer {ty} is not a u64/i64 word buffer (enums, bool, u8, u32, f64, strings and functions have invalid byte images)"
        )),
    }
}

fn validate_op(
    world: &World,
    effect: &str,
    sig: &Function,
    binding: &OperationBinding,
) -> Result<BoundOp, String> {
    let name = format!("{effect}::{}", sig.name);
    if !sig.generics.is_empty() {
        return Err(format!("{name}: generic operations are unsupported"));
    }
    let buffer = match binding.operation {
        Operation::Yield => {
            if !sig.params.is_empty() {
                return Err(format!(
                    "{name} is bound to Yield but takes {} parameters",
                    sig.params.len()
                ));
            }
            None
        }
        Operation::Read => {
            let [param] = sig.params.as_slice() else {
                return Err(format!(
                    "{name} is bound to Read and must take exactly one inout buffer"
                ));
            };
            if param.pass != Pass::Inout {
                return Err(format!(
                    "{name} is bound to Read but its buffer is not passed inout"
                ));
            }
            word_buffer(world, &param.ty, 0).map_err(|e| format!("{name}: {e}"))?;
            let size = Layout::of(world, &param.ty)?.size;
            if size == 0 || size > MAX_READ {
                return Err(format!(
                    "{name}: read buffer of {size} bytes exceeds {MAX_READ}"
                ));
            }
            Some((param.ty.clone(), size))
        }
    };
    let result = crate::numeric::default_type(&sig.result);
    let layout = Layout::of(world, &result)?;
    let LayoutKind::Enum(_) = &layout.kind else {
        return Err(format!(
            "{name}: result {result} must be an enum with value, error and cancellation constructors"
        ));
    };
    let variant = |ctor: &str, role: &str| {
        layout
            .variant(ctor)
            .map(|(_, fields)| fields.to_vec())
            .ok_or_else(|| format!("{name}: result {result} has no {role} constructor '{ctor}'"))
    };
    if binding.value == binding.error {
        return Err(format!("{name}: value and error constructors must differ"));
    }
    let value_fields = variant(&binding.value, "value")?;
    let value_field = match (binding.operation, value_fields.as_slice()) {
        (Operation::Yield, []) => None,
        (Operation::Yield, [f]) if f.ty == Type::unit() || f.ty.name() == "Unit" => None,
        (Operation::Read, [f]) if integer(&f.ty, &["u64"]) => Some(f.ty.clone()),
        _ => {
            return Err(format!(
                "{name}: value constructor '{}' must carry {}",
                binding.value,
                if binding.operation == Operation::Read {
                    "exactly one u64 byte count"
                } else {
                    "no value or a unit value"
                }
            ));
        }
    };
    let error_fields = variant(&binding.error, "error")?;
    let error_field = match error_fields.as_slice() {
        [f] if integer(&f.ty, &["u32", "u64"]) => f.ty.clone(),
        _ => {
            return Err(format!(
                "{name}: error constructor '{}' must carry one u32 or u64 code",
                binding.error
            ));
        }
    };
    let cancel = match &binding.cancel {
        Cancellation::Constructor(c) => {
            if c == &binding.value || c == &binding.error {
                return Err(format!(
                    "{name}: cancellation constructor '{c}' must differ from the value and error constructors"
                ));
            }
            if !variant(c, "cancellation")?.is_empty() {
                return Err(format!(
                    "{name}: cancellation constructor '{c}' must not carry fields"
                ));
            }
            BoundCancel::Constructor(c.clone())
        }
        Cancellation::ErrorCode(code) => BoundCancel::Error(u64::from(*code)),
    };
    Ok(BoundOp {
        operation: binding.operation,
        buffer,
        result,
        value: binding.value.clone(),
        value_field,
        error: binding.error.clone(),
        error_field,
        cancel,
    })
}

fn align8(n: u32) -> Result<u32, String> {
    n.checked_add(7)
        .map(|n| n & !7)
        .ok_or_else(|| "session data frame is too large".into())
}

/// Spans of names that the lowered IR resolves to world functions rather
/// than to local bindings (the same resolution ordinary instance discovery
/// uses), so shadowing locals and local closures add no call edges.
type References = BTreeSet<(usize, usize, String)>;
fn function_references(world: &World) -> References {
    crate::ir::lower_world(world)
        .functions
        .values()
        .flat_map(|f| {
            f.blocks
                .iter()
                .flat_map(|b| &b.instructions)
                .filter_map(|i| match &i.op {
                    crate::ir::Op::Read { root, .. }
                        if world.functions.contains_key(root) && !f.locals.contains_key(root) =>
                    {
                        Some((i.span.start, i.span.end, root.clone()))
                    }
                    _ => None,
                })
        })
        .collect()
}

/// Direct callees of a function body that name world functions.
fn callees(world: &World, references: &References, f: &Function) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(body) = &f.body {
        visit(body, &mut |e| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                match &callee.kind {
                    // A callee typed at its own span is a local value.
                    ExprKind::Name(n)
                        if world.functions.contains_key(n)
                            && !world
                                .types
                                .contains_key(&(callee.span.start, callee.span.end)) =>
                    {
                        out.insert(n.clone());
                    }
                    ExprKind::Field(base, method) => {
                        if let Some(t) = world.types.get(&(base.span.start, base.span.end)) {
                            let name = format!("{}::{method}", t.name());
                            if world.functions.contains_key(&name) {
                                out.insert(name);
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let ExprKind::Name(n) = &e.kind
                && references.contains(&(e.span.start, e.span.end, n.clone()))
            {
                out.insert(n.clone());
            }
        });
    }
    out
}

/// Effectful calls are inlined into the session entry (that is how every
/// park site lands in one ABI entry), so recursion through them has no
/// static bound. Reject it instead of unrolling.
fn reject_suspending_recursion(world: &World, entry: &str) -> Result<(), String> {
    fn dfs(
        world: &World,
        references: &References,
        name: &str,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if let Some(at) = path.iter().position(|p| p == name) {
            return Err(format!(
                "recursive suspending call cycle {} -> {name} is unsupported: native sessions inline effectful calls",
                path[at..].join(" -> ")
            ));
        }
        let Some(f) = world.functions.get(name) else {
            return Ok(());
        };
        path.push(name.into());
        for callee in callees(world, references, f) {
            if world
                .functions
                .get(&callee)
                .is_some_and(|g| specialized_function(world, g))
            {
                dfs(world, references, &callee, path, done)?;
            }
        }
        path.pop();
        done.insert(name.into());
        Ok(())
    }
    let references = function_references(world);
    dfs(world, &references, entry, &mut vec![], &mut BTreeSet::new())
}

/// Validate explicit bindings against the checked entry signature and the
/// checked effect declarations. Nothing executable exists before this passes.
pub fn plan(world: &World, entry: &str, bindings: &[EffectBinding]) -> Result<Plan, String> {
    let f = world
        .functions
        .get(entry)
        .ok_or_else(|| format!("unknown session entry '{entry}'"))?;
    if f.body.is_none() {
        return Err(format!("session entry '{entry}' has no body"));
    }
    if !f.params.is_empty() {
        return Err("session entries take no parameters in this slice".into());
    }
    if !f.generics.is_empty() {
        return Err("session entries cannot be generic".into());
    }
    if f.effects.tail.is_some() {
        return Err("session entry requires a concrete effect row".into());
    }
    if contains_function(world, &f.result) {
        return Err("session entry cannot return closures or continuations".into());
    }
    reject_suspending_recursion(world, entry)?;
    let result = crate::numeric::default_type(&f.result);
    let result_layout = Layout::of(world, &result)?;
    let mut by_name = BTreeMap::new();
    for b in bindings {
        if by_name.insert(b.effect.as_str(), b).is_some() {
            return Err(format!("effect {} is bound twice", b.effect));
        }
    }
    let mut effects = vec![];
    let mut fails = vec![];
    let mut seen = BTreeSet::new();
    for instance in &f.effects.instances {
        if instance.name == "Fail" && instance.label.is_none() && instance.args.len() == 1 {
            fails.push(crate::numeric::default_type(&instance.args[0]));
            continue;
        }
        let Some(binding) = by_name.get(instance.name.as_str()) else {
            return Err(format!(
                "entry effect {} has no native binding",
                instance.name
            ));
        };
        if instance.label.is_some() {
            return Err(format!(
                "labelled external effect instance {} is unsupported",
                instance.name
            ));
        }
        if !seen.insert(instance.name.clone()) {
            return Err(format!(
                "effect {} appears twice in the entry row",
                instance.name
            ));
        }
        let decl = world
            .effects
            .get(&instance.name)
            .ok_or_else(|| format!("unknown effect {}", instance.name))?;
        if !decl.linear {
            return Err(format!(
                "external effect {} must be declared linear (one-shot resumption)",
                instance.name
            ));
        }
        if !decl.generics.is_empty() || !instance.args.is_empty() {
            return Err(format!(
                "generic external effect {} is unsupported",
                instance.name
            ));
        }
        let mut ops = BTreeMap::new();
        for op in &binding.ops {
            let sig = decl
                .ops
                .iter()
                .find(|s| s.name == op.op)
                .ok_or_else(|| format!("effect {} has no operation '{}'", decl.name, op.op))?;
            let bound = validate_op(world, &decl.name, sig, op)?;
            if ops.insert(op.op.clone(), bound).is_some() {
                return Err(format!("operation {}::{} is bound twice", decl.name, op.op));
            }
        }
        for sig in &decl.ops {
            if !ops.contains_key(&sig.name) {
                return Err(format!(
                    "operation {}::{} has no native binding",
                    decl.name, sig.name
                ));
            }
        }
        effects.push(BoundEffect {
            name: decl.name.clone(),
            ops,
        });
    }
    for b in bindings {
        if !seen.contains(&b.effect) {
            return Err(format!(
                "binding names effect {} which the entry does not declare",
                b.effect
            ));
        }
    }
    let result_size = if result_layout.aggregate() {
        result_layout.size
    } else {
        0
    };
    let mut failure_size = 0;
    for ty in &fails {
        // Function values are backend-local closure tags with no decodable
        // byte image, so a raised callable could not be reported faithfully.
        if contains_function(world, ty) {
            return Err(format!(
                "entry failure type {ty} contains a function value, which a session exit cannot report"
            ));
        }
        failure_size = failure_size.max(Layout::of(world, ty)?.size);
    }
    let staging_size = effects
        .iter()
        .flat_map(|e| e.ops.values())
        .filter_map(|o| o.buffer.as_ref().map(|(_, size)| *size))
        .max()
        .unwrap_or(0);
    let result_offset = 0;
    let failure_offset = align8(result_size)?;
    let staging_offset = align8(
        failure_offset
            .checked_add(failure_size)
            .ok_or("session data frame is too large")?,
    )?;
    let bytes = align8(
        staging_offset
            .checked_add(staging_size)
            .ok_or("session data frame is too large")?,
    )?
    .max(8);
    Ok(Plan {
        entry: entry.into(),
        effects,
        fails,
        result,
        frame: FrameLayout {
            result_offset,
            result_size,
            failure_offset,
            failure_size,
            staging_offset,
            staging_size,
            bytes,
        },
    })
}

/// Code-generation state of a session entry.
#[derive(Clone)]
pub(super) struct SessionCodegen {
    ctx: ir::Value,
    park: ir::FuncRef,
    violation: ir::Block,
    plan: Plan,
}

impl<M: Module> Codegen<'_, '_, M> {
    /// Lower an operation whose evidence is a session park or entry failure.
    /// `Ok(None)` for ordinary lexical evidence.
    pub(super) fn session_operation(
        &mut self,
        identity: &str,
        effect: &str,
        op: &str,
        values: &[ir::Value],
        refs: &[Option<ir::Value>],
    ) -> Result<Option<ir::Value>, String> {
        if let Some(depth) = identity.strip_prefix(PARK_IDENTITY) {
            let depth = depth.parse::<usize>().map_err(|e| e.to_string())?;
            return self.session_park(depth, effect, op, refs).map(Some);
        }
        if let Some(index) = identity.strip_prefix(FAIL_IDENTITY) {
            let index = index.parse::<usize>().map_err(|e| e.to_string())?;
            return self.session_fail(index, effect, op, values).map(Some);
        }
        Ok(None)
    }
    fn session(&self) -> Result<SessionCodegen, String> {
        self.session
            .clone()
            .ok_or_else(|| "session evidence used outside its session entry".into())
    }
    fn store_constructor(
        &mut self,
        out: ir::Value,
        ty: &Type,
        ctor: &str,
        payload: Option<(ir::Value, &Type)>,
    ) -> Result<(), String> {
        let layout = Layout::of(self.world, ty)?;
        let (tag, fields) = layout
            .variant(ctor)
            .ok_or("session result constructor vanished")?;
        let field = fields.first().cloned();
        let tag = self.b.ins().iconst(types::I64, tag as i64);
        self.b.ins().store(ir::MemFlags::new(), tag, out, 0);
        if let (Some(field), Some((value, value_ty))) = (field, payload) {
            let dest = self.offset(out, field.offset);
            let value = self.coerce(value, Some(value_ty));
            self.b.ins().store(ir::MemFlags::new(), value, dest, 0);
        }
        Ok(())
    }
    fn session_park(
        &mut self,
        depth: usize,
        effect: &str,
        op: &str,
        refs: &[Option<ir::Value>],
    ) -> Result<ir::Value, String> {
        let s = self.session()?;
        let bound = s
            .plan
            .effects
            .get(depth)
            .filter(|e| e.name == effect)
            .and_then(|e| e.ops.get(op))
            .cloned()
            .ok_or_else(|| format!("{effect}::{op} has no validated native binding"))?;
        let ptr = self.m.target_config().pointer_type();
        let size = Layout::of(self.world, &bound.result)?.size;
        let out = self.alloc(size);
        let evidence = clif::load_evidence(&mut self.b, ptr, s.ctx, depth as u32).identity;
        let staging = s.plan.frame.staging_offset;
        let buffer = match &bound.buffer {
            Some((_, len)) => {
                let place = refs
                    .first()
                    .copied()
                    .flatten()
                    .ok_or("read buffer must be a place")?;
                // Stage the current contents through the base loaded after the
                // previous park; it is dead once this park is issued.
                let base = clif::entry_data_base(&mut self.b, ptr, s.ctx);
                let staged = self.b.ins().iadd_imm(base, i64::from(staging));
                self.copy_bytes(place, staged, *len);
                Some((place, *len))
            }
            None => None,
        };
        let (tag, arg0, arg1) = match bound.operation {
            Operation::Yield => (OP_YIELD, 0, 0),
            Operation::Read => (
                OP_READ,
                i64::from(buffer.map_or(0, |(_, len)| len)),
                i64::from(staging),
            ),
        };
        let arg0 = self.b.ins().iconst(types::I64, arg0);
        let arg1 = self.b.ins().iconst(types::I64, arg1);
        let park = clif::emit_park(
            &mut self.b,
            ptr,
            s.park,
            s.ctx,
            evidence,
            tag,
            arg0,
            arg1,
            s.violation,
        );
        let on_value = self.b.create_block();
        let on_error = self.b.create_block();
        let on_cancel = self.b.create_block();
        let join = self.b.create_block();
        clif::branch_on_kind(
            &mut self.b,
            park.kind,
            on_value,
            on_error,
            on_cancel,
            s.violation,
        );
        self.b.switch_to_block(on_value);
        if let Some((place, len)) = buffer {
            // The data frame may have moved while parked: only the base this
            // park returned is valid here.
            let staged = self.b.ins().iadd_imm(park.data_base, i64::from(staging));
            self.copy_bytes(staged, place, len);
        }
        let value_field = bound.value_field.clone();
        self.store_constructor(
            out,
            &bound.result,
            &bound.value,
            value_field.as_ref().map(|t| (park.value, t)),
        )?;
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(on_error);
        self.store_constructor(
            out,
            &bound.result,
            &bound.error,
            Some((park.value, &bound.error_field)),
        )?;
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(on_cancel);
        match &bound.cancel {
            BoundCancel::Constructor(c) => self.store_constructor(out, &bound.result, c, None)?,
            BoundCancel::Error(code) => {
                let code = self.b.ins().iconst(types::I64, *code as i64);
                self.store_constructor(
                    out,
                    &bound.result,
                    &bound.error,
                    Some((code, &bound.error_field)),
                )?
            }
        }
        self.b.ins().jump(join, &[]);
        self.b.switch_to_block(join);
        Ok(out)
    }
    /// An unhandled raise of the entry's `Fail` instance `index`: store the
    /// payload in the data frame and exit `FAILED` with result `index + 1`.
    fn session_fail(
        &mut self,
        index: usize,
        effect: &str,
        op: &str,
        values: &[ir::Value],
    ) -> Result<ir::Value, String> {
        let s = self.session()?;
        if effect != "Fail" || op != "raise" || values.len() != 1 {
            return Err("session failure exit requires Fail::raise".into());
        }
        let ty = s
            .plan
            .fails
            .get(index)
            .cloned()
            .ok_or("session failure names an unknown instance")?;
        let ptr = self.m.target_config().pointer_type();
        let base = clif::entry_data_base(&mut self.b, ptr, s.ctx);
        let dest = self
            .b
            .ins()
            .iadd_imm(base, i64::from(s.plan.frame.failure_offset));
        self.store(dest, values[0], &ty)?;
        let code = self.b.ins().iconst(types::I64, index as i64 + 1);
        clif::emit_exit(&mut self.b, s.ctx, exit::FAILED, code);
        let dead = self.b.create_block();
        self.b.switch_to_block(dead);
        Ok(self.zero())
    }
    /// Normal completion of the entry body.
    fn session_return(&mut self, value: ir::Value) -> Result<(), String> {
        let s = self.session()?;
        let ptr = self.m.target_config().pointer_type();
        let layout = Layout::of(self.world, &s.plan.result)?;
        let word = if layout.aggregate() {
            let base = clif::entry_data_base(&mut self.b, ptr, s.ctx);
            let dest = self
                .b
                .ins()
                .iadd_imm(base, i64::from(s.plan.frame.result_offset));
            self.copy_bytes(value, dest, layout.size);
            self.b.ins().iconst(types::I64, 0)
        } else if self.b.func.dfg.value_type(value) == types::F64 {
            self.b.ins().bitcast(types::I64, ir::MemFlags::new(), value)
        } else {
            self.coerce(value, Some(&s.plan.result))
        };
        clif::emit_exit(&mut self.b, s.ctx, exit::COMPLETE, word);
        Ok(())
    }
}

fn symbol(entry: &str) -> String {
    let clean = entry
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>();
    format!("vesper_session_{clean}")
}

/// Compile the session entry and every ordinary function it calls into `m`.
/// Returns the exported ABI v2 entry and the CLIF text of all functions.
pub(super) fn compile_session<M: Module>(
    m: &mut M,
    world: &World,
    plan: &Plan,
    optimize: bool,
) -> Result<(FuncId, String), String> {
    let instances: InstanceMap = super::instances(world, &plan.entry)?;
    let mut program = crate::ir::lower_world(world);
    if optimize {
        crate::ir_opt::optimize(world, &mut program)?;
    }
    let ids = super::declare_instances(m, world, &plan.entry, &instances)?;
    let mut text = super::define_instances(m, world, &instances, &ids, &program)?;
    let ptr = m.target_config().pointer_type();
    let call_conv = m.isa().default_call_conv();
    let sig = clif::entry_signature(call_conv, ptr);
    let name = symbol(&plan.entry);
    let id = m
        .declare_function(&name, Linkage::Export, &sig)
        .map_err(|e| e.to_string())?;
    let park_id = clif::declare_park(m).map_err(|e| e.to_string())?;
    let graph = program
        .functions
        .get(&plan.entry)
        .ok_or("session entry lacks lowered IR")?
        .clone();
    let mut ctx = m.make_context();
    ctx.func.signature = sig;
    let mut fbctx = FunctionBuilderContext::new();
    let (context, park) = {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let block = b.create_block();
        b.append_block_params_for_function_params(block);
        b.switch_to_block(block);
        let context = b.block_params(block)[0];
        let park = m.declare_func_in_func(park_id, b.func);
        let violation = b.create_block();
        let mut capabilities = BTreeMap::new();
        for (depth, effect) in plan.effects.iter().enumerate() {
            capabilities.insert(
                format!("$effect:{}", effect.name),
                format!("{PARK_IDENTITY}{depth}"),
            );
        }
        for (index, ty) in plan.fails.iter().enumerate() {
            let identity = format!("{FAIL_IDENTITY}{index}");
            capabilities.insert("$effect:Fail".into(), identity.clone());
            if let Some(key) = crate::numeric::instance_key("Fail", std::slice::from_ref(ty)) {
                capabilities.insert(key, identity);
            }
        }
        let denorm = crate::capability_effects::const_denorm_spans(world);
        let mut c = Codegen {
            b,
            m,
            world,
            ids: &ids,
            program: &program,
            capabilities,
            next_capability: 0,
            closures: vec![],
            infos: BTreeMap::new(),
            variable_infos: BTreeMap::new(),
            pointer_parents: BTreeMap::new(),
            vars: BTreeMap::new(),
            local_types: BTreeMap::new(),
            generic_vars: BTreeMap::new(),
            pointers: BTreeMap::new(),
            handlers: vec![],
            inline_depth: 0,
            fail_record: None,
            source_locations: true,
            srcloc: cranelift_codegen::ir::SourceLoc::default(),
            session: Some(SessionCodegen {
                ctx: context,
                park,
                violation,
                plan: plan.clone(),
            }),
            denorm: &denorm,
        };
        let layout = Layout::of(world, &plan.result)?;
        let join = c.b.create_block();
        c.b.append_block_param(
            join,
            if layout.aggregate() {
                ptr
            } else {
                super::scalar(&plan.result, &[])?
            },
        );
        c.ir_graph(&graph, None, Some(join))?;
        c.b.switch_to_block(join);
        let value = c.b.block_params(join)[0];
        c.session_return(value)?;
        clif::emit_violation(&mut c.b, violation);
        c.b.seal_all_blocks();
        c.b.finalize();
        (context, park)
    };
    check_data_base_freshness(&ctx.func, context, park)?;
    text.push_str(&format!("; {name}\n{}\n", ctx.func.display()));
    m.define_function(id, &mut ctx)
        .map_err(|e| format!("native session codegen '{}': {e}", plan.entry))?;
    m.clear_context(&mut ctx);
    Ok((id, text))
}

/// Reject any use, after a park, of a data-frame address derived from a base
/// that was loaded before that park.
///
/// Bases are loads of `ctx.data_base` and of `ResumeSignal::data_base` from a
/// park's returned signal. Addresses derive from a base through any
/// non-memory instruction and through block parameters. A use is stale if
/// some control path from the base's definition to the use crosses a call to
/// `park` without passing the base's definition again.
pub fn check_data_base_freshness(
    func: &ir::Function,
    ctx: ir::Value,
    park: ir::FuncRef,
) -> Result<(), String> {
    let dfg = &func.dfg;
    let resolve = |v: ir::Value| dfg.resolve_aliases(v);
    let ctx = resolve(ctx);
    let is_park = |inst: ir::Inst| match dfg.insts[inst] {
        InstructionData::Call { func_ref, .. } => func_ref == park,
        _ => false,
    };
    let mut blocks = vec![];
    let mut position = BTreeMap::new();
    for block in func.layout.blocks() {
        let insts = func.layout.block_insts(block).collect::<Vec<_>>();
        for (i, inst) in insts.iter().enumerate() {
            position.insert(*inst, (block, i));
        }
        blocks.push((block, insts));
    }
    let insts_of = blocks.iter().cloned().collect::<BTreeMap<_, _>>();
    // Base roots and their defining instruction.
    let mut roots = BTreeMap::new();
    for (_, insts) in &blocks {
        for inst in insts {
            if let InstructionData::Load {
                opcode: Opcode::Load,
                arg,
                offset,
                ..
            } = dfg.insts[*inst]
            {
                let arg = resolve(arg);
                let offset = i32::from(offset);
                let from_ctx = arg == ctx && offset == offsets::CTX_DATA_BASE;
                let from_signal = offset == offsets::RESUME_DATA_BASE
                    && matches!(dfg.value_def(arg), ir::ValueDef::Result(def, 0) if is_park(def));
                if from_ctx || from_signal {
                    roots.insert(dfg.inst_results(*inst)[0], *inst);
                }
            }
        }
    }
    if roots.is_empty() {
        return Ok(());
    }
    // Which roots each value derives from.
    let mut derived: BTreeMap<ir::Value, BTreeSet<ir::Value>> =
        roots.keys().map(|r| (*r, BTreeSet::from([*r]))).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for (_, insts) in &blocks {
            for inst in insts {
                let data = &dfg.insts[*inst];
                let memory = matches!(
                    data.opcode(),
                    Opcode::Load | Opcode::Store | Opcode::Call | Opcode::CallIndirect
                ) || data.opcode().can_load()
                    || data.opcode().can_store();
                let mut incoming = BTreeSet::new();
                for arg in dfg.inst_args(*inst) {
                    if let Some(set) = derived.get(&resolve(*arg)) {
                        incoming.extend(set.iter().copied());
                    }
                }
                if !memory && !incoming.is_empty() {
                    for result in dfg.inst_results(*inst) {
                        let entry = derived.entry(*result).or_default();
                        let before = entry.len();
                        entry.extend(incoming.iter().copied());
                        changed |= entry.len() != before;
                    }
                }
                for call in data.branch_destination(&dfg.jump_tables, &dfg.exception_tables) {
                    let target = call.block(&dfg.value_lists);
                    let params = dfg.block_params(target);
                    for (param, arg) in params.iter().zip(call.args(&dfg.value_lists)) {
                        if let Some(value) = arg.as_value()
                            && let Some(set) = derived.get(&resolve(value)).cloned()
                        {
                            let entry = derived.entry(*param).or_default();
                            let before = entry.len();
                            entry.extend(set);
                            changed |= entry.len() != before;
                        }
                    }
                }
            }
        }
    }
    for (root, def) in &roots {
        let (block, index) = position[def];
        let mut queue = VecDeque::from([(block, index + 1, false)]);
        let mut visited = BTreeSet::new();
        while let Some((block, start, mut crossed)) = queue.pop_front() {
            let insts = &insts_of[&block];
            let mut reached_end = true;
            for inst in &insts[start..] {
                if inst == def {
                    // Redefined: later uses observe the fresh value.
                    reached_end = false;
                    break;
                }
                if crossed {
                    for value in dfg.inst_values(*inst) {
                        if derived
                            .get(&resolve(value))
                            .is_some_and(|set| set.contains(root))
                        {
                            return Err(format!(
                                "stale data-frame base: {value} (derived from base {root} loaded at {def}) is used by {inst} after a park"
                            ));
                        }
                    }
                }
                if is_park(*inst) {
                    crossed = true;
                }
            }
            if !reached_end {
                continue;
            }
            if let Some(last) = insts.last() {
                for call in
                    dfg.insts[*last].branch_destination(&dfg.jump_tables, &dfg.exception_tables)
                {
                    let target = call.block(&dfg.value_lists);
                    if visited.insert((target, crossed)) {
                        queue.push_back((target, 0, crossed));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Codegen flags for a session module. DetFloat programs
/// ([`super::needs_nan_canonicalization`]) get Cranelift's NaN
/// canonicalisation, as object emission and the JIT do (lane 23, #346).
fn isa_flags(world: &World, pic: bool) -> Result<cranelift_codegen::settings::Flags, String> {
    use cranelift_codegen::settings::{self, Configurable};
    let mut flags = settings::builder();
    if super::needs_nan_canonicalization(world) {
        flags
            .set("enable_nan_canonicalization", "true")
            .map_err(|e| e.to_string())?;
    }
    if !pic {
        flags
            .set("use_colocated_libcalls", "false")
            .map_err(|e| e.to_string())?;
    }
    flags
        .set("is_pic", if pic { "true" } else { "false" })
        .map_err(|e| e.to_string())?;
    Ok(settings::Flags::new(flags))
}

/// Emit a relocatable object containing the session entry
/// (`vesper_session_<entry>`, ABI v2) for the host or for `target`
/// (x86_64 or AArch64). It imports `vesper_fiber_park`, which a linked
/// runtime must export, and `fmod`.
pub fn object(
    world: &World,
    entry: &str,
    bindings: &[EffectBinding],
    target: Option<&str>,
) -> Result<(Vec<u8>, String), String> {
    let plan = plan(world, entry, bindings)?;
    let builder = if let Some(target) = target {
        let triple: target_lexicon::Triple =
            target.parse().map_err(|e| format!("invalid target: {e}"))?;
        if !matches!(
            triple.architecture,
            target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::Aarch64(_)
        ) {
            return Err("native session targets must be x86_64 or AArch64".into());
        }
        cranelift_codegen::isa::lookup(triple).map_err(|e| e.to_string())?
    } else {
        cranelift_native::builder().map_err(str::to_string)?
    };
    let isa = builder
        .finish(isa_flags(world, true)?)
        .map_err(|e| e.to_string())?;
    let builder = cranelift_object::ObjectBuilder::new(
        isa,
        "vesper-session",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| e.to_string())?;
    let mut m = cranelift_object::ObjectModule::new(builder);
    let (_, clif) = compile_session(&mut m, world, &plan, true)?;
    let bytes = m.finish().emit().map_err(|e| e.to_string())?;
    Ok((bytes, clif))
}

/// The ABI v2 symbol name of a session entry in emitted objects.
pub fn entry_symbol(entry: &str) -> String {
    symbol(entry)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use host::*;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod host {
    use super::*;
    use crate::runtime::Value;
    use std::{fs::File, io, rc::Rc};
    use vesper_fiber_abi::{
        evidence::{EvidenceRegistry, EvidenceVector, Installation},
        fiber::{EntryFn, Exit, Fiber, FiberConfig},
        pulse::{Completed, NativeRuntime, NativeTask},
    };
    use vesper_pulse::scheduler::Driver;

    struct Code {
        module: Option<cranelift_jit::JITModule>,
    }
    impl Drop for Code {
        fn drop(&mut self) {
            if let Some(module) = self.module.take() {
                // SAFETY: the last share is gone, so no fiber stack can
                // return into this code.
                unsafe { module.free_memory() };
            }
        }
    }

    /// How a session fiber ended.
    #[derive(Debug)]
    pub enum SessionResult {
        /// The entry returned this value (possibly after handling a
        /// cancellation result in source).
        Returned(Value),
        /// An unhandled raise of the entry's `Fail` instance `instance`.
        Failed { instance: usize, error: Value },
        /// Cancelled before start: no generated code ran.
        CancelledBeforeStart,
    }

    /// A JIT-compiled session for the host. Each fiber holds a share of the
    /// machine code, so dropping this handle never unmaps a parked stack's
    /// return addresses.
    pub struct NativeSession {
        code: Rc<Code>,
        entry: EntryFn,
        plan: Plan,
        clif: String,
    }

    impl std::fmt::Debug for NativeSession {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NativeSession")
                .field("plan", &self.plan)
                .finish_non_exhaustive()
        }
    }

    impl NativeSession {
        /// Validate `bindings` against the checked `entry` and JIT it.
        pub fn compile(
            world: &World,
            entry: &str,
            bindings: &[EffectBinding],
        ) -> Result<Self, String> {
            Self::compile_with_options(world, entry, bindings, true)
        }
        pub fn compile_with_options(
            world: &World,
            entry: &str,
            bindings: &[EffectBinding],
            optimize: bool,
        ) -> Result<Self, String> {
            let plan = plan(world, entry, bindings)?;
            let isa = cranelift_native::builder()
                .map_err(str::to_string)?
                .finish(isa_flags(world, false)?)
                .map_err(|e| e.to_string())?;
            let mut builder =
                cranelift_jit::JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
            builder.symbol(
                clif::PARK_SYMBOL,
                vesper_fiber_abi::fiber::vesper_fiber_park as *const u8,
            );
            builder.symbol("fmod", super::super::native_float_remainder as *const u8);
            let mut module = cranelift_jit::JITModule::new(builder);
            let (id, clif) = compile_session(&mut module, world, &plan, optimize)?;
            module.finalize_definitions().map_err(|e| e.to_string())?;
            // SAFETY: `id` was defined with `clif::entry_signature`.
            let entry = unsafe {
                std::mem::transmute::<*const u8, EntryFn>(module.get_finalized_function(id))
            };
            Ok(Self {
                code: Rc::new(Code {
                    module: Some(module),
                }),
                entry,
                plan,
                clif,
            })
        }
        pub fn plan(&self) -> &Plan {
            &self.plan
        }
        pub fn clif(&self) -> &str {
            &self.clif
        }
        /// A fresh installation: one parking slot per bound effect, with
        /// never-reused identities.
        pub fn evidence(&self) -> Result<Rc<EvidenceVector>, String> {
            let handlers = (0..self.plan.effects.len())
                .map(|depth| Installation::parking(depth as u32 + 1))
                .collect::<Vec<_>>();
            EvidenceVector::install(&mut EvidenceRegistry::new(), &handlers)
                .map(Rc::new)
                .map_err(|e| e.to_string())
        }
        /// Check that `vector` has the slots generated code loads.
        pub fn check_evidence(&self, vector: &EvidenceVector) -> Result<(), String> {
            let slots = vector.slots();
            if slots.len() < self.plan.effects.len() {
                return Err(format!(
                    "session needs {} evidence slots, vector has {}",
                    self.plan.effects.len(),
                    slots.len()
                ));
            }
            for (depth, effect) in self.plan.effects.iter().enumerate() {
                let slot = &slots[depth];
                if slot.effect != depth as u32 + 1
                    || slot.depth as usize != depth
                    || slot.handler_fn.is_some()
                {
                    return Err(format!(
                        "evidence slot {depth} is not a parking installation of {}",
                        effect.name
                    ));
                }
            }
            Ok(())
        }
        pub fn fiber(&self) -> io::Result<Fiber> {
            self.fiber_with(self.evidence().map_err(io::Error::other)?)
        }
        pub fn fiber_with(&self, evidence: Rc<EvidenceVector>) -> io::Result<Fiber> {
            self.check_evidence(&evidence).map_err(io::Error::other)?;
            let config = FiberConfig {
                data_bytes: self.plan.frame.bytes as usize,
                ..Default::default()
            };
            // SAFETY: the entry follows the ABI entry contract, is owned by
            // `self.code`, and the evidence depths were checked above.
            unsafe { Fiber::new(self.entry, 0, evidence, self.code.clone(), config) }
        }
        /// Spawn a fresh fiber into an owned runtime. `file` is the source
        /// of every read operation.
        pub fn spawn<D: Driver<NativeTask>>(
            &self,
            runtime: &mut NativeRuntime<D>,
            file: Option<File>,
        ) -> io::Result<u64> {
            Ok(runtime.spawn(self.fiber()?, file))
        }
        fn decode_parts(
            &self,
            world: &World,
            status: u64,
            result: u64,
            data: &[u8],
            parks: Option<u64>,
        ) -> Result<SessionResult, String> {
            let frame = self.plan.frame;
            match status {
                exit::COMPLETE => {
                    let ty = &self.plan.result;
                    if Layout::of(world, ty)?.aggregate() {
                        let start = frame.result_offset as usize;
                        let bytes = data
                            .get(start..start + frame.result_size as usize)
                            .ok_or("session result outside the data frame")?;
                        crate::layout::decode(world, ty, bytes).map(SessionResult::Returned)
                    } else {
                        crate::layout::decode(world, ty, &result.to_ne_bytes())
                            .map(SessionResult::Returned)
                    }
                }
                exit::CANCELLED if parks.is_none_or(|p| p == 0) => {
                    Ok(SessionResult::CancelledBeforeStart)
                }
                exit::FAILED => {
                    let instance = usize::try_from(result)
                        .ok()
                        .and_then(|r| r.checked_sub(1))
                        .filter(|i| *i < self.plan.fails.len())
                        .ok_or("session failure names an unknown Fail instance")?;
                    let start = frame.failure_offset as usize;
                    let bytes = data
                        .get(start..start + frame.failure_size as usize)
                        .ok_or("session failure outside the data frame")?;
                    let error = crate::layout::decode(world, &self.plan.fails[instance], bytes)?;
                    Ok(SessionResult::Failed { instance, error })
                }
                other => Err(format!("session fiber ended with status {other}")),
            }
        }
        /// Decode a fiber exit from the direct fiber API.
        pub fn decode_exit(&self, world: &World, exit: &Exit) -> Result<SessionResult, String> {
            self.decode_parts(
                world,
                exit.status,
                exit.result,
                &exit.data,
                Some(exit.parks),
            )
        }
        /// Decode a completion collected by a [`NativeRuntime`].
        pub fn decode_completed(
            &self,
            world: &World,
            completed: &Completed,
        ) -> Result<SessionResult, String> {
            self.decode_parts(
                world,
                completed.status,
                completed.result,
                &completed.data,
                None,
            )
        }
    }
}

#[cfg(test)]
mod nan_tests {
    use super::isa_flags;

    fn world(source: &str) -> crate::check::World {
        let parsed = crate::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let checked = crate::check::check(&parsed.module);
        assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
        checked.world
    }

    #[test]
    fn detfloat_sessions_canonicalize_nans() {
        let det = world(
            "fn half(x: f64) -> f64 eff [DetFloat] {\n    x / 2.0\n}\n\nfn main() -> f64 {\n    half(3.0)\n}\n",
        );
        let plain = world("fn main() -> f64 {\n    1.5\n}\n");
        for pic in [false, true] {
            assert!(isa_flags(&det, pic).unwrap().enable_nan_canonicalization());
            assert!(
                !isa_flags(&plain, pic)
                    .unwrap()
                    .enable_nan_canonicalization()
            );
        }
    }
}
