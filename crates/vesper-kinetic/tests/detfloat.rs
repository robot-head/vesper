//! DetFloat auditor tests on real Cranelift 0.125 IR built with
//! `cranelift-frontend`.

use cranelift_codegen::Context;
use cranelift_codegen::ir::condcodes::FloatCC;
use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, ExternalName, Function, InstBuilder, LibCall, MemFlags, Signature,
    UserExternalName, UserFuncName, Value, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use vesper_kinetic::{DetFloatPolicy, ReductionOrder, audit};

fn sig(params: &[types::Type], returns: &[types::Type]) -> Signature {
    let mut s = Signature::new(CallConv::SystemV);
    s.params.extend(params.iter().map(|t| AbiParam::new(*t)));
    s.returns.extend(returns.iter().map(|t| AbiParam::new(*t)));
    s
}

/// Build a function `name(params) -> returns` whose body is `body`, which
/// gets the entry block's parameters and must return the values to return.
fn build(
    name: &str,
    params: &[types::Type],
    returns: &[types::Type],
    body: impl FnOnce(&mut FunctionBuilder<'_>, &[Value]) -> Vec<Value>,
) -> Function {
    let mut func =
        Function::with_name_signature(UserFuncName::testcase(name), sig(params, returns));
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let args = b.block_params(entry).to_vec();
    let rets = body(&mut b, &args);
    b.ins().return_(&rets);
    b.finalize();
    func
}

/// The exact canonicalisation Cranelift's own pass emits for a scalar f32.
fn canon_f32(b: &mut FunctionBuilder<'_>, x: Value) -> Value {
    let nan = b.ins().f32const(f32::from_bits(0x7fc0_0000));
    let is_nan = b.ins().fcmp(FloatCC::Unordered, x, x);
    b.ins().select(is_nan, nan, x)
}

fn rules(func: &Function, policy: &DetFloatPolicy) -> Vec<&'static str> {
    audit(func, policy)
        .expect("valid IR")
        .into_iter()
        .map(|f| f.rule)
        .collect()
}

#[test]
fn two_rounded_mul_add_with_canonical_result_is_clean() {
    let f = build("muladd", &[types::F32; 3], &[types::F32], |b, a| {
        let m = b.ins().fmul(a[0], a[1]);
        let s = b.ins().fadd(m, a[2]);
        vec![canon_f32(b, s)]
    });
    assert_eq!(rules(&f, &DetFloatPolicy::default()), Vec::<&str>::new());
}

#[test]
fn fused_multiply_add_is_rejected_unless_explicitly_allowed() {
    let f = build("fused", &[types::F32; 3], &[types::F32], |b, a| {
        let r = b.ins().fma(a[0], a[1], a[2]);
        vec![canon_f32(b, r)]
    });
    let findings = audit(&f, &DetFloatPolicy::default()).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule, "detfloat.fma");
    assert_eq!(findings[0].function, "%fused");
    assert_eq!(findings[0].inst, Some(0), "fma is the first instruction");
    let allowed = DetFloatPolicy {
        allow_fma: true,
        ..DetFloatPolicy::default()
    };
    assert!(rules(&f, &allowed).is_empty());
}

fn call_fn(
    name: ExternalName,
    params: &[types::Type],
    returns: &[types::Type],
    f: &mut FunctionBuilder<'_>,
    args: &[Value],
) -> Vec<Value> {
    let sig_ref = f.import_signature(sig(params, returns));
    let func_ref = f.import_function(ExtFuncData {
        name,
        signature: sig_ref,
        colocated: false,
    });
    let call = f.ins().call(func_ref, args);
    f.inst_results(call).to_vec()
}

#[test]
fn libm_transcendentals_need_the_allowlist() {
    let f = build("trig", &[types::F32], &[types::F32], |b, a| {
        let r = call_fn(
            ExternalName::testcase("sinf"),
            &[types::F32],
            &[types::F32],
            b,
            &[a[0]],
        );
        vec![r[0]]
    });
    assert_eq!(rules(&f, &DetFloatPolicy::default()), vec!["detfloat.libm"]);
    let core_math = DetFloatPolicy {
        allowed_float_calls: vec!["sinf".into()],
        ..DetFloatPolicy::default()
    };
    assert!(rules(&f, &core_math).is_empty());
}

