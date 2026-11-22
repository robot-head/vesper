//! The shim calls SH4's lanes lacked (self-hosting wave SH5, lane 2, #1325).
//!
//! - Parameter and result extension (`vcl_sig_param_ext`,
//!   `vcl_sig_return_ext`): a `uext`/`sext` attribute on an ABI parameter,
//!   which the C ABIs need for narrow integers (SH4 lane 13's FFI glue).
//! - Defining a function from finished machine code and relocations
//!   (`vcl_define_function_bytes`, `Module::define_function_bytes`), so a
//!   lowering can place hand-assembled helpers (lane 13's `isolated(mpk)`
//!   gates) or re-place code it compiled with `vcl_compile`.
//! - Listing a module's declarations (`vcl_module_function_at`,
//!   `vcl_module_data_*`, linkage and signature queries: lanes 11 and 13).
//! - Read-back of the last compiled or defined function: its CLIF text
//!   (with `vcl_module_keep_clif`), relocations, call sites and frame size
//!   (lanes 14 and 17: the float check, Silicon's stack and interrupt
//!   analysis, the verifiers). Its code and traps were already readable
//!   (`vcl_module_last_code`, `vcl_module_last_traps`).
//!
//! The vector float conversions `fvpromote_low` and `fvdemote` (lane 11)
//! are `vcl_unary` opcodes 20 and 21 ([`crate::codes::UNARY`]).
//!
//! A relocation is five words: the code offset, the kind
//! ([`crate::codes::RELOC`]), the target kind ([`crate::codes::RELOC_TARGET`]),
//! the target and the addend (as `u64`). Targets are the module's function
//! or data index (kinds 0 and 1), an index into `LibCall::all_libcalls()`
//! (2), a `KnownSymbol` (3: 0 is the ELF GOT, 1 the COFF TLS index), an
//! offset in the function itself (4), or another user name
//! (`namespace << 32 | index`, 5: read-back only). The words a read-back
//! gives are the words `vcl_define_function_bytes` takes.
use crate::{
    INVALID, Module, codes, copy_out, data_id, data_index, func_id, func_index, sig_mut, slice,
    slice_mut, ty, with_module,
};
use cranelift_codegen::FinalizedMachReloc;
use cranelift_codegen::FinalizedRelocTarget;
use cranelift_codegen::ir::{self, ExternalName, KnownSymbol, LibCall};
use cranelift_module::{DataId, FuncId, ModuleReloc, ModuleRelocTarget};

/// Words per relocation.
pub const RELOC_WORDS: usize = 5;
const TARGET_FUNCTION: u64 = 0;
const TARGET_DATA: u64 = 1;
const TARGET_LIBCALL: u64 = 2;
const TARGET_KNOWN_SYMBOL: u64 = 3;
const TARGET_FUNCTION_OFFSET: u64 = 4;
const TARGET_USER_NAME: u64 = 5;
/// The user-name namespaces `cranelift_module` gives functions and data.
const FUNCTION_NAMESPACE: u32 = 0;
const DATA_NAMESPACE: u32 = 1;

/// The relocation words of `relocs`, which `func` was compiled with;
/// functions and data are named by Cranelift id ([`reloc_handles`] maps
/// them to the module's indices).
pub(crate) fn reloc_words(func: &ir::Function, relocs: &[FinalizedMachReloc]) -> Vec<u64> {
    let mut out = Vec::with_capacity(relocs.len() * RELOC_WORDS);
    for r in relocs {
        let (kind, target) = match &r.target {
            FinalizedRelocTarget::ExternalName(ExternalName::User(reff)) => {
                let name = &func.params.user_named_funcs()[*reff];
                match name.namespace {
                    FUNCTION_NAMESPACE => (TARGET_FUNCTION, u64::from(name.index)),
                    DATA_NAMESPACE => (TARGET_DATA, u64::from(name.index)),
                    ns => (
                        TARGET_USER_NAME,
                        u64::from(ns) << 32 | u64::from(name.index),
                    ),
                }
            }
            FinalizedRelocTarget::ExternalName(ExternalName::LibCall(l)) => (
                TARGET_LIBCALL,
                LibCall::all_libcalls()
                    .iter()
                    .position(|x| x == l)
                    .map_or(u64::MAX, |i| i as u64),
            ),
            FinalizedRelocTarget::ExternalName(ExternalName::KnownSymbol(k)) => (
                TARGET_KNOWN_SYMBOL,
                match k {
                    KnownSymbol::ElfGlobalOffsetTable => 0,
                    KnownSymbol::CoffTlsIndex => 1,
                },
            ),
            FinalizedRelocTarget::ExternalName(ExternalName::TestCase(_)) => {
                (TARGET_USER_NAME, u64::MAX)
            }
            FinalizedRelocTarget::Func(offset) => (TARGET_FUNCTION_OFFSET, u64::from(*offset)),
        };
        out.extend([
            u64::from(r.offset),
            u64::from(codes::reloc_code(r.kind)),
            kind,
            target,
            r.addend as u64,
        ]);
    }
    out
}

