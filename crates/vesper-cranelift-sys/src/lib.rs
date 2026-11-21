//! A C-ABI shim over Cranelift 0.125 (self-hosting wave SH0, lane 4, #1126).
//!
//! Stage 2 keeps Cranelift as a vetted external library (owner decision,
//! 2026-11-19) and moves only the compiler's lowering to Vesper. This crate
//! is the boundary: Vesper imports these functions through the `.vsys`
//! library in `packages/vesper-cranelift`, and C callers use
//! `include/vesper_cranelift.h`.
//!
//! Design:
//! - Four opaque handles, each a `linear extern type` on the Vesper side:
//!   [`Module`] (an `ObjectModule`, its signatures, function and data ids and
//!   a reusable `codegen::Context`), [`Builder`] (a `FunctionBuilder` with its
//!   own `Function` and `FunctionBuilderContext`), [`Data`] (a
//!   `DataDescription`) and [`Object`] (the emitted object bytes). Each has
//!   exactly one finalizer, and the consuming calls (`vcl_define_function`,
//!   `vcl_define_data`, `vcl_module_finish`, `vcl_object_publish`) take
//!   ownership.
//! - Everything else is a plain integer: values, blocks, variables, stack
//!   slots, function and signature references and global values are
//!   Cranelift's own entity numbers (so a Vesper lowering and a Rust one
//!   number them the same), module-level signatures, functions and data are
//!   indices into the module's tables, and types, opcodes, condition codes,
//!   linkages, calling conventions, trap codes and memory flags are the codes
//!   of [`codes`].
//! - Instructions are grouped by shape (`vcl_binary(b, op, x, y)`,
//!   `vcl_icmp(b, cc, x, y)`, ...), so about forty calls reach the 99
//!   instructions the compiler uses. `vcl_emit` takes a batch of
//!   word-encoded instructions in one call.
//! - Every `vcl_*` function is `unsafe`: its pointer arguments must be live
//!   handles of the right kind (or null) and `(ptr, len)` pairs valid for
//!   the call, which is what the `.vsys` declarations guarantee to Vesper.
//! - No Cranelift panic crosses the C boundary: every call runs under
//!   `catch_unwind`. A failed call returns [`INVALID`] (or a null handle) and
//!   records a sticky error on its handle; `vcl_define_function` reports it
//!   and `vcl_*_error` reads the message.
#![allow(clippy::missing_safety_doc)]

pub mod codes;

/// The Cranelift crates the shim wraps, for Rust hosts that build the same
/// IR directly (the comparison tests).
pub use {
    cranelift_codegen, cranelift_frontend, cranelift_jit, cranelift_module, cranelift_native,
    cranelift_object,
};

pub mod lend;
pub use lend::{
    Crossing, lend, lend_module, vcl_call_arg, vcl_call_arg_count, vcl_call_args, vcl_call_builder,
    vcl_call_module, vcl_call_op, vcl_call_result, vcl_call_return_builder, vcl_call_return_module,
};

use cranelift_codegen::ir::{
    self, AbiParam, BlockArg, Function, InstBuilder, MemFlags, Signature, StackSlotData,
    StackSlotKind, UserFuncName,
};
use cranelift_codegen::{Context, settings, settings::Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// The result of a failed call that returns an entity number.
pub const INVALID: u32 = u32::MAX;
/// A `vcl_emit` operand with this bit set names the result of an earlier
/// instruction of the same batch (the low 32 bits are its index).
pub const BATCH_RESULT: u64 = 1 << 63;

// ---------------------------------------------------------------------------
// Live-handle counters: the leak tests read them through `live_handles()`.

static LIVE: [AtomicI64; 4] = [
    AtomicI64::new(0),
    AtomicI64::new(0),
    AtomicI64::new(0),
    AtomicI64::new(0),
];
const MODULE: usize = 0;
const BUILDER: usize = 1;
const DATA: usize = 2;
const OBJECT: usize = 3;

/// Live handles of each kind: `[modules, builders, datas, objects]`.
pub fn live_handles() -> [i64; 4] {
    std::array::from_fn(|i| LIVE[i].load(Ordering::SeqCst))
}
fn born(kind: usize) {
    LIVE[kind].fetch_add(1, Ordering::SeqCst);
}
fn died(kind: usize) {
    LIVE[kind].fetch_sub(1, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Published objects: a Rust host collects what Vesper finished.

static PUBLISHED: Mutex<BTreeMap<u64, Vec<u8>>> = Mutex::new(BTreeMap::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Take the object bytes `vcl_object_publish` returned `id` for.
pub fn take_published(id: u64) -> Option<Vec<u8>> {
    PUBLISHED.lock().unwrap().remove(&id)
}

// ---------------------------------------------------------------------------
// Handles.

/// A module under construction: an object file, a JIT image, or a module
/// a Rust host lends for one call ([`lend`]).
pub struct Module {
    m: Backend,
    sigs: Vec<Signature>,
    funcs: Vec<FuncId>,
    datas: Vec<DataId>,
    /// Made on first use, so a lent module costs no allocation.
    ctx: Option<Context>,
    error: Option<String>,
    /// Keep the CLIF text of each defined function (`vcl_module_keep_clif`).
    keep_clif: bool,
    clif: String,
    code_bytes: u64,
    /// The machine code of the last `vcl_compile` and the Windows x64
    /// unwind information of the last compiled or defined function.
    last_code: Vec<u8>,
    last_unwind: Vec<u8>,
}

/// The Cranelift module behind a [`Module`] handle.
// One per handle, and the object backend is the common one: boxing it
// would add an indirection to every call for no saving.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Object(ObjectModule),
    Jit(Box<JITModule>),
    /// A module the Rust host owns and lends for one hook call ([`lend`]).
    /// Its function and data indices are Cranelift's own ids.
    Lent(NonNull<dyn cranelift_module::Module>),
}

impl Module {
    fn get(&self) -> &dyn cranelift_module::Module {
        match &self.m {
            Backend::Object(m) => m,
            Backend::Jit(m) => &**m,
            // SAFETY: `lend` keeps the lent module alive and unaliased for
            // the whole call, and the handle never outlives it.
            Backend::Lent(m) => unsafe { m.as_ref() },
        }
    }
    fn get_mut(&mut self) -> &mut dyn cranelift_module::Module {
        match &mut self.m {
            Backend::Object(m) => m,
            Backend::Jit(m) => &mut **m,
            // SAFETY: as in `get`.
            Backend::Lent(m) => unsafe { m.as_mut() },
        }
    }
    fn lent(&self) -> bool {
        matches!(self.m, Backend::Lent(_))
    }
    /// The module and its reusable context, both mutable.
    fn parts(&mut self) -> (&mut dyn cranelift_module::Module, &mut Context) {
        if self.ctx.is_none() {
            self.ctx = Some(self.get().make_context());
        }
        let ctx = self.ctx.as_mut().expect("context made above");
        let m: &mut dyn cranelift_module::Module = match &mut self.m {
            Backend::Object(m) => m,
            Backend::Jit(m) => &mut **m,
            // SAFETY: as in `get`.
            Backend::Lent(m) => unsafe { m.as_mut() },
        };
        (m, ctx)
    }
    fn empty(m: Backend) -> Module {
        Module {
            m,
            sigs: Vec::new(),
            funcs: Vec::new(),
            datas: Vec::new(),
            ctx: None,
            error: None,
            keep_clif: false,
            clif: String::new(),
            code_bytes: 0,
            last_code: Vec::new(),
            last_unwind: Vec::new(),
        }
    }
}

/// A function under construction. `fb` borrows `func` and `fctx`, which are
/// boxed so they never move while it lives.
pub struct Builder {
    fb: Option<FunctionBuilder<'static>>,
    /// A builder the Rust host lends for one hook call ([`lend`]): calls
    /// emit into it, and nothing here owns or finalizes it.
    lent: Option<NonNull<FunctionBuilder<'static>>>,
    func: Option<NonNull<Function>>,
    fctx: Option<NonNull<FunctionBuilderContext>>,
    last: Option<ir::Inst>,
    error: Option<String>,
}

/// A data object's contents and relocations.
pub struct Data {
    d: DataDescription,
    error: Option<String>,
}

/// Finished object bytes.
pub struct Object {
    bytes: Vec<u8>,
}

impl Drop for Data {
    fn drop(&mut self) {
        died(DATA);
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        died(OBJECT);
    }
}
impl Drop for Builder {
    fn drop(&mut self) {
        if self.lent.is_some() {
            // Lent: the host owns the builder and counts nothing.
            return;
        }
        // The builder borrows the boxes: end it first.
        self.fb = None;
        if let Some(f) = self.func.take() {
            // SAFETY: allocated by Box::into_raw in Builder::new and not
            // yet reclaimed (the Option was Some).
            drop(unsafe { Box::from_raw(f.as_ptr()) });
        }
        if let Some(c) = self.fctx.take() {
            // SAFETY: as above.
            drop(unsafe { Box::from_raw(c.as_ptr()) });
        }
        died(BUILDER);
    }
}

impl Builder {
    fn new(sig: Signature) -> Box<Builder> {
        let func = NonNull::from(Box::leak(Box::new(Function::with_name_signature(
            UserFuncName::default(),
            sig,
        ))));
        let fctx = NonNull::from(Box::leak(Box::new(FunctionBuilderContext::new())));
        // SAFETY: both boxes live until Drop/finish, which drop `fb` first;
        // nothing else touches them while `fb` borrows them.
        let fb = unsafe { FunctionBuilder::new(&mut *func.as_ptr(), &mut *fctx.as_ptr()) };
        born(BUILDER);
        Box::new(Builder {
            fb: Some(fb),
            lent: None,
            func: Some(func),
            fctx: Some(fctx),
            last: None,
            error: None,
        })
    }
    /// Finalize the builder and hand back its function.
    fn finish(mut self) -> Result<Function, String> {
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        let fb = self.fb.take().ok_or("builder already finished")?;
        catch_unwind(AssertUnwindSafe(|| fb.finalize())).map_err(panic_message)?;
        let f = self.func.take().ok_or("builder already finished")?;
        // SAFETY: `fb` is gone, so the box is no longer borrowed.
        Ok(*unsafe { Box::from_raw(f.as_ptr()) })
    }
    fn fb(&mut self) -> &mut FunctionBuilder<'static> {
        if let Some(lent) = &mut self.lent {
            // SAFETY: `lend` keeps the lent builder alive and unaliased for
            // the whole call, and the handle never outlives it.
            return unsafe { lent.as_mut() };
        }
        self.fb.as_mut().expect("live builder")
    }
    fn live(&self) -> bool {
        self.fb.is_some() || self.lent.is_some()
    }
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        format!("cranelift panicked: {s}")
    } else if let Some(s) = p.downcast_ref::<String>() {
        format!("cranelift panicked: {s}")
    } else {
        "cranelift panicked".to_string()
    }
}

/// Run `f` on a live, error-free builder. Errors and panics poison it.
fn with_builder<T>(
    b: *mut Builder,
    fail: T,
    f: impl FnOnce(&mut Builder) -> Result<T, String>,
) -> T {
    // SAFETY: the `.vsys` contract: `b` is a live handle from vcl_builder_new.
    let Some(b) = (unsafe { b.as_mut() }) else {
        return fail;
    };
    if b.error.is_some() || !b.live() {
        return fail;
    }
    match catch_unwind(AssertUnwindSafe(|| f(b))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            b.error = Some(e);
            fail
        }
        Err(p) => {
            b.error = Some(panic_message(p));
            fail
        }
    }
}
fn with_module<T>(m: *mut Module, fail: T, f: impl FnOnce(&mut Module) -> Result<T, String>) -> T {
    // SAFETY: the `.vsys` contract: `m` is a live handle from vcl_module_new.
    let Some(m) = (unsafe { m.as_mut() }) else {
        return fail;
    };
    match catch_unwind(AssertUnwindSafe(|| f(m))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            m.error.get_or_insert(e);
            fail
        }
        Err(p) => {
            m.error.get_or_insert(panic_message(p));
            fail
        }
    }
}

