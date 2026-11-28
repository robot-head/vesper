//! The CLIF seam (#2606): `vcl_function_words` and `vcl_apply_edits` give
//! the same words and the same edited function as `clif_words` called on
//! the function directly, and `vcl_define_function` takes the edited,
//! finalized builder.
mod support;

use support::*;
use vesper_cranelift_sys::clif_words::{self, E_BLOCK, E_END, E_PLACE};
use vesper_cranelift_sys::cranelift_codegen::ir;
use vesper_cranelift_sys::seam::{WORDS_SHAPES, WORDS_SIMD, finalized_function};
use vesper_cranelift_sys::*;

/// The shim's `i64` type code.
const I64: u32 = 4;
/// `vcl_icmp_imm`'s `sgt` and `vcl_binary`'s `imul` codes.
const SGT: u32 = 4;
const IMUL: u32 = 3;
/// `vcl_binary_imm`'s `iadd_imm` code.
const IADD_IMM: u32 = 1;
/// Exported linkage.
const EXPORT: u32 = 2;

/// A module and a finalized builder for `f(x) = if x > 0 { x + 1 } else
/// { x * x }`, built through the C ABI.
unsafe fn build() -> (*mut Module, u32, *mut Builder) {
    // SAFETY: every handle comes from the call before it, and null slices
    // have length 0.
    unsafe {
        let null = std::ptr::null();
        let m = vcl_module_new(null, 0, null, 0, null, 0);
        let sig = vcl_sig_new(m, 0);
        vcl_sig_param(m, sig, I64);
        vcl_sig_return(m, sig, I64);
        let name = b"seam_f";
        let func = vcl_declare_function(m, name.as_ptr(), name.len(), EXPORT, sig);
        let b = vcl_builder_new(m, sig);
        let entry = vcl_create_block(b);
        let pos = vcl_create_block(b);
        let neg = vcl_create_block(b);
        let x = vcl_append_block_param(b, entry, I64);
        vcl_switch_to_block(b, entry);
        let c = vcl_icmp_imm(b, SGT, x, 0);
        vcl_brif(b, c, pos, std::ptr::null(), 0, neg, std::ptr::null(), 0);
        vcl_switch_to_block(b, pos);
        let y = vcl_binary_imm(b, IADD_IMM, x, 1);
        vcl_return(b, &y, 1);
        vcl_switch_to_block(b, neg);
        let z = vcl_binary(b, IMUL, x, x);
        vcl_return(b, &z, 1);
        vcl_seal_all_blocks(b);
        assert_eq!(vcl_builder_finalize(b), 0, "{}", error(b));
        (m, func, b)
    }
}

unsafe fn error(b: *const Builder) -> String {
    let mut out = vec![0u8; 4096];
    // SAFETY: a live handle and a buffer of `out.len()` bytes.
    let n = unsafe { vcl_builder_error(b, out.as_mut_ptr(), out.len()) } as usize;
    String::from_utf8_lossy(&out[..n.min(out.len())]).into_owned()
}

/// The seam's words, read as its callers do: the length, then the bytes.
unsafe fn words(b: *mut Builder, flags: u32, ptr: u32) -> Vec<u8> {
    // SAFETY: a live handle and buffers of the lengths passed.
    unsafe {
        let n = vcl_function_words(b, flags, ptr, 0, std::ptr::null_mut(), 0) as usize;
        let mut out = vec![0u8; n];
        let head = n / 2;
        vcl_function_words(b, flags, ptr, 0, out.as_mut_ptr(), head);
        let rest = &mut out[head..];
        vcl_function_words(b, flags, ptr, head as u64, rest.as_mut_ptr(), rest.len());
        out
    }
}

#[test]
fn function_words_match_clif_words() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: handles from `build`, freed once.
    unsafe {
        let (m, _, b) = build();
        let f = finalized_function(b).expect("finalized");
        assert_eq!(words(b, 0, 0), clif_words::encode_with(&f, false, None));
        assert_eq!(
            words(b, WORDS_SIMD, I64),
            clif_words::encode_with(&f, true, Some(ir::types::I64))
        );
        let mut shaped = clif_words::encode(&f, false);
        shaped.extend(clif_words::shapes(&f));
        assert_eq!(words(b, WORDS_SHAPES, 0), shaped);
        vcl_builder_free(b);
        vcl_module_free(m);
    }
}

