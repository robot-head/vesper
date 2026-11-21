//! Lent modules (#1275): the kind a host lends a module as, function
//! lookup by name, and JIT reference normalization.
use vesper_cranelift_sys::cranelift_codegen::ir::{AbiParam, types};
use vesper_cranelift_sys::cranelift_codegen::settings::{self, Configurable};
use vesper_cranelift_sys::cranelift_module::{Linkage, Module as _, default_libcall_names};
use vesper_cranelift_sys::cranelift_object::{ObjectBuilder, ObjectModule};
use vesper_cranelift_sys::*;

fn object_module(triple: &str) -> ObjectModule {
    let mut flags = settings::builder();
    flags.set("is_pic", "true").unwrap();
    let isa = vesper_cranelift_sys::cranelift_codegen::isa::lookup_by_name(triple)
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    ObjectModule::new(ObjectBuilder::new(isa, "lend", default_libcall_names()).unwrap())
}

fn host_triple() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// `(kind, get_function("f"), get_function("missing"))` of `m` lent as
/// `kind`.
fn probe(m: &mut ObjectModule, kind: ModuleKind) -> (u32, u32, u32) {
    let (answer, crossing) = lend_module_as(kind, m, 0, &[], |call| {
        // SAFETY: the lent call's address, live for this closure.
        unsafe {
            let m = vcl_call_module(call);
            let k = vcl_module_kind(m);
            let f = vcl_module_get_function(m, b"f".as_ptr(), 1);
            let missing = vcl_module_get_function(m, b"missing".as_ptr(), 7);
            vcl_call_return_module(call, m);
            (k, f, missing)
        }
    });
    assert!(crossing.error.is_none() && !crossing.unreturned);
    answer
}

#[test]
fn a_lent_module_has_the_kind_its_host_says_and_finds_functions_by_name() {
    let mut m = object_module(host_triple());
    let mut sig = m.make_signature();
    sig.returns.push(AbiParam::new(types::I64));
    let f = m.declare_function("f", Linkage::Local, &sig).unwrap();
    let object = probe(&mut m, ModuleKind::Object);
    assert_eq!(object, (ModuleKind::Object as u32, f.as_u32(), INVALID));
    let jit = probe(&mut m, ModuleKind::Jit);
    assert_eq!(jit, (ModuleKind::Jit as u32, f.as_u32(), INVALID));
}

#[test]
fn an_object_for_an_os_less_target_is_freestanding() {
    let arch = host_triple().split('-').next().unwrap();
    let mut m = object_module(&format!("{arch}-unknown-none-elf"));
    let mut sig = m.make_signature();
    sig.returns.push(AbiParam::new(types::I64));
    m.declare_function("f", Linkage::Local, &sig).unwrap();
    assert_eq!(
        probe(&mut m, ModuleKind::Object).0,
        ModuleKind::Freestanding as u32
    );
}

#[test]
fn jit_definitions_are_not_colocated() {
    // Define `g` calling a colocated `f` into a lent module lent as JIT
    // and as an object, keeping the CLIF text: only the object keeps the
    // colocated reference, as the Rust backend's `normalize_references`.
    let clif_for = |kind: ModuleKind| -> String {
        let mut m = object_module(host_triple());
        let mut sig = m.make_signature();
        sig.returns.push(AbiParam::new(types::I64));
        let f = m.declare_function("f", Linkage::Local, &sig).unwrap();
        let g = m.declare_function("g", Linkage::Local, &sig).unwrap();
        let (clif, crossing) = lend_module_as(kind, &mut m, 0, &[], |call| {
            // SAFETY: the lent call's address, live for this closure; every
            // handle below is the shim's own.
            unsafe {
                let m = vcl_call_module(call);
                vcl_module_keep_clif(m, true);
                let s = vcl_sig_new(m, 0);
                vcl_sig_return(m, s, codes::ty_code(types::I64));
                let b = vcl_builder_new(m, s);
                let entry = vcl_create_block(b);
                vcl_switch_to_block(b, entry);
                let callee = vcl_import_function_colocated(b, m, f.as_u32(), true);
                let inst = vcl_call(b, callee, std::ptr::null(), 0);
                let _ = inst;
                let v = vcl_inst_result(b, 0);
                vcl_return(b, &v, 1);
                vcl_seal_all_blocks(b);
                let r = vcl_define_function(m, g.as_u32(), b);
                assert_eq!(r, 0, "define failed");
                let mut out = vec![0u8; 1 << 16];
                let n = vcl_module_clif(m, 0, out.as_mut_ptr(), out.len()) as usize;
                out.truncate(n);
                vcl_call_return_module(call, m);
                String::from_utf8(out).unwrap()
            }
        });
        assert!(crossing.error.is_none(), "{:?}", crossing.error);
        clif
    };
    let object = clif_for(ModuleKind::Object);
    let jit = clif_for(ModuleKind::Jit);
    assert!(object.contains("colocated"), "{object}");
    assert!(!jit.contains("colocated"), "{jit}");
}

#[test]
fn data_objects_are_found_by_name() {
    let mut m = object_module(host_triple());
    let d = m.declare_data("d", Linkage::Local, false, false).unwrap();
    let mut sig = m.make_signature();
    sig.returns.push(AbiParam::new(types::I64));
    m.declare_function("f", Linkage::Local, &sig).unwrap();
    let (found, _) = lend_module(&mut m, 0, &[], |call| {
        // SAFETY: the lent call's address, live for this closure.
        unsafe {
            let m = vcl_call_module(call);
            let found = (
                vcl_module_get_data(m, b"d".as_ptr(), 1),
                vcl_module_get_data(m, b"f".as_ptr(), 1),
                vcl_module_get_function(m, b"d".as_ptr(), 1),
            );
            vcl_call_return_module(call, m);
            found
        }
    });
    assert_eq!(found, (d.as_u32(), INVALID, INVALID));
}

#[test]
fn compiles_report_their_traps_and_data_imports_take_a_colocated_flag() {
    let mut m = object_module(host_triple());
    let d = m.declare_data("d", Linkage::Local, false, false).unwrap();
    let (words, crossing) = lend_module(&mut m, 0, &[], |call| {
        // SAFETY: the lent call's address, live for this closure; every
        // handle below is the shim's own.
        unsafe {
            let m = vcl_call_module(call);
            vcl_module_keep_clif(m, true);
            let s = vcl_sig_new(m, 0);
            let b = vcl_builder_new(m, s);
            let entry = vcl_create_block(b);
            vcl_switch_to_block(b, entry);
            let near = vcl_import_data(b, m, d.as_u32());
            let far = vcl_import_data_colocated(b, m, d.as_u32(), false);
            assert_ne!(near, INVALID);
            assert_ne!(far, INVALID);
            // An `unreachable` trap (code 1, `TrapCode::unwrap_user(1)`).
            vcl_trap(b, 1);
            vcl_seal_all_blocks(b);
            assert_ne!(vcl_compile(m, b), u64::MAX, "compile failed");
            let n = vcl_module_last_traps(m, std::ptr::null_mut(), 0) as usize;
            let mut out = vec![0u64; n];
            vcl_module_last_traps(m, out.as_mut_ptr(), out.len());
            vcl_call_return_module(call, m);
            out
        }
    });
    assert!(crossing.error.is_none(), "{:?}", crossing.error);
    assert_eq!(
        words.len(),
        2,
        "one trap: offset and code words, got {words:?}"
    );
}
