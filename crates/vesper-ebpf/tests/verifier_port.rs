//! The port verifier against the Rust verifier, result for result: every
//! corpus, depth and kfunc case, and random programs of every program type,
//! under the default limits (at a fifth of the budget), without widening and
//! with small budgets. The two builds must agree exactly: the verdict, the
//! rule, the instruction, the message, and every count of an accepted
//! program's report. Port wave P3a lane 6 (#1041).
#![cfg(feature = "vesper-port")]

mod common;

use common::{GenOptions, Rng, random_program, retarget};
use vesper_ebpf::verifier::{VerifierPolicy, verify};
use vesper_ebpf::{Program, ProgramType};

use vesper_ebpf_rust as rust;

/// The fields of `p` the verifier reads, as a program of the Rust build.
fn to_rust(p: &Program) -> rust::Program {
    let prog_type = rust::ProgramType::from_kernel_type(p.prog_type.kernel_type()).unwrap();
    rust::Program {
        name: p.name.clone(),
        prog_type,
        insns: p
            .insns
            .iter()
            .map(|i| rust::insn::RawInsn {
                code: i.code,
                dst: i.dst,
                src: i.src,
                off: i.off,
                imm: i.imm,
            })
            .collect(),
        maps: p
            .maps
            .iter()
            .map(|m| rust::MapDef {
                name: m.name.clone(),
                kind: rust::MapKind::from_kernel_type(m.kind.kernel_type()).unwrap(),
                key_size: m.key_size,
                value_size: m.value_size,
                max_entries: m.max_entries,
                flags: m.flags,
            })
            .collect(),
        map_relocs: p.map_relocs.clone(),
        functions: p.functions.clone(),
        license: p.license.clone(),
        attach: p.attach.clone(),
        kfuncs: p
            .kfuncs
            .iter()
            .map(|k| rust::program::KfuncRef {
                slot: k.slot,
                name: k.name.clone(),
            })
            .collect(),
        core_relos: Vec::new(),
        lines: Vec::new(),
    }
}

fn policies() -> Vec<(VerifierPolicy, rust::verifier::VerifierPolicy)> {
    let p = |max_insns_processed, max_states_per_insn, widen_loops| {
        (
            VerifierPolicy {
                max_insns_processed,
                max_states_per_insn,
                widen_loops,
            },
            rust::verifier::VerifierPolicy {
                max_insns_processed,
                max_states_per_insn,
                widen_loops,
            },
        )
    };
    // the default policy but a fifth of its budget: the cases that run out
    // run out the same way, in a fifth of the time
    vec![
        p(200_000, 64, true),
        p(200_000, 64, false),
        p(2_000, 2, true),
        p(300, 64, true),
    ]
}

/// The budgets that stop well short of a million instructions.
const SMALL: u64 = 10_000;

/// Both builds' results, formatted (the types are the same definitions),
/// under every policy, or only the small budgets when `full` is false.
/// Returns whether a full budget ran out (`complexity.limit`).
fn agree(name: &str, prog: &Program, checked: &mut u64, full: bool) -> bool {
    let rp = to_rust(prog);
    let mut exhausted = false;
    for (pp, rpol) in policies() {
        let big = pp.max_insns_processed > SMALL;
        if big && !full {
            continue;
        }
        let port = format!("{:?}", verify(prog, &pp));
        let rust = format!("{:?}", rust::verifier::verify(&rp, &rpol));
        assert_eq!(port, rust, "{name} under {pp:?}");
        exhausted |= big && rust.contains("complexity.limit");
        *checked += 1;
    }
    exhausted
}

#[test]
fn port_matches_rust_on_every_corpus_case() {
    let mut n = 0;
    for c in common::corpus::corpus() {
        // a case that runs a whole large budget takes a second per policy
        // in either build (far longer in a debug Rust build), so its
        // retargeted copies run under the small budgets only
        let full = !agree(c.name, &c.prog, &mut n, true);
        for t in ProgramType::ALL {
            agree(c.name, &retarget(&c.prog, t), &mut n, full);
        }
    }
    for c in common::depth::cases() {
        agree(c.name, &c.prog, &mut n, true);
    }
    for c in common::kfuncs::cases() {
        agree(c.name, &c.prog, &mut n, true);
    }
    assert!(n > 500, "{n} comparisons");
}

#[test]
fn port_matches_rust_on_random_programs() {
    let mut rng = Rng(0x005e_ed0f_9041);
    let mut n = 0;
    let mut accepted = 0;
    for i in 0..400 {
        let prog = random_program(
            &mut rng,
            GenOptions {
                items: 4 + i % 24,
                nondeterministic_helpers: i % 2 == 0,
            },
        );
        if verify(&prog, &VerifierPolicy::default()).is_ok() {
            accepted += 1;
        }
        agree("random", &prog, &mut n, true);
        let t = ProgramType::ALL[(i as usize) % ProgramType::ALL.len()];
        agree("random retargeted", &retarget(&prog, t), &mut n, true);
    }
    assert!(accepted > 40, "{accepted} accepted");
}
