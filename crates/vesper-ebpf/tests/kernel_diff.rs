//! Differential evidence against the Linux kernel, when the host permits
//! `bpf()` (Linux with CAP_BPF); otherwise each test reports that the
//! evidence is unavailable and passes.
//!
//! * Every random program this crate's verifier accepts must also be
//!   accepted by the kernel verifier (the soundness direction).
//! * For those programs, `BPF_PROG_TEST_RUN` and this crate's interpreter
//!   must agree on the return value and the output packet over the same
//!   packet sequence (map state evolves in both).
//! * Programs this crate rejects are loaded too, and the agreement rate is
//!   printed; the kernel accepting more is expected (see the lane report).

#![cfg(target_os = "linux")]

mod common;

use common::{GenOptions, Rng, random_packet, random_program, retarget};
use vesper_ebpf::interp::{Env, SkbMeta, XdpEnv, run_skb, run_xdp};
use vesper_ebpf::kernel::{self, LoadError};
use vesper_ebpf::{VerifierPolicy, verify};

fn available() -> bool {
    match kernel::probe() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("kernel BPF evidence unavailable on this host: {e}");
            false
        }
    }
}

#[test]
fn kernel_accepts_what_we_accept_and_executes_identically() {
    if !available() {
        return;
    }
    kernel::pin_to_cpu(0).expect("pin to CPU 0");
    let n: u64 = std::env::var("VESPER_EBPF_DIFF_PROGRAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let seed: u64 = std::env::var("VESPER_EBPF_DIFF_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x1234_5678_9abc_def1);
    let mut rng = Rng(seed);
    let policy = VerifierPolicy::default();
    let (mut ours, mut both, mut kernel_only, mut neither) = (0, 0, 0, 0);
    let mut why: std::collections::BTreeMap<&str, u32> = Default::default();
    let mut runs = 0;
    for i in 0..n {
        let prog = random_program(&mut rng, GenOptions::default());
        let mine = verify(&prog, &policy);
        let theirs = kernel::load(&prog);
        match (&mine, &theirs) {
            (Ok(_), Ok(_)) => both += 1,
            (Ok(_), Err(LoadError::Rejected { log, .. })) => {
                panic!(
                    "program {i}: accepted here, rejected by the kernel:\n{log}\n{}",
                    vesper_ebpf::insn::disassemble(&prog.insns)
                );
            }
            (Ok(_), Err(e)) => panic!("program {i}: kernel load failed: {e}"),
            (Err(e), Ok(_)) => {
                kernel_only += 1;
                *why.entry(e.rule).or_insert(0u32) += 1;
            }
            (Err(_), Err(_)) => neither += 1,
        }
        if mine.is_ok() {
            ours += 1;
            let loaded = theirs.expect("checked above");
            let mut env = XdpEnv::new(&prog);
            for _ in 0..8 {
                let mut pkt = random_packet(&mut rng);
                // BPF_PROG_TEST_RUN needs at least an Ethernet header
                pkt.resize(pkt.len().max(14), 0);
                let k = kernel::test_run(&loaded.prog, &pkt).expect("test run");
                let u = run_xdp(&prog, &mut env, &pkt).expect("interpreter on an accepted program");
                assert_eq!(
                    (k.retval as u64, &k.packet),
                    (u.ret, &u.packet),
                    "program {i}: kernel and interpreter disagree on packet {pkt:02x?}\n{}",
                    vesper_ebpf::insn::disassemble(&prog.insns)
                );
                runs += 1;
            }
        }
    }
    eprintln!(
        "accepted by both {both}, kernel only {kernel_only}, neither {neither} (ours {ours}); {runs} identical test runs"
    );
    eprintln!("our rules on kernel-only programs: {why:?}");
    assert!(both >= 100);
}

