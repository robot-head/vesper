//! The verifier corpus: accepted programs are accepted, and every other
//! program is rejected with the expected rule. Every rule the verifier
//! can report on real input is covered at least once.

mod common;

use common::corpus::corpus;
use std::collections::BTreeSet;
use vesper_ebpf::{VerifierPolicy, findings, verify};

fn policy() -> VerifierPolicy {
    // the kernel's limit is 1M; a lower one keeps the complexity case fast
    VerifierPolicy {
        max_insns_processed: 200_000,
        ..VerifierPolicy::default()
    }
}

#[test]
fn corpus_verdicts() {
    let mut failures = Vec::new();
    for case in corpus() {
        let got = verify(&case.prog, &policy());
        match (case.rule, &got) {
            (None, Ok(_)) => {}
            (Some(r), Err(e)) if e.rule == r => {}
            _ => failures.push(format!(
                "{}: expected {:?}, got {:?}",
                case.name,
                case.rule,
                got.map(|_| ())
            )),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_rule_is_covered() {
    let covered: BTreeSet<&str> = corpus().iter().filter_map(|c| c.rule).collect();
    let src = include_str!("../src/verifier.rs");
    let mut rules = BTreeSet::new();
    for (i, _) in src.match_indices('"') {
        let rest = &src[i + 1..];
        let Some(end) = rest.find('"') else { continue };
        let lit = &rest[..end];
        let shaped = lit.contains('.')
            && lit
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_' || c.is_ascii_digit())
            && !lit.starts_with('.')
            && !lit.ends_with('.');
        if shaped {
            rules.insert(lit);
        }
    }
    // Defensive rules no well-formed input reaches: the only program type
    // is XDP, the call graph is built from decoded calls, and the
    // branch outcomes of a comparison cannot both be infeasible.
    for internal in [
        "prog.unsupported_type",
        "call.graph",
        "internal.no_successor",
    ] {
        assert!(rules.remove(internal), "{internal} no longer exists");
    }
    let missing: Vec<_> = rules.difference(&covered).collect();
    assert!(
        missing.is_empty(),
        "rules without a corpus rejection: {missing:?}"
    );
}

#[test]
fn findings_use_the_contract_shape() {
    for case in corpus() {
        if case.rule.is_none() || case.name == "reject/too_large" {
            continue;
        }
        let f = findings(&case.prog, &policy());
        assert_eq!(f.len(), 1, "{}", case.name);
        assert_eq!(Some(f[0].rule), case.rule);
        assert_eq!(f[0].function, case.prog.name);
    }
}

#[test]
fn mitigator_stack_and_report() {
    let p = vesper_ebpf::mitigator::build(&Default::default()).unwrap();
    let r = verify(&p, &VerifierPolicy::default()).unwrap();
    assert!(r.worst_stack <= vesper_ebpf::BPF_MAX_STACK_BYTES);
    assert_eq!(r.call_graph.functions.len(), 1);
    assert_eq!(r.subprog_stack, vec![(0, 16)]);
}
