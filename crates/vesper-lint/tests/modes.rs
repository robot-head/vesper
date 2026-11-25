//! L0103 `overly_restrictive_mode` (manuscript §19.6 `[Lint-Demote-Mode]`,
//! the `inout` half), which only the Vesper port has (#1888). Cargo-only,
//! with `--features vesper-port`; port/test-port.sh runs it.
#![cfg(vesper_port)]

mod support;

use vesper_lint::{Action, FindingKind, Limits, analyze_modes};

/// `(function, parameter)` of every L0103 finding in `source`.
fn demotable(source: &str) -> Vec<(String, String)> {
    let facts = support::facts(source).expect("the fixture checks");
    let report = analyze_modes(&support::to_port(&facts), Limits::default()).unwrap();
    assert!(report.skipped.is_empty());
    report
        .findings
        .into_iter()
        .map(|f| {
            assert_eq!(f.code, "L0103");
            let FindingKind::OverlyRestrictiveMode { parameter } = f.detail else {
                panic!("L0103 carries OverlyRestrictiveMode");
            };
            assert!(
                f.message
                    .starts_with(&format!("inout {parameter} is never written"))
            );
            (f.function, parameter)
        })
        .collect()
}

fn pair(f: &str, p: &str) -> (String, String) {
    (f.into(), p.into())
}

const BROKER: &str = "copy struct Broker { index: u64, metrics: u64, config: u64 }\n";

#[test]
fn read_only_inout_is_demotable() {
    let source = format!(
        "{BROKER}fn peek(inout broker: Broker) -> u64 {{ broker.index }}
fn main() -> u64 {{ let mut b = Broker {{ index: 1, metrics: 2, config: 3 }}; peek(inout b) }}"
    );
    assert_eq!(demotable(&source), [pair("peek", "broker")]);
}

#[test]
fn unused_inout_is_demotable() {
    let source = format!(
        "{BROKER}fn ignore(inout broker: Broker) -> u64 {{ 7 }}
fn main() -> u64 {{ let mut b = Broker {{ index: 1, metrics: 2, config: 3 }}; ignore(inout b) }}"
    );
    assert_eq!(demotable(&source), [pair("ignore", "broker")]);
}

#[test]
fn read_only_projection_is_demotable() {
    let source = format!(
        "{BROKER}fn peek(inout broker.{{index, config}}: Broker) -> u64 {{
    wrapping_add(broker.index, broker.config)
}}
fn main() -> u64 {{ let mut b = Broker {{ index: 1, metrics: 2, config: 3 }}; peek(inout b) }}"
    );
    assert_eq!(demotable(&source), [pair("peek", "broker")]);
}

#[test]
fn field_writes_keep_inout() {
    let source = format!(
        "{BROKER}fn record_batch(inout broker: Broker, count: u64) {{
    broker.index = wrapping_add(broker.index, count);
}}
fn bump(inout broker.{{metrics}}: Broker) {{ broker.metrics = 0; }}
fn main() -> u64 {{
    let mut b = Broker {{ index: 1, metrics: 2, config: 3 }};
    record_batch(inout b, 4);
    bump(inout b);
    b.index
}}"
    );
    assert!(demotable(&source).is_empty());
}

#[test]
fn sequence_mutation_keeps_inout_and_length_does_not() {
    let source = "struct Bag { items: ChunkSeq[u64], n: u64 }
fn add(inout bag: Bag) { bag.items.push(1); }
fn look(inout bag: Bag) -> u64 { bag.items.len() }
fn main() -> u64 {
    let mut b = Bag { items: ChunkSeq::new[u64](), n: 0 };
    add(inout b);
    look(inout b)
}";
    assert_eq!(demotable(source), [pair("look", "bag")]);
}

#[test]
fn passing_inout_on_keeps_inout() {
    // Lending the parameter `inout` to a callee counts as a write, even if
    // that callee is itself demotable: one finding per signature, no chain.
    let source = format!(
        "{BROKER}fn peek(inout broker: Broker) -> u64 {{ broker.index }}
fn relay(inout broker: Broker) -> u64 {{ peek(inout broker) }}
fn main() -> u64 {{ let mut b = Broker {{ index: 1, metrics: 2, config: 3 }}; relay(inout b) }}"
    );
    assert_eq!(demotable(&source), [pair("peek", "broker")]);
}

#[test]
fn in_and_sink_parameters_are_not_reported() {
    let source = format!(
        "{BROKER}fn read(in broker: Broker) -> u64 {{ broker.index }}
fn take(sink broker: Broker) -> u64 {{ broker.index }}
fn main() -> u64 {{
    let b = Broker {{ index: 1, metrics: 2, config: 3 }};
    wrapping_add(read(in b), take(b))
}}"
    );
    assert!(demotable(&source).is_empty());
}

#[test]
fn opaque_bodies_are_not_reported() {
    let source = format!(
        "{BROKER}fn peek(inout broker: Broker) -> u64 {{
    let k = broker.index;
    let f = || k;
    f()
}}
fn main() -> u64 {{ let mut b = Broker {{ index: 1, metrics: 2, config: 3 }}; peek(inout b) }}"
    );
    let facts = support::facts(&source).expect("the fixture checks");
    let program = support::to_port(&facts);
    assert!(
        program.functions["peek"]
            .actions
            .iter()
            .any(|a| matches!(a, Action::Opaque(_)))
    );
    let report = analyze_modes(&program, Limits::default()).unwrap();
    assert!(report.findings.is_empty());
}

#[test]
fn limits_fail_like_analyze() {
    let facts = support::facts(&format!("{BROKER}fn main() -> u64 {{ 0 }}")).unwrap();
    let program = support::to_port(&facts);
    for limits in [
        Limits {
            work: 0,
            ..Limits::default()
        },
        Limits {
            functions: vesper_lint::MAX_FUNCTIONS + 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            analyze_modes(&program, limits).unwrap_err(),
            vesper_lint::analyze(&program, limits).unwrap_err()
        );
    }
}

/// Over every repository source that checks as one module: each finding
/// names an `inout` parameter of an owned function that no `Access` writes
/// or moves, and every such parameter of a function without opaque actions
/// is reported (the rule is exactly its definition, in both directions).
#[test]
fn repository_sources_match_the_definition() {
    let mut seen = 0usize;
    for (_, facts) in support::corpus(65536) {
        let program = support::to_port(&facts);
        let Ok(report) = analyze_modes(&program, Limits::default()) else {
            continue;
        };
        let mut expected = Vec::new();
        for (name, f) in &program.functions {
            if !f.source_owned
                || f.declaration.body.is_none()
                || f.actions.iter().any(|a| matches!(a, Action::Opaque(_)))
            {
                continue;
            }
            for p in &f.parameters {
                let p = &p.parameter;
                if p.pass != vesper_core::ast::Pass::Inout {
                    continue;
                }
                let written = f.actions.iter().any(|a| {
                    matches!(a, Action::Access { root, pass, moves_noncopy, .. }
                        if root == &p.name
                            && (*moves_noncopy || *pass == vesper_core::ast::Pass::Inout))
                });
                if !written {
                    expected.push(pair(name, &p.name));
                }
            }
        }
        let actual: Vec<_> = report
            .findings
            .iter()
            .map(|f| match &f.detail {
                FindingKind::OverlyRestrictiveMode { parameter } => pair(&f.function, parameter),
                _ => panic!("analyze_modes reports L0103 only"),
            })
            .collect();
        assert_eq!(actual, expected);
        seen += actual.len();
    }
    assert!(seen > 0, "the corpus has demotable parameters");
}
