//! Transitive realtime confinement over hand-built call graphs.

use vesper_resonance::confine::{
    CallGraph, Effect, FnId, FnNode, GraphError, IndirectCall, LoopBound, RealtimePolicy,
    RealtimeRoot, rules, verify_realtime,
};

fn graph(nodes: Vec<FnNode>, calls: &[(u32, u32)]) -> CallGraph {
    CallGraph {
        functions: nodes,
        calls: calls.iter().map(|&(a, b)| (FnId(a), FnId(b))).collect(),
        indirect_calls: Vec::new(),
    }
}

fn rules_of(f: &[vesper_resonance::VerifierFinding]) -> Vec<&'static str> {
    f.iter().map(|f| f.rule).collect()
}

#[test]
fn allocation_two_calls_deep_is_rejected_with_its_path() {
    let g = graph(
        vec![
            FnNode::new(
                "process_quantum",
                vec![Effect::Realtime, Effect::RtBus],
                256,
            ),
            FnNode::new("render_voice", vec![Effect::Isa], 128),
            FnNode::new("log_debug", vec![Effect::Alloc], 64),
        ],
        &[(0, 1), (1, 2)],
    );
    let findings = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
    assert_eq!(rules_of(&findings), vec![rules::FORBIDDEN_EFFECT]);
    assert_eq!(findings[0].function, "process_quantum");
    assert!(
        findings[0]
            .message
            .contains("process_quantum -> render_voice -> log_debug"),
        "{}",
        findings[0].message
    );
    assert!(findings[0].message.contains("`Alloc`"));
}

/// The design's sketch accepts any function whose *own* effect row is in an
/// allowlist. That is a name check: here the root's own row is clean and
/// every name looks harmless, yet a callee three levels down takes a mutex.
#[test]
fn clean_own_effect_row_does_not_hide_transitive_lock() {
    let g = graph(
        vec![
            FnNode::new("rt_root", vec![Effect::Realtime], 0),
            FnNode::new("Isa", vec![], 0),
            FnNode::new("Realtime", vec![], 0),
            FnNode::new("RtBus", vec![Effect::BlockingLock], 0),
        ],
        &[(0, 1), (1, 2), (2, 3)],
    );
    let findings = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
    assert_eq!(rules_of(&findings), vec![rules::FORBIDDEN_EFFECT]);
    assert!(findings[0].message.contains("BlockingLock"));
}