#[test]
fn kernel_agrees_on_the_rule_corpus() {
    use common::corpus::{Kernel, corpus};
    if !available() {
        return;
    }
    let mut wrong = Vec::new();
    for case in corpus() {
        if case.kernel == Kernel::NotLoaded {
            continue;
        }
        let got = kernel::load(&case.prog);
        let ok = matches!(
            (case.kernel, &got),
            (Kernel::Accepts | Kernel::AcceptsButStricterHere, Ok(_))
                | (Kernel::Rejects, Err(LoadError::Rejected { .. }))
        );
        if !ok {
            let detail = match &got {
                Ok(_) => "kernel accepted".to_owned(),
                Err(LoadError::Rejected { log, .. }) => {
                    log.lines().rev().take(4).collect::<Vec<_>>().join(" | ")
                }
                Err(e) => e.to_string(),
            };
            wrong.push(format!(
                "{} (expected kernel {:?}): {detail}",
                case.name, case.kernel
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The soundness direction over tc, tracepoint and kprobe programs: the
/// random XDP programs moved to each type's context layout.
#[test]
fn kernel_accepts_what_we_accept_for_every_program_type() {
    use vesper_ebpf::ProgramType;
    if !available() {
        return;
    }
    let n: u64 = std::env::var("VESPER_EBPF_DIFF_PROGRAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600);
    let policy = VerifierPolicy::default();
    kernel::pin_to_cpu(0).expect("pin to CPU 0");
    for t in [
        ProgramType::SchedCls,
        ProgramType::Tracepoint,
        ProgramType::Kprobe,
    ] {
        let mut rng = Rng(0x7e57_0000 + t.kernel_type() as u64);
        let (mut both, mut kernel_only, mut neither, mut runs) = (0, 0, 0, 0);
        for i in 0..n {
            let prog = retarget(&random_program(&mut rng, GenOptions::default()), t);
            let mine = verify(&prog, &policy);
            let theirs = kernel::load(&prog);
            match (&mine, &theirs) {
                (Ok(_), Ok(loaded)) => {
                    both += 1;
                    // tc programs also run: BPF_PROG_TEST_RUN against the
                    // interpreter, map state evolving in both
                    if t == ProgramType::SchedCls {
                        let mut env = Env::new(&prog);
                        for _ in 0..6 {
                            let mut pkt = random_packet(&mut rng);
                            pkt.resize(pkt.len().max(14), 0);
                            let meta = SkbMeta {
                                mark: rng.next() as u32,
                                priority: rng.next() as u32,
                                ..SkbMeta::test_run(&pkt)
                            };
                            let ctx_in = SkbMeta {
                                mark: meta.mark,
                                priority: meta.priority,
                                ..SkbMeta::default()
                            }
                            .to_ctx(0);
                            let k = match kernel::test_run_ctx(&loaded.prog, &pkt, Some(&ctx_in)) {
                                Ok(k) => k,
                                // the kernel refuses some short IP packets
                                Err(LoadError::Invalid(_)) => continue,
                                Err(e) => panic!("{e}"),
                            };
                            let u = run_skb(&prog, &mut env, &pkt, meta)
                                .expect("interpreter on an accepted program");
                            assert_eq!(
                                (k.retval as u64, &k.packet),
                                (u.ret, &u.packet),
                                "tc program {i}: kernel and interpreter disagree on {pkt:02x?}\n{}",
                                vesper_ebpf::insn::disassemble(&prog.insns)
                            );
                            runs += 1;
                        }
                    }
                }
                (Ok(_), Err(LoadError::Rejected { log, .. })) => panic!(
                    "{t:?} program {i}: accepted here, rejected by the kernel:\n{log}\n{}",
                    vesper_ebpf::insn::disassemble(&prog.insns)
                ),
                (Ok(_), Err(e)) => panic!("{t:?} program {i}: kernel load failed: {e}"),
                (Err(_), Ok(_)) => kernel_only += 1,
                (Err(_), Err(_)) => neither += 1,
            }
        }
        eprintln!(
            "{t:?}: accepted by both {both}, kernel only {kernel_only}, neither {neither}; {runs} identical test runs"
        );
        if t == ProgramType::SchedCls {
            assert!(both >= 30, "{t:?}: too few accepted programs ({both})");
        }
    }
}

/// Accepted XDP corpus programs (tail calls, `bpf_loop`, kfuncs included)
/// return the same value and packet under `BPF_PROG_TEST_RUN` and the
/// interpreter.
#[test]
fn kernel_and_interpreter_agree_on_the_accepted_corpus() {
    use common::corpus::{Kernel, corpus};
    use vesper_ebpf::ProgramType;
    if !available() {
        return;
    }
    kernel::pin_to_cpu(0).expect("pin to CPU 0");
    let mut ran = 0;
    for case in corpus() {
        if case.rule.is_some()
            || case.kernel != Kernel::Accepts
            || case.prog.prog_type != ProgramType::Xdp
        {
            continue;
        }
        let loaded = kernel::load(&case.prog).expect("loads");
        let mut env = XdpEnv::new(&case.prog);
        let pkt: Vec<u8> = (0..64u8).collect();
        let k = kernel::test_run(&loaded.prog, &pkt).expect("test run");
        let u = run_xdp(&case.prog, &mut env, &pkt)
            .unwrap_or_else(|e| panic!("{}: interpreter: {e}", case.name));
        assert_eq!(
            (k.retval as u64, &k.packet),
            (u.ret, &u.packet),
            "{}",
            case.name
        );
        if case.name == "accept/bpf_loop_count" {
            assert_eq!(u.ret, 3);
        }
        ran += 1;
    }
    assert!(ran >= 10, "{ran}");
}