/// # Safety
/// `ptr` is valid for `len` elements (the `.vsys` CSlice contract).
unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if len == 0 || ptr.is_null() {
        &[]
    } else {
        // SAFETY: the caller lends `len` elements at `ptr` for the call.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}
/// # Safety
/// As [`slice`], writable.
unsafe fn slice_mut<'a, T>(ptr: *mut T, len: usize) -> &'a mut [T] {
    if len == 0 || ptr.is_null() {
        &mut []
    } else {
        // SAFETY: the caller lends `len` elements at `ptr` for the call.
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }
}
fn text(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))
}
fn ty(code: u32) -> Result<ir::Type, String> {
    codes::ty(code).ok_or_else(|| format!("unknown type code {code}"))
}
fn value(v: u32) -> ir::Value {
    ir::Value::from_u32(v)
}
fn block(v: u32) -> ir::Block {
    ir::Block::from_u32(v)
}
fn values(ptr: *const u32, len: usize) -> Vec<ir::Value> {
    // SAFETY: the `.vsys` CSlice contract.
    unsafe { slice(ptr, len) }
        .iter()
        .map(|&v| value(v))
        .collect()
}
fn block_args(ptr: *const u32, len: usize) -> Vec<BlockArg> {
    values(ptr, len).into_iter().map(BlockArg::Value).collect()
}
fn copy_out(src: &[u8], offset: u64, out: *mut u8, len: usize) -> u64 {
    // SAFETY: the `.vsys` CSlice contract.
    let out = unsafe { slice_mut(out, len) };
    let from = usize::try_from(offset).unwrap_or(usize::MAX).min(src.len());
    let n = out.len().min(src.len() - from);
    out[..n].copy_from_slice(&src[from..from + n]);
    n as u64
}

// ---------------------------------------------------------------------------
// Modules.

/// The ISA for `triple` (empty: the host) with Cranelift `flags`.
fn isa_for(triple: &str, flags: &str) -> Result<cranelift_codegen::isa::OwnedTargetIsa, String> {
    let mut fb = settings::builder();
    for line in flags
        .split([';', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("flag '{line}' is not key=value"))?;
        fb.set(k.trim(), v.trim())
            .map_err(|e| format!("flag {k}: {e}"))?;
    }
    let flags = settings::Flags::new(fb);
    let isa_builder = if triple.is_empty() {
        cranelift_native::builder().map_err(str::to_string)?
    } else {
        let t: target_lexicon::Triple = triple.parse().map_err(|e| format!("triple: {e}"))?;
        cranelift_codegen::isa::lookup(t).map_err(|e| e.to_string())?
    };
    isa_builder.finish(flags).map_err(|e| e.to_string())
}

fn new_module(triple: &str, flags: &str, name: &str) -> Result<Module, String> {
    let isa = isa_for(triple, flags)?;
    let name = if name.is_empty() { "vesper" } else { name };
    let builder = ObjectBuilder::new(isa, name, cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    born(MODULE);
    Ok(Module::empty(Backend::Object(ObjectModule::new(builder))))
}

fn new_jit_module(flags: &str, symbols: &str, addrs: &[u64]) -> Result<Module, String> {
    let isa = isa_for("", flags)?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let names: Vec<&str> = symbols
        .split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if names.len() != addrs.len() {
        return Err(format!(
            "{} symbol names for {} addresses",
            names.len(),
            addrs.len()
        ));
    }
    for (name, &addr) in names.into_iter().zip(addrs) {
        builder.symbol(name, addr as usize as *const u8);
    }
    born(MODULE);
    Ok(Module::empty(Backend::Jit(Box::new(JITModule::new(
        builder,
    )))))
}

/// The error of the last module constructor that returned null.
static NEW_ERROR: Mutex<String> = Mutex::new(String::new());

/// Create an object module for `triple` (empty: the host) with Cranelift
/// `flags` (`key=value` pairs separated by `;` or newlines). Returns null on
/// error; `vcl_new_error` reads the message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_new(
    triple: *const u8,
    triple_len: usize,
    flags: *const u8,
    flags_len: usize,
    name: *const u8,
    name_len: usize,
) -> *mut Module {
    let made = catch_unwind(|| {
        // SAFETY: the `.vsys` CSlice contract.
        let (t, f, n) = unsafe {
            (
                slice(triple, triple_len),
                slice(flags, flags_len),
                slice(name, name_len),
            )
        };
        new_module(text(t)?, text(f)?, text(n)?)
    })
    .unwrap_or_else(|p| Err(panic_message(p)));
    match made {
        Ok(m) => Box::into_raw(Box::new(m)),
        Err(e) => {
            *NEW_ERROR.lock().unwrap() = e;
            std::ptr::null_mut()
        }
    }
}

/// Copy the last constructor error into `out`; returns its full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_new_error(out: *mut u8, out_len: usize) -> u64 {
    let e = NEW_ERROR.lock().unwrap();
    copy_out(e.as_bytes(), 0, out, out_len);
    e.len() as u64
}

