//! The shim calls SH4's lanes lacked (SH5 lane 2, #1325). Each call is
//! made through the C ABI and checked against the same Rust builder calls:
//! the objects must be byte-identical, and read-back must equal what
//! Cranelift reports to a Rust host.
mod support;

use support::*;
use vesper_cranelift_sys::cranelift_codegen::binemit::Reloc;
use vesper_cranelift_sys::cranelift_codegen::ir::{
    self, AbiParam, InstBuilder, Signature, StackSlotData, StackSlotKind, TrapCode, types,
};
use vesper_cranelift_sys::cranelift_codegen::settings::{self, Configurable};
use vesper_cranelift_sys::cranelift_codegen::{Context, FinalizedRelocTarget, control};
use vesper_cranelift_sys::cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use vesper_cranelift_sys::cranelift_module::{
    DataDescription, Linkage, Module, ModuleReloc, default_libcall_names,
};
use vesper_cranelift_sys::cranelift_object::{ObjectBuilder, ObjectModule};
use vesper_cranelift_sys::*;

/// The handle counters are process-wide: every test that makes handles
/// runs alone, so `vesper_reaches_every_new_call` can count leaks.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fixed targets, so every CI host checks the same bytes.
const TRIPLES: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];
const FLAGS: &str = "is_pic=true";
const NAME: &str = "sh5";

fn rust_module(triple: &str) -> ObjectModule {
    let mut flags = settings::builder();
    flags.set("is_pic", "true").unwrap();
    let isa = vesper_cranelift_sys::cranelift_codegen::isa::lookup_by_name(triple)
        .unwrap()
        .finish(settings::Flags::new(flags))
        .unwrap();
    ObjectModule::new(ObjectBuilder::new(isa, NAME, default_libcall_names()).unwrap())
}

/// A shim object module for `triple`.
fn shim_module(triple: &str) -> *mut vesper_cranelift_sys::Module {
    // SAFETY: valid (ptr, len) pairs.
    let m = unsafe {
        vcl_module_new(
            triple.as_ptr(),
            triple.len(),
            FLAGS.as_ptr(),
            FLAGS.len(),
            NAME.as_ptr(),
            NAME.len(),
        )
    };
    assert!(!m.is_null(), "{}", new_error());
    m
}
fn new_error() -> String {
    let mut out = vec![0u8; 4096];
    // SAFETY: a writable buffer.
    let n = unsafe { vcl_new_error(out.as_mut_ptr(), out.len()) } as usize;
    out.truncate(n.min(4096));
    String::from_utf8_lossy(&out).into_owned()
}
fn module_error(m: *const vesper_cranelift_sys::Module) -> String {
    let mut out = vec![0u8; 1 << 16];
    // SAFETY: a live handle and a writable buffer.
    let n = unsafe { vcl_module_error(m, out.as_mut_ptr(), out.len()) } as usize;
    out.truncate(n.min(1 << 16));
    String::from_utf8_lossy(&out).into_owned()
}
/// Finish `m` and return its object bytes.
fn finish(m: *mut vesper_cranelift_sys::Module) -> Vec<u8> {
    // SAFETY: a live owned module, consumed once; the object likewise.
    unsafe {
        let o = vcl_module_finish(m);
        assert!(!o.is_null(), "{}", new_error());
        let mut bytes = vec![0u8; vcl_object_len(o) as usize];
        vcl_object_read(o, 0, bytes.as_mut_ptr(), bytes.len());
        vcl_object_free(o);
        bytes
    }
}
/// Read a variable-length word table through `f(out, len) -> total`.
fn words(f: impl Fn(*mut u64, usize) -> u64) -> Vec<u64> {
    let n = f(std::ptr::null_mut(), 0) as usize;
    let mut out = vec![0u64; n];
    f(out.as_mut_ptr(), out.len());
    out
}
fn bytes_of(f: impl Fn(*mut u8, usize) -> u64) -> Vec<u8> {
    let n = f(std::ptr::null_mut(), 0) as usize;
    let mut out = vec![0u8; n];
    f(out.as_mut_ptr(), out.len());
    out
}
fn ty(t: ir::Type) -> u32 {
    codes::ty_code(t)
}
fn decl(m: *mut vesper_cranelift_sys::Module, name: &str, linkage: Linkage, sig: u32) -> u32 {
    // SAFETY: a live handle and a valid name.
    let f = unsafe {
        vcl_declare_function(
            m,
            name.as_ptr(),
            name.len(),
            codes::linkage_code(linkage),
            sig,
        )
    };
    assert_ne!(f, INVALID, "{}", module_error(m));
    f
}