/// [`reloc_words`] with Cranelift ids replaced by `m`'s indices.
pub(crate) fn reloc_handles(m: &mut Module, mut words: Vec<u64>) -> Vec<u64> {
    for r in words.as_chunks_mut::<RELOC_WORDS>().0 {
        r[3] = match r[2] {
            TARGET_FUNCTION => u64::from(func_index(m, FuncId::from_u32(r[3] as u32))),
            TARGET_DATA => u64::from(data_index(m, DataId::from_u32(r[3] as u32))),
            _ => r[3],
        };
    }
    words
}

/// The relocations `words` describe for function `func` of `m`.
fn module_relocs(m: &Module, func: FuncId, words: &[u64]) -> Result<Vec<ModuleReloc>, String> {
    if !words.len().is_multiple_of(RELOC_WORDS) {
        return Err(format!(
            "{} relocation words is not a multiple of {RELOC_WORDS}",
            words.len()
        ));
    }
    words
        .as_chunks::<RELOC_WORDS>()
        .0
        .iter()
        .map(|r| {
            let offset = u32::try_from(r[0]).map_err(|_| format!("offset {} too large", r[0]))?;
            let kind = u32::try_from(r[1])
                .ok()
                .and_then(codes::reloc)
                .ok_or_else(|| format!("unknown relocation kind {}", r[1]))?;
            let t = r[3];
            let small =
                |what: &str| u32::try_from(t).map_err(|_| format!("{what} {t} out of range"));
            let name = match r[2] {
                TARGET_FUNCTION => ModuleRelocTarget::from(func_id(m, small("function")?)?),
                TARGET_DATA => ModuleRelocTarget::from(data_id(m, small("data")?)?),
                TARGET_LIBCALL => ModuleRelocTarget::LibCall(
                    *usize::try_from(t)
                        .ok()
                        .and_then(|i| LibCall::all_libcalls().get(i))
                        .ok_or_else(|| format!("unknown libcall {t}"))?,
                ),
                TARGET_KNOWN_SYMBOL => ModuleRelocTarget::KnownSymbol(match t {
                    0 => KnownSymbol::ElfGlobalOffsetTable,
                    1 => KnownSymbol::CoffTlsIndex,
                    _ => return Err(format!("unknown known symbol {t}")),
                }),
                TARGET_FUNCTION_OFFSET => {
                    ModuleRelocTarget::FunctionOffset(func, small("function offset")?)
                }
                k => return Err(format!("relocation target kind {k} cannot be defined")),
            };
            Ok(ModuleReloc {
                offset,
                kind,
                name,
                addend: r[4] as i64,
            })
        })
        .collect()
}

fn push_param(m: *mut Module, sig: u32, ty_code: u32, ext: u32, returns: bool) -> u32 {
    with_module(m, INVALID, |m| {
        let p = codes::abi_param(ty(ty_code)?, ext)
            .ok_or_else(|| format!("unknown extension {ext}"))?;
        let s = sig_mut(m, sig)?;
        let list = if returns {
            &mut s.returns
        } else {
            &mut s.params
        };
        list.push(p);
        Ok((list.len() - 1) as u32)
    })
}

/// `vcl_sig_param` with extension `ext` (0 none, 1 `uext`, 2 `sext`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_sig_param_ext(
    m: *mut Module,
    sig: u32,
    ty_code: u32,
    ext: u32,
) -> u32 {
    push_param(m, sig, ty_code, ext, false)
}
/// `vcl_sig_return` with extension `ext` (0 none, 1 `uext`, 2 `sext`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_sig_return_ext(
    m: *mut Module,
    sig: u32,
    ty_code: u32,
    ext: u32,
) -> u32 {
    push_param(m, sig, ty_code, ext, true)
}

/// Define function `func` as the machine code `bytes`, aligned to `align`
/// bytes (0: 1), with relocations `relocs` (five words each, see the
/// module docs). Returns 0, or 1 with the module's error set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_define_function_bytes(
    m: *mut Module,
    func: u32,
    align: u64,
    bytes: *const u8,
    bytes_len: usize,
    relocs: *const u64,
    relocs_len: usize,
) -> u32 {
    // SAFETY: the `.vsys` CSlice contract.
    let (bytes, words) = unsafe { (slice(bytes, bytes_len), slice(relocs, relocs_len)) };
    with_module(m, 1, |m| {
        let id = func_id(m, func)?;
        let relocs = module_relocs(m, id, words)?;
        m.get_mut()
            .define_function_bytes(id, align.max(1), bytes, &relocs)
            .map_err(|e| e.to_string())?;
        m.code_bytes += bytes.len() as u64;
        // Read-back now describes this function: its code and relocations
        // are the caller's, and nothing else is known about it.
        m.last_code = bytes.to_vec();
        m.last_relocs = words.to_vec();
        m.last_unwind.clear();
        m.last_traps.clear();
        m.last_call_sites.clear();
        m.last_frame_size = 0;
        m.last_clif.clear();
        Ok(0)
    })
}

