//! The CLIF seam (#2606, bootstrap slice 2a-3 of #1397): a finished
//! function read back as the words of [`crate::clif_words`] and edited by
//! the scripts the Vesper CLIF passes answer with (`@vesper/clif`:
//! native_vectorize, native_cold_outline, native_clif_opt).
//!
//! A builder goes through three states: building (`vcl_builder_new`),
//! finalized (`vcl_builder_finalize`: the function builder is gone and the
//! function stays), defined (`vcl_define_function`, which takes a building
//! or a finalized builder). While it is finalized, `vcl_function_words`
//! reads the function and `vcl_apply_edits` replays a script on it, with
//! the same Cranelift calls the Rust passes make, so Stage 2 runs the
//! passes on its own functions without the Rust compiler.
use crate::{Builder, clif_words, codes, copy_out, slice};
use cranelift_codegen::ir::Function;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// `vcl_function_words` flag: the vectorizer's `simd` header bit.
pub const WORDS_SIMD: u32 = 1;
/// `vcl_function_words` flag: append the shapes section
/// (`clif_words::shapes`, which `merge_tails` reads).
pub const WORDS_SHAPES: u32 = 2;

/// A builder's last [`vcl_function_words`] encoding and the `(flags,
/// ptr_ty)` it was made with (`None`: none, or edited since).
#[derive(Default)]
pub struct Words {
    key: Option<(u32, u32)>,
    bytes: Vec<u8>,
}

/// Run `f` on the function of a finalized, error-free builder. Errors and
/// panics poison the builder, as [`crate::with_builder`] does.
fn with_function<T>(
    b: *mut Builder,
    fail: T,
    f: impl FnOnce(&mut Function, &mut Words) -> Result<T, String>,
) -> T {
    // SAFETY: the `.vsys` contract: `b` is a live handle from vcl_builder_new.
    let Some(b) = (unsafe { b.as_mut() }) else {
        return fail;
    };
    if b.error.is_some() {
        return fail;
    }
    let Some(func) = b.func.filter(|_| b.finalized) else {
        b.error = Some("the CLIF seam needs a finalized builder".into());
        return fail;
    };
    // SAFETY: a finalized builder owns its function box and nothing else
    // borrows it (the function builder is gone).
    let func = unsafe { &mut *func.as_ptr() };
    match catch_unwind(AssertUnwindSafe(|| f(func, &mut b.words))) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            b.error = Some(e);
            fail
        }
        Err(p) => {
            b.error = Some(crate::panic_message(p));
            fail
        }
    }
}

/// Finalize builder `b` in place (`FunctionBuilder::finalize`) and keep its
/// function for the seam. Returns 0, or 1 with the builder's error set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_builder_finalize(b: *mut Builder) -> u32 {
    // SAFETY: the `.vsys` contract: `b` is a live handle from vcl_builder_new.
    let Some(b) = (unsafe { b.as_mut() }) else {
        return 1;
    };
    if b.error.is_some() {
        return 1;
    }
    if b.lent.is_some() {
        b.error = Some("a lent builder stays with its host".into());
        return 1;
    }
    match b.finalize() {
        Ok(()) => 0,
        Err(e) => {
            b.error = Some(e);
            1
        }
    }
}

/// The function words of finalized builder `b` (`clif_words::encode_with`)
/// copied from byte `offset` into `out`; returns their full length (0 on
/// an error, which the builder keeps). `flags` holds [`WORDS_SIMD`] and
/// [`WORDS_SHAPES`]; `ptr_ty` is the pointer type's code, or 0 for none.
/// The encoding is kept until the next `vcl_apply_edits` or a call with
/// other `flags` or `ptr_ty`, so a caller can ask for the length and then
/// read it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_function_words(
    b: *mut Builder,
    flags: u32,
    ptr_ty: u32,
    offset: u64,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    with_function(b, 0, |func, words| {
        let key = Some((flags, ptr_ty));
        if words.key != key {
            let ptr = match ptr_ty {
                0 => None,
                code => Some(codes::ty(code).ok_or_else(|| format!("unknown type {code}"))?),
            };
            words.bytes = clif_words::encode_with(func, flags & WORDS_SIMD != 0, ptr);
            if flags & WORDS_SHAPES != 0 {
                words.bytes.extend(clif_words::shapes(func));
            }
            words.key = key;
        }
        copy_out(&words.bytes, offset, out, out_len);
        Ok(words.bytes.len() as u64)
    })
}