// ---------------------------------------------------------------------------
// 1. sext/uext attributes.

fn ext_sig(m: &dyn Module) -> (Signature, Signature) {
    let mut callee = m.make_signature();
    callee.params.push(AbiParam::new(types::I8).sext());
    callee.params.push(AbiParam::new(types::I16).uext());
    callee.returns.push(AbiParam::new(types::I32).sext());
    let mut caller = m.make_signature();
    caller.params.push(AbiParam::new(types::I8).sext());
    caller.returns.push(AbiParam::new(types::I32).sext());
    (callee, caller)
}

fn rust_ext(triple: &str) -> Vec<u8> {
    let mut m = rust_module(triple);
    let (callee_sig, caller_sig) = ext_sig(&m);
    let callee = m
        .declare_function("ext_callee", Linkage::Import, &callee_sig)
        .unwrap();
    let caller = m
        .declare_function("ext_caller", Linkage::Export, &caller_sig)
        .unwrap();
    let mut ctx = m.make_context();
    ctx.func.signature = caller_sig;
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let x = b.block_params(entry)[0];
    let seven = b.ins().iconst(types::I16, 7);
    let r = m.declare_func_in_func(callee, b.func);
    let call = b.ins().call(r, &[x, seven]);
    let v = b.inst_results(call)[0];
    b.ins().return_(&[v]);
    b.seal_all_blocks();
    b.finalize();
    m.define_function(caller, &mut ctx).unwrap();
    m.finish().emit().unwrap()
}

fn shim_ext(triple: &str) -> (Vec<u8>, String) {
    let m = shim_module(triple);
    // SAFETY: live handles from this module throughout.
    unsafe {
        vcl_module_keep_clif(m, true);
        let callee_sig = vcl_sig_new(m, 0);
        vcl_sig_param_ext(m, callee_sig, ty(types::I8), 2);
        vcl_sig_param_ext(m, callee_sig, ty(types::I16), 1);
        vcl_sig_return_ext(m, callee_sig, ty(types::I32), 2);
        let caller_sig = vcl_sig_new(m, 0);
        vcl_sig_param_ext(m, caller_sig, ty(types::I8), 2);
        vcl_sig_return_ext(m, caller_sig, ty(types::I32), 2);
        let callee = decl(m, "ext_callee", Linkage::Import, callee_sig);
        let caller = decl(m, "ext_caller", Linkage::Export, caller_sig);
        let b = vcl_builder_new(m, caller_sig);
        let entry = vcl_create_block(b);
        vcl_append_block_params_for_function_params(b, entry);
        vcl_switch_to_block(b, entry);
        let x = vcl_block_param(b, entry, 0);
        let seven = vcl_iconst(b, ty(types::I16), 7);
        let r = vcl_import_function(b, m, callee);
        let args = [x, seven];
        vcl_call(b, r, args.as_ptr(), args.len());
        let v = vcl_inst_result(b, 0);
        vcl_return(b, &v, 1);
        vcl_seal_all_blocks(b);
        assert_eq!(vcl_define_function(m, caller, b), 0, "{}", module_error(m));
        let clif = String::from_utf8(bytes_of(|o, n| vcl_module_last_clif(m, 0, o, n))).unwrap();
        (finish(m), clif)
    }
}

#[test]
fn sext_and_uext_attributes_match_the_rust_builder() {
    let _serial = serial();
    for triple in TRIPLES {
        let (shim, clif) = shim_ext(triple);
        assert_eq!(shim, rust_ext(triple), "{triple}: objects differ");
        assert!(
            clif.contains("i8 sext") && clif.contains("i32 sext"),
            "{clif}"
        );
    }
    // An unknown extension is refused and poisons the module.
    let m = shim_module(TRIPLES[0]);
    // SAFETY: a live handle.
    unsafe {
        let s = vcl_sig_new(m, 0);
        assert_eq!(vcl_sig_param_ext(m, s, ty(types::I8), 3), INVALID);
        assert!(module_error(m).contains("extension"));
        vcl_module_free(m);
    }
}

