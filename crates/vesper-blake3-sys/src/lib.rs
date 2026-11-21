//! The official BLAKE3 C implementation and the `.vsys` library `Blake3` of
//! `@vesper-std/hash-sys` (`vesper_blake3_*`, defined in C in
//! packages/vesper-std/hash/sys/c/vesper_blake3.c), built by build.rs into
//! the static library `vesper_std_blake3`.
//!
//! This crate holds no logic: it builds C for Cargo and Bazel hosts and
//! names the symbols. A crate that links a Vesper object importing
//! `@vesper-std/hash-sys` depends on it and writes
//! `// @export vsys Blake3 direct rust=vesper_blake3_sys`; a JIT host binds
//! [`symbols`]. It replaces the Rust shim over the `blake3` crate
//! (Rust-exit SH6 lane 17, #1412).
#![allow(unsafe_code)]

use std::ffi::c_void;

unsafe extern "C" {
    pub fn vesper_blake3_new() -> *mut c_void;
    pub fn vesper_blake3_new_keyed(key: *const u8, len: usize) -> *mut c_void;
    pub fn vesper_blake3_new_derive_key(context: *const u8, len: usize) -> *mut c_void;
    pub fn vesper_blake3_update(h: *mut c_void, data: *const u8, len: usize);
    pub fn vesper_blake3_finalize(h: *const c_void, out: *mut u8, len: usize);
    pub fn vesper_blake3_free(h: *mut c_void);
    pub fn vesper_blake3_hash(data: *const u8, len: usize, out: *mut u8, out_len: usize);
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
