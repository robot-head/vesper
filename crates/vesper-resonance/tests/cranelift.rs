//! The realtime verifier on real Cranelift IR built with cranelift-frontend.

use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, ExternalName, Function, InstBuilder, LibCall, Signature, StackSlotData,
    StackSlotKind, UserExternalName, UserFuncName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::verify_function;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use vesper_resonance::clif::{ClifError, SymbolEffects, call_graph_from_clif};
use vesper_resonance::confine::{
    Effect, LoopBound, RealtimePolicy, RealtimeRoot, rules, verify_realtime,
};

/// What a test function body does.
#[derive(Clone)]
enum Op {
    /// Call user external name (0, index).
    Call(u32),
    /// Call a libcall.
    Lib(LibCall),
    /// Indirect call through the first parameter.
    Indirect,
    /// Tail call user external name (0, index).
    TailCall(u32),
}

fn sig() -> Signature {
    let mut s = Signature::new(CallConv::Tail);
    s.params.push(AbiParam::new(types::I64));
    s
}

/// Build `fn(i64)` with optional stack slot bytes and an optional loop.
fn build(index: u32, ops: &[Op], slot_bytes: u32, with_loop: bool) -> Function {
    let mut func = Function::with_name_signature(UserFuncName::user(0, index), sig());
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut ctx);
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    b.seal_block(entry);
    let arg = b.block_params(entry)[0];
    if slot_bytes > 0 {
        b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            slot_bytes,
            3,
        ));
    }
    let sigref = b.import_signature(sig());
    let mut tail = None;
    for op in ops {
        match op {
            Op::Call(i) | Op::TailCall(i) => {
                let name = b
                    .func
                    .declare_imported_user_function(UserExternalName::new(0, *i));
                let fref = b.import_function(ExtFuncData {
                    name: ExternalName::user(name),
                    signature: sigref,
                    colocated: false,
                });
                if matches!(op, Op::TailCall(_)) {
                    tail = Some(fref);
                } else {
                    b.ins().call(fref, &[arg]);
                }
            }
            Op::Lib(l) => {
                let fref = b.import_function(ExtFuncData {
                    name: ExternalName::LibCall(*l),
                    signature: sigref,
                    colocated: false,
                });
                b.ins().call(fref, &[arg]);
            }
            Op::Indirect => {
                b.ins().call_indirect(sigref, arg, &[arg]);
            }
        }
    }
    if with_loop {
        let header = b.create_block();
        let exit = b.create_block();
        b.ins().jump(header, &[]);
        b.switch_to_block(header);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().brif(arg, header, &[], exit, &[]);
        let _ = zero;
        b.seal_block(header);
        b.switch_to_block(exit);
        b.seal_block(exit);
    }
    match tail {
        Some(fref) => {
            b.ins().return_call(fref, &[arg]);
        }
        None => {
            b.ins().return_(&[]);
        }
    }
    b.finalize();
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    verify_function(&func, &flags).expect("valid IR");
    func
}

fn table() -> SymbolEffects {
    SymbolEffects::default()
        .user_name(0, 0, "process_quantum")
        .user_name(0, 1, "render_subblock")
        .user_name(0, 2, "voice_math")
        .user_name(0, 10, "malloc")
        .user_name(0, 11, "pthread_mutex_lock")
        .user_name(0, 12, "rt_bus_commit")
        .user_name(0, 13, "mystery")
        .symbol("malloc", vec![Effect::Alloc], 64)
        .symbol("pthread_mutex_lock", vec![Effect::BlockingLock], 64)
        .symbol("rt_bus_commit", vec![Effect::RtBus], 32)
}

fn root(graph: &vesper_resonance::confine::CallGraph, budget: Option<u64>) -> RealtimePolicy {
    RealtimePolicy::new(vec![RealtimeRoot {
        function: graph.by_name("process_quantum").unwrap(),
        stack_budget_bytes: budget,
    }])
}

#[test]
fn transitive_malloc_in_emitted_code_is_found_with_call_instruction() {
    let fns = vec![
        (
            "process_quantum".to_string(),
            build(0, &[Op::Call(12), Op::Call(1)], 0, false),
        ),
        (
            "render_subblock".to_string(),
            build(1, &[Op::Call(2)], 0, false),
        ),
        (
            "voice_math".to_string(),
            build(2, &[Op::Call(10)], 0, false),
        ),
    ];
    let g = call_graph_from_clif(&fns, &table()).unwrap();
    let f = verify_realtime(&g, &root(&g, None)).unwrap();
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].rule, rules::FORBIDDEN_EFFECT);
    assert_eq!(f[0].function, "process_quantum");
    // Call to render_subblock is the second instruction of the root.
    assert_eq!(f[0].inst, Some(1));
    assert!(
        f[0].message
            .contains("process_quantum -> render_subblock -> voice_math -> malloc"),
        "{}",
        f[0].message
    );
}

#[test]
fn confined_emitted_code_passes_and_stack_slots_count() {
    let fns = vec![
        (
            "process_quantum".to_string(),
            build(0, &[Op::Call(12), Op::Call(1)], 256, false),
        ),
        ("render_subblock".to_string(), build(1, &[], 1024, false)),
    ];
    let g = call_graph_from_clif(&fns, &table()).unwrap();
    assert_eq!(g.functions[0].frame_bytes, 256);
    // 256 + max(rt_bus_commit 32, render_subblock 1024).
    assert!(
        verify_realtime(&g, &root(&g, Some(1280)))
            .unwrap()
            .is_empty()
    );
    let f = verify_realtime(&g, &root(&g, Some(1279))).unwrap();
    assert_eq!(f[0].rule, rules::STACK_BUDGET);
}