// ---------------------------------------------------------------------------
// 2. fvpromote_low / fvdemote.

fn rust_vector(triple: &str) -> Vec<u8> {
    let mut m = rust_module(triple);
    for (name, from, to, promote) in [
        ("promote", types::F32X4, types::F64X2, true),
        ("demote", types::F64X2, types::F32X4, false),
    ] {
        let mut sig = m.make_signature();
        sig.params.push(AbiParam::new(from));
        sig.returns.push(AbiParam::new(to));
        let f = m.declare_function(name, Linkage::Export, &sig).unwrap();
        let mut ctx = m.make_context();
        ctx.func.signature = sig;
        let mut fctx = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let x = b.block_params(entry)[0];
        let v = if promote {
            b.ins().fvpromote_low(x)
        } else {
            b.ins().fvdemote(x)
        };
        b.ins().return_(&[v]);
        b.seal_all_blocks();
        b.finalize();
        m.define_function(f, &mut ctx).unwrap();
    }
    m.finish().emit().unwrap()
}

fn shim_vector(triple: &str) -> Vec<u8> {
    let m = shim_module(triple);
    // SAFETY: live handles from this module throughout.
    unsafe {
        for (name, from, to, op) in [
            ("promote", types::F32X4, types::F64X2, 20),
            ("demote", types::F64X2, types::F32X4, 21),
        ] {
            let sig = vcl_sig_new(m, 0);
            vcl_sig_param(m, sig, ty(from));
            vcl_sig_return(m, sig, ty(to));
            let f = decl(m, name, Linkage::Export, sig);
            let b = vcl_builder_new(m, sig);
            let entry = vcl_create_block(b);
            vcl_append_block_params_for_function_params(b, entry);
            vcl_switch_to_block(b, entry);
            let x = vcl_block_param(b, entry, 0);
            let v = vcl_unary(b, op, x);
            assert_eq!(vcl_value_type(b, v), ty(to));
            vcl_return(b, &v, 1);
            vcl_seal_all_blocks(b);
            assert_eq!(vcl_define_function(m, f, b), 0, "{}", module_error(m));
        }
        finish(m)
    }
}

#[test]
fn vector_float_conversions_match_the_rust_builder() {
    let _serial = serial();
    for triple in TRIPLES {
        assert_eq!(shim_vector(triple), rust_vector(triple), "{triple}");
    }
    // The batch form reaches them too (they are `vcl_unary` opcodes).
    let m = shim_module(TRIPLES[0]);
    // SAFETY: live handles from this module.
    unsafe {
        let sig = vcl_sig_new(m, 0);
        vcl_sig_param(m, sig, ty(types::F32X4));
        let b = vcl_builder_new(m, sig);
        let entry = vcl_create_block(b);
        vcl_append_block_params_for_function_params(b, entry);
        vcl_switch_to_block(b, entry);
        let x = vcl_block_param(b, entry, 0);
        let code = [
            batch::head(batch::UNARY, 20, 0),
            u64::from(x),
            batch::head(batch::UNARY, 21, 0),
            BATCH_RESULT,
        ];
        let mut out = [0u32; 2];
        assert_eq!(
            vcl_emit(b, code.as_ptr(), code.len(), out.as_mut_ptr(), 2),
            2
        );
        assert_eq!(vcl_value_type(b, out[0]), ty(types::F64X2));
        assert_eq!(vcl_value_type(b, out[1]), ty(types::F32X4));
        vcl_builder_free(b);
        vcl_module_free(m);
    }
}

// ---------------------------------------------------------------------------
// 3. Declaration listing.