#[test]
fn apply_edits_matches_clif_words_and_defines() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: handles from `build`, each consumed once.
    unsafe {
        let (m, func, b) = build();
        let mut f = finalized_function(b).expect("finalized");
        let before = words(b, 0, 0);
        // A new block, placed before the last one.
        let n = f.dfg.num_blocks() as u64;
        let script = [E_BLOCK, n, E_PLACE, n, n - 1, E_END];
        assert_eq!(
            vcl_apply_edits(b, script.as_ptr(), script.len()),
            0,
            "{}",
            error(b)
        );
        clif_words::apply(&mut f, &script).unwrap();
        let edited = finalized_function(b).expect("still finalized");
        assert_eq!(edited.display().to_string(), f.display().to_string());
        // The edit drops the kept encoding.
        let after = words(b, 0, 0);
        assert_ne!(after, before);
        assert_eq!(after, clif_words::encode(&f, false));
        // An empty placed block does not verify, so take it out again.
        let undo = [clif_words::E_REMOVE_BLOCK, n, E_END];
        assert_eq!(
            vcl_apply_edits(b, undo.as_ptr(), undo.len()),
            0,
            "{}",
            error(b)
        );
        assert_eq!(vcl_define_function(m, func, b), 0);
        let o = vcl_module_finish(m);
        assert!(!o.is_null());
        assert!(vcl_object_len(o) > 0);
        vcl_object_free(o);
    }
}

#[test]
fn the_seam_refuses_what_it_cannot_do() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: handles from the calls before, each consumed once.
    unsafe {
        let null = std::ptr::null();
        let m = vcl_module_new(null, 0, null, 0, null, 0);
        let sig = vcl_sig_new(m, 0);
        // A building builder has no function to read yet.
        let b = vcl_builder_new(m, sig);
        assert_eq!(vcl_function_words(b, 0, 0, 0, std::ptr::null_mut(), 0), 0);
        assert!(error(b).contains("finalized"), "{}", error(b));
        vcl_builder_free(b);
        vcl_module_free(m);
        // A script that names the wrong new block poisons the builder.
        let (m, func, b) = build();
        let script = [E_BLOCK, 99, E_END];
        assert_eq!(vcl_apply_edits(b, script.as_ptr(), script.len()), 1);
        assert!(error(b).contains("block"), "{}", error(b));
        assert_eq!(vcl_define_function(m, func, b), 1);
        vcl_module_free(m);
    }
}

/// Hash of function words as tests/fixtures/seam.ves's `words_hash`
/// computes it (FNV-1a's prime over whole words, seeded with the length).
fn words_hash(bytes: &[u8]) -> u64 {
    const PRIME: u64 = 1_099_511_628_211;
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .fold(bytes.len() as u64, |h, w| {
            (h ^ u64::from_le_bytes(*w)).wrapping_mul(PRIME)
        })
}

