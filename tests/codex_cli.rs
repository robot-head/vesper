//! Wave-six lane 14: path-less `vesper lint` / `vesper test --doc`, hermetic
//! SimPulse doctests, §40.3 diagrams and proof badges through the real CLI.
use vesper::doctest::{self, DocTestLimits};

const DEVICES: &str = "linear effect Clock {\n    fn now() -> u64;\n    fn sleep(ns: u64) -> u64;\n}\nlinear effect Net {\n    fn send(port: u64, value: u64) -> u64;\n    fn recv(port: u64, timeout_ns: u64) -> u64;\n}\n";

fn run_doc(source: &str) -> doctest::DocTestReport {
    doctest::run(source, DocTestLimits::default()).unwrap()
}
fn sim_source(seed: &str) -> String {
    format!(
        "{DEVICES}/// ```vesper,sim(seed={seed},clock,net),entry=ping\n/// fn ping() -> u64 eff [Clock, Net] {{\n///     let a = Net::send(1, 5);\n///     let b = Net::send(1, 6);\n///     let x = Net::recv(1, 500);\n///     let y = Net::recv(1, 500);\n///     Clock::now()\n/// }}\n/// ```\nfn anchor() -> u64 {{\n    0\n}}\n"
    )
}

#[test]
fn simulated_doctests_are_seed_deterministic_and_seed_sensitive() {
    let first = run_doc(&sim_source("7"));
    let second = run_doc(&sim_source("7"));
    assert!(first.passed, "{first:?}");
    let a = serde_json::to_value(&first.examples[0]).unwrap();
    let b = serde_json::to_value(&second.examples[0]).unwrap();
    assert_eq!(a, b);
    let times: std::collections::BTreeSet<_> = (0..12)
        .map(|seed| {
            run_doc(&sim_source(&seed.to_string())).examples[0]
                .result
                .clone()
        })
        .collect();
    assert!(
        times.len() > 1,
        "seeded network latency must change virtual time"
    );
}

#[test]
fn simulated_doctests_reject_undeclared_devices_and_contract_mismatches() {
    // The entry uses Net but the fence declares only the clock.
    let undeclared = format!(
        "{DEVICES}/// ```vesper,sim(seed=1,clock),entry=leak\n/// fn leak() -> u64 eff [Net] {{\n///     Net::send(1, 2)\n/// }}\n/// ```\nfn anchor() -> u64 {{\n    0\n}}\n"
    );
    let report = run_doc(&undeclared);
    assert!(!report.passed);
    assert!(
        report.examples[0]
            .message
            .contains("not a simulated device declared"),
        "{}",
        report.examples[0].message
    );
    // A body fence gets exactly the declared row, so using Net fails checking.
    let body = format!(
        "{DEVICES}/// ```vesper,sim(seed=1,clock)\n/// let x = Net::send(1, 2);\n/// ```\nfn anchor() -> u64 {{\n    0\n}}\n"
    );
    let report = run_doc(&body);
    assert!(!report.passed);
    assert!(!report.examples[0].diagnostics.is_empty());
    // Operation signatures must match the Codex contract.
    let wrong = "linear effect Clock {\n    fn now() -> i64;\n}\n/// ```vesper,sim(seed=1,clock),entry=t\n/// fn t() -> i64 eff [Clock] {\n///     Clock::now()\n/// }\n/// ```\nfn anchor() -> u64 {\n    0\n}\n";
    let report = run_doc(wrong);
    assert!(
        report.examples[0]
            .message
            .contains("must take 0 u64 argument(s)")
    );
    for bad in [
        "/// ```vesper,sim(clock)\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
        "/// ```vesper,sim(seed=1,gpu)\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
        "/// ```vesper,compile_fail(E0302),sim(seed=1,clock)\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
        "/// ```vesper,sim(seed=1,clock\n/// let x = 1;\n/// ```\nfn a() -> u64 {\n    0\n}\n",
    ] {
        assert!(
            doctest::run(bad, DocTestLimits::default()).is_err(),
            "{bad}"
        );
    }
}
