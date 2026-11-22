//! Lent handles (self-hosting SH4 lane 1, #1275): a Rust host lends its
//! live `FunctionBuilder` and module to a Vesper hook for one call, and
//! takes them back.
//!
//! The host calls [`lend`] with the builder it is emitting into, its module,
//! the request (an opcode and argument words) and the hook. The hook gets
//! one word, the address of a [`Call`], and reads the request through the
//! `vcl_call_*` imports:
//!
//! ```text
//! let b = vcl_call_builder(call);      // Option[Builder]: the lent builder
//! let m = vcl_call_module(call);       // Option[Module]: the lent module
//! let op = vcl_call_op(call);
//! let x = vcl_call_arg(call, 0);
//! ... emit through `inout b` and `inout m` with the usual calls ...
//! vcl_call_result(call, v.id);         // result words, in order
//! vcl_call_return_builder(call, b);    // give both back (they are linear)
//! vcl_call_return_module(call, m);
//! ```
//!
//! Values, blocks, variables and stack slots are Cranelift's own entity
//! numbers, so a value the host made is the same number in the hook and a
//! value the hook made is the host's too. In a lent module, function and
//! data indices are Cranelift's `FuncId`/`DataId` numbers.
//!
//! The lent handles live in `lend`'s frame and own nothing: the consuming
//! calls (`vcl_define_function`, `vcl_compile`, `vcl_builder_free`,
//! `vcl_module_finish`, `vcl_module_free`) refuse or ignore them, and
//! nothing is counted in [`crate::live_handles`]. A failed call poisons the
//! lent builder or module as usual; `lend` reports the error.
use crate::{Backend, Builder, Module, ModuleKind, slice_mut};
use cranelift_frontend::FunctionBuilder;
use std::ptr::NonNull;

/// Marks a live [`Call`] (a stale or forged word reads as no call).
const MAGIC: u64 = 0x7665_7370_6572_6c64;

/// One hook crossing: what the host lends and asks, and what the hook
/// answers. The hook sees only its address.
pub struct Call<'a> {
    magic: u64,
    op: u32,
    args: &'a [u64],
    results: Vec<u64>,
    builder: Option<NonNull<Builder>>,
    module: Option<NonNull<Module>>,
    /// The handles the hook has taken and not yet returned.
    builder_out: bool,
    module_out: bool,
}

/// What a crossing left behind.
#[derive(Debug, Default)]
pub struct Crossing {
    /// The words the hook answered with (`vcl_call_result`), in order.
    pub results: Vec<u64>,
    /// The sticky error of the lent builder or module, if a call failed.
    pub error: Option<String>,
    /// The hook took a handle and did not give it back. Vesper's linear
    /// handles make this impossible for a checked hook; a hand-written one
    /// can, and the host treats it as an error.
    pub unreturned: bool,
}

/// Lend `fb` and `m` to `hook` for one call of request `op` with `args`.
/// `hook` gets the [`Call`]'s address and returns the host's answer
/// (whether it handled the request); [`Crossing`] says what it emitted.
/// The module is lent as an object module ([`lend_as`] says otherwise).
pub fn lend<R>(
    fb: &mut FunctionBuilder<'_>,
    m: &mut dyn cranelift_module::Module,
    op: u32,
    args: &[u64],
    hook: impl FnOnce(u64) -> R,
) -> (R, Crossing) {
    lend_parts(Some(fb), ModuleKind::Object, m, op, args, hook)
}

/// [`lend`] of a module of `kind`: [`ModuleKind::Jit`] makes
/// `vcl_module_kind` answer JIT and `vcl_define_function` normalize
/// references as the Rust backend's JIT definitions do. (A freestanding
/// object module is told apart by its target, so `kind` is
/// [`ModuleKind::Object`] for one.)
pub fn lend_as<R>(
    kind: ModuleKind,
    fb: &mut FunctionBuilder<'_>,
    m: &mut dyn cranelift_module::Module,
    op: u32,
    args: &[u64],
    hook: impl FnOnce(u64) -> R,
) -> (R, Crossing) {
    lend_parts(Some(fb), kind, m, op, args, hook)
}

/// [`lend`] with no builder: a module-level request (data, declarations,
/// whole glue functions the hook builds with its own builders).
pub fn lend_module<R>(
    m: &mut dyn cranelift_module::Module,
    op: u32,
    args: &[u64],
    hook: impl FnOnce(u64) -> R,
) -> (R, Crossing) {
    lend_parts(None, ModuleKind::Object, m, op, args, hook)
}

/// [`lend_module`] of a module of `kind` (see [`lend_as`]).
pub fn lend_module_as<R>(
    kind: ModuleKind,
    m: &mut dyn cranelift_module::Module,
    op: u32,
    args: &[u64],
    hook: impl FnOnce(u64) -> R,
) -> (R, Crossing) {
    lend_parts(None, kind, m, op, args, hook)
}