/// Free a module without finishing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_free(m: *mut Module) {
    // SAFETY: a live handle; a lent one belongs to `lend`'s frame.
    if unsafe { m.as_ref() }.is_some_and(Module::lent) {
        return;
    }
    if !m.is_null() {
        // SAFETY: the sink import consumes the handle exactly once.
        let m = *unsafe { Box::from_raw(m) };
        if let Backend::Jit(j) = m.m {
            // SAFETY: freeing the module ends its code; the `.vsys`
            // contract is that no address it handed out is called after.
            unsafe { j.free_memory() };
        }
        died(MODULE);
    }
}

/// The module's first error (0 if none), copied into `out`; returns its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_error(m: *const Module, out: *mut u8, out_len: usize) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    let e = m.error.as_deref().unwrap_or("");
    copy_out(e.as_bytes(), 0, out, out_len);
    e.len() as u64
}

/// The target's pointer type code (`VCL_I64` on 64-bit targets).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_pointer_type(m: *const Module) -> u32 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(INVALID, |m| {
        codes::ty_code(m.get().target_config().pointer_type())
    })
}

/// Keep the CLIF text of every function defined from now on.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_keep_clif(m: *mut Module, keep: bool) {
    with_module(m, (), |m| {
        m.keep_clif = keep;
        Ok(())
    })
}

/// Bytes of machine code defined so far.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_code_bytes(m: *const Module) -> u64 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(0, |m| m.code_bytes)
}

/// Copy the kept CLIF text from `offset` into `out`; returns its full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_clif(
    m: *const Module,
    offset: u64,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    copy_out(m.clif.as_bytes(), offset, out, out_len);
    m.clif.len() as u64
}

/// Rust hosts: the kept CLIF text.
pub fn module_clif(m: &Module) -> &str {
    &m.clif
}

/// A new signature with calling convention `call_conv`; returns its index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_sig_new(m: *mut Module, call_conv: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let cc = codes::call_conv(call_conv, m.get().isa().default_call_conv())
            .ok_or_else(|| format!("unknown calling convention {call_conv}"))?;
        m.sigs.push(Signature::new(cc));
        Ok((m.sigs.len() - 1) as u32)
    })
}
fn sig_mut(m: &mut Module, sig: u32) -> Result<&mut Signature, String> {
    m.sigs
        .get_mut(sig as usize)
        .ok_or_else(|| format!("unknown signature {sig}"))
}
/// Append a parameter of type `ty` to signature `sig`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_sig_param(m: *mut Module, sig: u32, ty_code: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let t = ty(ty_code)?;
        let s = sig_mut(m, sig)?;
        s.params.push(AbiParam::new(t));
        Ok((s.params.len() - 1) as u32)
    })
}
/// Append a result of type `ty` to signature `sig`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_sig_return(m: *mut Module, sig: u32, ty_code: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let t = ty(ty_code)?;
        let s = sig_mut(m, sig)?;
        s.returns.push(AbiParam::new(t));
        Ok((s.returns.len() - 1) as u32)
    })
}

/// Declare function `name` with `linkage` and signature `sig`; returns the
/// module's function index (declaring a name again returns the same index).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_declare_function(
    m: *mut Module,
    name: *const u8,
    name_len: usize,
    linkage: u32,
    sig: u32,
) -> u32 {
    with_module(m, INVALID, |m| {
        // SAFETY: the `.vsys` CSlice contract.
        let name = text(unsafe { slice(name, name_len) })?;
        let l = codes::linkage(linkage).ok_or_else(|| format!("unknown linkage {linkage}"))?;
        let s = m
            .sigs
            .get(sig as usize)
            .ok_or_else(|| format!("unknown signature {sig}"))?
            .clone();
        let id = m
            .get_mut()
            .declare_function(name, l, &s)
            .map_err(|e| e.to_string())?;
        Ok(func_index(m, id))
    })
}
/// Declare data `name`; returns the module's data index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_declare_data(
    m: *mut Module,
    name: *const u8,
    name_len: usize,
    linkage: u32,
    writable: bool,
    tls: bool,
) -> u32 {
    with_module(m, INVALID, |m| {
        // SAFETY: the `.vsys` CSlice contract.
        let name = text(unsafe { slice(name, name_len) })?;
        let l = codes::linkage(linkage).ok_or_else(|| format!("unknown linkage {linkage}"))?;
        let id = m
            .get_mut()
            .declare_data(name, l, writable, tls)
            .map_err(|e| e.to_string())?;
        Ok(data_index(m, id))
    })
}
/// The handle's index of a declared function: Cranelift's own id in a lent
/// module, else a slot of the module's table.
fn func_index(m: &mut Module, id: FuncId) -> u32 {
    if m.lent() {
        id.as_u32()
    } else {
        index_of(&mut m.funcs, id)
    }
}
fn data_index(m: &mut Module, id: DataId) -> u32 {
    if m.lent() {
        id.as_u32()
    } else {
        index_of(&mut m.datas, id)
    }
}
fn index_of<T: PartialEq>(table: &mut Vec<T>, id: T) -> u32 {
    if let Some(i) = table.iter().position(|x| *x == id) {
        return i as u32;
    }
    table.push(id);
    (table.len() - 1) as u32
}
fn func_id(m: &Module, f: u32) -> Result<FuncId, String> {
    if m.lent() {
        return Ok(FuncId::from_u32(f));
    }
    m.funcs
        .get(f as usize)
        .copied()
        .ok_or_else(|| format!("unknown function {f}"))
}
fn data_id(m: &Module, d: u32) -> Result<DataId, String> {
    if m.lent() {
        return Ok(DataId::from_u32(d));
    }
    m.datas
        .get(d as usize)
        .copied()
        .ok_or_else(|| format!("unknown data {d}"))
}

/// Compile and define function `func` from the finished builder `b`
/// (consumed). Returns 0, or 1 with the module's error set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_define_function(m: *mut Module, func: u32, b: *mut Builder) -> u32 {
    let Some(b) = (unsafe { take_builder(b) }) else {
        return with_module(m, 1, |_| {
            Err("a lent or null builder cannot be defined".into())
        });
    };
    with_module(m, 1, |m| {
        let id = func_id(m, func)?;
        let f = b.finish()?;
        let keep_clif = m.keep_clif;
        let (module, ctx) = m.parts();
        ctx.func = f;
        let clif = keep_clif.then(|| ctx.func.display().to_string());
        let r = module.define_function(id, ctx);
        let r = finished(module.isa(), ctx, r);
        module.clear_context(ctx);
        if let Some(clif) = clif {
            m.clif.push_str(&clif);
        }
        let (code, unwind) = r?;
        m.code_bytes += code.len() as u64;
        m.last_unwind = unwind;
        Ok(0)
    })
}

/// Take ownership of builder `b` for a consuming call. A lent builder
/// stays with its host and a null one is refused: both are `None`.
///
/// # Safety
/// `b` is null or a live handle.
unsafe fn take_builder(b: *mut Builder) -> Option<Box<Builder>> {
    // SAFETY: a live handle or null.
    let lent = unsafe { b.as_ref() }.is_none_or(|b| b.lent.is_some());
    // SAFETY: an owned handle, consumed exactly once by the sink import.
    (!lent).then(|| unsafe { Box::from_raw(b) })
}

/// The compiled code and Windows x64 unwind information of the function
/// `ctx` just compiled, or the compile's error with verifier errors
/// pretty-printed.
fn finished(
    isa: &dyn cranelift_codegen::isa::TargetIsa,
    ctx: &Context,
    r: Result<(), cranelift_module::ModuleError>,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    match r {
        Ok(()) => {
            let Some(c) = ctx.compiled_code() else {
                return Ok((Vec::new(), Vec::new()));
            };
            let mut unwind = Vec::new();
            if isa.triple().operating_system == target_lexicon::OperatingSystem::Windows
                && let Ok(Some(cranelift_codegen::isa::unwind::UnwindInfo::WindowsX64(info))) =
                    c.create_unwind_info(isa)
            {
                unwind = vec![0; info.emit_size()];
                info.emit(&mut unwind);
            }
            Ok((c.code_buffer().to_vec(), unwind))
        }
        Err(cranelift_module::ModuleError::Compilation(
            cranelift_codegen::CodegenError::Verifier(errs),
        )) => Err(cranelift_codegen::print_errors::pretty_verifier_error(
            &ctx.func, None, errs,
        )),
        Err(e) => Err(format!("{e:?}")),
    }
}