#[test]
fn declarations_are_listed_with_their_names_linkage_and_signatures() {
    let _serial = serial();
    let m = shim_module(TRIPLES[0]);
    // SAFETY: live handles from this module throughout.
    unsafe {
        let s2 = vcl_sig_new(m, 0);
        vcl_sig_param(m, s2, ty(types::I64));
        vcl_sig_param(m, s2, ty(types::I32));
        vcl_sig_return(m, s2, ty(types::I64));
        let s0 = vcl_sig_new(m, 0);
        let f = decl(m, "listed_f", Linkage::Export, s2);
        let g = decl(m, "listed_g", Linkage::Import, s0);
        let d = vcl_declare_data(m, b"listed_d".as_ptr(), 8, 1, true, false);
        let t = vcl_declare_data(m, b"listed_t".as_ptr(), 8, 0, false, true);
        assert_eq!(vcl_module_function_count(m), 2);
        assert_eq!(vcl_module_function_at(m, 0), f);
        assert_eq!(vcl_module_function_at(m, 1), g);
        assert_eq!(vcl_module_function_at(m, 2), INVALID);
        assert_eq!(vcl_module_function_linkage(m, f), 4);
        assert_eq!(vcl_module_function_linkage(m, g), 0);
        let name = bytes_of(|o, n| vcl_module_function_name(m, g, o, n));
        assert_eq!(name, b"listed_g");
        // The declared signature comes back as a new signature index that
        // builds a function with the same parameters.
        let s = vcl_module_function_signature(m, f);
        assert_ne!(s, INVALID);
        let b = vcl_builder_new(m, s);
        let entry = vcl_create_block(b);
        assert_eq!(vcl_append_block_params_for_function_params(b, entry), 2);
        vcl_builder_free(b);
        assert_eq!(vcl_module_data_count(m), 2);
        assert_eq!(vcl_module_data_at(m, 0), d);
        assert_eq!(vcl_module_data_at(m, 1), t);
        assert_eq!(vcl_module_data_at(m, 2), INVALID);
        assert_eq!(
            bytes_of(|o, n| vcl_module_data_name(m, t, o, n)),
            b"listed_t"
        );
        assert_eq!(vcl_module_data_linkage(m, d), 1 | 1 << 8);
        assert_eq!(vcl_module_data_linkage(m, t), 1 << 9);
        assert!(module_error(m).is_empty(), "{}", module_error(m));
        assert_eq!(vcl_module_function_linkage(m, 9), INVALID);
        vcl_module_free(m);
    }
}

#[test]
fn a_lent_module_lists_the_hosts_declarations_by_cranelift_id() {
    let _serial = serial();
    let mut m = rust_module(TRIPLES[0]);
    let sig = m.make_signature();
    let a = m.declare_function("a", Linkage::Local, &sig).unwrap();
    let b = m.declare_function("b", Linkage::Export, &sig).unwrap();
    let d = m.declare_data("d", Linkage::Local, false, false).unwrap();
    let (got, crossing) = lend_module(&mut m, 0, &[], |call| {
        // SAFETY: the lent call's address, live for this closure.
        unsafe {
            let lm = vcl_call_module(call);
            let got = (
                vcl_module_function_at(lm, 0),
                vcl_module_function_at(lm, 1),
                vcl_module_function_linkage(lm, b.as_u32()),
                vcl_module_data_at(lm, 0),
                bytes_of(|o, n| vcl_module_data_name(lm, d.as_u32(), o, n)),
            );
            vcl_call_return_module(call, lm);
            got
        }
    });
    assert!(crossing.error.is_none(), "{:?}", crossing.error);
    assert_eq!(got, (a.as_u32(), b.as_u32(), 4, d.as_u32(), b"d".to_vec()));
}

// ---------------------------------------------------------------------------
// 4 and 5. Read-back and functions defined from bytes.

/// The function the read-back checks compile: a stack slot, a call to an
/// imported function, a data address and a conditional trap.
///
/// fn probe(x: i64) -> i64 { slot = x; if x == 0 trap; callee(slot) + &table }
struct Probe {
    callee: u32,
    table: u32,
}

fn rust_probe(b: &mut FunctionBuilder, callee: ir::FuncRef, table: ir::GlobalValue) {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let x = b.block_params(entry)[0];
    let slot = b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 32, 3));
    b.ins().stack_store(x, slot, 8);
    b.ins().trapz(x, TrapCode::STACK_OVERFLOW);
    let y = b.ins().stack_load(types::I64, slot, 8);
    let call = b.ins().call(callee, &[y]);
    let r = b.inst_results(call)[0];
    let t = b.ins().symbol_value(types::I64, table);
    let v = b.ins().iadd(r, t);
    b.ins().return_(&[v]);
    b.seal_all_blocks();
}

type Ids = (
    vesper_cranelift_sys::cranelift_module::FuncId,
    vesper_cranelift_sys::cranelift_module::FuncId,
    vesper_cranelift_sys::cranelift_module::DataId,
);