#[test]
fn correctly_rounded_math_symbols_are_allowlisted() {
    for (symbol, ty) in [
        ("vesper_crmath_sin", types::F64),
        ("vesper_crmath_atan2f", types::F32),
        ("vesper_crmath_logf", types::F32),
    ] {
        let two = symbol.contains("atan2");
        let params: Vec<types::Type> = if two { vec![ty, ty] } else { vec![ty] };
        let f = build("crmath", &params, &[ty], |b, a| {
            let r = call_fn(ExternalName::testcase(symbol), &params, &[ty], b, a);
            vec![r[0]]
        });
        assert_eq!(
            rules(&f, &DetFloatPolicy::default()),
            vec!["detfloat.unlisted-float-call"],
            "{symbol}"
        );
        let policy = DetFloatPolicy::default().with_correctly_rounded_math();
        assert!(rules(&f, &policy).is_empty(), "{symbol}");
    }
    // The plain libm names stay flagged even with the crmath allowlist.
    let f = build("libm", &[types::F64], &[types::F64], |b, a| {
        let r = call_fn(
            ExternalName::testcase("sin"),
            &[types::F64],
            &[types::F64],
            b,
            &[a[0]],
        );
        vec![r[0]]
    });
    let policy = DetFloatPolicy::default().with_correctly_rounded_math();
    assert_eq!(rules(&f, &policy), vec!["detfloat.libm"]);
    assert_eq!(
        policy.allowed_float_calls.len(),
        vesper_kinetic::crmath::SYMBOLS.len()
    );
}

#[test]
fn user_named_callees_resolve_through_the_policy_symbol_table() {
    let f = build("user", &[types::F64], &[types::F64], |b, a| {
        let name = b
            .func
            .declare_imported_user_function(UserExternalName::new(0, 7));
        let r = call_fn(
            ExternalName::user(name),
            &[types::F64],
            &[types::F64],
            b,
            &[a[0]],
        );
        vec![r[0]]
    });
    // Unnamed: unknown float callee.
    assert_eq!(
        rules(&f, &DetFloatPolicy::default()),
        vec!["detfloat.unlisted-float-call"]
    );
    let named = DetFloatPolicy {
        user_names: vec![((0, 7), "acos".into())],
        ..DetFloatPolicy::default()
    };
    assert_eq!(rules(&f, &named), vec!["detfloat.libm"]);
    let allowed = DetFloatPolicy {
        allowed_float_calls: vec!["acos".into()],
        ..named
    };
    assert!(rules(&f, &allowed).is_empty());
}

#[test]
fn integer_calls_exact_libcalls_and_fma_libcalls() {
    let f = build("calls", &[types::I32, types::F32], &[types::I32], |b, a| {
        let r = call_fn(
            ExternalName::testcase("hash"),
            &[types::I32],
            &[types::I32],
            b,
            &[a[0]],
        );
        let c = call_fn(
            ExternalName::LibCall(LibCall::CeilF32),
            &[types::F32],
            &[types::F32],
            b,
            &[a[1]],
        );
        let _ = call_fn(
            ExternalName::LibCall(LibCall::FmaF32),
            &[types::F32; 3],
            &[types::F32],
            b,
            &[c[0], c[0], c[0]],
        );
        vec![r[0]]
    });
    // The ceil libcall result (a NaN-producing operation) also escapes into
    // the fma call's arguments uncanonicalised.
    assert_eq!(
        rules(&f, &DetFloatPolicy::default()),
        vec!["detfloat.fma", "detfloat.nan-escape"]
    );
}