#[test]
fn tail_calls_and_locks_are_edges() {
    let fns = vec![(
        "process_quantum".to_string(),
        build(0, &[Op::TailCall(11)], 0, false),
    )];
    let g = call_graph_from_clif(&fns, &table()).unwrap();
    let f = verify_realtime(&g, &root(&g, None)).unwrap();
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("BlockingLock"));
}

#[test]
fn undeclared_symbols_and_libcalls_are_unknown_callees() {
    let fns = vec![(
        "process_quantum".to_string(),
        build(0, &[Op::Call(13), Op::Lib(LibCall::Memcpy)], 0, false),
    )];
    let g = call_graph_from_clif(&fns, &table()).unwrap();
    let f = verify_realtime(&g, &root(&g, None)).unwrap();
    let names: Vec<_> = f
        .iter()
        .map(|f| (f.rule, f.message.contains("mystery")))
        .collect();
    assert_eq!(
        names,
        vec![
            (rules::UNKNOWN_CALLEE, true),
            (rules::UNKNOWN_CALLEE, false)
        ]
    );

    // Declaring the libcall makes it a known, allowed callee.
    let t = table().symbol("Memcpy", vec![Effect::WaitFreeAtomic], 0);
    let g = call_graph_from_clif(&fns, &t).unwrap();
    assert_eq!(verify_realtime(&g, &root(&g, None)).unwrap().len(), 1);
}

#[test]
fn call_indirect_needs_policy_targets() {
    let fns = vec![
        (
            "process_quantum".to_string(),
            build(0, &[Op::Indirect], 0, false),
        ),
        ("voice_math".to_string(), build(2, &[], 0, false)),
    ];
    let g = call_graph_from_clif(&fns, &table()).unwrap();
    assert_eq!(g.indirect_calls.len(), 1);
    let mut policy = root(&g, None);
    let f = verify_realtime(&g, &policy).unwrap();
    assert_eq!(f[0].rule, rules::UNRESOLVED_INDIRECT);
    let site = g.indirect_calls[0].site;
    policy.indirect_targets.insert(
        (g.indirect_calls[0].caller, site),
        vec![g.by_name("voice_math").unwrap()],
    );
    assert!(verify_realtime(&g, &policy).unwrap().is_empty());
}

#[test]
fn loops_found_by_cranelift_loop_analysis() {
    let fns = vec![
        (
            "process_quantum".to_string(),
            build(0, &[Op::Call(1)], 0, false),
        ),
        ("render_subblock".to_string(), build(1, &[], 0, true)),
    ];
    let mut t = table();
    let g = call_graph_from_clif(&fns, &t).unwrap();
    assert_eq!(g.functions[1].loops, LoopBound::Unbounded);
    assert_eq!(g.functions[0].loops, LoopBound::NoLoops);
    let f = verify_realtime(&g, &root(&g, None)).unwrap();
    assert_eq!(f[0].rule, rules::UNBOUNDED_LOOP);

    t.loop_bounds.insert("render_subblock".into(), 64);
    let g = call_graph_from_clif(&fns, &t).unwrap();
    assert_eq!(g.functions[1].loops, LoopBound::Bounded(64));
    assert!(verify_realtime(&g, &root(&g, None)).unwrap().is_empty());
}

/// `entry -> A | B`, `A -> B`, `B -> A`: neither block dominates the other,
/// so natural-loop analysis finds no loop; the cycle must still count.
#[test]
fn irreducible_cycles_count_as_loops() {
    let mut func = Function::with_name_signature(UserFuncName::user(0, 0), sig());
    let mut ctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut func, &mut ctx);
    let entry = b.create_block();
    let a = b.create_block();
    let bb = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let arg = b.block_params(entry)[0];
    b.ins().brif(arg, a, &[], bb, &[]);
    b.switch_to_block(a);
    b.ins().jump(bb, &[]);
    b.switch_to_block(bb);
    b.ins().jump(a, &[]);
    b.seal_all_blocks();
    b.finalize();
    let flags = cranelift_codegen::settings::Flags::new(cranelift_codegen::settings::builder());
    verify_function(&func, &flags).expect("valid IR");
    let fns = vec![("process_quantum".to_string(), func)];
    let g = call_graph_from_clif(&fns, &table()).unwrap();
    assert_eq!(g.functions[0].loops, LoopBound::Unbounded);
    let f = verify_realtime(&g, &root(&g, None)).unwrap();
    assert_eq!(f[0].rule, rules::UNBOUNDED_LOOP);
}

#[test]
fn declared_effect_rows_of_defined_functions_are_used() {
    let fns = vec![
        (
            "process_quantum".to_string(),
            build(0, &[Op::Call(1)], 0, false),
        ),
        ("render_subblock".to_string(), build(1, &[], 0, false)),
    ];
    let mut t = table();
    t.defined_effects
        .insert("render_subblock".into(), vec![Effect::Storage]);
    let g = call_graph_from_clif(&fns, &t).unwrap();
    let f = verify_realtime(&g, &root(&g, None)).unwrap();
    assert!(f[0].message.contains("Storage"));
}

#[test]
fn malformed_inputs_are_typed_errors() {
    let f0 = build(0, &[], 0, false);
    let dup = vec![("a".to_string(), f0.clone()), ("a".to_string(), f0)];
    assert_eq!(
        call_graph_from_clif(&dup, &table()),
        Err(ClifError::DuplicateFunction("a".into()))
    );
    let fns = vec![(
        "process_quantum".to_string(),
        build(0, &[Op::Call(99)], 0, false),
    )];
    assert!(matches!(
        call_graph_from_clif(&fns, &table()),
        Err(ClifError::UnnamedUserSymbol { index: 99, .. })
    ));
}