/// Declare the probe's callee, table and itself, in the shim test's order.
fn rust_declare(m: &mut ObjectModule) -> (Signature, Ids) {
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    let callee = m
        .declare_function("probe_callee", Linkage::Import, &sig)
        .unwrap();
    let table = m
        .declare_data("probe_table", Linkage::Local, false, false)
        .unwrap();
    let probe = m.declare_function("probe", Linkage::Export, &sig).unwrap();
    let mut d = DataDescription::new();
    d.define_zeroinit(16);
    m.define_data(table, &d).unwrap();
    (sig, (callee, probe, table))
}

/// What Cranelift tells a Rust host about the compiled probe, in the
/// shim's words: (code, relocs, call sites, traps, frame size, CLIF).
type Readback = (Vec<u8>, Vec<u64>, Vec<u64>, Vec<u64>, u64, String);

/// Compile the probe in Rust; optionally define it from its bytes.
fn rust_readback(triple: &str, define_bytes: bool) -> (Readback, Vec<u8>) {
    let mut m = rust_module(triple);
    let (sig, (callee, probe, table)) = rust_declare(&mut m);
    let mut ctx = Context::new();
    ctx.func.signature = sig;
    let mut fctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fctx);
        let cr = m.declare_func_in_func(callee, b.func);
        let tg = m.declare_data_in_func(table, b.func);
        rust_probe(&mut b, cr, tg);
        b.finalize();
    }
    let clif = ctx.func.display().to_string();
    ctx.compile(m.isa(), &mut control::ControlPlane::default())
        .unwrap();
    let c = ctx.compiled_code().unwrap();
    let code = c.code_buffer().to_vec();
    // The shim's handle indices: callee 0, probe 1; table 0.
    let mut relocs = Vec::new();
    for r in c.buffer.relocs() {
        let (kind, target) = match &r.target {
            FinalizedRelocTarget::ExternalName(ir::ExternalName::User(reff)) => {
                let name = &ctx.func.params.user_named_funcs()[*reff];
                assert!(name.namespace <= 1);
                let index = if name.namespace == 0 {
                    [callee, probe]
                        .iter()
                        .position(|f| f.as_u32() == name.index)
                        .unwrap()
                } else {
                    assert_eq!(name.index, table.as_u32());
                    0
                };
                (u64::from(name.namespace), index as u64)
            }
            FinalizedRelocTarget::Func(off) => (4, u64::from(*off)),
            other => panic!("unexpected relocation target {other:?}"),
        };
        let code_of = codes::reloc_code(r.kind);
        assert_ne!(code_of, 0, "{:?} missing from codes::RELOC", r.kind);
        relocs.extend([
            u64::from(r.offset),
            u64::from(code_of),
            kind,
            target,
            r.addend as u64,
        ]);
    }
    let calls = c
        .buffer
        .call_sites()
        .flat_map(|s| {
            [
                u64::from(s.ret_addr),
                s.frame_offset.map_or(u64::MAX, u64::from),
            ]
        })
        .collect();
    let traps = c
        .buffer
        .traps()
        .iter()
        .flat_map(|t| [u64::from(t.offset), u64::from(t.code.as_raw().get())])
        .collect();
    let frame = u64::from(c.frame_size);
    let module_relocs: Vec<ModuleReloc> = c
        .buffer
        .relocs()
        .iter()
        .map(|r| ModuleReloc::from_mach_reloc(r, &ctx.func, probe))
        .collect();
    if define_bytes {
        m.define_function_bytes(probe, 16, &code, &module_relocs)
            .unwrap();
    }
    let object = if define_bytes {
        m.finish().emit().unwrap()
    } else {
        Vec::new()
    };
    ((code, relocs, calls, traps, frame, clif), object)
}

