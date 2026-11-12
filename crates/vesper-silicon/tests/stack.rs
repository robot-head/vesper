//! Stack analysis on real Cranelift functions compiled for two ISAs.

mod common;

use common::{ceil_user, func};
use vesper_silicon::Arch;
use vesper_silicon::stack::{
    Program, RULE_BUDGET, RULE_EXTERNAL, RULE_INDIRECT, RULE_RECURSION, StackPolicy, StackRoot,
};

const ISAS: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

fn policy(root: &str, budget: u32) -> StackPolicy {
    StackPolicy {
        roots: vec![StackRoot {
            function: root.to_string(),
            budget_bytes: budget,
        }],
        ..StackPolicy::default()
    }
}

fn tree() -> Vec<cranelift_codegen::ir::Function> {
    vec![
        func("task", 32, &["parse", "log"], false),
        func("parse", 256, &["leaf"], false),
        func("log", 64, &["leaf"], false),
        func("leaf", 16, &[], false),
    ]
}

#[test]
fn frames_are_measured_from_machine_code_and_match_the_layout() {
    for arch in ISAS {
        let p = Program::compile(arch, tree()).unwrap();
        for m in p.measurements() {
            let total = m.prologue.total().unwrap();
            assert!(
                m.prologue.instructions > 0,
                "{arch:?} {}: empty prologue",
                m.name
            );
            assert!(total >= m.cranelift_frame_size, "{arch:?} {m:?}");
            // Every function reserves at least its explicit slot.
            let slot = match m.name.as_str() {
                "task" => 32,
                "parse" => 256,
                "log" => 64,
                _ => 16,
            };
            assert!(m.prologue.sp_sub_bytes >= slot, "{arch:?} {m:?}");
            // 16-byte stack alignment holds at every call boundary.
            assert_eq!(total % 16, 0, "{arch:?} {m:?}");
        }
        let fr: Vec<_> = p.graph().functions.iter().map(|f| f.frame_bytes).collect();
        let x: Vec<_> = p
            .measurements()
            .iter()
            .map(|m| m.prologue.total().unwrap())
            .collect();
        assert_eq!(fr, x);
    }
}

#[test]
fn call_graph_comes_from_call_instructions() {
    let p = Program::compile(Arch::X86_64, tree()).unwrap();
    let g = p.graph();
    let id = |n| g.id_of(n).unwrap();
    let mut calls = g.calls.clone();
    calls.sort();
    let mut want = vec![
        (id("task"), id("parse")),
        (id("task"), id("log")),
        (id("parse"), id("leaf")),
        (id("log"), id("leaf")),
    ];
    want.sort();
    assert_eq!(calls, want);
    assert!(g.indirect_calls.is_empty() && g.external_calls.is_empty());
}

#[test]
fn worst_path_and_budget_on_both_isas() {
    for arch in ISAS {
        let p = Program::compile(arch, tree()).unwrap();
        let g = p.graph();
        let frame = |n: &str| u64::from(g.functions[g.id_of(n).unwrap().0 as usize].frame_bytes);
        let expect = frame("task") + frame("parse") + frame("leaf");
        assert!(frame("parse") > frame("log"));

        let ok = p.check_stack(&policy("task", u32::try_from(expect).unwrap()));
        assert!(ok.findings.is_empty(), "{arch:?} {:?}", ok.findings);
        let r = &ok.roots[0];
        assert_eq!(r.worst_case_bytes, Some(expect));
        assert_eq!(r.worst_path, ["task", "parse", "leaf"]);
        assert!(r.within_budget);

        let over = p.check_stack(&policy("task", u32::try_from(expect - 1).unwrap()));
        assert!(!over.roots[0].within_budget);
        assert_eq!(over.findings.len(), 1);
        assert_eq!(over.findings[0].rule, RULE_BUDGET);
        assert!(over.findings[0].message.contains("task -> parse -> leaf"));
    }
}

#[test]
fn isas_split_frames_differently() {
    // x86_64 pays an 8-byte return address per activation; AArch64 instead
    // pushes the 16-byte frame record itself.
    let x = Program::compile(Arch::X86_64, tree()).unwrap();
    let a = Program::compile(Arch::Aarch64, tree()).unwrap();
    for (mx, ma) in x.measurements().iter().zip(a.measurements()) {
        assert_eq!(mx.prologue.call_push_bytes, 8);
        assert_eq!(ma.prologue.call_push_bytes, 0);
    }
}

#[test]
fn large_frames_use_wide_immediates() {
    for arch in ISAS {
        let p = Program::compile(arch, vec![func("big", 70_000, &[], false)]).unwrap();
        let f = &p.graph().functions[0];
        assert!(f.frame_bytes >= 70_000, "{arch:?} {}", f.frame_bytes);
        assert!(f.frame_bytes < 70_100, "{arch:?} {}", f.frame_bytes);
    }
}

