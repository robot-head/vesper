//! Integration B (#136): `vesper ebpf check` and the shared stack-bound
//! report that merges the eBPF and Silicon stack analyzers.
use vesper::stack_bound::{
    GraphOrigin, RULE_EBPF_KERNEL, StackBoundPolicy, StackBudget, StackGraph, stack_bound_report,
};

fn ebpf_graph(frames: &[u32], calls: &[(u32, u32)]) -> vesper_ebpf::CallGraph {
    vesper_ebpf::CallGraph {
        functions: frames
            .iter()
            .enumerate()
            .map(|(i, &frame_bytes)| vesper_ebpf::FnNode {
                id: vesper_ebpf::FnId(i as u32),
                name: format!("f{i}"),
                frame_bytes,
            })
            .collect(),
        calls: calls
            .iter()
            .map(|&(a, b)| (vesper_ebpf::FnId(a), vesper_ebpf::FnId(b)))
            .collect(),
        indirect: Vec::new(),
    }
}

fn root(name: &str, budget: u32) -> StackBoundPolicy {
    StackBoundPolicy {
        roots: vec![StackBudget {
            function: name.into(),
            budget_bytes: budget,
        }],
        ..Default::default()
    }
}

#[test]
fn one_report_covers_ebpf_and_cranelift_graphs() {
    // eBPF: f0 -> f1 -> f2, 256 + 192 + 64 = 512 bytes: within the kernel limit.
    let g = StackGraph::from(&ebpf_graph(&[256, 192, 64], &[(0, 1), (1, 2)]));
    assert_eq!(g.origin, GraphOrigin::Ebpf);
    let r = stack_bound_report(&g, &root("f0", 512)).unwrap();
    assert!(r.accepted(), "{r:?}");
    assert_eq!(r.roots[0].worst_case_bytes, Some(512));
    assert_eq!(r.roots[0].worst_path, ["f0", "f1", "f2"]);
    // 544 bytes: both the shared budget and the kernel limit reject it.
    let g = StackGraph::from(&ebpf_graph(&[256, 224, 64], &[(0, 1), (1, 2)]));
    let r = stack_bound_report(&g, &root("f0", 512)).unwrap();
    assert!(!r.accepted());
    assert!(!r.roots[0].within_budget);
    assert!(
        r.findings.iter().any(|f| f.rule == RULE_EBPF_KERNEL),
        "{r:?}"
    );
    // Nine frames exceed the kernel's call depth even within 512 bytes.
    let chain: Vec<(u32, u32)> = (0..8).map(|i| (i, i + 1)).collect();
    let g = StackGraph::from(&ebpf_graph(&[32; 9], &chain));
    let r = stack_bound_report(&g, &root("f0", 512)).unwrap();
    assert!(
        r.findings
            .iter()
            .any(|f| f.rule == RULE_EBPF_KERNEL && f.message.contains("call frames")),
        "{r:?}"
    );
    // Recursion is rejected by the shared analysis.
    let g = StackGraph::from(&ebpf_graph(&[32, 32], &[(0, 1), (1, 0)]));
    let r = stack_bound_report(&g, &root("f0", 512)).unwrap();
    assert!(
        r.findings
            .iter()
            .any(|f| f.rule == "silicon.stack.recursion"),
        "{r:?}"
    );
    // The same algorithm on a Silicon graph, with an external bound.
    let silicon = vesper_silicon::stack::CallGraph {
        functions: vec![
            vesper_silicon::stack::FnNode {
                id: vesper_silicon::stack::FnId(0),
                name: "main".into(),
                frame_bytes: 48,
            },
            vesper_silicon::stack::FnNode {
                id: vesper_silicon::stack::FnId(1),
                name: "leaf".into(),
                frame_bytes: 16,
            },
        ],
        calls: vec![(
            vesper_silicon::stack::FnId(0),
            vesper_silicon::stack::FnId(1),
        )],
        indirect_calls: vec![],
        external_calls: vec![vesper_silicon::stack::ExternalCall {
            caller: vesper_silicon::stack::FnId(1),
            symbol: "memcpy".into(),
            inst: Some(3),
        }],
    };
    let g = StackGraph::from(&silicon);
    assert_eq!(g.origin, GraphOrigin::Cranelift);
    let r = stack_bound_report(&g, &root("main", 128)).unwrap();
    assert!(
        r.findings
            .iter()
            .any(|f| f.rule == "silicon.stack.unbounded_external_call")
    );
    let mut policy = root("main", 128);
    policy.external_bounds.insert("memcpy".into(), 32);
    let r = stack_bound_report(&g, &policy).unwrap();
    assert!(r.accepted(), "{r:?}");
    assert_eq!(r.roots[0].worst_case_bytes, Some(96));
    // Malformed graphs are errors, not reports.
    let mut bad = StackGraph::from(&ebpf_graph(&[32], &[]));
    bad.calls.push((0, 5));
    assert!(stack_bound_report(&bad, &root("f0", 512)).is_err());
}
