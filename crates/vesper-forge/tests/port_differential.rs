//! The Vesper build of the hygiene core against the frozen Rust seed copy
//! (self-hosting SH1 lane 14, #1157): random fragments, expansions with
//! splices and claims, and tight identity and nesting limits, resolved by
//! both builds. A case passes only when the results (fragment or error)
//! are equal, and so is the host state after them (the next scope and the
//! next symbol). Under the default (Rust) build both sides are Rust, so the
//! test checks the generator; under `vesper-port` it checks the port.

#[macro_use]
#[path = "corpus/mod.rs"]
mod corpus;

use corpus::{Case, case};

world!(port, vesper_forge, crate::Case);
world!(seed, forge_seed, crate::Case);

#[test]
fn builds_are_the_expected_ones() {
    assert_eq!(seed::implementation(), "rust");
    let expected = if cfg!(feature = "vesper-port") {
        "vesper"
    } else {
        "rust"
    };
    assert_eq!(port::implementation(), expected);
}

#[test]
fn random_fragments_resolve_identically() {
    let n: u64 = std::env::var("VESPER_FORGE_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let mut outcomes = std::collections::BTreeMap::<String, u32>::new();
    for s in 0..n {
        let c = case(s);
        let a = port::run(&c);
        let b = seed::run(&c);
        assert_eq!(a, b, "case {s} differs: {c:#?}");
        let kind = b.split(['(', ' ', '{']).next().unwrap_or("").to_owned();
        let variant = b
            .strip_prefix("Err(\n    ")
            .map(|e| e.split([' ', '(', '\n']).next().unwrap_or("").to_owned())
            .unwrap_or(kind);
        *outcomes.entry(variant).or_default() += 1;
    }
    // The corpus reaches success and the main error classes.
    eprintln!("outcomes: {outcomes:?}");
    for needed in [
        "Ok",
        "Parse",
        "Unbound",
        "UnbalancedDelimiters",
        "IdentityExhausted",
        "PrintedNameMismatch",
        "WrongDefinitionKind",
        "Ambiguous",
        "MixedPath",
        "PreResolvedBinder",
        "NotADependency",
        "QuoteLimit",
    ] {
        assert!(
            outcomes.contains_key(needed),
            "no case ended in {needed}: {outcomes:?}"
        );
    }
}
