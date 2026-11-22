//! The crate-provided side of the `.vsys` import in `vesper/fnptrs.ves`
//! (`sys library Ops`): a lookup that hands the Vesper library C function
//! pointers by name, as `dlsym` would.

extern "C" fn add(a: u64, b: u64) -> u64 {
    a.wrapping_add(b)
}

extern "C" fn mul(a: u64, b: u64) -> u64 {
    a.wrapping_mul(b)
}

/// `pilot_ops_lookup(name)`: the address of `add` or `mul`, or NULL.
///
/// # Safety
///
/// `name` is a NUL-terminated string (the Vesper call passes a copy of its
/// bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pilot_ops_lookup(name: *const core::ffi::c_char) -> usize {
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { core::ffi::CStr::from_ptr(name) };
    match name.to_bytes() {
        b"add" => add as *const () as usize,
        b"mul" => mul as *const () as usize,
        _ => 0,
    }
}