/// Define data `data` from description `d` (consumed). Returns 0 or 1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_define_data(m: *mut Module, data: u32, d: *mut Data) -> u32 {
    // SAFETY: the sink import consumes the description exactly once.
    let d = (!d.is_null()).then(|| unsafe { Box::from_raw(d) });
    with_module(m, 1, |m| {
        let id = data_id(m, data)?;
        let mut d = d.ok_or("null data")?;
        if let Some(e) = d.error.take() {
            return Err(e);
        }
        m.get_mut()
            .define_data(id, &d.d)
            .map_err(|e| e.to_string())?;
        Ok(0)
    })
}

/// Finish the module (consumed) and emit its object. Returns null on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_finish(m: *mut Module) -> *mut Object {
    if m.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a live handle.
    if let Some(lent) = unsafe { m.as_mut() }.filter(|m| !matches!(m.m, Backend::Object(_))) {
        let e = if lent.lent() {
            "a lent module is finished by its host"
        } else {
            "a JIT module is finalized (vcl_module_finalize), not finished"
        };
        lent.error.get_or_insert(e.into());
        *NEW_ERROR.lock().unwrap() = e.into();
        return std::ptr::null_mut();
    }
    // SAFETY: the sink import consumes the module exactly once.
    let m = *unsafe { Box::from_raw(m) };
    died(MODULE);
    if let Some(e) = &m.error {
        *NEW_ERROR.lock().unwrap() = e.clone();
        return std::ptr::null_mut();
    }
    let Backend::Object(object) = m.m else {
        unreachable!("only object modules reach here");
    };
    let made = catch_unwind(AssertUnwindSafe(move || {
        object.finish().emit().map_err(|e| e.to_string())
    }))
    .unwrap_or_else(|p| Err(panic_message(p)));
    match made {
        Ok(bytes) => {
            born(OBJECT);
            Box::into_raw(Box::new(Object { bytes }))
        }
        Err(e) => {
            *NEW_ERROR.lock().unwrap() = e;
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Objects.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_object_len(o: *const Object) -> u64 {
    // SAFETY: a live handle.
    unsafe { o.as_ref() }.map_or(0, |o| o.bytes.len() as u64)
}
/// Copy object bytes from `offset` into `out`; returns how many were copied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_object_read(
    o: *const Object,
    offset: u64,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    unsafe { o.as_ref() }.map_or(0, |o| copy_out(&o.bytes, offset, out, out_len))
}
/// Hand the object (consumed) to the Rust host; returns the id
/// [`take_published`] takes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_object_publish(o: *mut Object) -> u64 {
    if o.is_null() {
        return 0;
    }
    // SAFETY: the sink import consumes the object exactly once.
    let mut o = unsafe { Box::from_raw(o) };
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    PUBLISHED
        .lock()
        .unwrap()
        .insert(id, std::mem::take(&mut o.bytes));
    id
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_object_free(o: *mut Object) {
    if !o.is_null() {
        // SAFETY: the sink import consumes the object exactly once.
        drop(unsafe { Box::from_raw(o) });
    }
}

// ---------------------------------------------------------------------------
// Data descriptions.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_new() -> *mut Data {
    born(DATA);
    Box::into_raw(Box::new(Data {
        d: DataDescription::new(),
        error: None,
    }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_free(d: *mut Data) {
    if !d.is_null() {
        // SAFETY: the sink import consumes the description exactly once.
        drop(unsafe { Box::from_raw(d) });
    }
}
fn with_data(d: *mut Data, f: impl FnOnce(&mut Data) -> Result<(), String>) {
    // SAFETY: a live handle.
    let Some(d) = (unsafe { d.as_mut() }) else {
        return;
    };
    if d.error.is_some() {
        return;
    }
    match catch_unwind(AssertUnwindSafe(|| f(d))) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => d.error = Some(e),
        Err(p) => d.error = Some(panic_message(p)),
    }
}
/// Initialize the contents with `bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_define(d: *mut Data, bytes: *const u8, len: usize) {
    with_data(d, |d| {
        // SAFETY: the `.vsys` CSlice contract.
        d.d.define(unsafe { slice(bytes, len) }.into());
        Ok(())
    })
}
/// Initialize the contents with `size` zero bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_zeroinit(d: *mut Data, size: u64) {
    with_data(d, |d| {
        d.d.define_zeroinit(usize::try_from(size).map_err(|e| e.to_string())?);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_align(d: *mut Data, align: u64) {
    with_data(d, |d| {
        d.d.set_align(align);
        Ok(())
    })
}
/// Write the address of function `func` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_func_addr(
    d: *mut Data,
    m: *const Module,
    offset: u32,
    func: u32,
) {
    with_data(d, |d| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_ref() }.ok_or("null module")?;
        let r = m.get().declare_func_in_data(func_id(m, func)?, &mut d.d);
        d.d.write_function_addr(offset, r);
        Ok(())
    })
}
/// Write the address of data `data` plus `addend` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_data_data_addr(
    d: *mut Data,
    m: *const Module,
    offset: u32,
    data: u32,
    addend: i64,
) {
    with_data(d, |d| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_ref() }.ok_or("null module")?;
        let gv = m.get().declare_data_in_data(data_id(m, data)?, &mut d.d);
        d.d.write_data_addr(offset, gv, addend);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Builders.

/// A builder for a function of signature `sig`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_builder_new(m: *const Module, sig: u32) -> *mut Builder {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return std::ptr::null_mut();
    };
    match m.sigs.get(sig as usize) {
        Some(s) => Box::into_raw(Builder::new(s.clone())),
        None => std::ptr::null_mut(),
    }
}
/// Abandon a builder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_builder_free(b: *mut Builder) {
    // A lent builder stays with its host (`vcl_call_return_builder`).
    // SAFETY: the sink import consumes the handle exactly once.
    drop(unsafe { take_builder(b) });
}
/// The builder's error, copied into `out`; returns its length (0: none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_builder_error(b: *const Builder, out: *mut u8, out_len: usize) -> u64 {
    // SAFETY: a live handle.
    let Some(b) = (unsafe { b.as_ref() }) else {
        return 0;
    };
    let e = b.error.as_deref().unwrap_or("");
    copy_out(e.as_bytes(), 0, out, out_len);
    e.len() as u64
}

