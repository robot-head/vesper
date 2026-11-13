//! Acceptance and per-rule rejection tests for the §23.5 constant-time
//! verifier, over real Cranelift functions built with cranelift-frontend.

use cranelift_codegen::ir::{
    AbiParam, AliasRegion, ExtFuncData, ExternalName, Function, Inst, InstBuilder, JumpTableData,
    MemFlags, Signature, StackSlotData, StackSlotKind, TrapCode, UserFuncName, Value,
    condcodes::IntCC,
    types::{I32, I64},
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use vesper_vault::{CtPolicy, DECLASSIFY_MARKER, VerifierFinding, verify_constant_time};

fn sig(params: usize, rets: usize) -> Signature {
    let mut s = Signature::new(CallConv::SystemV);
    for _ in 0..params {
        s.params.push(AbiParam::new(I64));
    }
    for _ in 0..rets {
        s.returns.push(AbiParam::new(I64));
    }
    s
}

/// Build `fn name(i64 x params) -> i64` with `body` filling the entry block
/// (already switched to, params appended). `body` must terminate every block.
fn build(
    name: &str,
    params: usize,
    body: impl FnOnce(&mut FunctionBuilder<'_>, &[Value]),
) -> Function {
    let mut func = Function::with_name_signature(UserFuncName::testcase(name), sig(params, 1));
    let mut ctx = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut func, &mut ctx);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let ps = b.block_params(entry).to_vec();
        body(&mut b, &ps);
        b.seal_all_blocks();
        b.finalize();
    }
    // Every test function is well-formed Cranelift IR.
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    if let Err(e) = cranelift_codegen::verify_function(&func, &flags) {
        panic!("invalid test IR for {name}: {e}\n{}", func.display());
    }
    func
}

fn def_inst(b: &FunctionBuilder<'_>, v: Value) -> Inst {
    b.func.dfg.value_def(v).unwrap_inst()
}

fn rules(f: &[VerifierFinding]) -> Vec<&'static str> {
    f.iter().map(|x| x.rule).collect()
}

fn import(
    b: &mut FunctionBuilder<'_>,
    name: &str,
    params: usize,
) -> cranelift_codegen::ir::FuncRef {
    let sigref = b.import_signature(sig(params, 1));
    b.import_function(ExtFuncData {
        name: ExternalName::testcase(name),
        signature: sigref,
        colocated: false,
    })
}

/// The accepted kernel: a constant-time comparison of two secret buffers of
/// public length `len`, accumulating differences in a loop block parameter,
/// with a branchless zero test, a `select` on a secret mask and an explicit
/// declassification of the verdict before the (public) loop exit.
fn ct_compare() -> Function {
    build("ct_compare", 3, |b, p| {
        let (pe, pc, len) = (p[0], p[1], p[2]);
        let header = b.create_block();
        b.append_block_param(header, I64); // i
        b.append_block_param(header, I64); // acc (secret)
        let body = b.create_block();
        let exit = b.create_block();
        b.append_block_param(exit, I64);
        let zero = b.ins().iconst(I64, 0);
        b.ins().jump(header, &[zero.into(), zero.into()]);

        b.switch_to_block(header);
        let i = b.block_params(header)[0];
        let acc = b.block_params(header)[1];
        let more = b.ins().icmp(IntCC::UnsignedLessThan, i, len);
        b.ins().brif(more, body, &[], exit, &[acc.into()]);

        b.switch_to_block(body);
        let off = b.ins().ishl_imm(i, 3);
        let ae = b.ins().iadd(pe, off);
        let ac = b.ins().iadd(pc, off);
        let e = b.ins().load(I64, MemFlags::trusted(), ae, 0);
        let c = b.ins().load(I64, MemFlags::trusted(), ac, 0);
        let d = b.ins().bxor(e, c);
        let acc2 = b.ins().bor(acc, d);
        let i2 = b.ins().iadd_imm(i, 1);
        b.ins().jump(header, &[i2.into(), acc2.into()]);

        b.switch_to_block(exit);
        let acc = b.block_params(exit)[0];
        // ((acc | -acc) >> 63) ^ 1: 1 when every word matched.
        let neg = b.ins().ineg(acc);
        let or = b.ins().bor(acc, neg);
        let bit = b.ins().ushr_imm(or, 63);
        let eq = b.ins().bxor_imm(bit, 1);
        // Branchless select on the secret bit: still accepted.
        let one = b.ins().iconst(I64, 1);
        let two = b.ins().iconst(I64, 2);
        let sel = b.ins().select(eq, one, two);
        let declass = import(b, "vesper_ct_declassify", 1);
        let call = b.ins().call(declass, &[sel]);
        let verdict = b.inst_results(call)[0];
        // Branching on the declassified verdict is allowed.
        let yes = b.create_block();
        let no = b.create_block();
        b.ins().brif(verdict, yes, &[], no, &[]);
        b.switch_to_block(yes);
        b.ins().return_(&[verdict]);
        b.switch_to_block(no);
        b.ins().return_(&[zero]);
    })
}

