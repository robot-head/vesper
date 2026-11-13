//! Soundness of the pre-flight verifier against the concrete interpreter:
//! every random program the verifier accepts must run over random packets
//! and map states without any fault the interpreter detects (out-of-region
//! access, uninitialised read, use after release, leaked reference).
//! Rejected programs are counted per rule so the generator demonstrably
//! reaches many rules.

mod common;

use common::{GenOptions, Rng, random_packet, random_program};
use std::collections::BTreeMap;
use vesper_ebpf::interp::{InterpError, XdpEnv, run_xdp};
use vesper_ebpf::{VerifierPolicy, verify};

#[test]
fn accepted_random_programs_never_fault() {
    let n: u64 = std::env::var("VESPER_EBPF_FUZZ_PROGRAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let seed: u64 = std::env::var("VESPER_EBPF_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut rng = Rng(seed);
    let policy = VerifierPolicy::default();
    let mut accepted = 0;
    let mut rules: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut faults_in_rejected = 0;
    for i in 0..n {
        let prog = random_program(
            &mut rng,
            GenOptions {
                nondeterministic_helpers: true,
                ..GenOptions::default()
            },
        );
        let verdict = verify(&prog, &policy);
        let mut env = XdpEnv::new(&prog);
        env.now_ns = i;
        let mut faulted = None;
        for _ in 0..12 {
            let pkt = random_packet(&mut rng);
            match run_xdp(&prog, &mut env, &pkt) {
                Ok(_) => {}
                Err(InterpError::InsnBudget) => {}
                Err(e) => {
                    faulted = Some(e);
                    break;
                }
            }
        }
        match verdict {
            Ok(_) => {
                accepted += 1;
                if let Some(e) = faulted {
                    panic!(
                        "verifier accepted program {i} but it faulted: {e}\n{}",
                        vesper_ebpf::insn::disassemble(&prog.insns)
                    );
                }
            }
            Err(e) => {
                if std::env::var("SHOW").as_deref() == Ok(e.rule) {
                    eprintln!("{e}\n{}", vesper_ebpf::insn::disassemble(&prog.insns));
                }
                *rules.entry(e.rule).or_default() += 1;
                if faulted.is_some() {
                    faults_in_rejected += 1;
                }
            }
        }
    }
    eprintln!(
        "accepted {accepted}/{n}; rejected by rule: {rules:?}; rejected programs that faulted: {faults_in_rejected}"
    );
    assert!(
        accepted >= n / 10,
        "generator too weak: {accepted} accepted"
    );
    assert!(
        rules.len() >= 8,
        "generator reaches too few rules: {rules:?}"
    );
}