#[test]
fn recursion_is_rejected() {
    for arch in ISAS {
        let fs = vec![
            func("root", 0, &["a"], false),
            func("a", 16, &["b"], false),
            func("b", 16, &["a"], false),
            func("self_rec", 16, &["self_rec"], false),
        ];
        let p = Program::compile(arch, fs).unwrap();
        let r = p.check_stack(&policy("root", 1 << 20));
        let rec: Vec<_> = r
            .findings
            .iter()
            .filter(|f| f.rule == RULE_RECURSION)
            .collect();
        assert_eq!(rec.len(), 2, "{:?}", r.findings);
        assert!(rec.iter().any(|f| f.message.contains("a, b")));
        assert!(rec.iter().any(|f| f.message.contains("self_rec")));
        assert_eq!(r.roots[0].worst_case_bytes, None);
        assert!(!r.roots[0].within_budget);
        assert!(
            r.findings
                .iter()
                .any(|f| f.rule == RULE_BUDGET && f.function == "root")
        );
    }
}

#[test]
fn indirect_calls_need_policy_targets() {
    let fs = vec![
        func("dispatch", 0, &[], true),
        func("h1", 512, &[], false),
        func("h2", 16, &[], false),
    ];
    let p = Program::compile(Arch::Aarch64, fs).unwrap();
    let g = p.graph();
    assert_eq!(g.indirect_calls.len(), 1);

    let r = p.check_stack(&policy("dispatch", 1 << 20));
    assert_eq!(r.findings[0].rule, RULE_INDIRECT);
    assert_eq!(r.findings[0].inst, Some(g.indirect_calls[0].inst));
    assert_eq!(r.roots[0].worst_case_bytes, None);

    let mut pol = policy("dispatch", 1 << 20);
    pol.indirect_targets
        .insert("dispatch".into(), vec!["h1".into(), "h2".into()]);
    let r = p.check_stack(&pol);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
    assert_eq!(r.roots[0].worst_path, ["dispatch", "h1"]);
}

#[test]
fn external_calls_and_lowering_libcalls_need_bounds() {
    let fs = vec![
        func("caller", 0, &["memcpy_ext"], false),
        ceil_user("rounds"),
    ];
    let p = Program::compile(Arch::X86_64, fs).unwrap();
    let ext: Vec<_> = p
        .graph()
        .external_calls
        .iter()
        .map(|e| (e.symbol.as_str(), e.inst.is_some()))
        .collect();
    assert!(ext.contains(&("memcpy_ext", true)), "{ext:?}");
    // Baseline x86_64 has no ROUNDSD; Cranelift emits a call to CeilF64 that
    // only shows up as a relocation in the machine code.
    assert!(
        ext.iter().any(|(s, ir)| s.starts_with("libcall:") && !ir),
        "{ext:?}"
    );

    let mut pol = StackPolicy {
        roots: vec![
            StackRoot {
                function: "caller".into(),
                budget_bytes: 4096,
            },
            StackRoot {
                function: "rounds".into(),
                budget_bytes: 4096,
            },
        ],
        ..StackPolicy::default()
    };
    let r = p.check_stack(&pol);
    assert_eq!(
        r.findings
            .iter()
            .filter(|f| f.rule == RULE_EXTERNAL)
            .count(),
        2
    );
    assert!(r.roots.iter().all(|x| x.worst_case_bytes.is_none()));

    pol.external_bounds.insert("memcpy_ext".into(), 128);
    for (s, _) in &ext {
        pol.external_bounds.entry((*s).to_string()).or_insert(256);
    }
    let r = p.check_stack(&pol);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
    let caller = &r.roots[0];
    assert_eq!(caller.worst_path, ["caller", "memcpy_ext"]);
    let own = u64::from(p.graph().functions[0].frame_bytes);
    assert_eq!(caller.worst_case_bytes, Some(own + 128));
}

#[test]
fn unknown_root_and_unsupported_isa_are_typed_errors() {
    let p = Program::compile(Arch::X86_64, tree()).unwrap();
    let r = p.check_stack(&policy("nope", 1));
    assert_eq!(r.findings[0].rule, vesper_silicon::stack::RULE_UNKNOWN_ROOT);
    assert!(matches!(
        Program::compile(Arch::Riscv64, tree()),
        Err(vesper_silicon::stack::StackError::UnsupportedIsa(
            Arch::Riscv64
        ))
    ));
    let dup = vec![func("x", 0, &[], false), func("x", 0, &[], false)];
    assert!(matches!(
        Program::compile(Arch::X86_64, dup),
        Err(vesper_silicon::stack::StackError::DuplicateFunction(_))
    ));
}

#[test]
fn callee_saved_clobbers_are_counted() {
    for arch in ISAS {
        let fs = vec![
            common::clobbering("hot", "leaf", 12),
            func("leaf", 0, &[], false),
        ];
        let p = Program::compile(arch, fs).unwrap();
        let m = &p.measurements()[0];
        // Twelve values live across the call exceed the callee-saved
        // registers on both ISAs, so the frame holds clobbers and spills.
        assert!(m.cranelift_frame_size > 0, "{arch:?} {m:?}");
        assert!(
            m.prologue.total().unwrap() >= m.cranelift_frame_size + 16,
            "{arch:?} {m:?}"
        );
        if arch == Arch::Aarch64 {
            assert!(m.prologue.clobber_push_bytes > 0, "{m:?}");
        }
    }
}