/// Import module function `func` into the function; returns a FuncRef.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_import_function(b: *mut Builder, m: *mut Module, func: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_mut() }.ok_or("null module")?;
        let id = func_id(m, func)?;
        Ok(m.get_mut().declare_func_in_func(id, b.fb().func).as_u32())
    })
}
/// Import module signature `sig` (for `vcl_call_indirect`); returns a SigRef.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_import_signature(b: *mut Builder, m: *const Module, sig: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_ref() }.ok_or("null module")?;
        let s = m
            .sigs
            .get(sig as usize)
            .ok_or_else(|| format!("unknown signature {sig}"))?;
        Ok(b.fb().import_signature(s.clone()).as_u32())
    })
}
/// Import module data `data`; returns a GlobalValue.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_import_data(b: *mut Builder, m: *const Module, data: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_ref() }.ok_or("null module")?;
        let id = data_id(m, data)?;
        Ok(m.get().declare_data_in_func(id, b.fb().func).as_u32())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_create_block(b: *mut Builder) -> u32 {
    with_builder(b, INVALID, |b| Ok(b.fb().create_block().as_u32()))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_append_block_param(b: *mut Builder, blk: u32, ty_code: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        Ok(b.fb().append_block_param(block(blk), t).as_u32())
    })
}
/// Give `blk` the function's parameters; returns how many.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_append_block_params_for_function_params(
    b: *mut Builder,
    blk: u32,
) -> u32 {
    with_builder(b, INVALID, |b| {
        let fb = b.fb();
        fb.append_block_params_for_function_params(block(blk));
        Ok(fb.block_params(block(blk)).len() as u32)
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_block_param(b: *mut Builder, blk: u32, i: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let fb = b.fb();
        fb.block_params(block(blk))
            .get(i as usize)
            .map(|v| v.as_u32())
            .ok_or_else(|| format!("block{blk} has no parameter {i}"))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_switch_to_block(b: *mut Builder, blk: u32) {
    with_builder(b, (), |b| {
        b.fb().switch_to_block(block(blk));
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_seal_block(b: *mut Builder, blk: u32) {
    with_builder(b, (), |b| {
        b.fb().seal_block(block(blk));
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_seal_all_blocks(b: *mut Builder) {
    with_builder(b, (), |b| {
        b.fb().seal_all_blocks();
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_declare_var(b: *mut Builder, ty_code: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        Ok(b.fb().declare_var(t).as_u32())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_def_var(b: *mut Builder, var: u32, v: u32) {
    with_builder(b, (), |b| {
        b.fb().def_var(Variable::from_u32(var), value(v));
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_use_var(b: *mut Builder, var: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        Ok(b.fb().use_var(Variable::from_u32(var)).as_u32())
    })
}
/// An explicit stack slot of `size` bytes aligned to `1 << align_shift`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_stack_slot(b: *mut Builder, size: u32, align_shift: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let shift = u8::try_from(align_shift).map_err(|e| e.to_string())?;
        Ok(b.fb()
            .create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, size, shift))
            .as_u32())
    })
}
/// Result `i` of the last instruction emitted.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_inst_result(b: *mut Builder, i: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let inst = b.last.ok_or("no instruction yet")?;
        b.fb()
            .inst_results(inst)
            .get(i as usize)
            .map(|v| v.as_u32())
            .ok_or_else(|| format!("the last instruction has no result {i}"))
    })
}
/// The type code of value `v`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_value_type(b: *mut Builder, v: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        Ok(codes::ty_code(b.fb().func.dfg.value_type(value(v))))
    })
}

/// Record the instruction that defined `v` (for `vcl_inst_result`).
fn defined(b: &mut Builder, v: ir::Value) -> u32 {
    b.last = b.fb().func.dfg.value_def(v).inst();
    v.as_u32()
}
fn placed(b: &mut Builder, i: ir::Inst) {
    b.last = Some(i);
}

fn unknown(kind: &str, op: u32) -> String {
    format!("unknown {kind} opcode {op}")
}

fn binary(
    fb: &mut FunctionBuilder<'static>,
    op: u32,
    x: ir::Value,
    y: ir::Value,
) -> Result<ir::Value, String> {
    let i = fb.ins();
    Ok(match op {
        1 => i.iadd(x, y),
        2 => i.isub(x, y),
        3 => i.imul(x, y),
        4 => i.udiv(x, y),
        5 => i.sdiv(x, y),
        6 => i.urem(x, y),
        7 => i.srem(x, y),
        8 => i.umulhi(x, y),
        9 => i.smulhi(x, y),
        10 => i.band(x, y),
        11 => i.bor(x, y),
        12 => i.bxor(x, y),
        13 => i.band_not(x, y),
        14 => i.bor_not(x, y),
        15 => i.bxor_not(x, y),
        16 => i.ishl(x, y),
        17 => i.ushr(x, y),
        18 => i.sshr(x, y),
        19 => i.rotl(x, y),
        20 => i.rotr(x, y),
        21 => i.umin(x, y),
        22 => i.umax(x, y),
        23 => i.smin(x, y),
        24 => i.smax(x, y),
        25 => i.fadd(x, y),
        26 => i.fsub(x, y),
        27 => i.fmul(x, y),
        28 => i.fdiv(x, y),
        29 => i.fmin(x, y),
        30 => i.fmax(x, y),
        31 => i.fcopysign(x, y),
        32 => i.swizzle(x, y),
        33 => i.iadd_pairwise(x, y),
        34 => i.avg_round(x, y),
        35 => i.uadd_sat(x, y),
        36 => i.sadd_sat(x, y),
        37 => i.usub_sat(x, y),
        38 => i.ssub_sat(x, y),
        _ => return Err(unknown("binary", op)),
    })
}
fn binary_imm(
    fb: &mut FunctionBuilder<'static>,
    op: u32,
    x: ir::Value,
    imm: i64,
) -> Result<ir::Value, String> {
    let i = fb.ins();
    Ok(match op {
        1 => i.iadd_imm(x, imm),
        2 => i.imul_imm(x, imm),
        3 => i.udiv_imm(x, imm),
        4 => i.sdiv_imm(x, imm),
        5 => i.urem_imm(x, imm),
        6 => i.srem_imm(x, imm),
        7 => i.irsub_imm(x, imm),
        10 => i.band_imm(x, imm),
        11 => i.bor_imm(x, imm),
        12 => i.bxor_imm(x, imm),
        16 => i.ishl_imm(x, imm),
        17 => i.ushr_imm(x, imm),
        18 => i.sshr_imm(x, imm),
        19 => i.rotl_imm(x, imm),
        20 => i.rotr_imm(x, imm),
        _ => return Err(unknown("binary_imm", op)),
    })
}
fn unary(fb: &mut FunctionBuilder<'static>, op: u32, x: ir::Value) -> Result<ir::Value, String> {
    let i = fb.ins();
    Ok(match op {
        1 => i.ineg(x),
        2 => i.bnot(x),
        3 => i.clz(x),
        4 => i.ctz(x),
        5 => i.popcnt(x),
        6 => i.bswap(x),
        7 => i.bitrev(x),
        8 => i.cls(x),
        9 => i.iabs(x),
        10 => i.fneg(x),
        11 => i.fabs(x),
        12 => i.sqrt(x),
        13 => i.ceil(x),
        14 => i.floor(x),
        15 => i.trunc(x),
        16 => i.nearest(x),
        18 => i.vany_true(x),
        19 => i.vall_true(x),
        _ => return Err(unknown("unary", op)),
    })
}
fn convert(
    fb: &mut FunctionBuilder<'static>,
    op: u32,
    t: ir::Type,
    x: ir::Value,
) -> Result<ir::Value, String> {
    let i = fb.ins();
    Ok(match op {
        1 => i.uextend(t, x),
        2 => i.sextend(t, x),
        3 => i.ireduce(t, x),
        4 => i.bitcast(t, MemFlags::new(), x),
        5 => i.fpromote(t, x),
        6 => i.fdemote(t, x),
        7 => i.fcvt_to_uint(t, x),
        8 => i.fcvt_to_sint(t, x),
        9 => i.fcvt_to_uint_sat(t, x),
        10 => i.fcvt_to_sint_sat(t, x),
        11 => i.fcvt_from_uint(t, x),
        12 => i.fcvt_from_sint(t, x),
        13 => i.splat(t, x),
        14 => i.vhigh_bits(t, x),
        15 => {
            let mut f = MemFlags::new();
            f.set_endianness(ir::Endianness::Little);
            i.bitcast(t, f, x)
        }
        _ => return Err(unknown("convert", op)),
    })
}
fn icmp(
    fb: &mut FunctionBuilder<'static>,
    cc: u32,
    x: ir::Value,
    y: ir::Value,
) -> Result<ir::Value, String> {
    let cc = codes::intcc(cc).ok_or_else(|| format!("unknown IntCC {cc}"))?;
    Ok(fb.ins().icmp(cc, x, y))
}
fn icmp_imm(
    fb: &mut FunctionBuilder<'static>,
    cc: u32,
    x: ir::Value,
    imm: i64,
) -> Result<ir::Value, String> {
    let cc = codes::intcc(cc).ok_or_else(|| format!("unknown IntCC {cc}"))?;
    Ok(fb.ins().icmp_imm(cc, x, imm))
}
fn iconst(fb: &mut FunctionBuilder<'static>, ty_code: u32, imm: i64) -> Result<ir::Value, String> {
    let t = ty(ty_code)?;
    Ok(fb.ins().iconst(t, imm))
}
fn ternary(
    fb: &mut FunctionBuilder<'static>,
    op: u32,
    x: ir::Value,
    y: ir::Value,
    z: ir::Value,
) -> Result<ir::Value, String> {
    let i = fb.ins();
    Ok(match op {
        1 => i.select(x, y, z),
        2 => i.bitselect(x, y, z),
        3 => i.fma(x, y, z),
        4 => i.select_spectre_guard(x, y, z),
        _ => return Err(unknown("ternary", op)),
    })
}
fn flags(bits: u32) -> Result<MemFlags, String> {
    codes::mem_flags(bits).ok_or_else(|| format!("bad memory flags {bits:#x}"))
}
fn load(
    fb: &mut FunctionBuilder<'static>,
    op: u32,
    ty_code: u32,
    fl: u32,
    addr: ir::Value,
    offset: i32,
) -> Result<ir::Value, String> {
    let t = ty(ty_code)?;
    let f = flags(fl)?;
    let i = fb.ins();
    Ok(match op {
        1 => i.load(t, f, addr, offset),
        2 => i.uload8(t, f, addr, offset),
        3 => i.sload8(t, f, addr, offset),
        4 => i.uload16(t, f, addr, offset),
        5 => i.sload16(t, f, addr, offset),
        6 => i.uload32(f, addr, offset),
        7 => i.sload32(f, addr, offset),
        _ => return Err(unknown("load", op)),
    })
}
fn store(
    fb: &mut FunctionBuilder<'static>,
    op: u32,
    fl: u32,
    x: ir::Value,
    addr: ir::Value,
    offset: i32,
) -> Result<ir::Inst, String> {
    let f = flags(fl)?;
    let i = fb.ins();
    Ok(match op {
        1 => i.store(f, x, addr, offset),
        2 => i.istore8(f, x, addr, offset),
        3 => i.istore16(f, x, addr, offset),
        4 => i.istore32(f, x, addr, offset),
        _ => return Err(unknown("store", op)),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_iconst(b: *mut Builder, ty_code: u32, imm: i64) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = iconst(b.fb(), ty_code, imm)?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_f32const(b: *mut Builder, bits: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = b
            .fb()
            .ins()
            .f32const(ir::immediates::Ieee32::with_bits(bits));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_f64const(b: *mut Builder, bits: u64) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = b
            .fb()
            .ins()
            .f64const(ir::immediates::Ieee64::with_bits(bits));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_binary(b: *mut Builder, op: u32, x: u32, y: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = binary(b.fb(), op, value(x), value(y))?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_binary_imm(b: *mut Builder, op: u32, x: u32, imm: i64) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = binary_imm(b.fb(), op, value(x), imm)?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_unary(b: *mut Builder, op: u32, x: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = unary(b.fb(), op, value(x))?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_convert(b: *mut Builder, op: u32, ty_code: u32, x: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = convert(b.fb(), op, t, value(x))?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_ternary(b: *mut Builder, op: u32, x: u32, y: u32, z: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = ternary(b.fb(), op, value(x), value(y), value(z))?;
        Ok(defined(b, v))
    })
}
/// An overflow-checked operation: returns the value; the flag is
/// `vcl_inst_result(b, 1)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_overflow(b: *mut Builder, op: u32, x: u32, y: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let (x, y) = (value(x), value(y));
        let i = b.fb().ins();
        let (v, _) = match op {
            1 => i.uadd_overflow(x, y),
            2 => i.sadd_overflow(x, y),
            3 => i.usub_overflow(x, y),
            4 => i.ssub_overflow(x, y),
            5 => i.umul_overflow(x, y),
            6 => i.smul_overflow(x, y),
            _ => return Err(unknown("overflow", op)),
        };
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_icmp(b: *mut Builder, cc: u32, x: u32, y: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = icmp(b.fb(), cc, value(x), value(y))?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_icmp_imm(b: *mut Builder, cc: u32, x: u32, imm: i64) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = icmp_imm(b.fb(), cc, value(x), imm)?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_fcmp(b: *mut Builder, cc: u32, x: u32, y: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let c = codes::floatcc(cc).ok_or_else(|| format!("unknown FloatCC {cc}"))?;
        let v = b.fb().ins().fcmp(c, value(x), value(y));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_load(
    b: *mut Builder,
    op: u32,
    ty_code: u32,
    fl: u32,
    addr: u32,
    offset: i32,
) -> u32 {
    with_builder(b, INVALID, |b| {
        let v = load(b.fb(), op, ty_code, fl, value(addr), offset)?;
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_store(
    b: *mut Builder,
    op: u32,
    fl: u32,
    x: u32,
    addr: u32,
    offset: i32,
) {
    with_builder(b, (), |b| {
        let i = store(b.fb(), op, fl, value(x), value(addr), offset)?;
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_stack_addr(
    b: *mut Builder,
    ty_code: u32,
    slot: u32,
    offset: i32,
) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = b
            .fb()
            .ins()
            .stack_addr(t, ir::StackSlot::from_u32(slot), offset);
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_stack_load(
    b: *mut Builder,
    ty_code: u32,
    slot: u32,
    offset: i32,
) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = b
            .fb()
            .ins()
            .stack_load(t, ir::StackSlot::from_u32(slot), offset);
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_stack_store(b: *mut Builder, x: u32, slot: u32, offset: i32) {
    with_builder(b, (), |b| {
        let i = b
            .fb()
            .ins()
            .stack_store(value(x), ir::StackSlot::from_u32(slot), offset);
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_extractlane(b: *mut Builder, x: u32, lane: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let lane = u8::try_from(lane).map_err(|e| e.to_string())?;
        let v = b.fb().ins().extractlane(value(x), lane);
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_insertlane(b: *mut Builder, x: u32, y: u32, lane: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let lane = u8::try_from(lane).map_err(|e| e.to_string())?;
        let v = b.fb().ins().insertlane(value(x), value(y), lane);
        Ok(defined(b, v))
    })
}
/// A vector constant of type `ty` from little-endian `bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_vconst(
    b: *mut Builder,
    ty_code: u32,
    bytes: *const u8,
    len: usize,
) -> u32 {
    // SAFETY: the `.vsys` CSlice contract.
    let data = unsafe { slice(bytes, len) }.to_vec();
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let fb = b.fb();
        let c = fb.func.dfg.constants.insert(ir::ConstantData::from(data));
        let v = fb.ins().vconst(t, c);
        Ok(defined(b, v))
    })
}
/// `shuffle(x, y, mask)` with a 16-byte lane mask.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_shuffle(
    b: *mut Builder,
    x: u32,
    y: u32,
    mask: *const u8,
    len: usize,
) -> u32 {
    // SAFETY: the `.vsys` CSlice contract.
    let data = unsafe { slice(mask, len) }.to_vec();
    with_builder(b, INVALID, |b| {
        if data.len() != 16 {
            return Err(format!("a shuffle mask has 16 bytes, not {}", data.len()));
        }
        let fb = b.fb();
        let imm = fb.func.dfg.immediates.push(ir::ConstantData::from(data));
        let v = fb.ins().shuffle(value(x), value(y), imm);
        Ok(defined(b, v))
    })
}
/// `atomic_rmw` with op 1 add, 2 sub, 3 and, 4 nand, 5 or, 6 xor, 7 xchg,
/// 8 umin, 9 umax, 10 smin, 11 smax.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_atomic_rmw(
    b: *mut Builder,
    op: u32,
    ty_code: u32,
    fl: u32,
    addr: u32,
    x: u32,
) -> u32 {
    with_builder(b, INVALID, |b| {
        use ir::AtomicRmwOp as A;
        let rmw = match op {
            1 => A::Add,
            2 => A::Sub,
            3 => A::And,
            4 => A::Nand,
            5 => A::Or,
            6 => A::Xor,
            7 => A::Xchg,
            8 => A::Umin,
            9 => A::Umax,
            10 => A::Smin,
            11 => A::Smax,
            _ => return Err(unknown("atomic_rmw", op)),
        };
        let (t, f) = (ty(ty_code)?, flags(fl)?);
        let v = b.fb().ins().atomic_rmw(t, f, rmw, value(addr), value(x));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_atomic_load(b: *mut Builder, ty_code: u32, fl: u32, addr: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let (t, f) = (ty(ty_code)?, flags(fl)?);
        let v = b.fb().ins().atomic_load(t, f, value(addr));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_atomic_store(b: *mut Builder, fl: u32, x: u32, addr: u32) {
    with_builder(b, (), |b| {
        let f = flags(fl)?;
        let i = b.fb().ins().atomic_store(f, value(x), value(addr));
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_func_addr(b: *mut Builder, ty_code: u32, fref: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = b.fb().ins().func_addr(t, ir::FuncRef::from_u32(fref));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_symbol_value(b: *mut Builder, ty_code: u32, gv: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = b.fb().ins().symbol_value(t, ir::GlobalValue::from_u32(gv));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_global_value(b: *mut Builder, ty_code: u32, gv: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = b.fb().ins().global_value(t, ir::GlobalValue::from_u32(gv));
        Ok(defined(b, v))
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_get_stack_pointer(b: *mut Builder, ty_code: u32) -> u32 {
    with_builder(b, INVALID, |b| {
        let t = ty(ty_code)?;
        let v = b.fb().ins().get_stack_pointer(t);
        Ok(defined(b, v))
    })
}

/// Call `fref` with `args`; returns how many results it has (read them with
/// `vcl_inst_result`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call(
    b: *mut Builder,
    fref: u32,
    args: *const u32,
    args_len: usize,
) -> u32 {
    with_builder(b, INVALID, |b| {
        // Converted inside the panic boundary: an invalid number panics.
        let args = values(args, args_len);
        let i = b.fb().ins().call(ir::FuncRef::from_u32(fref), &args);
        placed(b, i);
        Ok(b.fb().inst_results(i).len() as u32)
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_indirect(
    b: *mut Builder,
    sigref: u32,
    callee: u32,
    args: *const u32,
    args_len: usize,
) -> u32 {
    with_builder(b, INVALID, |b| {
        // Converted inside the panic boundary: an invalid number panics.
        let args = values(args, args_len);
        let i = b
            .fb()
            .ins()
            .call_indirect(ir::SigRef::from_u32(sigref), value(callee), &args);
        placed(b, i);
        Ok(b.fb().inst_results(i).len() as u32)
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_return_call(
    b: *mut Builder,
    fref: u32,
    args: *const u32,
    args_len: usize,
) {
    with_builder(b, (), |b| {
        // Converted inside the panic boundary: an invalid number panics.
        let args = values(args, args_len);
        let i = b.fb().ins().return_call(ir::FuncRef::from_u32(fref), &args);
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_jump(b: *mut Builder, blk: u32, args: *const u32, args_len: usize) {
    with_builder(b, (), |b| {
        // Converted inside the panic boundary: an invalid number panics.
        let args = block_args(args, args_len);
        let i = b.fb().ins().jump(block(blk), &args);
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_brif(
    b: *mut Builder,
    c: u32,
    then_blk: u32,
    then_args: *const u32,
    then_len: usize,
    else_blk: u32,
    else_args: *const u32,
    else_len: usize,
) {
    with_builder(b, (), |b| {
        // Converted inside the panic boundary: an invalid number panics.
        let t = block_args(then_args, then_len);
        let e = block_args(else_args, else_len);
        let i = b
            .fb()
            .ins()
            .brif(value(c), block(then_blk), &t, block(else_blk), &e);
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_return(b: *mut Builder, vals: *const u32, len: usize) {
    with_builder(b, (), |b| {
        // Converted inside the panic boundary: an invalid number panics.
        let vals = values(vals, len);
        let i = b.fb().ins().return_(&vals);
        placed(b, i);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_trap(b: *mut Builder, code: u32) {
    with_builder(b, (), |b| {
        let t = codes::trap(code).ok_or_else(|| format!("unknown trap code {code}"))?;
        let i = b.fb().ins().trap(t);
        placed(b, i);
        Ok(())
    })
}
/// `trapnz` (`if_nonzero`) or `trapz`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_trap_if(b: *mut Builder, c: u32, if_nonzero: bool, code: u32) {
    with_builder(b, (), |b| {
        let t = codes::trap(code).ok_or_else(|| format!("unknown trap code {code}"))?;
        let i = if if_nonzero {
            b.fb().ins().trapnz(value(c), t)
        } else {
            b.fb().ins().trapz(value(c), t)
        };
        placed(b, i);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// JIT modules (SH4 lane 1, #1275).

/// Create a JIT module for the host with Cranelift `flags`, binding each
/// name of `symbols` (one per line) to the address at the same index of
/// `addrs`. Returns null on error; `vcl_new_error` reads the message.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_jit_module_new(
    flags: *const u8,
    flags_len: usize,
    symbols: *const u8,
    symbols_len: usize,
    addrs: *const u64,
    addrs_len: usize,
) -> *mut Module {
    let made = catch_unwind(|| {
        // SAFETY: the `.vsys` CSlice contract.
        let (f, s, a) = unsafe {
            (
                slice(flags, flags_len),
                slice(symbols, symbols_len),
                slice(addrs, addrs_len),
            )
        };
        new_jit_module(text(f)?, text(s)?, a)
    })
    .unwrap_or_else(|p| Err(panic_message(p)));
    match made {
        Ok(m) => Box::into_raw(Box::new(m)),
        Err(e) => {
            *NEW_ERROR.lock().unwrap() = e;
            std::ptr::null_mut()
        }
    }
}

/// Finalize every function a JIT module defined. Returns 0, or 1 with the
/// module's error set (also for an object or lent module).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_finalize(m: *mut Module) -> u32 {
    with_module(m, 1, |m| match &mut m.m {
        Backend::Jit(j) => {
            j.finalize_definitions().map_err(|e| e.to_string())?;
            Ok(0)
        }
        _ => Err("only a JIT module is finalized".into()),
    })
}

/// The address of finalized JIT function `func` (0 if none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_function_address(m: *mut Module, func: u32) -> u64 {
    with_module(m, 0, |m| {
        let id = func_id(m, func)?;
        match &m.m {
            Backend::Jit(j) => Ok(j.get_finalized_function(id) as usize as u64),
            _ => Err("only a JIT module has function addresses".into()),
        }
    })
}

/// How many functions the module has declared (its `declarations()`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_function_count(m: *const Module) -> u32 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(0, |m| m.get().declarations().get_functions().count() as u32)
}

/// The symbol name of declared function `func` (Cranelift's id), copied
/// into `out`; returns its full length (0 for an anonymous function).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_function_name(
    m: *const Module,
    func: u32,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    let Ok(id) = func_id(m, func) else {
        return 0;
    };
    let decls = m.get().declarations();
    if !decls.get_functions().any(|(f, _)| f == id) {
        return 0;
    }
    let name = decls.get_function_decl(id).linkage_name(id);
    copy_out(name.as_bytes(), 0, out, out_len);
    name.len() as u64
}

/// Compile the finished builder `b` (consumed) for the module's ISA without
/// defining it (`Context::compile` with a default `ControlPlane`), as the
/// verifiers and freestanding images do. Returns the code size, or
/// `u64::MAX` with the module's error set; `vcl_module_last_code` reads it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_compile(m: *mut Module, b: *mut Builder) -> u64 {
    let Some(b) = (unsafe { take_builder(b) }) else {
        return with_module(m, u64::MAX, |_| {
            Err("a lent or null builder cannot be compiled".into())
        });
    };
    with_module(m, u64::MAX, |m| {
        let f = b.finish()?;
        let (module, ctx) = m.parts();
        ctx.func = f;
        let isa = module.isa();
        let r = ctx
            .compile(
                isa,
                &mut cranelift_codegen::control::ControlPlane::default(),
            )
            .map(|_| ())
            .map_err(|e| cranelift_module::ModuleError::Compilation(e.inner));
        let r = finished(isa, ctx, r);
        module.clear_context(ctx);
        let (code, unwind) = r?;
        m.last_unwind = unwind;
        m.last_code = code;
        Ok(m.last_code.len() as u64)
    })
}

/// Copy the last `vcl_compile`'s code from `offset` into `out`; returns its
/// full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_last_code(
    m: *const Module,
    offset: u64,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    copy_out(&m.last_code, offset, out, out_len);
    m.last_code.len() as u64
}

/// Copy the Windows x64 unwind information (`UNWIND_INFO` bytes) of the
/// last function compiled or defined into `out`; returns its full length,
/// 0 on other targets.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_last_unwind(
    m: *const Module,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    copy_out(&m.last_unwind, 0, out, out_len);
    m.last_unwind.len() as u64
}

// ---------------------------------------------------------------------------
// Builder extras the compiler uses (SH4 lane 1, #1275).

/// Attach source location `loc` to the instructions emitted from now on.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_set_srcloc(b: *mut Builder, loc: u32) {
    with_builder(b, (), |b| {
        b.fb().set_srcloc(ir::SourceLoc::new(loc));
        Ok(())
    })
}
/// Mark `blk` cold (laid out last).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_set_cold_block(b: *mut Builder, blk: u32) {
    with_builder(b, (), |b| {
        b.fb().set_cold_block(block(blk));
        Ok(())
    })
}
/// The block being emitted into (`INVALID` if none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_current_block(b: *mut Builder) -> u32 {
    with_builder(b, INVALID, |b| {
        Ok(b.fb().current_block().map_or(INVALID, |blk| blk.as_u32()))
    })
}
/// Name the function `UserFuncName::user(namespace, index)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_set_func_name(b: *mut Builder, namespace: u32, index: u32) {
    with_builder(b, (), |b| {
        b.fb().func.name = UserFuncName::user(namespace, index);
        Ok(())
    })
}
/// `vcl_import_function`, then set the import's `colocated` flag (its
/// `ExtFuncData`); returns the FuncRef.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_import_function_colocated(
    b: *mut Builder,
    m: *mut Module,
    func: u32,
    colocated: bool,
) -> u32 {
    with_builder(b, INVALID, |b| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_mut() }.ok_or("null module")?;
        let id = func_id(m, func)?;
        let fb = b.fb();
        let r = m.get_mut().declare_func_in_func(id, fb.func);
        fb.func.dfg.ext_funcs[r].colocated = colocated;
        Ok(r.as_u32())
    })
}
/// A `cranelift_frontend::Switch` on `x`: jump to `blocks[i]` when `x`
/// equals `indices[i]`, else to `otherwise`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_switch(
    b: *mut Builder,
    x: u32,
    otherwise: u32,
    indices: *const u64,
    indices_len: usize,
    blocks: *const u32,
    blocks_len: usize,
) {
    // SAFETY: the `.vsys` CSlice contract.
    let (indices, blocks) = unsafe { (slice(indices, indices_len), slice(blocks, blocks_len)) };
    with_builder(b, (), |b| {
        if indices.len() != blocks.len() {
            return Err("switch indices and blocks differ in length".into());
        }
        let mut s = Switch::new();
        for (&i, &blk) in indices.iter().zip(blocks) {
            s.set_entry(u128::from(i), block(blk));
        }
        s.emit(b.fb(), value(x), block(otherwise));
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Batches.

/// The `vcl_emit` instruction kinds (the low byte of an instruction's first
/// word; the opcode or condition code is bits 8-23 and the type code bits
/// 32-63).
pub mod batch {
    pub const ICONST: u64 = 1; // [head, imm]
    pub const BINARY: u64 = 2; // [head, x, y]
    pub const BINARY_IMM: u64 = 3; // [head, x, imm]
    pub const UNARY: u64 = 4; // [head, x]
    pub const ICMP: u64 = 5; // [head, x, y]
    pub const ICMP_IMM: u64 = 6; // [head, x, imm]
    pub const CONVERT: u64 = 7; // [head, x]
    pub const TERNARY: u64 = 8; // [head, x, y, z]
    pub const LOAD: u64 = 9; // [head, flags, addr, offset]
    pub const STORE: u64 = 10; // [head, flags, x, addr, offset] (no result)

    /// The first word of an instruction.
    pub const fn head(kind: u64, op: u32, ty: u32) -> u64 {
        kind | ((op as u64) << 8) | ((ty as u64) << 32)
    }
}

/// Emit a batch of word-encoded instructions (see [`batch`]); writes one
/// entry of `out` per instruction (the result value, or `INVALID` for a
/// store) and returns how many instructions it emitted, or `INVALID` on
/// error (the builder's error says which).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_emit(
    b: *mut Builder,
    code: *const u64,
    code_len: usize,
    out: *mut u32,
    out_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` CSlice contract.
    let (code, out) = unsafe { (slice(code, code_len), slice_mut(out, out_len)) };
    with_builder(b, INVALID, |b| emit(b, code, out))
}
fn emit(b: &mut Builder, code: &[u64], out: &mut [u32]) -> Result<u32, String> {
    let mut pc = 0;
    let mut n = 0usize;
    let short = || "truncated batch".to_string();
    while pc < code.len() {
        if n >= out.len() {
            return Err("batch output is full".into());
        }
        let head = code[pc];
        let (kind, op, t) = (
            head & 0xff,
            ((head >> 8) & 0xffff) as u32,
            (head >> 32) as u32,
        );
        let arg = |k: usize| code.get(pc + k).copied().ok_or_else(short);
        let val = |k: usize, out: &[u32]| -> Result<ir::Value, String> {
            let w = arg(k)?;
            if w & BATCH_RESULT != 0 {
                let i = (w & 0xffff_ffff) as usize;
                out.get(..n)
                    .and_then(|done| done.get(i))
                    .map(|&v| value(v))
                    .ok_or_else(|| format!("batch result {i} is not defined yet"))
            } else {
                Ok(value(w as u32))
            }
        };
        let fb = b.fb();
        let (v, width) = match kind {
            batch::ICONST => (Some(iconst(fb, t, arg(1)? as i64)?), 2),
            batch::BINARY => (Some(binary(fb, op, val(1, out)?, val(2, out)?)?), 3),
            batch::BINARY_IMM => (Some(binary_imm(fb, op, val(1, out)?, arg(2)? as i64)?), 3),
            batch::UNARY => (Some(unary(fb, op, val(1, out)?)?), 2),
            batch::ICMP => (Some(icmp(fb, op, val(1, out)?, val(2, out)?)?), 3),
            batch::ICMP_IMM => (Some(icmp_imm(fb, op, val(1, out)?, arg(2)? as i64)?), 3),
            batch::CONVERT => (Some(convert(fb, op, ty(t)?, val(1, out)?)?), 2),
            batch::TERNARY => (
                Some(ternary(fb, op, val(1, out)?, val(2, out)?, val(3, out)?)?),
                4,
            ),
            batch::LOAD => (
                Some(load(
                    fb,
                    op,
                    t,
                    arg(1)? as u32,
                    val(2, out)?,
                    arg(3)? as i32,
                )?),
                4,
            ),
            batch::STORE => {
                let i = store(
                    fb,
                    op,
                    arg(1)? as u32,
                    val(2, out)?,
                    val(3, out)?,
                    arg(4)? as i32,
                )?;
                b.last = Some(i);
                (None, 5)
            }
            _ => return Err(format!("unknown batch kind {kind} at word {pc}")),
        };
        out[n] = match v {
            Some(v) => defined(b, v),
            None => INVALID,
        };
        n += 1;
        pc += width;
    }
    Ok(n as u32)
}

/// Every exported symbol, for hosts that bind them by name (the JIT).
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    macro_rules! syms {
        ($($f:ident),* $(,)?) => { vec![$((stringify!($f), $f as *const u8)),*] };
    }
    syms![
        vcl_module_new,
        vcl_new_error,
        vcl_module_free,
        vcl_module_error,
        vcl_module_pointer_type,
        vcl_module_keep_clif,
        vcl_module_code_bytes,
        vcl_module_clif,
        vcl_sig_new,
        vcl_sig_param,
        vcl_sig_return,
        vcl_declare_function,
        vcl_declare_data,
        vcl_define_function,
        vcl_define_data,
        vcl_module_finish,
        vcl_object_len,
        vcl_object_read,
        vcl_object_publish,
        vcl_object_free,
        vcl_data_new,
        vcl_data_free,
        vcl_data_define,
        vcl_data_zeroinit,
        vcl_data_align,
        vcl_data_func_addr,
        vcl_data_data_addr,
        vcl_builder_new,
        vcl_builder_free,
        vcl_builder_error,
        vcl_import_function,
        vcl_import_signature,
        vcl_import_data,
        vcl_create_block,
        vcl_append_block_param,
        vcl_append_block_params_for_function_params,
        vcl_block_param,
        vcl_switch_to_block,
        vcl_seal_block,
        vcl_seal_all_blocks,
        vcl_declare_var,
        vcl_def_var,
        vcl_use_var,
        vcl_stack_slot,
        vcl_inst_result,
        vcl_value_type,
        vcl_iconst,
        vcl_f32const,
        vcl_f64const,
        vcl_binary,
        vcl_binary_imm,
        vcl_unary,
        vcl_convert,
        vcl_ternary,
        vcl_overflow,
        vcl_icmp,
        vcl_icmp_imm,
        vcl_fcmp,
        vcl_load,
        vcl_store,
        vcl_stack_addr,
        vcl_stack_load,
        vcl_stack_store,
        vcl_extractlane,
        vcl_insertlane,
        vcl_vconst,
        vcl_shuffle,
        vcl_atomic_rmw,
        vcl_atomic_load,
        vcl_atomic_store,
        vcl_func_addr,
        vcl_symbol_value,
        vcl_global_value,
        vcl_get_stack_pointer,
        vcl_call,
        vcl_call_indirect,
        vcl_return_call,
        vcl_jump,
        vcl_brif,
        vcl_return,
        vcl_trap,
        vcl_trap_if,
        vcl_emit,
        vcl_jit_module_new,
        vcl_module_finalize,
        vcl_function_address,
        vcl_module_function_count,
        vcl_module_function_name,
        vcl_compile,
        vcl_module_last_code,
        vcl_module_last_unwind,
        vcl_set_srcloc,
        vcl_set_cold_block,
        vcl_current_block,
        vcl_set_func_name,
        vcl_import_function_colocated,
        vcl_switch,
        vcl_call_op,
        vcl_call_arg_count,
        vcl_call_arg,
        vcl_call_args,
        vcl_call_result,
        vcl_call_builder,
        vcl_call_module,
        vcl_call_return_builder,
        vcl_call_return_module,
    ]
}