#[test]
fn indirect_float_calls_are_rejected() {
    let f = build("indirect", &[types::I64, types::F32], &[], |b, a| {
        let sig_ref = b.import_signature(sig(&[types::F32], &[]));
        b.ins().call_indirect(sig_ref, a[0], &[a[1]]);
        let int_sig = b.import_signature(sig(&[types::I64], &[]));
        b.ins().call_indirect(int_sig, a[0], &[a[0]]);
        vec![]
    });
    assert_eq!(
        rules(&f, &DetFloatPolicy::default()),
        vec!["detfloat.indirect-float-call"]
    );
}

fn lanes(b: &mut FunctionBuilder<'_>, v: Value) -> Vec<Value> {
    (0..4).map(|i| b.ins().extractlane(v, i)).collect()
}

fn reduction(name: &str, shape: fn(&mut FunctionBuilder<'_>, &[Value]) -> Value) -> Function {
    build(name, &[types::F32X4], &[types::F32], |b, a| {
        let l = lanes(b, a[0]);
        let r = shape(b, &l);
        vec![canon_f32(b, r)]
    })
}

fn pairwise(b: &mut FunctionBuilder<'_>, l: &[Value]) -> Value {
    let x = b.ins().fadd(l[0], l[1]);
    let y = b.ins().fadd(l[2], l[3]);
    b.ins().fadd(x, y)
}

fn pairwise_swapped(b: &mut FunctionBuilder<'_>, l: &[Value]) -> Value {
    let x = b.ins().fadd(l[1], l[0]);
    let y = b.ins().fadd(l[3], l[2]);
    b.ins().fadd(y, x)
}

fn sequential(b: &mut FunctionBuilder<'_>, l: &[Value]) -> Value {
    let x = b.ins().fadd(l[0], l[1]);
    let y = b.ins().fadd(x, l[2]);
    b.ins().fadd(y, l[3])
}

fn scrambled(b: &mut FunctionBuilder<'_>, l: &[Value]) -> Value {
    let x = b.ins().fadd(l[0], l[2]);
    let y = b.ins().fadd(l[1], l[3]);
    b.ins().fadd(x, y)
}

#[test]
fn horizontal_reductions_must_follow_the_marked_order() {
    let tree = DetFloatPolicy::default();
    let seq = DetFloatPolicy {
        reduction_order: ReductionOrder::Sequential,
        ..DetFloatPolicy::default()
    };
    let p = reduction("pairwise", pairwise);
    let ps = reduction("pairwise_swapped", pairwise_swapped);
    let s = reduction("sequential", sequential);
    let x = reduction("scrambled", scrambled);
    assert!(rules(&p, &tree).is_empty());
    assert!(
        rules(&ps, &tree).is_empty(),
        "operand order is not significant"
    );
    assert_eq!(rules(&s, &tree), vec!["detfloat.reduction-order"]);
    assert_eq!(rules(&x, &tree), vec!["detfloat.reduction-order"]);
    assert!(rules(&s, &seq).is_empty());
    assert_eq!(rules(&p, &seq), vec!["detfloat.reduction-order"]);
    let finding = &audit(&x, &tree).unwrap()[0];
    assert_eq!(
        finding.inst,
        Some(6),
        "root fadd after four extracts and two adds"
    );
    assert!(finding.message.contains("4-lane"));
}

#[test]
fn partial_and_mixed_lane_sums_are_not_reductions() {
    let f = build("partial", &[types::F32X4], &[types::F32], |b, a| {
        let l = lanes(b, a[0]);
        let x = b.ins().fadd(l[2], l[0]);
        let y = b.ins().fmul(l[1], l[3]);
        let z = b.ins().fadd(x, l[1]);
        let w = b.ins().fadd(z, y); // mixes a product node into a sum
        vec![canon_f32(b, w)]
    });
    assert!(rules(&f, &DetFloatPolicy::default()).is_empty());
}

#[test]
fn uncanonicalized_nan_escapes_are_reported() {
    let ret = build("ret", &[types::F32; 2], &[types::F32], |b, a| {
        vec![b.ins().fdiv(a[0], a[1])]
    });
    assert_eq!(
        rules(&ret, &DetFloatPolicy::default()),
        vec!["detfloat.nan-escape"]
    );

    let store = build("store", &[types::I64, types::F32], &[], |b, a| {
        let s = b.ins().sqrt(a[1]);
        let n = b.ins().fneg(s); // propagates, does not canonicalise
        b.ins().store(MemFlags::trusted(), n, a[0], 0);
        vec![]
    });
    assert_eq!(
        rules(&store, &DetFloatPolicy::default()),
        vec!["detfloat.nan-escape"]
    );

    let bits = build("bits", &[types::F32; 2], &[types::I32], |b, a| {
        let s = b.ins().fsub(a[0], a[1]);
        vec![b.ins().bitcast(types::I32, MemFlags::new(), s)]
    });
    assert_eq!(
        rules(&bits, &DetFloatPolicy::default()),
        vec!["detfloat.nan-escape"]
    );

    // Comparing or converting a NaN does not expose its bits.
    let compare = build(
        "compare",
        &[types::F32; 2],
        &[types::I8, types::I32],
        |b, a| {
            let s = b.ins().fsub(a[0], a[1]);
            let c = b.ins().fcmp(FloatCC::LessThan, s, a[0]);
            let i = b.ins().fcvt_to_sint_sat(types::I32, s);
            vec![c, i]
        },
    );
    assert!(rules(&compare, &DetFloatPolicy::default()).is_empty());

    // Bit-exact operations on inputs never create a new NaN.
    let exact = build("exact", &[types::F32; 2], &[types::F32], |b, a| {
        let n = b.ins().fneg(a[0]);
        vec![b.ins().fcopysign(n, a[1])]
    });
    assert!(rules(&exact, &DetFloatPolicy::default()).is_empty());

    // The ordered form of the pattern also canonicalises.
    let ordered = build("ordered", &[types::F32; 2], &[types::F32], |b, a| {
        let s = b.ins().fmul(a[0], a[1]);
        let nan = b.ins().f32const(f32::from_bits(0x7fc0_0000));
        let ok = b.ins().fcmp(FloatCC::Ordered, s, s);
        vec![b.ins().select(ok, s, nan)]
    });
    assert!(rules(&ordered, &DetFloatPolicy::default()).is_empty());

    // A non-canonical NaN constant does not canonicalise.
    let wrong = build("wrong", &[types::F32; 2], &[types::F32], |b, a| {
        let s = b.ins().fmul(a[0], a[1]);
        let nan = b.ins().f32const(f32::from_bits(0xffc0_0000));
        let bad = b.ins().fcmp(FloatCC::Unordered, s, s);
        vec![b.ins().select(bad, nan, s)]
    });
    assert_eq!(
        rules(&wrong, &DetFloatPolicy::default()),
        vec!["detfloat.nan-escape"]
    );
}

#[test]
fn taint_flows_through_loop_block_parameters() {
    let mut func = Function::with_name_signature(
        UserFuncName::testcase("loop"),
        sig(&[types::F32, types::I32], &[types::F32]),
    );
    let mut fctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut fctx);
    let entry = b.create_block();
    let header = b.create_block();
    let exit = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.append_block_param(header, types::F32);
    b.append_block_param(header, types::I32);
    b.append_block_param(exit, types::F32);
    b.switch_to_block(entry);
    let (x, n) = (b.block_params(entry)[0], b.block_params(entry)[1]);
    b.ins().jump(header, &[x.into(), n.into()]);
    b.switch_to_block(header);
    let (acc, i) = (b.block_params(header)[0], b.block_params(header)[1]);
    let next = b.ins().fmul(acc, acc);
    let i1 = b.ins().iadd_imm(i, -1);
    b.ins()
        .brif(i1, header, &[next.into(), i1.into()], exit, &[acc.into()]);
    b.switch_to_block(exit);
    let out = b.block_params(exit)[0];
    b.ins().return_(&[out]);
    b.seal_all_blocks();
    b.finalize();
    let findings = audit(&func, &DetFloatPolicy::default()).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule, "detfloat.nan-escape");
}

