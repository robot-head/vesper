use vesper::{
    parser,
    tooling::{
        frontend::{AnalysisFailure, AnalysisLimits, check_source},
        recovery::{RecoveryFailure, RecoveryLimits, UnavailableReason, analyze},
    },
};
use vesper_syntax::recovery::Limits as SyntaxLimits;
fn syntax(source: &str) -> vesper_syntax::recovery::RecoverySyntax {
    parser::parse_recovery(source, SyntaxLimits::default()).unwrap()
}
const SOURCE: &str =
    "// Unicode 🙂\r\nlinear struct Tx{} fn damaged(sink tx:Tx){tx.} fn leak(sink tx:Tx){}";
#[test]
fn damaged_function_is_wholly_unavailable_and_real_independent_leak_survives() {
    let snapshot = syntax(SOURCE);
    assert!(!snapshot.diagnostics().is_empty());
    let result = analyze(&snapshot, RecoveryLimits::default()).unwrap();
    assert_eq!(result.source(), SOURCE);
    assert_eq!(
        result.source_hash(),
        *blake3::hash(SOURCE.as_bytes()).as_bytes()
    );
    assert_eq!(
        result
            .analyzed_functions()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["leak"]
    );
    assert!(
        result
            .unavailable()
            .iter()
            .any(|u| u.reason == UnavailableReason::DamagedFunction
                && SOURCE[u.span.clone()].starts_with("fn damaged"))
    );
    assert!(!result.diagnostics().is_empty());
    assert!(result.diagnostics().iter().any(|d| {
        d.code == "E0302"
            && d.message.contains("tx")
            && d.related
                .iter()
                .any(|l| &SOURCE[l.span.clone()] == "sink tx:Tx")
    }));
    let leak = SOURCE.find("fn leak").unwrap();
    assert!(result.diagnostics().iter().all(|d| d.span.start >= leak));
    for c in result.analyzed_functions() {
        assert!(SOURCE[c.span.clone()].starts_with("fn leak"));
    }
}
#[test]
fn repairing_original_syntax_restores_strict_full_program_checking() {
    let empty = SOURCE.replace("{tx.}", "{}");
    assert!(syntax(&empty).diagnostics().is_empty());
    assert!(matches!(
        analyze(&syntax(&empty), RecoveryLimits::default()),
        Err(RecoveryFailure::Unsupported { .. })
    ));
    let failure = check_source(&empty, AnalysisLimits::default()).unwrap_err();
    assert!(matches!(failure, AnalysisFailure::Semantic { .. }));
    assert_eq!(
        failure
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E0302")
            .count(),
        2
    );
    let consumed = SOURCE.replace("{tx.}", "{let Tx{}=tx;}");
    let failure = check_source(&consumed, AnalysisLimits::default()).unwrap_err();
    assert_eq!(
        failure
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E0302")
            .count(),
        1
    );
    let all = consumed.replace("fn leak(sink tx:Tx){}", "fn leak(sink tx:Tx){let Tx{}=tx;}");
    check_source(&all, AnalysisLimits::default()).unwrap();
}
#[test]
fn complete_scopes_keep_shadowing_and_actual_errors_without_global_dependencies() {
    let source = "linear struct Tx{} fn damaged(sink tx:Tx){tx.} fn good(sink tx:Tx){{let x=1;let x=2;};let Tx{}=tx;} fn bad(sink tx:Tx){let Tx{}=tx;let Tx{}=tx;}";
    let result = analyze(&syntax(source), RecoveryLimits::default()).unwrap();
    assert_eq!(result.analyzed_functions().len(), 2);
    assert!(result.diagnostics().iter().any(|d| d.code == "E0303"));
    assert!(
        !result
            .diagnostics()
            .iter()
            .any(|d| d.span.start < source.find("fn bad").unwrap())
    );
}
#[test]
fn global_unknown_indirect_higher_order_and_generic_functions_are_unavailable() {
    for function in [
        "fn f(){other();}",
        "fn f(){mystery;}",
        "fn f(in callback:fn()->i64){callback();}",
        "fn f[T](in value:T){}",
        "fn f(){let c=||0;c();}",
    ] {
        let source = format!(
            "linear struct Tx{{}} fn damaged(sink tx:Tx){{tx.}} {function} fn leak(sink tx:Tx){{}}"
        );
        let result = analyze(&syntax(&source), RecoveryLimits::default()).unwrap();
        assert!(
            result
                .unavailable()
                .iter()
                .any(|u| source[u.span.clone()].starts_with("fn f")),
            "{function}: {result:?}"
        );
        assert!(
            result.analyzed_functions().iter().any(|c| c.name == "leak"),
            "{function}: {result:?}"
        );
        assert!(!result.diagnostics().iter().any(|d| d.code == "E0201"));
    }
}
#[test]
fn unknown_cyclic_generic_ambiguous_and_incomplete_contexts_never_guess_types() {
    for context in [
        "linear struct Tx{field:Unknown}",
        "linear struct Tx{field:Tx}",
        "linear struct Tx[T]{field:T}",
        "linear struct Tx{} linear struct Tx{}",
        "linear enum Tx{Same} enum Other{Same}",
        "trait T{fn f(in self);} linear struct Tx{}",
        "linear struct Tx{field:",
    ] {
        let source = format!("{context} fn damaged(sink tx:Tx){{tx.}} fn leak(sink tx:Tx){{}}");
        let snapshot = syntax(&source);
        let result = analyze(&snapshot, RecoveryLimits::default());
        match result {
            Ok(result) => {
                assert!(
                    result.analyzed_functions().is_empty(),
                    "{source}: {result:?}"
                );
                assert!(!result.unavailable().is_empty());
                assert!(result.diagnostics().is_empty());
            }
            Err(RecoveryFailure::Unsupported { .. }) => (),
            other => panic!("{source}: {other:?}"),
        }
    }
}
#[test]
fn opaque_lexical_top_level_gaps_and_spoofed_anchor_text_are_unavailable() {
    for source in [
        "linear struct Tx{} fn damaged(sink tx:Tx){tx.} junk fn leak(sink tx:Tx){}",
        "linear struct Tx{} fn damaged(sink tx:Tx){tx.} /* unterminated fn leak(sink tx:Tx){}",
        "linear struct Tx{} fn damaged(sink tx:Tx){\"fn leak(sink tx:Tx){}\";tx.}",
    ] {
        let result = analyze(&syntax(source), RecoveryLimits::default()).unwrap();
        assert!(result.analyzed_functions().is_empty());
        assert!(result.diagnostics().is_empty());
    }
}
#[test]
fn context_semantic_errors_are_actual_once_and_do_not_authorize_dependent_functions() {
    let source =
        "linear struct L{} copy struct Tx{field:L @@ copy} fn damaged(){x.} fn a(){} fn b(){}";
    let result = analyze(&syntax(source), RecoveryLimits::default()).unwrap();
    assert!(result.analyzed_functions().is_empty());
    assert!(!result.diagnostics().is_empty());
    assert!(
        result
            .unavailable()
            .iter()
            .any(|u| u.reason == UnavailableReason::InvalidContext)
    );
    let keys = result
        .diagnostics()
        .iter()
        .map(|d| (&d.code, &d.message, d.span.start, d.span.end))
        .collect::<Vec<_>>();
    assert_eq!(
        keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
        keys.len()
    );
}
#[test]
fn direct_limits_and_aggregate_diagnostic_context_and_check_work_fail_explicitly() {
    let snapshot = syntax(SOURCE);
    for limits in [
        RecoveryLimits {
            max_work: 1,
            ..RecoveryLimits::default()
        },
        RecoveryLimits {
            max_functions: 1,
            ..RecoveryLimits::default()
        },
        RecoveryLimits {
            max_context_items: 0,
            ..RecoveryLimits::default()
        },
        RecoveryLimits {
            max_work: usize::MAX,
            ..RecoveryLimits::default()
        },
        RecoveryLimits {
            analysis: AnalysisLimits {
                max_diagnostic_bytes: 1,
                ..AnalysisLimits::default()
            },
            ..RecoveryLimits::default()
        },
        RecoveryLimits {
            analysis: AnalysisLimits {
                max_diagnostics: 1,
                ..AnalysisLimits::default()
            },
            ..RecoveryLimits::default()
        },
        RecoveryLimits {
            analysis: AnalysisLimits {
                max_source_bytes: 1,
                ..AnalysisLimits::default()
            },
            ..RecoveryLimits::default()
        },
    ] {
        assert!(
            matches!(
                analyze(&snapshot, limits),
                Err(RecoveryFailure::Resource { .. })
            ),
            "{limits:?}"
        );
    }
    let mut source = "linear struct Tx{} fn damaged(sink tx:Tx){tx.} fn f(sink tx:Tx){".to_owned();
    for i in 0..40 {
        source.push_str(&format!("let x{i}={i};"));
    }
    source.push_str("if true{let y=0;}else{let z=0;};let Tx{}=tx;}");
    let snapshot = syntax(&source);
    analyze(&snapshot, RecoveryLimits::default()).unwrap();
    assert!(matches!(
        analyze(
            &snapshot,
            RecoveryLimits {
                max_work: source.len() * 4,
                ..RecoveryLimits::default()
            }
        ),
        Err(RecoveryFailure::Resource { .. })
    ));
}