/// The fixture built by Stage 2 and run with the Cranelift component
/// (tests/support/stage2.rs).
#[test]
fn vesper_reaches_the_seam() {
    let Some(stage2) = stage2::stage2() else {
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: handles from `build`, freed once.
    let want = unsafe {
        let (m, _, b) = build();
        let f = finalized_function(b).expect("finalized");
        vcl_builder_free(b);
        vcl_module_free(m);
        clif_words::encode_with(&f, true, Some(ir::types::I64))
    };
    let seam = fixture("seam.ves");
    let answer = |entry| stage2.answer(&seam, entry);
    assert_eq!(answer("seam_words"), words_hash(&want), "function words");
    let id = answer("seam");
    assert_ne!(id, 0, "the fixture failed");
    let outlined = answer("seam_outline");
    assert_ne!(outlined, 0, "the outlining fixture failed");
    let together = answer("seam_outline_together");
    assert_ne!(together, 0, "the one-call outlining fixture failed");
    let outlined = stage2.published(outlined).expect("published");
    assert!(!outlined.is_empty());
    assert_eq!(
        stage2.published(together),
        Some(outlined),
        "one call, same object"
    );
    assert!(stage2.published(id).is_some_and(|o| !o.is_empty()));
}

/// The cold outliner's answer for `build`'s function: block 2 (the `else`
/// arm, `x * x` returned) becomes a helper taking `x` (value 0).
const OUTLINE_ELSE: [u64; 15] = [
    clif_words::E_OUTLINE,
    0, // idx
    2, // root
    1,
    2, // owned
    0, // shared blocks
    1, // has_return
    0, // trap
    1,
    0, // live_ins
    0, // remat
    0, // shared slots
    0, // exclusive slots
    0, // ordered blocks
    E_END,
];

#[test]
fn outline_cold_matches_the_extraction_called_directly() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use vesper_cranelift_sys::clif_outline::apply_outline;
    use vesper_cranelift_sys::cranelift_module::{Linkage, Module as _, default_libcall_names};
    use vesper_cranelift_sys::cranelift_object::{ObjectBuilder, ObjectModule};
    // SAFETY: handles from `build`, each consumed once.
    unsafe {
        let (m, func, b) = build();
        let mut want = finalized_function(b).expect("finalized");
        let script = &OUTLINE_ELSE;
        assert_eq!(
            vcl_outline_cold(b, m, script.as_ptr(), script.len()),
            1,
            "{}",
            error(b)
        );
        // The same replay on a module with the same declarations.
        let isa = vesper_cranelift_sys::cranelift_native::builder()
            .unwrap()
            .finish(
                vesper_cranelift_sys::cranelift_codegen::settings::Flags::new(
                    vesper_cranelift_sys::cranelift_codegen::settings::builder(),
                ),
            )
            .unwrap();
        let mut direct =
            ObjectModule::new(ObjectBuilder::new(isa, "seam", default_libcall_names()).unwrap());
        direct
            .declare_function("seam_f", Linkage::Export, &want.signature)
            .unwrap();
        let helpers = apply_outline(&mut direct, "outlined", &mut want, script).unwrap();
        assert_eq!(helpers.len(), 1);
        let parent = finalized_function(b).expect("still finalized");
        assert_eq!(parent.display().to_string(), want.display().to_string());
        let helper = vcl_take_cold_helper(b);
        assert!(!helper.is_null());
        assert!(vcl_take_cold_helper(b).is_null(), "one helper");
        let got = finalized_function(helper).expect("a finalized helper");
        assert_eq!(
            got.display().to_string(),
            helpers[0].2.display().to_string()
        );
        let helper_func = vcl_cold_helper_func(helper);
        assert_ne!(helper_func, INVALID);
        assert_eq!(vcl_cold_helper_func(b), INVALID, "the parent is no helper");
        // Parent first, then the helper, as src/native.rs defines them.
        assert_eq!(vcl_define_function(m, func, b), 0);
        assert_eq!(vcl_define_function(m, helper_func, helper), 0);
        let o = vcl_module_finish(m);
        assert!(!o.is_null());
        vcl_object_free(o);
    }
}

#[test]
fn outlining_nothing_leaves_the_function_be() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: handles from `build`, each consumed once.
    unsafe {
        let (m, func, b) = build();
        let before = finalized_function(b)
            .expect("finalized")
            .display()
            .to_string();
        let skip = [clif_words::E_SKIP, E_END];
        assert_eq!(vcl_outline_cold(b, m, skip.as_ptr(), skip.len()), 0);
        let after = finalized_function(b)
            .expect("finalized")
            .display()
            .to_string();
        assert_eq!(after, before);
        assert!(vcl_take_cold_helper(b).is_null());
        assert_eq!(vcl_define_function(m, func, b), 0);
        vcl_module_free(m);
    }
}

/// The bytes of object `o`, which is freed.
unsafe fn object_bytes(o: *mut vesper_cranelift_sys::Object) -> Vec<u8> {
    assert!(!o.is_null());
    // SAFETY: `o` is a live object handle, read and then freed once.
    unsafe {
        let mut bytes = vec![0; vcl_object_len(o) as usize];
        vcl_object_read(o, 0, bytes.as_mut_ptr(), bytes.len());
        vcl_object_free(o);
        bytes
    }
}

#[test]
fn define_with_cold_helpers_matches_defining_each() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: handles from `build`, each consumed once.
    unsafe {
        let script = &OUTLINE_ELSE;
        let (m, func, b) = build();
        assert_eq!(vcl_outline_cold(b, m, script.as_ptr(), script.len()), 1);
        let helper = vcl_take_cold_helper(b);
        let helper_func = vcl_cold_helper_func(helper);
        assert_eq!(vcl_define_function(m, func, b), 0);
        assert_eq!(vcl_define_function(m, helper_func, helper), 0);
        let each = object_bytes(vcl_module_finish(m));

        let (m, func, b) = build();
        assert_eq!(vcl_outline_cold(b, m, script.as_ptr(), script.len()), 1);
        assert_eq!(vcl_define_with_cold_helpers(m, func, b), 0);
        let together = object_bytes(vcl_module_finish(m));
        assert_eq!(together, each);

        // Without helpers it is `vcl_define_function`.
        let (m, func, b) = build();
        assert_eq!(vcl_define_with_cold_helpers(m, func, b), 0);
        vcl_module_free(m);
    }
}