#[test]
fn vector_canonicalisation_and_codegen_flag() {
    let f = build("vec", &[types::F32X4; 2], &[types::F32X4], |b, a| {
        let s = b.ins().fadd(a[0], a[1]);
        let nan = b.ins().f32const(f32::from_bits(0x7fc0_0000));
        let nan = b.ins().splat(types::F32X4, nan);
        let is_nan = b.ins().fcmp(FloatCC::Unordered, s, s);
        let mask = b.ins().bitcast(types::F32X4, MemFlags::new(), is_nan);
        vec![b.ins().bitselect(mask, nan, s)]
    });
    assert!(rules(&f, &DetFloatPolicy::default()).is_empty());

    let raw = build("raw", &[types::F32X4; 2], &[types::F32X4], |b, a| {
        vec![b.ins().fadd(a[0], a[1])]
    });
    assert_eq!(
        rules(&raw, &DetFloatPolicy::default()),
        vec!["detfloat.nan-escape"]
    );
    let mut builder = settings::builder();
    builder.set("enable_nan_canonicalization", "true").unwrap();
    let flags = settings::Flags::new(builder);
    let trusted = DetFloatPolicy::default().with_flags(&flags);
    assert!(trusted.codegen_canonicalizes_nans);
    assert!(rules(&raw, &trusted).is_empty());
}

