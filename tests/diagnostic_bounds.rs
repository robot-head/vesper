use vesper::{
    ast::{Diagnostic, DiagnosticLabel},
    check, parser,
    tooling::frontend::{AnalysisFailure, AnalysisLimits, check_source},
};
const MOVED: &str =
    "// Unicode 🙂 source\nlinear struct Tx{} fn bad(sink tx:Tx){let Tx{}=tx;let Tx{}=tx;}";
const ABORT: &str = "// Unicode 🙂 source\nlinear struct Tx{} effect Ask{fn ask();} fn bad(sink tx:Tx) eff[Ask]{Ask::ask();let Tx{}=tx;}";
fn raw_diagnostics(source: &str) -> Vec<Diagnostic> {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty());
    check::check(&parsed.module).diagnostics
}
fn occupied_bytes(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .map(|d| {
            core::mem::size_of::<Diagnostic>()
                + d.code.len()
                + d.message.len()
                + d.related.len() * core::mem::size_of::<DiagnosticLabel>()
                + d.related.iter().map(|l| l.message.len()).sum::<usize>()
                + d.help.as_ref().map_or(0, String::len)
        })
        .sum()
}
#[test]
fn actual_rich_checker_failures_survive_guard_with_exact_causal_metadata() {
    for (source, code) in [(MOVED, "E0303"), (ABORT, "E0402")] {
        let raw = raw_diagnostics(source);
        let rich = raw
            .iter()
            .find(|d| d.code == code)
            .expect("actual expected checker error");
        assert!(!rich.related.is_empty());
        assert!(rich.help.is_some());
        assert!(
            rich.related
                .iter()
                .all(|label| !source[label.span.clone()].is_empty())
        );
        let failure = check_source(source, AnalysisLimits::default()).unwrap_err();
        assert!(matches!(failure, AnalysisFailure::Semantic { .. }));
        assert_eq!(failure.diagnostics(), raw);
        let moved = failure
            .diagnostics()
            .iter()
            .find(|d| d.code == code)
            .unwrap();
        if code == "E0303" {
            assert_eq!(&source[moved.span.clone()], "tx");
            assert!(
                moved
                    .related
                    .iter()
                    .any(|l| &source[l.span.clone()] == "sink tx:Tx")
            );
            assert!(
                moved
                    .related
                    .iter()
                    .any(|l| &source[l.span.clone()] == "tx" && l.span != moved.span)
            );
        }
    }
}
#[test]
fn actual_rich_diagnostics_obey_aggregate_exact_budget_and_resource_failure() {
    for source in [MOVED, ABORT] {
        let raw = raw_diagnostics(source);
        let bytes = occupied_bytes(&raw);
        let failure = check_source(
            source,
            AnalysisLimits {
                max_diagnostic_bytes: bytes,
                ..AnalysisLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(failure, AnalysisFailure::Semantic { .. }));
        assert_eq!(failure.diagnostics(), raw);
        let failure = check_source(
            source,
            AnalysisLimits {
                max_diagnostic_bytes: bytes - 1,
                ..AnalysisLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(failure, AnalysisFailure::Resource { .. }));
        assert!(failure.diagnostics().is_empty());
    }
}
#[test]
fn metadata_refusal_never_promotes_failure_and_successful_snapshot_is_unchanged() {
    let failure = check_source(
        MOVED,
        AnalysisLimits {
            max_diagnostic_bytes: 1,
            ..AnalysisLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(failure, AnalysisFailure::Resource { .. }));
    let source = "// Unicode 🙂 source\nlinear struct Tx{} fn good(sink tx:Tx){let Tx{}=tx;}";
    let snapshot = check_source(
        source,
        AnalysisLimits {
            max_diagnostic_bytes: 1,
            ..AnalysisLimits::default()
        },
    )
    .unwrap();
    assert_eq!(snapshot.source(), source);
    assert!(snapshot.checked().diagnostics.is_empty());
    assert!(snapshot.world().functions.contains_key("good"));
    assert_eq!(
        snapshot.source_hash(),
        *blake3::hash(source.as_bytes()).as_bytes()
    );
}
