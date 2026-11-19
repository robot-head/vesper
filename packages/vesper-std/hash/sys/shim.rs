//! The Rust side of `@vesper-std/hash-sys` (self-hosting SH0 lane 5, #1127):
//! the `.vsys` library `Blake3` over the vetted `blake3` crate.
//!
//! A crate that links a Vesper object importing `@vesper-std/hash-sys`
//! includes this file as a module and names it on the `@export vsys` line
//! (`rust=crate::blake3_shim`), or hands [`symbols`] to the JIT. A `Hasher`
//! handle is a `Box<blake3::Hasher>`: the constructors return
//! `Box::into_raw` (or null), `vesper_blake3_free` takes it back once.
#![allow(unsafe_code)]

/// # Safety
/// `ptr` is valid for `len` bytes for the call (the `.vsys` slice contract).
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    // SAFETY: the caller lends `len` bytes at `ptr` for the call.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// # Safety
/// `ptr` is valid for writes of `len` bytes for the call.
unsafe fn bytes_mut<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
    if len == 0 {
        return &mut [];
    }
    // SAFETY: the caller lends `len` writable bytes at `ptr` for the call.
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_new() -> *mut blake3::Hasher {
    Box::into_raw(Box::new(blake3::Hasher::new()))
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_new_keyed(ptr: *const u8, len: usize) -> *mut blake3::Hasher {
    // SAFETY: the key is lent for the call.
    let key = unsafe { bytes(ptr, len) };
    match <&[u8; 32]>::try_from(key) {
        Ok(k) => Box::into_raw(Box::new(blake3::Hasher::new_keyed(k))),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_new_derive_key(ptr: *const u8, len: usize) -> *mut blake3::Hasher {
    // SAFETY: the context is lent for the call.
    match std::str::from_utf8(unsafe { bytes(ptr, len) }) {
        Ok(context) => Box::into_raw(Box::new(blake3::Hasher::new_derive_key(context))),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_update(h: *mut blake3::Hasher, ptr: *const u8, len: usize) {
    // SAFETY: `h` is a live handle lent `inout`; the data is lent.
    unsafe {
        (*h).update(bytes(ptr, len));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_finalize(h: *const blake3::Hasher, ptr: *mut u8, len: usize) {
    // SAFETY: `h` is a live handle lent `in`; the output is lent `inout`.
    unsafe {
        (*h).finalize_xof().fill(bytes_mut(ptr, len));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_free(h: *mut blake3::Hasher) {
    // SAFETY: the `sink` import consumes the handle exactly once.
    drop(unsafe { Box::from_raw(h) });
}

#[unsafe(no_mangle)]
pub extern "C" fn vesper_blake3_hash(ptr: *const u8, len: usize, out: *mut u8, out_len: usize) {
    // SAFETY: the input and output are lent for the call.
    unsafe {
        let mut x = blake3::Hasher::new();
        x.update(bytes(ptr, len));
        x.finalize_xof().fill(bytes_mut(out, out_len));
    }
}

/// The library's symbols, for binding JIT imports.
pub fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("vesper_blake3_new", vesper_blake3_new as *const u8),
        (
            "vesper_blake3_new_keyed",
            vesper_blake3_new_keyed as *const u8,
        ),
        (
            "vesper_blake3_new_derive_key",
            vesper_blake3_new_derive_key as *const u8,
        ),
        ("vesper_blake3_update", vesper_blake3_update as *const u8),
        (
            "vesper_blake3_finalize",
            vesper_blake3_finalize as *const u8,
        ),
        ("vesper_blake3_free", vesper_blake3_free as *const u8),
        ("vesper_blake3_hash", vesper_blake3_hash as *const u8),
    ]
}