/// The module index of the `i`th declared function in Cranelift's
/// declaration order, or [`INVALID`] past the end
/// (`vcl_module_function_count` says how many).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_function_at(m: *mut Module, i: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let id = m.get().declarations().get_functions().nth(i as usize);
        Ok(id.map(|(id, _)| id).map_or(INVALID, |id| func_index(m, id)))
    })
}
/// The linkage code of declared function `func` ([`INVALID`] if none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_function_linkage(m: *mut Module, func: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let id = func_id(m, func)?;
        let decls = m.get().declarations();
        if !decls.get_functions().any(|(f, _)| f == id) {
            return Err(format!("function {func} is not declared"));
        }
        Ok(codes::linkage_code(decls.get_function_decl(id).linkage))
    })
}
/// Add declared function `func`'s signature to the module's signatures;
/// returns its new index (for `vcl_builder_new`, `vcl_import_signature` or
/// declaring a function like it).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_function_signature(m: *mut Module, func: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let id = func_id(m, func)?;
        let decls = m.get().declarations();
        if !decls.get_functions().any(|(f, _)| f == id) {
            return Err(format!("function {func} is not declared"));
        }
        let sig = decls.get_function_decl(id).signature.clone();
        m.sigs.push(sig);
        Ok((m.sigs.len() - 1) as u32)
    })
}
/// How many data objects the module has declared.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_data_count(m: *const Module) -> u32 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(0, |m| {
        m.get().declarations().get_data_objects().count() as u32
    })
}
/// The module index of the `i`th declared data object, or [`INVALID`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_data_at(m: *mut Module, i: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let id = m.get().declarations().get_data_objects().nth(i as usize);
        Ok(id.map(|(id, _)| id).map_or(INVALID, |id| data_index(m, id)))
    })
}
/// The symbol name of declared data `data`, copied into `out`; returns its
/// full length (0 if none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_data_name(
    m: *const Module,
    data: u32,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    let Ok(id) = data_id(m, data) else {
        return 0;
    };
    let decls = m.get().declarations();
    if !decls.get_data_objects().any(|(d, _)| d == id) {
        return 0;
    }
    let name = decls.get_data_decl(id).linkage_name(id);
    copy_out(name.as_bytes(), 0, out, out_len);
    name.len() as u64
}
/// Declared data `data`'s linkage code, with bit 8 set when it is writable
/// and bit 9 when it is thread-local ([`INVALID`] if none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_data_linkage(m: *mut Module, data: u32) -> u32 {
    with_module(m, INVALID, |m| {
        let id = data_id(m, data)?;
        let decls = m.get().declarations();
        if !decls.get_data_objects().any(|(d, _)| d == id) {
            return Err(format!("data {data} is not declared"));
        }
        let d = decls.get_data_decl(id);
        Ok(codes::linkage_code(d.linkage) | u32::from(d.writable) << 8 | u32::from(d.tls) << 9)
    })
}

/// Copy the CLIF text of the last compiled or defined function from
/// `offset` into `out` (kept only while `vcl_module_keep_clif` is on);
/// returns its full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_last_clif(
    m: *const Module,
    offset: u64,
    out: *mut u8,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    let Some(m) = (unsafe { m.as_ref() }) else {
        return 0;
    };
    copy_out(m.last_clif.as_bytes(), offset, out, out_len);
    m.last_clif.len() as u64
}
fn copy_words(src: &[u64], out: *mut u64, out_len: usize) -> u64 {
    // SAFETY: the `.vsys` CSlice contract.
    let out = unsafe { slice_mut(out, out_len) };
    let n = out.len().min(src.len());
    out[..n].copy_from_slice(&src[..n]);
    src.len() as u64
}
/// Copy the relocations of the last compiled or defined function into
/// `out`, five words each (see the module docs); returns how many words
/// there are.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_last_relocs(
    m: *const Module,
    out: *mut u64,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(0, |m| copy_words(&m.last_relocs, out, out_len))
}
/// Copy the call sites of the last compiled or defined function into
/// `out`, two words each: the return address's code offset and the frame
/// offset at the call (`u64::MAX` when the backend gives none); returns how
/// many words there are.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_last_call_sites(
    m: *const Module,
    out: *mut u64,
    out_len: usize,
) -> u64 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(0, |m| copy_words(&m.last_call_sites, out, out_len))
}
/// The stack frame size of the last compiled or defined function
/// (`CompiledCode::frame_size`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vcl_module_last_frame_size(m: *const Module) -> u64 {
    // SAFETY: a live handle.
    unsafe { m.as_ref() }.map_or(0, |m| m.last_frame_size)
}
