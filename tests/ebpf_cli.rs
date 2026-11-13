//! Integration B (#136): `vesper ebpf check` and the shared stack-bound
//! report that merges the eBPF and Silicon stack analyzers.
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::stack_bound::{
    GraphOrigin, RULE_EBPF_KERNEL, StackBoundPolicy, StackBudget, StackGraph, stack_bound_report,
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vesper-ebpf-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn vesper(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn ebpf_check_accepts_the_mitigator_object() {
    let temp = Temp::new();
    let prog = vesper_ebpf::mitigator::build(&Default::default()).unwrap();
    let object = temp.write(
        "mitigator.o",
        &vesper_ebpf::elf::write_object(&prog).unwrap(),
    );
    let o = vesper(&["ebpf", "check", &object]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("ebpf check: '"), "{t}");
    assert!(t.contains("accepted"), "{t}");
    assert!(t.contains("of 512 byte budget (ok)"), "{t}");
    // --load either loads it or says the kernel path is unavailable.
    let o = vesper(&["ebpf", "check", &object, "--load"]);
    let t = text(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("kernel: loaded") || t.contains("kernel: unavailable"),
        "{t}"
    );
    // A budget below its worst case is a stack-bound failure.
    let o = vesper(&["ebpf", "check", &object, "--stack-budget", "8"]);
    let t = text(&o);
    assert!(!o.status.success(), "{t}");
    assert!(t.contains("silicon.stack.budget_exceeded"), "{t}");
    assert!(t.contains("ebpf check: stack bound exceeded"), "{t}");
}

#[test]
fn ebpf_check_reads_raw_and_hex_slots_and_reports_typed_findings() {
    let temp = Temp::new();
    let ok = temp.write(
        "ok.hex",
        b"# r0 = 2; exit\nb700000002000000\n95 00 00 00 00 00 00 00\n",
    );
    let o = vesper(&["ebpf", "check", &ok, "--disassemble"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("worst case 32 bytes of 512 byte budget (ok)"),
        "{}",
        text(&o)
    );
    let raw = temp.write(
        "ok.bin",
        &[0xb7, 0, 0, 0, 2, 0, 0, 0, 0x95, 0, 0, 0, 0, 0, 0, 0],
    );
    assert!(vesper(&["ebpf", "check", &raw]).status.success());
    // `exit` with an uninitialised r0.
    let bad = temp.write("bad.hex", b"9500000000000000\n");
    let o = vesper(&["ebpf", "check", &bad]);
    let t = text(&o);
    assert!(!o.status.success());
    assert!(t.contains("error[reg.uninit_read]: bad at insn 0"), "{t}");
    assert!(
        t.contains("ebpf check: 'bad' rejected (reg.uninit_read)"),
        "{t}"
    );
    // A packet read without a bounds check.
    let unchecked = temp.write(
        "unchecked.hex",
        // r2 = *(u32 *)(r1 + 0) (ctx->data); r0 = *(u8 *)(r2 + 0); exit
        b"6112000000000000\n7120000000000000\n9500000000000000\n",
    );
    let o = vesper(&["ebpf", "check", &unchecked]);
    assert!(!o.status.success());
    assert!(text(&o).contains("rejected (mem."), "{}", text(&o));
    for (name, bytes, message) in [
        ("short.bin", &b"\x95\0\0"[..], "short.bin"),
        (
            "garbage.hex",
            &b"zz\n"[..],
            "line 1: expected one 8-byte slot",
        ),
        (
            "empty.hex",
            &b"# nothing\n"[..],
            "program has no instructions",
        ),
    ] {
        let path = temp.write(name, bytes);
        let o = vesper(&["ebpf", "check", &path]);
        assert!(!o.status.success(), "{name}");
        assert!(text(&o).contains(message), "{name}: {}", text(&o));
    }
    let o = vesper(&["ebpf", "verify"]);
    assert!(text(&o).contains("unknown ebpf subcommand 'verify'"));
}

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