/// Run Cranelift's real NaN-canonicalisation pass for the host ISA and check
/// that the auditor recognises its output (scalar and vector forms).
#[test]
fn auditor_accepts_cranelifts_own_nan_canonicalisation_pass() {
    let mut builder = settings::builder();
    builder.set("enable_nan_canonicalization", "true").unwrap();
    let flags = settings::Flags::new(builder);
    let isa = match cranelift_codegen::isa::lookup(target_lexicon::Triple::host()) {
        Ok(isa) => isa.finish(flags).expect("host ISA"),
        Err(e) => {
            eprintln!("no Cranelift backend for this host ({e}); skipped");
            return;
        }
    };
    for (name, ty) in [
        ("scalar", types::F32),
        ("vector", types::F32X4),
        ("double", types::F64),
    ] {
        let f = build(name, &[ty; 2], &[ty], |b, a| {
            let m = b.ins().fmul(a[0], a[1]);
            vec![b.ins().fadd(m, a[0])]
        });
        assert_eq!(
            rules(&f, &DetFloatPolicy::default()),
            vec!["detfloat.nan-escape"],
            "{name} before the pass"
        );
        let mut ctx = Context::for_function(f);
        ctx.canonicalize_nans(isa.as_ref()).expect("pass");
        assert!(
            rules(&ctx.func, &DetFloatPolicy::default()).is_empty(),
            "{name} after the pass:\n{}",
            ctx.func.display()
        );
    }
}

#[test]
fn invalid_ir_is_a_typed_error() {
    // A block with no terminator fails Cranelift's verifier.
    let mut func = Function::with_name_signature(UserFuncName::testcase("bad"), sig(&[], &[]));
    let block = func.dfg.make_block();
    func.layout.append_block(block);
    assert!(matches!(
        audit(&func, &DetFloatPolicy::default()),
        Err(vesper_kinetic::AuditError::InvalidIr(_))
    ));
}

#[test]
fn permitted_fma_libcall_results_are_still_nan_tracked() {
    let f = build("fma_lib", &[types::F32; 3], &[types::F32], |b, a| {
        let r = call_fn(
            ExternalName::LibCall(LibCall::FmaF32),
            &[types::F32; 3],
            &[types::F32],
            b,
            &[a[0], a[1], a[2]],
        );
        vec![r[0]]
    });
    let allowed = DetFloatPolicy {
        allow_fma: true,
        ..DetFloatPolicy::default()
    };
    assert_eq!(rules(&f, &allowed), vec!["detfloat.nan-escape"]);
    assert_eq!(
        rules(&f, &DetFloatPolicy::default()),
        vec!["detfloat.fma", "detfloat.nan-escape"]
    );
}
