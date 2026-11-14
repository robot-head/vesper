//! The wave-seven kfuncs (RCU read-side regions, irq flags, open-coded
//! `bpf_iter_num` iterators) and loop widening: loops whose state changes
//! on every iteration converge only with widening, run to the expected
//! results in the interpreter, and (where the host loads programs) return
//! the same values under `BPF_PROG_TEST_RUN`.

mod common;

use common::kfuncs::{iter_counter, iter_sum, loop_counter};
use vesper_ebpf::insn::{JmpOp, Reg, Size};
use vesper_ebpf::interp::{Env, run_xdp};
use vesper_ebpf::{Asm, MapDef, MapKind, Program, ProgramType, VerifierPolicy, verify};

fn exact() -> VerifierPolicy {
    VerifierPolicy {
        max_insns_processed: 200_000,
        widen_loops: false,
        ..VerifierPolicy::default()
    }
}

fn run(p: &Program) -> (u64, Env) {
    let mut env = Env::new(p);
    let r = run_xdp(p, &mut env, &[0; 64]).expect("runs");
    (r.ret, env)
}

#[test]
fn loops_that_change_state_converge_only_with_widening() {
    // a running sum grows its range on every iteration too
    for (name, p) in [
        ("iter_counter", iter_counter(1000)),
        ("loop_counter", loop_counter(5)),
        ("iter_sum", iter_sum(10)),
    ] {
        let e = verify(&p, &exact()).expect_err(name);
        assert_eq!(e.rule, "complexity.limit", "{name}: {e}");
        let r = verify(&p, &VerifierPolicy::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(r.widenings > 0, "{name}");
        assert!(r.insns_processed < 1000, "{name}: {}", r.insns_processed);
    }
}

#[test]
fn iterators_and_loops_run_to_the_expected_values() {
    assert_eq!(run(&iter_sum(10)).0, 45);
    assert_eq!(run(&iter_sum(0)).0, 0);
    // start > end: bpf_iter_num_new fails and next() is NULL at once
    assert_eq!(run(&iter_sum(-3)).0, 0);
    let (ret, env) = run(&iter_counter(1000));
    assert_eq!(ret, 2);
    assert_eq!(
        env.map_get(0, &0u32.to_le_bytes()),
        Some(3000u64.to_le_bytes().to_vec())
    );
    // 5 iterations add 15; bpf_loop returns 5
    assert_eq!(run(&loop_counter(5)).0, 20);
}

/// An index counter used to address a map value. The kernel marks the
/// counter precise (it is a pointer offset), so it never widens it, the
/// loop does not converge and the kernel runs out of budget (checked
/// below against the host kernel). This verifier widens it at first, sees
/// the widened value reach pointer arithmetic, and explores again without
/// widening: the same verdict.
fn precise_counter() -> Program {
    let mut a = Asm::new();
    let am = a
        .map(MapDef::new("am", MapKind::Array, 4, 64, 1))
        .expect("map");
    let head = a.label();
    let done = a.label();
    let skip = a.label();
    let out = a.label();
    a.st(Size::W, Reg::R10, -12, 0);
    a.ld_map(Reg::R1, am);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -12);
    a.call(vesper_ebpf::helpers::MAP_LOOKUP_ELEM);
    a.jmp(JmpOp::Eq, Reg::R0, 0, out);
    a.mov64(Reg::R7, Reg::R0);
    a.mov64(Reg::R6, 0);
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, -8);
    a.mov64(Reg::R2, 0);
    a.mov64(Reg::R3, 100);
    a.call_kfunc("bpf_iter_num_new");
    a.bind(head).unwrap();
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, -8);
    a.call_kfunc("bpf_iter_num_next");
    a.jmp(JmpOp::Eq, Reg::R0, 0, done);
    a.jmp(JmpOp::Ge, Reg::R6, 64, skip);
    a.mov64(Reg::R1, Reg::R7);
    a.add64(Reg::R1, Reg::R6);
    a.st(Size::B, Reg::R1, 0, 1);
    a.bind(skip).unwrap();
    a.add64(Reg::R6, 1);
    a.ja(head);
    a.bind(done).unwrap();
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, -8);
    a.call_kfunc("bpf_iter_num_destroy");
    a.bind(out).unwrap();
    a.mov64(Reg::R0, 2);
    a.exit();
    a.finish("precise", ProgramType::Xdp).unwrap()
}

#[test]
fn widened_precise_scalars_are_not_widened() {
    let p = precise_counter();
    let small = VerifierPolicy {
        max_insns_processed: 200_000,
        ..VerifierPolicy::default()
    };
    let e = verify(&p, &small).expect_err("rejected like the kernel");
    assert_eq!(e.rule, "complexity.limit", "{e}");
    // the program is safe: it runs, and writes every byte once
    let (ret, env) = run(&p);
    assert_eq!(ret, 2);
    assert_eq!(env.map_get(0, &0u32.to_le_bytes()), Some(vec![1; 64]));
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_runs_iterators_and_widened_loops_identically() {
    use vesper_ebpf::kernel;
    if let Err(e) = kernel::probe() {
        eprintln!("kernel BPF evidence unavailable on this host: {e}");
        return;
    }
    for (name, p, want) in [
        ("iter_sum(10)", iter_sum(10), 45),
        ("iter_sum(-3)", iter_sum(-3), 0),
        ("iter_sum(1000)", iter_sum(1000), 499_500),
        ("iter_counter(1000)", iter_counter(1000), 2),
        ("loop_counter(5)", loop_counter(5), 20),
        ("loop_counter(100)", loop_counter(100), 300 % 256 + 100),
    ] {
        let mut p = p;
        p.license = "GPL".into();
        assert!(verify(&p, &VerifierPolicy::default()).is_ok(), "{name}");
        let loaded = kernel::load(&p).unwrap_or_else(|e| panic!("{name}: {e}"));
        let k = kernel::test_run(&loaded.prog, &[0; 64]).expect("test run");
        let (u, env) = run(&p);
        assert_eq!((k.retval as u64, u), (want, want), "{name}");
        if name.starts_with("iter_counter") {
            let kv = kernel::lookup(&loaded.maps[0], &0u32.to_le_bytes())
                .unwrap()
                .unwrap();
            assert_eq!(Some(kv), env.map_get(0, &0u32.to_le_bytes()), "{name}");
        }
    }
    let mut p = precise_counter();
    p.license = "GPL".into();
    match kernel::load(&p) {
        Err(kernel::LoadError::Rejected { log, .. }) => {
            assert!(log.contains("BPF program is too large"), "{log}")
        }
        other => panic!("precise index counter: {other:?}"),
    }
}