fn pointer_policy() -> CtPolicy {
    CtPolicy {
        secret_pointer_params: vec![0, 1],
        ..CtPolicy::default()
    }
}

#[test]
fn accepts_constant_time_compare_loop() {
    let f = ct_compare();
    let findings = verify_constant_time(&f, &pointer_policy());
    assert!(findings.is_empty(), "{findings:#?}\n{}", f.display());
}

#[test]
fn rejects_branch_on_declassify_removed() {
    // Same kernel, but the declassify marker is not in the policy: the
    // marker is now an unknown callee receiving a secret.
    let f = ct_compare();
    let policy = CtPolicy {
        declassify: vec![],
        ..pointer_policy()
    };
    let findings = verify_constant_time(&f, &policy);
    let r = rules(&findings);
    assert!(r.contains(&"secret-call-unknown"), "{findings:#?}");
}

#[test]
fn rejects_secret_branch_with_index() {
    let mut brif = None;
    let f = build("secret_branch", 1, |b, p| {
        let t = b.create_block();
        let e = b.create_block();
        brif = Some(b.ins().brif(p[0], t, &[], e, &[]));
        b.switch_to_block(t);
        b.ins().return_(&[p[0]]);
        b.switch_to_block(e);
        let z = b.ins().iconst(I64, 0);
        b.ins().return_(&[z]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].rule, "secret-branch");
    assert_eq!(findings[0].inst, Some(brif.unwrap().as_u32()));
    assert_eq!(findings[0].function, "%secret_branch");
    // With no secret parameters the same function is accepted.
    assert!(verify_constant_time(&f, &CtPolicy::default()).is_empty());
}

#[test]
fn secret_reaches_branch_only_through_loop_back_edge() {
    // x starts public (0); the back edge feeds it the secret parameter. Only
    // a fixpoint over block parameters sees the secret at the brif.
    let mut brif = None;
    let f = build("loop_carried", 2, |b, p| {
        let header = b.create_block();
        b.append_block_param(header, I64);
        let latch = b.create_block();
        let exit = b.create_block();
        let z = b.ins().iconst(I64, 0);
        b.ins().jump(header, &[z.into()]);
        b.switch_to_block(header);
        let x = b.block_params(header)[0];
        brif = Some(b.ins().brif(x, exit, &[], latch, &[]));
        b.switch_to_block(latch);
        let y = b.ins().bxor(x, p[1]);
        b.ins().jump(header, &[y.into()]);
        b.switch_to_block(exit);
        b.ins().return_(&[p[0]]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([1]));
    assert_eq!(rules(&findings), vec!["secret-branch"], "{findings:#?}");
    assert_eq!(findings[0].inst, Some(brif.unwrap().as_u32()));
    assert!(verify_constant_time(&f, &CtPolicy::secret_params([0])).is_empty());
}

#[test]
fn rejects_secret_br_table() {
    let mut inst = None;
    let f = build("secret_table", 1, |b, p| {
        let a = b.create_block();
        let c = b.create_block();
        let call_a = b.func.dfg.block_call(a, &[]);
        let call_c = b.func.dfg.block_call(c, &[]);
        let jt = b.create_jump_table(JumpTableData::new(call_c, &[call_a]));
        let idx = b.ins().ireduce(I32, p[0]);
        inst = Some(b.ins().br_table(idx, jt));
        b.switch_to_block(a);
        b.ins().return_(&[p[0]]);
        b.switch_to_block(c);
        b.ins().return_(&[p[0]]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(rules(&findings), vec!["secret-branch-table"]);
    assert_eq!(findings[0].inst, Some(inst.unwrap().as_u32()));
}

#[test]
fn rejects_sbox_lookup_indexed_by_secret_memory() {
    // A T-table lookup: the key byte is loaded through a secret pointer and
    // then used to index a public table.
    let mut lookup = None;
    let f = build("sbox", 2, |b, p| {
        let (key_ptr, table) = (p[0], p[1]);
        let k = b.ins().uload8(I64, MemFlags::trusted(), key_ptr, 0);
        let off = b.ins().ishl_imm(k, 3);
        let addr = b.ins().iadd(table, off);
        let v = b.ins().load(I64, MemFlags::trusted(), addr, 0);
        lookup = Some(def_inst(b, v));
        b.ins().return_(&[v]);
    });
    let policy = CtPolicy {
        secret_pointer_params: vec![0],
        ..CtPolicy::default()
    };
    let findings = verify_constant_time(&f, &policy);
    assert_eq!(
        rules(&findings),
        vec!["secret-load-address"],
        "{findings:#?}"
    );
    assert_eq!(findings[0].inst, Some(lookup.unwrap().as_u32()));
}

#[test]
fn rejects_secret_store_address() {
    let mut st = None;
    let f = build("scatter", 2, |b, p| {
        let addr = b.ins().iadd(p[0], p[1]);
        st = Some(b.ins().store(MemFlags::trusted(), p[1], addr, 0));
        b.ins().return_(&[p[0]]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([1]));
    assert_eq!(rules(&findings), vec!["secret-store-address"]);
    assert_eq!(findings[0].inst, Some(st.unwrap().as_u32()));
    // Storing a secret VALUE through a public address is allowed.
    let g = build("store_value", 2, |b, p| {
        b.ins().store(MemFlags::trusted(), p[1], p[0], 0);
        b.ins().return_(&[p[0]]);
    });
    assert!(verify_constant_time(&g, &CtPolicy::secret_params([1])).is_empty());
}

#[test]
fn rejects_each_variable_latency_division() {
    for (name, which) in [("udiv", 0), ("sdiv", 1), ("urem", 2), ("srem", 3)] {
        let mut inst = None;
        let f = build(name, 2, |b, p| {
            let v = match which {
                0 => b.ins().udiv(p[0], p[1]),
                1 => b.ins().sdiv(p[0], p[1]),
                2 => b.ins().urem(p[0], p[1]),
                _ => b.ins().srem(p[0], p[1]),
            };
            inst = Some(def_inst(b, v));
            b.ins().return_(&[v]);
        });
        // Secret numerator and secret denominator are both rejected.
        for secret in [0, 1] {
            let findings = verify_constant_time(&f, &CtPolicy::secret_params([secret]));
            assert_eq!(rules(&findings), vec!["variable-latency-div"], "{name}");
            assert_eq!(findings[0].inst, Some(inst.unwrap().as_u32()));
        }
        assert!(verify_constant_time(&f, &CtPolicy::default()).is_empty());
    }
}

#[test]
fn calls_unknown_trusted_and_declassify() {
    let mut call_inst = None;
    let f = build("calls", 1, |b, p| {
        let helper = import(b, "helper", 1);
        let c = b.ins().call(helper, &[p[0]]);
        call_inst = Some(c);
        let r = b.inst_results(c)[0];
        b.ins().return_(&[r]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(rules(&findings), vec!["secret-call-unknown"]);
    assert_eq!(findings[0].inst, Some(call_inst.unwrap().as_u32()));
    // Trusting the callee accepts the call; its result stays secret.
    let trusted = CtPolicy {
        constant_time_callees: vec!["%helper".into()],
        ..CtPolicy::secret_params([0])
    };
    assert!(verify_constant_time(&f, &trusted).is_empty());

    // A trusted callee's secret result still may not drive a branch.
    let g = build("trusted_then_branch", 1, |b, p| {
        let helper = import(b, "helper", 1);
        let c = b.ins().call(helper, &[p[0]]);
        let r = b.inst_results(c)[0];
        let t = b.create_block();
        let e = b.create_block();
        b.ins().brif(r, t, &[], e, &[]);
        b.switch_to_block(t);
        b.ins().return_(&[r]);
        b.switch_to_block(e);
        b.ins().return_(&[r]);
    });
    assert_eq!(
        rules(&verify_constant_time(&g, &trusted)),
        vec!["secret-branch"]
    );

    // After the declassify marker the branch is accepted.
    let h = build("declassified_branch", 1, |b, p| {
        let d = import(b, &DECLASSIFY_MARKER[1..], 1);
        let c = b.ins().call(d, &[p[0]]);
        let r = b.inst_results(c)[0];
        let t = b.create_block();
        let e = b.create_block();
        b.ins().brif(r, t, &[], e, &[]);
        b.switch_to_block(t);
        b.ins().return_(&[r]);
        b.switch_to_block(e);
        b.ins().return_(&[r]);
    });
    assert!(verify_constant_time(&h, &CtPolicy::secret_params([0])).is_empty());
}

#[test]
fn rejects_pointer_to_secret_memory_passed_to_unknown_callee() {
    let f = build("leak_ptr", 1, |b, p| {
        let helper = import(b, "memcmp_vartime", 1);
        let c = b.ins().call(helper, &[p[0]]);
        let r = b.inst_results(c)[0];
        b.ins().return_(&[r]);
    });
    let policy = CtPolicy {
        secret_pointer_params: vec![0],
        ..CtPolicy::default()
    };
    assert_eq!(
        rules(&verify_constant_time(&f, &policy)),
        vec!["secret-call-unknown"]
    );
}

#[test]
fn rejects_secret_indirect_call_target() {
    let mut inst = None;
    let f = build("indirect", 2, |b, p| {
        let sigref = b.import_signature(sig(1, 1));
        let c = b.ins().call_indirect(sigref, p[0], &[p[1]]);
        inst = Some(c);
        let r = b.inst_results(c)[0];
        b.ins().return_(&[r]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(rules(&findings), vec!["secret-call-target"]);
    assert_eq!(findings[0].inst, Some(inst.unwrap().as_u32()));
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([1]));
    assert_eq!(rules(&findings), vec!["secret-call-unknown"]);
}

#[test]
fn rejects_secret_conditional_trap() {
    let mut inst = None;
    let f = build("trap", 1, |b, p| {
        inst = Some(b.ins().trapz(p[0], TrapCode::user(1).unwrap()));
        b.ins().return_(&[p[0]]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(rules(&findings), vec!["secret-trap"]);
    assert_eq!(findings[0].inst, Some(inst.unwrap().as_u32()));
}

#[test]
fn secret_through_stack_slot_is_tracked() {
    let mut brif = None;
    let f = build("spill", 1, |b, p| {
        let ss = b.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        b.ins().stack_store(p[0], ss, 0);
        let v = b.ins().stack_load(I64, ss, 0);
        let t = b.create_block();
        let e = b.create_block();
        brif = Some(b.ins().brif(v, t, &[], e, &[]));
        b.switch_to_block(t);
        b.ins().return_(&[v]);
        b.switch_to_block(e);
        b.ins().return_(&[v]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(rules(&findings), vec!["secret-branch"]);
    assert_eq!(findings[0].inst, Some(brif.unwrap().as_u32()));
}

#[test]
fn secret_spilled_to_untracked_memory_taints_later_loads() {
    let f = build("smash", 3, |b, p| {
        b.ins().store(MemFlags::trusted(), p[0], p[1], 0);
        let v = b.ins().load(I64, MemFlags::trusted(), p[2], 0);
        let t = b.create_block();
        let e = b.create_block();
        b.ins().brif(v, t, &[], e, &[]);
        b.switch_to_block(t);
        b.ins().return_(&[v]);
        b.switch_to_block(e);
        b.ins().return_(&[v]);
    });
    assert_eq!(
        rules(&verify_constant_time(&f, &CtPolicy::secret_params([0]))),
        vec!["secret-branch"]
    );
    assert!(verify_constant_time(&f, &CtPolicy::default()).is_empty());
}

#[test]
fn secret_alias_region_loads_are_secret() {
    let f = build("region", 1, |b, p| {
        let mut flags = MemFlags::trusted();
        flags.set_alias_region(Some(AliasRegion::Heap));
        let v = b.ins().load(I64, flags, p[0], 0);
        let addr = b.ins().iadd(p[0], v);
        let w = b.ins().load(I64, MemFlags::trusted(), addr, 0);
        b.ins().return_(&[w]);
    });
    let policy = CtPolicy {
        secret_regions: vec![AliasRegion::Heap],
        ..CtPolicy::default()
    };
    assert_eq!(
        rules(&verify_constant_time(&f, &policy)),
        vec!["secret-load-address"]
    );
    assert!(verify_constant_time(&f, &CtPolicy::default()).is_empty());
}

#[test]
fn invalid_policy_is_a_finding_not_a_panic() {
    let f = build("tiny", 1, |b, p| {
        b.ins().return_(&[p[0]]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([3]));
    assert_eq!(rules(&findings), vec!["invalid-policy"]);
    assert_eq!(findings[0].inst, None);
    let empty = Function::new();
    assert!(verify_constant_time(&empty, &CtPolicy::secret_params([0])).is_empty());
}

#[test]
fn multiple_violations_are_all_reported_once() {
    let f = build("many", 2, |b, p| {
        let q = b.ins().udiv(p[1], p[0]);
        let a = b.ins().iadd(p[1], q);
        let v = b.ins().load(I64, MemFlags::trusted(), a, 0);
        let t = b.create_block();
        let e = b.create_block();
        b.ins().brif(p[0], t, &[], e, &[]);
        b.switch_to_block(t);
        b.ins().return_(&[v]);
        b.switch_to_block(e);
        b.ins().return_(&[v]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(
        rules(&findings),
        vec![
            "variable-latency-div",
            "secret-load-address",
            "secret-branch"
        ],
        "{findings:#?}"
    );
}

#[test]
fn buffer_holding_a_spilled_secret_cannot_reach_an_unknown_callee() {
    // store secret -> public buffer, then hand that buffer to a helper.
    let mut call = None;
    let f = build("spill_then_call", 2, |b, p| {
        b.ins().store(MemFlags::trusted(), p[0], p[1], 0);
        let helper = import(b, "inspect", 1);
        let c = b.ins().call(helper, &[p[1]]);
        call = Some(c);
        let r = b.inst_results(c)[0];
        b.ins().return_(&[r]);
    });
    let findings = verify_constant_time(&f, &CtPolicy::secret_params([0]));
    assert_eq!(
        rules(&findings),
        vec!["secret-call-unknown"],
        "{findings:#?}"
    );
    assert_eq!(findings[0].inst, Some(call.unwrap().as_u32()));
    assert!(verify_constant_time(&f, &CtPolicy::default()).is_empty());
}

#[test]
fn trusted_callee_writes_taint_its_output_buffer() {
    // write_secret(secret, out) is trusted constant time, but what it wrote
    // to `out` is secret: branching on a load from `out` is rejected.
    let mut brif = None;
    let f = build("trusted_writes", 2, |b, p| {
        let helper = import(b, "write_secret", 2);
        b.ins().call(helper, &[p[0], p[1]]);
        let v = b.ins().load(I64, MemFlags::trusted(), p[1], 0);
        let t = b.create_block();
        let e = b.create_block();
        brif = Some(b.ins().brif(v, t, &[], e, &[]));
        b.switch_to_block(t);
        b.ins().return_(&[v]);
        b.switch_to_block(e);
        b.ins().return_(&[v]);
    });
    let policy = CtPolicy {
        constant_time_callees: vec!["%write_secret".into()],
        ..CtPolicy::secret_params([0])
    };
    let findings = verify_constant_time(&f, &policy);
    assert_eq!(rules(&findings), vec!["secret-branch"], "{findings:#?}");
    assert_eq!(findings[0].inst, Some(brif.unwrap().as_u32()));
    // With a public first argument the helper writes nothing secret.
    let public = CtPolicy {
        constant_time_callees: vec!["%write_secret".into()],
        ..CtPolicy::default()
    };
    assert!(verify_constant_time(&f, &public).is_empty());
}
