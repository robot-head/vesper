//! The `vesper-chroma.wasm` C ABI (wasm32 only). This is the crate's only
//! module allowed to use `unsafe`: it turns a host-supplied pointer and length
//! into a byte slice.
//!
//! Protocol (no WASI imports, no JavaScript glue required):
//! 1. `chroma_alloc(len)` returns a pointer to a `len`-byte input buffer
//!    owned by the module (valid until the next `chroma_alloc`).
//! 2. The host writes UTF-8 source there and calls `highlight(ptr, len)`,
//!    which runs Tier 0 token mode ([`crate::highlight_tier0_tokens`]).
//!    It returns the number of 16-byte `ChromaTokenSpanWire` records, or
//!    `u32::MAX` on error, with the error code in `chroma_error()`.
//! 3. The records are at `chroma_output_ptr()` (valid until the next call).
#![allow(unsafe_code)]

use std::sync::Mutex;

static INPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static OUTPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static ERROR: Mutex<u32> = Mutex::new(0);

/// `chroma_error()` codes.
const ERR_NONE: u32 = 0;
const ERR_NULL: u32 = 1;
const ERR_UTF8: u32 = 2;
const ERR_TOO_LARGE: u32 = 3;
const ERR_CST: u32 = 4;
const ERR_OTHER: u32 = 5;

fn fail(code: u32) -> u32 {
    if let Ok(mut e) = ERROR.lock() {
        *e = code;
    }
    u32::MAX
}

#[unsafe(no_mangle)]
pub extern "C" fn chroma_alloc(len: u32) -> *mut u8 {
    let Ok(mut input) = INPUT.lock() else {
        return core::ptr::null_mut();
    };
    input.clear();
    input.resize(len as usize, 0);
    input.as_mut_ptr()
}

/// # Safety
/// `ptr` must point to `len` readable bytes of this module's memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn highlight(ptr: *const u8, len: u32) -> u32 {
    if ptr.is_null() && len != 0 {
        return fail(ERR_NULL);
    }
    let bytes: &[u8] = if len == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `ptr..ptr+len` is readable.
        unsafe { core::slice::from_raw_parts(ptr, len as usize) }
    };
    let Ok(src) = core::str::from_utf8(bytes) else {
        return fail(ERR_UTF8);
    };
    let spans = match crate::highlight_tier0_tokens(src) {
        Ok(spans) => spans,
        Err(crate::ChromaError::SourceTooLarge { .. }) => return fail(ERR_TOO_LARGE),
        Err(crate::ChromaError::CstMismatch { .. }) => return fail(ERR_CST),
        Err(_) => return fail(ERR_OTHER),
    };
    let Ok(mut out) = OUTPUT.lock() else {
        return fail(ERR_OTHER);
    };
    out.clear();
    if crate::wire::encode_into(&spans, &mut out).is_err() {
        return fail(ERR_OTHER);
    }
    if let Ok(mut e) = ERROR.lock() {
        *e = ERR_NONE;
    }
    spans.len() as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn chroma_output_ptr() -> *const u8 {
    OUTPUT.lock().map_or(core::ptr::null(), |o| o.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn chroma_error() -> u32 {
    ERROR.lock().map_or(ERR_OTHER, |e| *e)
}
