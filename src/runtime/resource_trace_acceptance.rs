//! Private, controlled carrier faults after genuine checked/optimized execution.
//! These are handoff fault controls, not a public input-injection API or a
//! compiler-refinement argument. Neither the program nor the ledger is mutated.
use super::*;
use resource_trace::{
    ResourceAction as Action, ResourceTraceLimits as Limits, ResourceTraceReport as Report,
    ResourceTraceStatus as Status,
};

const SOURCE: &str = r#"
linear struct Ticket {}
linear struct Ghost {}
copy struct Pair { left: Ticket, right: Ticket }
fn make() -> Pair {
    Pair { left: Ticket {}, right: Ticket {} }
}
fn sequential(sink pair: Pair) {
    let Pair { left, right } = pair;
    let Ticket {} = left;
    let Ticket {} = right;
}
fn batch(sink pair: Pair) {
    let Pair { left: Ticket {}, right: Ticket {} } = pair;
}
"#;

#[derive(Clone, Copy)]
enum Mutation {
    None,
    SwapChildren,
    DuplicateRight,
    MissingRightIdentity,
    WrongRightType,
    NonemptyRight,
}

fn source_site(context: &str, token: &str) -> Span {
    let context_start = SOURCE.find(context).unwrap();
    let start = context_start + SOURCE[context_start..].find(token).unwrap();
    start..start + token.len()
}

fn child<'a>(value: &'a Value, field: &str) -> &'a Value {
    let Value::Data { fields, .. } = value else {
        panic!("make must actually return a data carrier")
    };
    &fields[field]
}

fn identity(value: &Value) -> ResourceIdentity {
    let Value::Data {
        ty,
        constructor,
        fields,
        resource_identity: Some(identity),
    } = value
    else {
        panic!("actual Ticket construction must mint an identity")
    };
    assert_eq!(ty, "Ticket");
    assert_eq!(constructor, "Ticket");
    assert!(fields.is_empty());
    *identity
}

fn mutate(pair: &mut Value, mutation: Mutation) {
    let Value::Data { fields, .. } = pair else {
        panic!("expected actual Pair")
    };
    if matches!(mutation, Mutation::SwapChildren) {
        let actual_left = fields.remove("left").unwrap();
        let actual_right = fields.remove("right").unwrap();
        fields.insert("left".into(), actual_right);
        fields.insert("right".into(), actual_left);
        return;
    }
    if matches!(mutation, Mutation::DuplicateRight) {
        let actual_left = fields["left"].clone();
        fields.insert("right".into(), actual_left);
        return;
    }
    let Value::Data {
        ty,
        fields,
        resource_identity,
        ..
    } = fields.get_mut("right").unwrap()
    else {
        panic!("expected actual right Ticket")
    };
    match mutation {
        Mutation::None => {}
        Mutation::MissingRightIdentity => *resource_identity = None,
        // Keep the constructor name Ticket: pure pattern matching succeeds,
        // so the nominal trace check must reject the actual changed header.
        Mutation::WrongRightType => *ty = "Ghost".into(),
        Mutation::NonemptyRight => {
            fields.insert("unexpected".into(), Value::Int(7));
        }
        Mutation::SwapChildren | Mutation::DuplicateRight => unreachable!(),
    }
}

fn handoff(optimized: bool, sink: &str, mutation: Mutation) -> (Result<Value, String>, Report) {
    let parsed = crate::parser::parse(SOURCE);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let checked = crate::check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let world = checked.world;
    let mut runtime = Runtime::new(&world);
    runtime.optimize = optimized;
    let errors = crate::ir_check::validate(&world, &runtime.program);
    assert!(
        errors.is_empty(),
        "valid source must lower valid IR: {errors:?}"
    );
    let config = ResourceTraceConfig::new(&world, &["Ticket"], Limits::default()).unwrap();
    let observer = runtime.observe_resources(config).unwrap();
    runtime
        .resource_trace
        .as_mut()
        .unwrap()
        .begin_run()
        .unwrap();

    // Use the ordinary optimization/invoke/drive path exactly once, before
    // mutating any value. Calling public run would start a different epoch.
    let mut pair = runtime.run_execution("make", vec![]).unwrap();
    assert_eq!(runtime.optimized, optimized);
    let errors = crate::ir_check::validate(&world, &runtime.program);
    assert!(
        errors.is_empty(),
        "executed program must remain valid: {errors:?}"
    );
    let left = identity(child(&pair, "left"));
    let right = identity(child(&pair, "right"));
    assert_ne!(left, right);
    let minted = observer.snapshot().unwrap();
    assert_eq!(minted.status(), Status::Running);
    assert!(minted.failure().is_none());
    assert_eq!(minted.events().len(), 2);
    assert_eq!(minted.live().len(), 2);
    for (event, (id, site)) in minted.events().iter().zip([
        (left, source_site("Pair { left: Ticket {}", "Ticket {}")),
        (right, source_site("right: Ticket {} }", "Ticket {}")),
    ]) {
        assert_eq!(event.action(), Action::Mint);
        assert_eq!(event.kind(), 0);
        assert_eq!(event.identity(), id);
        assert_eq!(event.site(), &site);
    }
    mutate(&mut pair, mutation);
    let result = runtime.run_execution(sink, vec![pair]);
    assert_eq!(runtime.optimized, optimized);
    runtime
        .resource_trace
        .as_mut()
        .unwrap()
        .finish(if result.is_ok() {
            ResourceTraceCompletion::Completed
        } else {
            ResourceTraceCompletion::RuntimeError
        })
        .unwrap();
    let report = observer.snapshot().unwrap();
    // A copied pre-fault snapshot must retain the actual distinct mint ledger.
    assert_eq!(minted.events().len(), 2);
    assert_eq!(minted.live().len(), 2);
    assert!(minted.failure().is_none());
    (result, report)
}

