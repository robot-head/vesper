//! §11 extraction breadth: structs, enums and `match`, recursion and bounded
//! loops (total fuel companions), `Fail` and state effects, and user contracts.
//! Each fixture is extracted, then compiled together with its proof script by
//! Lean 4.24.0 when `VESPER_LEAN` (or `lean` on PATH) is available. Every
//! theorem's axiom closure is printed and must not contain `sorryAx` or
//! `Lean.ofReduceBool`.
use std::{
    collections::BTreeMap,
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use vesper::{
    check, lean,
    lean::{ContractError, ExtractError, Totality},
    parser,
};
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn lean() -> Option<String> {
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    Command::new(&compiler)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| compiler)
}

/// Extract `source`, check the reported totality and the evaluator's `main`,
/// then compile the extraction with `proofs` and audit every theorem.
fn proves(source: &str, proofs: &str, totality: &[(&str, Totality)], main: &str) {
    let world = world(source);
    let report = lean::extract_report(&world).unwrap();
    for (function, expected) in totality {
        assert_eq!(report.totality.get(*function), Some(expected), "{function}");
    }
    let value = vesper::runtime::Runtime::new(&world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(value, main, "reference evaluator");
    let text = report.module.render().unwrap();
    for (function, expected) in totality {
        // Only members of recursive groups stay `partial`; their callers are
        // ordinary definitions that the kernel reasons about via `f$fuel`.
        if *expected == Totality::Total {
            assert!(
                !text.contains(&format!("partial def «{function}» ")),
                "{text}"
            );
        }
        let companion = text.contains(&format!("def «{function}$fuel» "));
        assert_eq!(
            companion,
            *expected == Totality::Fuel,
            "{function} in\n{text}"
        );
    }
    for forbidden in ["sorry", "admit", "native_decide", "bv_decide", "axiom "] {
        assert!(!proofs.contains(forbidden), "proof script uses {forbidden}");
    }
    let Some(compiler) = lean() else {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    };
    let mut namespace = String::new();
    let mut theorems = vec![];
    for line in proofs.lines() {
        if let Some(n) = line.strip_prefix("namespace ") {
            namespace = n.trim().to_owned();
        }
        if let Some(rest) = line.strip_prefix("theorem ") {
            let name = rest.split_whitespace().next().unwrap();
            theorems.push(format!("{namespace}.{name}"));
        }
    }
    assert!(!theorems.is_empty(), "fixture proves nothing");
    let mut file = text.clone();
    file.push('\n');
    file.push_str(proofs);
    for theorem in &theorems {
        file.push_str(&format!("\n#print axioms {theorem}"));
    }
    file.push('\n');
    let path = std::env::temp_dir().join(format!(
        "vesper-lean-extract-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, &file).unwrap();
    let result = Command::new(compiler).arg(&path).output().unwrap();
    let _ = fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{stdout}\n{}\n{file}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!stdout.contains("sorry"), "{stdout}");
    assert!(!stdout.contains("ofReduceBool"), "{stdout}");
    for theorem in &theorems {
        assert!(
            stdout.contains(&format!("'{theorem}' depends on axioms"))
                || stdout.contains(&format!("'{theorem}' does not depend on any axioms")),
            "missing axiom report for {theorem}:\n{stdout}"
        );
    }
}

macro_rules! fixture {
    ($name:literal) => {
        (
            include_str!(concat!("fixtures/lean_extract/", $name, ".ves")),
            include_str!(concat!("fixtures/lean_extract/", $name, ".proofs.lean")),
        )
    };
}

#[test]
fn structs_are_structures_with_lawful_frames() {
    let (source, proofs) = fixture!("structs");
    proves(
        source,
        proofs,
        &[
            ("swap", Totality::Total),
            ("move_start", Totality::Total),
            ("main", Totality::Total),
        ],
        "2",
    );
}

#[test]
fn enums_and_match_with_checked_arithmetic() {
    let (source, proofs) = fixture!("enums");
    proves(
        source,
        proofs,
        &[
            ("area", Totality::Total),
            ("is_round", Totality::Total),
            ("main", Totality::Total),
        ],
        "42",
    );
}

#[test]
fn recursion_gets_total_fuel_companions() {
    let (source, proofs) = fixture!("recursion");
    proves(
        source,
        proofs,
        &[
            ("length", Totality::Fuel),
            ("drain", Totality::Fuel),
            ("is_even", Totality::Fuel),
            ("is_odd", Totality::Fuel),
            ("main", Totality::Fuel),
        ],
        "2",
    );
}

#[test]
fn bounded_loops_as_tail_recursion() {
    let (source, proofs) = fixture!("loops");
    proves(
        source,
        proofs,
        &[
            ("sum_to", Totality::Fuel),
            ("triangle", Totality::Fuel),
            ("main", Totality::Fuel),
        ],
        "55",
    );
}

#[test]
fn fail_and_state_effects() {
    let (source, proofs) = fixture!("effects");
    proves(
        source,
        proofs,
        &[
            ("twice", Totality::Total),
            ("checked_halve", Totality::Total),
            ("halve_or_zero", Totality::Total),
        ],
        "11",
    );
}

#[test]
fn contracts_are_lean_propositions() {
    let (source, proofs) = fixture!("contracts");
    let report = lean::extract_report(&world(source)).unwrap();
    assert_eq!(report.contracts, ["add_small", "clamp", "countdown"]);
    proves(
        source,
        proofs,
        &[
            ("clamp", Totality::Total),
            ("add_small", Totality::Total),
            ("countdown", Totality::Fuel),
        ],
        "4",
    );
}

fn contract_error(source: &str) -> ContractError {
    match lean::extract_report(&world(source)) {
        Err(ExtractError::Contract(e)) => e,
        Err(other) => panic!("expected a contract error, got {other}"),
        Ok(_) => panic!("expected a contract error"),
    }
}

#[test]
fn malformed_contracts_are_typed_errors() {
    assert!(matches!(
        contract_error(
            "fn f(x: u64) -> u64 { x } fn ensures_f(x: u64) -> bool { true } fn main() -> u64 { 0 }"
        ),
        ContractError::Signature { function, .. } if function == "f"
    ));
    assert!(matches!(
        contract_error(
            "fn f(x: u64) -> u64 { x } fn ensures_f(x: u64, r: u64) -> u64 { r } fn main() -> u64 { 0 }"
        ),
        ContractError::Predicate { function, .. } if function == "ensures_f"
    ));
    assert!(matches!(
        contract_error(
            "fn f(x: u64) -> u64 { x } fn ensures_f(x: u64, r: u64) -> bool eff [Fail[ArithOverflow]] { r + 1 > x } fn main() -> u64 { 0 }"
        ),
        ContractError::Predicate { .. }
    ));
    assert!(matches!(
        contract_error(
            "fn f(inout x: u64) -> u64 { x = 1; x } fn ensures_f(x: u64, r: u64) -> bool { true } fn main() -> u64 { 0 }"
        ),
        ContractError::Unsupported { function, .. } if function == "f"
    ));
    assert!(matches!(
        contract_error(
            "linear effect Ask { fn ask() -> u64; } fn f(x: u64) -> u64 eff [Ask] { Ask::ask() } fn ensures_f(x: u64, r: u64) -> bool { true } fn main() -> u64 { 0 }"
        ),
        ContractError::Unsupported { .. }
    ));
}

#[test]
fn recursion_through_function_values_stays_partial_and_says_why() {
    let source = "fn apply(sink f: fn(u64) -> u64, x: u64) -> u64 { f(x) } fn spin(n: u64) -> u64 { if n == 0 { 0 } else { apply(spin, wrapping_sub(n, 1)) } } fn ensures_spin(n: u64, r: u64) -> bool { true } fn main() -> u64 { 0 }";
    let world = world(source);
    let without_contract = {
        let source = source.replace("fn ensures_spin(n: u64, r: u64) -> bool { true } ", "");
        lean::extract_report(&self::world(&source)).unwrap()
    };
    let totality: BTreeMap<_, _> = without_contract.totality;
    assert!(
        matches!(&totality["spin"], Totality::Partial(reason) if reason.contains("as a value")),
        "{:?}",
        totality["spin"]
    );
    assert_eq!(totality["apply"], Totality::Total);
    assert!(matches!(
        lean::extract_report(&world),
        Err(ExtractError::Contract(ContractError::Unsupported { function, .. })) if function == "spin"
    ));
}

#[test]
fn non_recursive_functions_are_ordinary_definitions_in_dependency_order() {
    let text = lean::extract(&world(
        "fn b(x: u64) -> u64 { wrapping_add(x, 1) } fn a(x: u64) -> u64 { b(b(x)) } fn main() -> u64 { a(1) }",
    ))
    .unwrap();
    assert!(!text.contains("partial def"), "{text}");
    let position = |n: &str| text.find(&format!("def «{n}» ")).unwrap();
    assert!(position("b") < position("a") && position("a") < position("main"));
}

#[test]
fn a_parameter_shadowing_a_function_is_not_recursion() {
    let source = "fn f(f: u64) -> u64 eff [Fail[ArithOverflow]] { f + 1 } fn main() -> u64 { 0 }";
    let report = lean::extract_report(&world(source)).unwrap();
    assert_eq!(report.totality["f"], Totality::Total);
    let text = report.module.render().unwrap();
    assert!(!text.contains("partial def"), "{text}");
    compiles(&text);
}

#[test]
fn recursion_through_a_pure_closure_stays_partial_and_compiles() {
    let source = "fn down(n: u64) -> u64 { if n == 0 { 0 } else { let step = |x: u64| down(x); step(wrapping_sub(n, 1)) } } fn main() -> u64 { down(3) }";
    let report = lean::extract_report(&world(source)).unwrap();
    assert!(
        matches!(&report.totality["down"], Totality::Partial(reason) if reason.contains("pure closure")),
        "{:?}",
        report.totality["down"]
    );
    assert!(matches!(&report.totality["main"], Totality::Partial(_)));
    compiles(&report.module.render().unwrap());
}

fn compiles(text: &str) {
    let Some(compiler) = lean() else {
        return;
    };
    let path = std::env::temp_dir().join(format!(
        "vesper-lean-extract-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, text).unwrap();
    let result = Command::new(compiler).arg(&path).output().unwrap();
    let _ = fs::remove_file(&path);
    assert!(
        result.status.success(),
        "{}\n{text}",
        String::from_utf8_lossy(&result.stdout)
    );
}