/// Replay the edit script `edits` (`clif_words::apply`) on the function of
/// finalized builder `b`. Returns 0, or 1 with the builder's error set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_apply_edits(b: *mut Builder, edits: *const u64, len: usize) -> u32 {
    // SAFETY: the `.vsys` CSlice contract.
    let edits = unsafe { slice(edits, len) };
    with_function(b, 1, |func, words| {
        words.key = None;
        clif_words::apply(func, edits).map(|()| 0)
    })
}

/// Rust hosts: a copy of finalized builder `b`'s function (`None` for a
/// null, building or poisoned builder), for comparing the seam with
/// `clif_words` called directly.
///
/// # Safety
/// `b` is null or a live handle.
pub unsafe fn finalized_function(b: *const Builder) -> Option<Function> {
    // SAFETY: the caller's contract.
    let b = unsafe { b.as_ref() }?;
    let func = b.func.filter(|_| b.finalized && b.error.is_none())?;
    // SAFETY: a finalized builder owns its function and nothing borrows it.
    Some(unsafe { func.as_ref() }.clone())
}

/// The cold outliner's key for the helpers' verifier errors (the Rust
/// pass names them after the parent's key, which the shim does not know).
const OUTLINE_KEY: &str = "outlined";

/// Replay the cold outliner's script `edits` (`E_OUTLINE`/`E_SKIP` ops,
/// [`crate::clif_outline::apply_outline`]) on finalized builder `b`: each
/// helper is declared in `m` as an anonymous function, in order, and the
/// parent's region becomes its stub. Returns how many helpers there are (0
/// leaves the function unchanged), or `INVALID` with the builder's error
/// set. Take the helpers with `vcl_take_cold_helper` before `b` is defined.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_outline_cold(
    b: *mut Builder,
    m: *mut crate::Module,
    edits: *const u64,
    len: usize,
) -> u32 {
    // SAFETY: the `.vsys` CSlice contract.
    let edits = unsafe { slice(edits, len) };
    // SAFETY: the `.vsys` contract: a live module handle or null.
    let Some(m) = (unsafe { m.as_mut() }) else {
        return crate::INVALID;
    };
    let mut helpers = Vec::new();
    let n = with_function(b, crate::INVALID, |func, words| {
        words.key = None;
        let made = crate::clif_outline::apply_outline(m.get_mut(), OUTLINE_KEY, func, edits)?;
        for (id, _, helper) in made {
            helpers.push((crate::func_index(m, id), helper));
        }
        Ok(helpers.len() as u32)
    });
    // SAFETY: as above; `with_function` has returned.
    if let Some(b) = unsafe { b.as_mut() } {
        b.cold.extend(helpers);
    }
    n
}

/// The next cold helper of builder `b` as a finalized builder of its own
/// (null when none is left); `vcl_cold_helper_func` names the function to
/// define it as.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_take_cold_helper(b: *mut Builder) -> *mut Builder {
    // SAFETY: the `.vsys` contract: a live handle or null.
    let Some(b) = (unsafe { b.as_mut() }) else {
        return std::ptr::null_mut();
    };
    match b.cold.pop_front() {
        Some((func, helper)) => Box::into_raw(Builder::finalized(helper, func)),
        None => std::ptr::null_mut(),
    }
}

/// The module function index a cold helper builder defines (`INVALID` for
/// any other builder).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_cold_helper_func(b: *const Builder) -> u32 {
    // SAFETY: the `.vsys` contract: a live handle or null.
    unsafe { b.as_ref() }
        .and_then(|b| b.cold_func)
        .unwrap_or(crate::INVALID)
}

/// Define `func` in `m` from builder `b` (as `vcl_define_function`), then
/// each cold helper `vcl_outline_cold` left on `b`, in outline order, as
/// src/native.rs does: the object bytes depend on that order, and a Vesper
/// caller cannot hold a list of linear helper builders. Returns 0, or 1
/// with the module's error set (helpers not yet defined are dropped).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_define_with_cold_helpers(
    m: *mut crate::Module,
    func: u32,
    b: *mut Builder,
) -> u32 {
    // SAFETY: the `.vsys` contract: a live handle or null.
    let cold = unsafe { b.as_mut() }
        .map(|b| std::mem::take(&mut b.cold))
        .unwrap_or_default();
    // SAFETY: as above; `b` is consumed here.
    if unsafe { crate::vcl_define_function(m, func, b) } != 0 {
        return 1;
    }
    for (helper_func, helper) in cold {
        let helper = Box::into_raw(Builder::finalized(helper, helper_func));
        // SAFETY: `helper` is a fresh owned handle.
        if unsafe { crate::vcl_define_function(m, helper_func, helper) } != 0 {
            return 1;
        }
    }
    0
}