/// The same through the shim.
fn shim_readback(triple: &str, define_bytes: bool) -> (Readback, Vec<u8>) {
    let m = shim_module(triple);
    // SAFETY: live handles from this module throughout.
    unsafe {
        vcl_module_keep_clif(m, true);
        let sig = vcl_sig_new(m, 0);
        vcl_sig_param(m, sig, ty(types::I64));
        vcl_sig_return(m, sig, ty(types::I64));
        let p = Probe {
            callee: decl(m, "probe_callee", Linkage::Import, sig),
            table: vcl_declare_data(m, b"probe_table".as_ptr(), 11, 1, false, false),
        };
        let probe = decl(m, "probe", Linkage::Export, sig);
        let d = vcl_data_new();
        vcl_data_zeroinit(d, 16);
        assert_eq!(vcl_define_data(m, p.table, d), 0);
        let b = vcl_builder_new(m, sig);
        let cr = vcl_import_function(b, m, p.callee);
        let tg = vcl_import_data(b, m, p.table);
        let entry = vcl_create_block(b);
        vcl_append_block_params_for_function_params(b, entry);
        vcl_switch_to_block(b, entry);
        let x = vcl_block_param(b, entry, 0);
        let slot = vcl_stack_slot(b, 32, 3);
        vcl_stack_store(b, x, slot, 8);
        vcl_trap_if(b, x, false, 1);
        let y = vcl_stack_load(b, ty(types::I64), slot, 8);
        vcl_call(b, cr, &y, 1);
        let r = vcl_inst_result(b, 0);
        let t = vcl_symbol_value(b, ty(types::I64), tg);
        let v = vcl_binary(b, 1, r, t);
        vcl_return(b, &v, 1);
        vcl_seal_all_blocks(b);
        let size = vcl_compile(m, b);
        assert_ne!(size, u64::MAX, "{}", module_error(m));
        let code = bytes_of(|o, n| vcl_module_last_code(m, 0, o, n));
        assert_eq!(code.len() as u64, size);
        let relocs = words(|o, n| vcl_module_last_relocs(m, o, n));
        let calls = words(|o, n| vcl_module_last_call_sites(m, o, n));
        let traps = words(|o, n| vcl_module_last_traps(m, o, n));
        let frame = vcl_module_last_frame_size(m);
        let clif = String::from_utf8(bytes_of(|o, n| vcl_module_last_clif(m, 0, o, n))).unwrap();
        if define_bytes {
            let r = vcl_define_function_bytes(
                m,
                probe,
                16,
                code.as_ptr(),
                code.len(),
                relocs.as_ptr(),
                relocs.len(),
            );
            assert_eq!(r, 0, "{}", module_error(m));
        }
        let object = if define_bytes {
            finish(m)
        } else {
            // The exported probe is not defined: the object cannot finish.
            vcl_module_free(m);
            Vec::new()
        };
        ((code, relocs, calls, traps, frame, clif), object)
    }
}

#[test]
fn read_back_equals_what_cranelift_reports_to_a_rust_host() {
    let _serial = serial();
    for triple in TRIPLES {
        let (shim, _) = shim_readback(triple, false);
        let (rust, _) = rust_readback(triple, false);
        assert_eq!(shim.0, rust.0, "{triple}: code");
        assert_eq!(shim.1, rust.1, "{triple}: relocations");
        assert_eq!(shim.2, rust.2, "{triple}: call sites");
        assert_eq!(shim.3, rust.3, "{triple}: traps");
        assert_eq!(shim.4, rust.4, "{triple}: frame size");
        assert_eq!(shim.5, rust.5, "{triple}: CLIF");
        // The probe calls once, references the callee and the table, traps
        // once and has a frame (its 32-byte slot at least).
        assert_eq!(shim.2.len(), 2, "{triple}: one call site");
        assert!(shim.1.len() >= 2 * 5, "{triple}: {:?}", shim.1);
        assert_eq!(shim.3.len(), 2, "{triple}: one trap");
        assert!(shim.4 >= 32, "{triple}: frame of {}", shim.4);
        assert!(shim.5.contains("ss0 = explicit_slot 32"), "{}", shim.5);
    }
}

#[test]
fn functions_defined_from_bytes_match_the_rust_module() {
    let _serial = serial();
    for triple in TRIPLES {
        let (_, shim) = shim_readback(triple, true);
        let (_, rust) = rust_readback(triple, true);
        assert_eq!(shim, rust, "{triple}: objects differ");
    }
}

