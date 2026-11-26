//! `vesper-cranelift-sys` as a WebAssembly component (#2210, part of #1397).
//!
//! Stage 2 runs as a Wasm component under the pinned wasmtime CLI, so the
//! native code generators it is made of (`@vesper/codegen-base` and the
//! packages over it) cannot call the Cranelift shim through a C ABI. This
//! crate builds the same shim for `wasm32-wasip2` and exports it as the
//! interface `vesper:cranelift/vcl@0.1.0`; the Wasm backend lowers
//! `Ffi[Cranelift]` calls to imports of that interface, and composing the two
//! components gives one that the wasmtime CLI runs on its own. The frozen C
//! runner (`bootstrap/seed/runner/vwasm.c`) is not involved.
//!
//! The interface is derived from `packages/vesper-cranelift/cranelift.vsys`
//! (see the `interface` module of `build.rs`), which generates the WIT and the export glue,
//! and [`WIT`] is the text. Cranelift itself is the same crate at the same
//! version as natively, so an object built through the component is the
//! object the native shim builds (`tests/objects.rs`).

/// The generated WIT package text of `vesper:cranelift@0.1.0`.
pub const WIT: &str = include_str!(concat!(env!("OUT_DIR"), "/vcl.wit"));

/// The generated import table of the Wasm backend
/// (`packages/vesper-wasm-backend/src/vcl.ves`).
pub const VCL_VES: &str = include_str!(concat!(env!("OUT_DIR"), "/vcl.ves"));

/// The generated `Ffi$Cranelift` declarations of the Stage 2 front end
/// (`packages/vesper-stage2/src/vcl_decls.ves`).
pub const VCL_DECLS_VES: &str = include_str!(concat!(env!("OUT_DIR"), "/vcl_decls.ves"));

#[cfg(target_family = "wasm")]
// The generated exports of the lend calls replace their crossing argument.
#[allow(
    unused_unsafe,
    unused_variables,
    clippy::unnecessary_cast,
    clippy::no_effect
)]
mod component {
    use std::alloc::{Layout, alloc, dealloc, realloc};

    // Force the shim's code into the component (its functions are reached
    // only through the generated exports).
    pub use vesper_cranelift_sys as _;

    /// The world, as the `component-type` custom section
    /// `wasm-component-ld` turns into the component's type.
    #[used]
    #[unsafe(link_section = "component-type:vesper-cranelift")]
    static COMPONENT_TYPE: [u8; include_bytes!(concat!(env!("OUT_DIR"), "/component-type.bin"))
        .len()] = *include_bytes!(concat!(env!("OUT_DIR"), "/component-type.bin"));

    /// The canonical ABI allocator: lifted lists land here.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn cabi_realloc(
        old: *mut u8,
        old_size: usize,
        align: usize,
        new_size: usize,
    ) -> *mut u8 {
        if new_size == 0 {
            return align as *mut u8;
        }
        // SAFETY: the canonical ABI passes a power-of-two alignment and,
        // for a non-empty `old`, the size and alignment it was made with.
        unsafe {
            let p = if old_size == 0 {
                alloc(Layout::from_size_align_unchecked(new_size, align))
            } else {
                realloc(
                    old,
                    Layout::from_size_align_unchecked(old_size, align),
                    new_size,
                )
            };
            if p.is_null() {
                std::process::abort();
            }
            p
        }
    }

    /// The return area of functions whose result goes through memory
    /// (`RET_AREA_BYTES` comes from build.rs). A component instance runs one
    /// call at a time.
    #[repr(align(8))]
    struct RetArea(#[allow(dead_code)] [u8; RET_AREA_BYTES]);
    static mut RET: RetArea = RetArea([0; RET_AREA_BYTES]);
    fn ret_area() -> *mut u8 {
        (&raw mut RET).cast()
    }

    /// A list the canonical ABI lifted into this component's memory. It is
    /// freed when dropped, or handed back as part of the result and freed
    /// by the post-return function.
    struct List<T> {
        ptr: *mut T,
        len: usize,
    }

    impl<T> List<T> {
        fn layout(len: usize) -> Layout {
            Layout::array::<T>(len).expect("list layout")
        }
        unsafe fn lifted(ptr: *mut u8, len: usize) -> List<T> {
            List {
                ptr: ptr.cast(),
                len,
            }
        }
        fn ptr(&self) -> *const T {
            self.ptr
        }
        fn ptr_mut(&self) -> *mut T {
            self.ptr
        }
        fn len(&self) -> usize {
            self.len
        }
        fn as_slice(&self) -> &[T] {
            if self.len == 0 {
                return &[];
            }
            // SAFETY: a lifted list of `len` elements.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
        fn as_mut_slice(&mut self) -> &mut [T] {
            if self.len == 0 {
                return &mut [];
            }
            // SAFETY: as in `as_slice`, owned here.
            unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
        }
        /// Store the list (pointer, length) at `at` and give up ownership.
        unsafe fn hand_back(self, at: *mut u8) {
            unsafe {
                at.cast::<usize>().write(self.ptr as usize);
                at.add(4).cast::<usize>().write(self.len);
            }
            std::mem::forget(self);
        }
        /// Free a list [`List::hand_back`] stored at `at`.
        unsafe fn free_handed_back(at: *mut u8) {
            unsafe {
                let ptr = at.cast::<usize>().read() as *mut T;
                let len = at.add(4).cast::<usize>().read();
                drop(List { ptr, len });
            }
        }
    }

    impl<T> Drop for List<T> {
        fn drop(&mut self) {
            if self.len != 0 && size_of::<T>() != 0 {
                // SAFETY: a non-empty list came from `cabi_realloc` with
                // this layout.
                unsafe { dealloc(self.ptr.cast(), Self::layout(self.len)) };
            }
        }
    }

    include!(concat!(env!("OUT_DIR"), "/exports.rs"));
}