fn lend_parts<R>(
    fb: Option<&mut FunctionBuilder<'_>>,
    kind: ModuleKind,
    m: &mut dyn cranelift_module::Module,
    op: u32,
    args: &[u64],
    hook: impl FnOnce(u64) -> R,
) -> (R, Crossing) {
    let lent_builder = fb.is_some();
    let mut builder = Builder {
        fb: None,
        // The `'static` is a lie the handle never acts on: it lives in this
        // frame, which `fb` outlives.
        lent: fb.map(|fb| NonNull::from(fb).cast()),
        func: None,
        fctx: None,
        last: None,
        error: None,
    };
    // SAFETY: only the trait object's lifetime changes; the handle that
    // holds it lives in this frame, which `m` outlives.
    let m: NonNull<dyn cranelift_module::Module + 'static> = unsafe {
        std::mem::transmute::<
            NonNull<dyn cranelift_module::Module + '_>,
            NonNull<dyn cranelift_module::Module + 'static>,
        >(NonNull::from(m))
    };
    let mut module = Module::empty(Backend::Lent(m));
    module.jit = kind == ModuleKind::Jit;
    let mut call = Call {
        magic: MAGIC,
        op,
        args,
        results: Vec::new(),
        builder: lent_builder.then(|| NonNull::from(&mut builder)),
        module: Some(NonNull::from(&mut module)),
        builder_out: false,
        module_out: false,
    };
    let answer = hook(&raw mut call as u64);
    call.magic = 0;
    let crossing = Crossing {
        results: std::mem::take(&mut call.results),
        error: builder.error.take().or(module.error.take()),
        unreturned: call.builder_out || call.module_out,
    };
    (answer, crossing)
}

/// The live call at `call`, if it is one.
fn call<'c>(call: u64) -> Option<&'c mut Call<'c>> {
    // SAFETY: a hook only has the word `lend` gave it, which is the
    // address of a `Call` in `lend`'s frame for the whole hook call; the
    // magic word rejects anything else that is still readable.
    let c = unsafe { (call as usize as *mut Call<'c>).as_mut() }?;
    (c.magic == MAGIC).then_some(c)
}

/// The request's opcode.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_op(c: u64) -> u32 {
    call(c).map_or(crate::INVALID, |c| c.op)
}
/// How many argument words the request has.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_arg_count(c: u64) -> u64 {
    call(c).map_or(0, |c| c.args.len() as u64)
}
/// Argument word `i` (0 past the end).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_arg(c: u64, i: u64) -> u64 {
    call(c)
        .and_then(|c| c.args.get(usize::try_from(i).ok()?).copied())
        .unwrap_or(0)
}
/// Copy the argument words into `out`; returns how many the request has.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_args(c: u64, out: *mut u64, out_len: usize) -> u64 {
    let Some(c) = call(c) else {
        return 0;
    };
    // SAFETY: the `.vsys` CSlice contract.
    let out = unsafe { slice_mut(out, out_len) };
    let n = out.len().min(c.args.len());
    out[..n].copy_from_slice(&c.args[..n]);
    c.args.len() as u64
}
/// Append result word `w`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_result(c: u64, w: u64) {
    if let Some(c) = call(c) {
        c.results.push(w);
    }
}
/// Take the lent builder (null if the call lends none or it is out).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_builder(c: u64) -> *mut Builder {
    match call(c) {
        Some(c) if !c.builder_out => c.builder.map_or(std::ptr::null_mut(), |b| {
            c.builder_out = true;
            b.as_ptr()
        }),
        _ => std::ptr::null_mut(),
    }
}
/// Take the lent module (null if the call lends none or it is out).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_module(c: u64) -> *mut Module {
    match call(c) {
        Some(c) if !c.module_out => c.module.map_or(std::ptr::null_mut(), |m| {
            c.module_out = true;
            m.as_ptr()
        }),
        _ => std::ptr::null_mut(),
    }
}
/// Give the lent builder back (consumed). A handle that is not this call's
/// lent builder is freed as `vcl_builder_free` would.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_return_builder(c: u64, b: *mut Builder) {
    if let Some(c) = call(c)
        && c.builder_out
        && c.builder.is_some_and(|lent| lent.as_ptr() == b)
    {
        c.builder_out = false;
        return;
    }
    // SAFETY: the sink import consumes the handle exactly once.
    unsafe { crate::vcl_builder_free(b) }
}
/// Give the lent module back (consumed), as [`vcl_call_return_builder`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_return_module(c: u64, m: *mut Module) {
    if let Some(c) = call(c)
        && c.module_out
        && c.module.is_some_and(|lent| lent.as_ptr() == m)
    {
        c.module_out = false;
        return;
    }
    // SAFETY: the sink import consumes the handle exactly once.
    unsafe { crate::vcl_module_free(m) }
}
