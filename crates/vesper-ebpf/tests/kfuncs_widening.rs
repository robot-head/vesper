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

fn fp8(a: &mut Asm) {
    a.mov64(Reg::R1, Reg::R10);
    a.add64(Reg::R1, -8);
}

fn iter_new(a: &mut Asm, start: i32, end: i32) {
    fp8(a);
    a.mov64(Reg::R2, start);
    a.mov64(Reg::R3, end);
    a.call_kfunc("bpf_iter_num_new");
}

fn iter_call(a: &mut Asm, kfunc: &str) {
    fp8(a);
    a.call_kfunc(kfunc);
}

/// A `next()` result kept past `destroy()`: in the same frame, or
/// returned from the subprogram whose stack held the iterator.
fn dangling(in_subprogram: bool) -> Program {
    let mut a = Asm::new();
    let f = a.label();
    let out = a.label();
    if in_subprogram {
        a.call_local(f);
        a.jmp(JmpOp::Eq, Reg::R0, 0, out);
        a.ldx(Size::W, Reg::R0, Reg::R0, 0);
        a.bind(out).unwrap();
        a.mov64(Reg::R0, 2);
        a.exit();
        a.bind(f).unwrap();
    }
    let null = a.label();
    iter_new(&mut a, 0, 1);
    iter_call(&mut a, "bpf_iter_num_next");
    a.mov64(Reg::R6, Reg::R0);
    iter_call(&mut a, "bpf_iter_num_destroy");
    if in_subprogram {
        a.mov64(Reg::R0, Reg::R6);
        a.exit();
    } else {
        a.jmp(JmpOp::Eq, Reg::R6, 0, null);
        a.ldx(Size::W, Reg::R0, Reg::R6, 0);
        a.bind(null).unwrap();
        a.mov64(Reg::R0, 2);
        a.exit();
    }
    let mut p = a.finish("dangling", ProgramType::Xdp).unwrap();
    p.license = "GPL".into();
    p
}

/// `new(0, 1); next(); destroy(); new(0, 1); goto next`: every `next()`
/// returns a value and the loop never ends; the revisit of `next()` is on
/// a new iterator, not progress on the old one.
fn recreated_iterator() -> Program {
    let mut a = Asm::new();
    let head = a.label();
    iter_new(&mut a, 0, 1);
    a.bind(head).unwrap();
    iter_call(&mut a, "bpf_iter_num_next");
    a.mov64(Reg::R6, Reg::R0);
    iter_call(&mut a, "bpf_iter_num_destroy");
    iter_new(&mut a, 0, 1);
    a.ja(head);
    let mut p = a.finish("recreated", ProgramType::Xdp).unwrap();
    p.license = "GPL".into();
    p
}

#[test]
fn iterator_pointers_die_with_their_iterator() {
    for sub in [false, true] {
        let p = dangling(sub);
        let e = verify(&p, &VerifierPolicy::default()).expect_err("dangling pointer");
        assert!(
            e.rule.starts_with("mem.") || e.rule.starts_with("reg."),
            "subprogram {sub}: {e}"
        );
    }
}

#[test]
fn a_recreated_iterator_does_not_bound_its_loop() {
    let p = recreated_iterator();
    let e = verify(&p, &VerifierPolicy::default()).expect_err("never terminates");
    assert!(
        matches!(e.rule, "loop.unbounded" | "complexity.limit"),
        "{e}"
    );
}
/// The kernel turns pointers into a destroyed iterator into scalars (so
/// both loads above are rejected there too), but accepts the recreated
/// iterator loop: a program that never terminates. This verifier is
/// stricter there, as it must be to keep "accepted programs terminate".
#[cfg(target_os = "linux")]
#[test]
fn kernel_rejects_dangling_iterator_pointers_and_accepts_the_recreated_loop() {
    use vesper_ebpf::kernel;
    if let Err(e) = kernel::probe() {
        eprintln!("kernel BPF evidence unavailable on this host: {e}");
        return;
    }
    for sub in [false, true] {
        match kernel::load(&dangling(sub)) {
            Err(kernel::LoadError::Rejected { log, .. }) => {
                assert!(log.contains("invalid mem access 'scalar'"), "{log}")
            }
            other => panic!("dangling({sub}): {:?}", other.map(|_| ())),
        }
    }
    kernel::load(&recreated_iterator()).expect("the kernel accepts the recreated loop");
}