fn pattern_site(sink: &str, side: &str) -> Span {
    let context = match (sink, side) {
        ("sequential", "left") => "let Ticket {} = left;",
        ("sequential", "right") => "let Ticket {} = right;",
        ("batch", "left") => "left: Ticket {}, right: Ticket {} } = pair;",
        ("batch", "right") => "right: Ticket {} } = pair;",
        _ => panic!("unexpected fixture pattern"),
    };
    source_site(context, "Ticket {}")
}

fn assert_fault(report: &Report, reason: ResourceTraceFailure, sink: &str, consumes: usize) {
    assert_eq!(report.status(), Status::TraceFailed);
    let failure = report.failure().unwrap();
    assert_eq!(failure.reason(), reason);
    assert_eq!(failure.site(), Some(&pattern_site(sink, "right")));
    assert_eq!(report.events().len(), 2 + consumes);
    assert_eq!(report.live().len(), 2 - consumes);
    assert_eq!(report.events()[0].action(), Action::Mint);
    assert_eq!(report.events()[1].action(), Action::Mint);
    if consumes == 1 {
        let event = &report.events()[2];
        assert_eq!(event.action(), Action::Consume);
        assert_eq!(event.identity(), report.events()[0].identity());
        assert_eq!(event.site(), &pattern_site(sink, "left"));
    }
    for live in report.live() {
        let mint = &report.events()[if consumes == 1 {
            1
        } else if live.identity() == report.events()[0].identity() {
            0
        } else {
            1
        }];
        assert_eq!(live.identity(), mint.identity());
        assert_eq!(live.kind(), mint.kind());
        assert_eq!(live.mint_site(), mint.site());
    }
}

#[test]
fn valid_private_handoffs_consume_the_two_actual_identities_in_both_ir_modes() {
    for optimized in [false, true] {
        for sink in ["sequential", "batch"] {
            let (result, report) = handoff(optimized, sink, Mutation::None);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(report.status(), Status::Completed);
            assert!(report.failure().is_none());
            assert!(report.live().is_empty());
            assert_eq!(report.events().len(), 4);
            for (index, side) in ["left", "right"].into_iter().enumerate() {
                let consume = &report.events()[index + 2];
                assert_eq!(consume.action(), Action::Consume);
                assert_eq!(consume.identity(), report.events()[index].identity());
                assert_eq!(consume.site(), &pattern_site(sink, side));
            }
        }
    }
}

#[test]
fn swapped_actual_carriers_reverse_consume_identities_at_unchanged_sites_in_both_ir_modes() {
    for optimized in [false, true] {
        for sink in ["sequential", "batch"] {
            let (result, report) = handoff(optimized, sink, Mutation::SwapChildren);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(report.status(), Status::Completed);
            assert!(report.failure().is_none());
            assert!(report.live().is_empty());
            assert_eq!(report.events().len(), 4);
            assert_eq!(report.events()[0].action(), Action::Mint);
            assert_eq!(report.events()[1].action(), Action::Mint);
            for (index, side) in ["left", "right"].into_iter().enumerate() {
                let consume = &report.events()[index + 2];
                let original_opposite_mint = &report.events()[1 - index];
                assert_eq!(consume.action(), Action::Consume);
                assert_eq!(consume.identity(), original_opposite_mint.identity());
                assert_eq!(consume.kind(), original_opposite_mint.kind());
                assert_eq!(consume.site(), &pattern_site(sink, side));
            }
        }
    }
}

#[test]
fn duplicate_actual_carrier_fails_sequentially_after_one_consume_in_both_ir_modes() {
    for optimized in [false, true] {
        let (result, report) = handoff(optimized, "sequential", Mutation::DuplicateRight);
        assert!(result.is_err());
        assert_fault(
            &report,
            ResourceTraceFailure::AlreadyConsumed,
            "sequential",
            1,
        );
    }
}

#[test]
fn duplicate_actual_carrier_refuses_the_entire_batch_in_both_ir_modes() {
    for optimized in [false, true] {
        let (result, report) = handoff(optimized, "batch", Mutation::DuplicateRight);
        assert!(result.is_err());
        assert_fault(&report, ResourceTraceFailure::DuplicateIdentity, "batch", 0);
    }
}

#[test]
fn actual_missing_and_wrong_headers_keep_precise_transaction_prefixes_in_both_ir_modes() {
    for optimized in [false, true] {
        for (mutation, reason) in [
            (
                Mutation::MissingRightIdentity,
                ResourceTraceFailure::MissingIdentity,
            ),
            (Mutation::WrongRightType, ResourceTraceFailure::WrongKind),
            (
                Mutation::NonemptyRight,
                ResourceTraceFailure::MalformedCarrier,
            ),
        ] {
            for (sink, consumes) in [("sequential", 1), ("batch", 0)] {
                let (result, report) = handoff(optimized, sink, mutation);
                assert!(result.is_err());
                assert_fault(&report, reason, sink, consumes);
            }
        }
    }
}