#[test]
fn malformed_relocations_are_refused() {
    let _serial = serial();
    let m = shim_module(TRIPLES[0]);
    // SAFETY: live handles from this module.
    unsafe {
        let sig = vcl_sig_new(m, 0);
        let f = decl(m, "bad", Linkage::Export, sig);
        let code = [0xc3u8];
        let short = [0u64; 4];
        assert_eq!(
            vcl_define_function_bytes(m, f, 1, code.as_ptr(), 1, short.as_ptr(), short.len()),
            1
        );
        assert!(
            module_error(m).contains("multiple of 5"),
            "{}",
            module_error(m)
        );
        vcl_module_free(m);
        for bad in [
            [0, 999, 0, 0, 0],
            [0, codes::reloc_code(Reloc::Abs8) as u64, 0, 7, 0],
            [0, codes::reloc_code(Reloc::Abs8) as u64, 5, 0, 0],
        ] {
            let m = shim_module(TRIPLES[0]);
            let sig = vcl_sig_new(m, 0);
            let f = decl(m, "bad", Linkage::Export, sig);
            let code = [0u8; 8];
            assert_eq!(
                vcl_define_function_bytes(m, f, 1, code.as_ptr(), 8, bad.as_ptr(), 5),
                1,
                "{bad:?}"
            );
            vcl_module_free(m);
        }
    }
}

#[test]
fn every_relocation_kind_round_trips_its_code() {
    for (code, r) in codes::RELOC {
        assert_eq!(codes::reloc(*code), Some(*r));
        assert_eq!(codes::reloc_code(*r), *code);
    }
    assert_eq!(codes::reloc(0), None);
}

// ---------------------------------------------------------------------------
// The calls from Vesper (tests/fixtures/sh5_extras.ves).

#[test]
fn vesper_reaches_every_new_call() {
    let Some(target) = host_target() else {
        eprintln!("no .vsys target for this host");
        return;
    };
    let _serial = serial();
    let before = live_handles();
    let checked = checked_with(&fixture("sh5_extras.ves"), target);
    assert_eq!(jit(&checked, "typed_words"), 1, "typed entity words");
    let id = jit(&checked, "extras");
    assert_ne!(id, 0, "the fixture failed");
    assert_eq!(live_handles(), before, "the fixture leaked a handle");
    let shim = take_published(id).expect("published object");
    let (_, rust) = rust_readback(TRIPLES[0], true);
    assert_eq!(shim, rust, "the Vesper-built object differs from Rust's");
}

#[test]
fn defining_bytes_replaces_the_read_back() {
    let _serial = serial();
    let m = shim_module(TRIPLES[0]);
    // SAFETY: live handles from this module.
    unsafe {
        let sig = vcl_sig_new(m, 0);
        let f = decl(m, "compiled", Linkage::Local, sig);
        let g = decl(m, "placed", Linkage::Local, sig);
        let _ = f;
        let b = vcl_builder_new(m, sig);
        let e = vcl_create_block(b);
        vcl_switch_to_block(b, e);
        vcl_trap(b, 1);
        vcl_seal_all_blocks(b);
        assert_ne!(vcl_compile(m, b), u64::MAX, "{}", module_error(m));
        assert_eq!(words(|o, n| vcl_module_last_traps(m, o, n)).len(), 2);
        let code = [0xc3u8, 0x90, 0x90, 0x90, 0, 0, 0, 0, 0, 0, 0, 0];
        let relocs = [4, codes::reloc_code(Reloc::Abs8) as u64, 4, 0, 0];
        let r = vcl_define_function_bytes(m, g, 1, code.as_ptr(), 12, relocs.as_ptr(), 5);
        assert_eq!(r, 0, "{}", module_error(m));
        assert_eq!(bytes_of(|o, n| vcl_module_last_code(m, 0, o, n)), code);
        assert_eq!(words(|o, n| vcl_module_last_relocs(m, o, n)), relocs);
        assert!(words(|o, n| vcl_module_last_traps(m, o, n)).is_empty());
        assert!(words(|o, n| vcl_module_last_call_sites(m, o, n)).is_empty());
        assert_eq!(vcl_module_last_frame_size(m), 0);
        vcl_module_free(m);
    }
}

#[test]
fn finishing_a_jit_module_frees_it() {
    let _serial = serial();
    let before = live_handles();
    // SAFETY: an owned JIT module, consumed by the refused finish.
    unsafe {
        let m = vcl_jit_module_new(
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
        );
        assert!(!m.is_null(), "{}", new_error());
        assert!(vcl_module_finish(m).is_null());
    }
    assert!(new_error().contains("finalized"), "{}", new_error());
    assert_eq!(
        live_handles(),
        before,
        "the refused finish leaked the module"
    );
}
