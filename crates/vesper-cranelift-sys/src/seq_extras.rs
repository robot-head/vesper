//! The shim calls the sequence emission needs (self-hosting SH4 lane 9,
//! #1283, part 2b): anonymous data (string literals), the `memset` and
//! `memmove` library calls `FunctionBuilder::call_memset` and
//! `call_memmove` make, and the ISA facts the byte scans choose by
//! (`target_lexicon::Architecture` and `has_x86_pshufb_lowering`).
use crate::{INVALID, Module, data_index, value, with_builder, with_module};

/// [`vcl_isa_triple_arch`]'s answers: another architecture, x86-64,
/// AArch64, RISC-V 64.
pub const ARCH_OTHER: u32 = 0;
pub const ARCH_X86_64: u32 = 1;
pub const ARCH_AARCH64: u32 = 2;
pub const ARCH_RISCV64: u32 = 3;

/// Declare anonymous data (`Module::declare_anonymous_data`); returns the
/// module's data index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_declare_anonymous_data(
    m: *mut Module,
    writable: bool,
    tls: bool,
) -> u32 {
    with_module(m, INVALID, |m| {
        let id = m
            .get_mut()
            .declare_anonymous_data(writable, tls)
            .map_err(|e| e.to_string())?;
        Ok(data_index(m, id))
    })
}

/// `FunctionBuilder::call_memset(config, dst, val, count)` with `m`'s
/// target configuration: `val` is an `i8`, `count` a pointer-sized value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_memset(
    b: *mut crate::Builder,
    m: *const Module,
    dst: u32,
    val: u32,
    count: u32,
) {
    with_builder(b, (), |b| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_ref() }.ok_or("null module")?;
        let config = m.get().target_config();
        b.fb()
            .call_memset(config, value(dst), value(val), value(count));
        Ok(())
    })
}

/// `FunctionBuilder::call_memmove(config, dst, src, count)` with `m`'s
/// target configuration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_call_memmove(
    b: *mut crate::Builder,
    m: *const Module,
    dst: u32,
    src: u32,
    count: u32,
) {
    with_builder(b, (), |b| {
        // SAFETY: a live handle.
        let m = unsafe { m.as_ref() }.ok_or("null module")?;
        let config = m.get().target_config();
        b.fb()
            .call_memmove(config, value(dst), value(src), value(count));
        Ok(())
    })
}

/// The module's target architecture: [`ARCH_X86_64`], [`ARCH_AARCH64`],
/// [`ARCH_RISCV64`] or [`ARCH_OTHER`] (`INVALID` for a null handle).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_isa_triple_arch(m: *const Module) -> u32 {
    use target_lexicon::Architecture;
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(INVALID, |m| match m.get().isa().triple().architecture {
        Architecture::X86_64 => ARCH_X86_64,
        Architecture::Aarch64(_) => ARCH_AARCH64,
        Architecture::Riscv64(_) => ARCH_RISCV64,
        _ => ARCH_OTHER,
    })
}

/// Whether the module's ISA lowers `swizzle` to `pshufb`
/// (`TargetIsa::has_x86_pshufb_lowering`): 1 or 0 (`INVALID` for a null
/// handle).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_has_x86_pshufb_lowering(m: *const Module) -> u32 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(INVALID, |m| {
        u32::from(m.get().isa().has_x86_pshufb_lowering())
    })
}