#[test]
fn confined_root_has_no_findings_and_unreachable_violations_are_ignored() {
    let g = graph(
        vec![
            FnNode::new("rt", vec![Effect::Realtime, Effect::ConstDenorm], 100),
            FnNode::new("simd", vec![Effect::Isa, Effect::WaitFreeAtomic], 50),
            FnNode::new("ui_only", vec![Effect::Alloc, Effect::Net], 0),
        ],
        &[(0, 1), (2, 1)],
    );
    assert!(
        verify_realtime(&g, &RealtimePolicy::root(FnId(0)))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn every_forbidden_effect_kind_is_reported() {
    for eff in [
        Effect::Alloc,
        Effect::Dealloc,
        Effect::BlockingLock,
        Effect::BlockingSyscall,
        Effect::Async,
        Effect::Net,
        Effect::Storage,
        Effect::Unwind,
        Effect::Ffi,
        Effect::Other("Gpu".into()),
    ] {
        let g = graph(
            vec![
                FnNode::new("rt", vec![], 0),
                FnNode::new("x", vec![eff.clone()], 0),
            ],
            &[(0, 1)],
        );
        let f = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
        assert_eq!(rules_of(&f), vec![rules::FORBIDDEN_EFFECT], "{eff:?}");
    }
}

#[test]
fn policy_can_allow_an_extra_effect() {
    let g = graph(
        vec![FnNode::new("rt", vec![Effect::Other("Gpio".into())], 0)],
        &[],
    );
    let mut policy = RealtimePolicy::root(FnId(0));
    assert_eq!(verify_realtime(&g, &policy).unwrap().len(), 1);
    policy.allowed_effects.insert(Effect::Other("Gpio".into()));
    assert!(verify_realtime(&g, &policy).unwrap().is_empty());
}

#[test]
fn unknown_callee_is_rejected() {
    let g = graph(
        vec![FnNode::new("rt", vec![], 0), FnNode::unknown("extern_fn")],
        &[(0, 1)],
    );
    let f = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
    assert_eq!(rules_of(&f), vec![rules::UNKNOWN_CALLEE]);
}

#[test]
fn indirect_calls_are_rejected_unless_targets_are_listed_and_then_checked() {
    let mut g = graph(
        vec![
            FnNode::new("rt", vec![], 0),
            FnNode::new("good_cb", vec![Effect::Isa], 0),
            FnNode::new("bad_cb", vec![Effect::BlockingSyscall], 0),
        ],
        &[],
    );
    g.indirect_calls.push(IndirectCall {
        caller: FnId(0),
        site: 7,
    });
    let mut policy = RealtimePolicy::root(FnId(0));
    let f = verify_realtime(&g, &policy).unwrap();
    assert_eq!(rules_of(&f), vec![rules::UNRESOLVED_INDIRECT]);

    policy.indirect_targets.insert((FnId(0), 7), vec![FnId(1)]);
    assert!(verify_realtime(&g, &policy).unwrap().is_empty());

    policy
        .indirect_targets
        .insert((FnId(0), 7), vec![FnId(1), FnId(2)]);
    let f = verify_realtime(&g, &policy).unwrap();
    assert_eq!(rules_of(&f), vec![rules::FORBIDDEN_EFFECT]);
    assert_eq!(f[0].inst, Some(7));
}

#[test]
fn loops_follow_the_policy() {
    let g = graph(
        vec![
            FnNode::new("rt", vec![], 0).with_loops(LoopBound::Bounded(64)),
            FnNode::new("spin", vec![], 0).with_loops(LoopBound::Unbounded),
        ],
        &[(0, 1)],
    );
    let mut policy = RealtimePolicy::root(FnId(0));
    assert_eq!(
        rules_of(&verify_realtime(&g, &policy).unwrap()),
        vec![rules::UNBOUNDED_LOOP]
    );
    policy.reject_unbounded_loops = false;
    assert!(verify_realtime(&g, &policy).unwrap().is_empty());
    policy.max_loop_iterations = Some(32);
    assert_eq!(
        rules_of(&verify_realtime(&g, &policy).unwrap()),
        vec![rules::LOOP_BOUND]
    );
}

#[test]
fn recursion_is_rejected() {
    let g = graph(
        vec![
            FnNode::new("rt", vec![], 16),
            FnNode::new("a", vec![], 16),
            FnNode::new("b", vec![], 16),
        ],
        &[(0, 1), (1, 2), (2, 1)],
    );
    let f = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
    assert_eq!(rules_of(&f), vec![rules::RECURSION]);
}

#[test]
fn worst_case_stack_depth_is_the_deepest_path() {
    // rt(100) -> a(1000) -> c(10); rt -> b(50) -> c. Deepest: 1110.
    let g = graph(
        vec![
            FnNode::new("rt", vec![], 100),
            FnNode::new("a", vec![], 1000),
            FnNode::new("b", vec![], 50),
            FnNode::new("c", vec![], 10),
        ],
        &[(0, 1), (0, 2), (1, 3), (2, 3)],
    );
    let policy = |budget| {
        RealtimePolicy::new(vec![RealtimeRoot {
            function: FnId(0),
            stack_budget_bytes: Some(budget),
        }])
    };
    assert!(verify_realtime(&g, &policy(1110)).unwrap().is_empty());
    let f = verify_realtime(&g, &policy(1109)).unwrap();
    assert_eq!(rules_of(&f), vec![rules::STACK_BUDGET]);
    assert!(f[0].message.contains("1110"));
}

#[test]
fn stack_overflowing_u64_is_a_finding_not_a_panic() {
    let g = graph(
        vec![
            FnNode::new("rt", vec![], u64::MAX),
            FnNode::new("a", vec![], 1),
        ],
        &[(0, 1)],
    );
    let f = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
    assert_eq!(rules_of(&f), vec![rules::STACK_BUDGET]);
}

#[test]
fn long_chains_do_not_overflow_the_native_stack() {
    let n = 200_000u32;
    let nodes = (0..n)
        .map(|i| FnNode::new(format!("f{i}"), vec![], 1))
        .collect();
    let calls: Vec<(u32, u32)> = (0..n - 1).map(|i| (i, i + 1)).collect();
    let mut g = graph(nodes, &calls);
    g.functions[(n - 1) as usize].effects.push(Effect::Alloc);
    let f = verify_realtime(&g, &RealtimePolicy::root(FnId(0))).unwrap();
    assert_eq!(rules_of(&f), vec![rules::FORBIDDEN_EFFECT]);
}

#[test]
fn each_root_is_checked_independently() {
    let g = graph(
        vec![
            FnNode::new("audio", vec![], 0),
            FnNode::new("motor", vec![], 0),
            FnNode::new("alloc", vec![Effect::Alloc], 0),
        ],
        &[(1, 2)],
    );
    let policy = RealtimePolicy::new(vec![
        RealtimeRoot {
            function: FnId(0),
            stack_budget_bytes: None,
        },
        RealtimeRoot {
            function: FnId(1),
            stack_budget_bytes: None,
        },
    ]);
    let f = verify_realtime(&g, &policy).unwrap();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].function, "motor");
}

#[test]
fn malformed_graphs_are_errors() {
    let g = graph(vec![FnNode::new("rt", vec![], 0)], &[(0, 5)]);
    assert_eq!(
        verify_realtime(&g, &RealtimePolicy::root(FnId(0))),
        Err(GraphError::UnknownFnId(FnId(5)))
    );
    let g = graph(vec![FnNode::new("rt", vec![], 0)], &[]);
    assert_eq!(
        verify_realtime(&g, &RealtimePolicy::root(FnId(3))),
        Err(GraphError::UnknownFnId(FnId(3)))
    );
    let mut policy = RealtimePolicy::root(FnId(0));
    policy.indirect_targets.insert((FnId(0), 1), vec![FnId(0)]);
    assert_eq!(
        verify_realtime(&g, &policy),
        Err(GraphError::UnknownIndirectSite {
            caller: FnId(0),
            site: 1
        })
    );
}
