//! Independent actual-value acceptance for the reference evaluator provider.
//! No native/resource-oracle or exhaustion theorem is inferred from this slice.
use vesper::{
    ast::{Expr, ExprKind, Literal, PatternKind, Statement},
    check::{self, World},
    optimize, parser,
    runtime::{
        Runtime, Value,
        resource_trace::{
            ResourceAction as Action, ResourceTraceConfig as Config,
            ResourceTraceFailure as Failure, ResourceTraceLimits as Limits,
            ResourceTraceReport as Report, ResourceTraceStatus as Status,
        },
    },
};

const LIFE: &str = include_str!("fixtures/runtime_resource_trace/lifecycle.ves");
const PROBE: &str = include_str!("fixtures/runtime_resource_trace/match_probe.ves");
const MUTATION: &str = include_str!("fixtures/runtime_resource_trace/mutation.ves");
const CAPACITY: &str = include_str!("fixtures/runtime_resource_trace/capacity.ves");
const HANDLERS: &str = include_str!("fixtures/runtime_resource_trace/handlers.ves");

fn checked(source: &str) -> World {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let checked = check::check(&parsed.module);
    assert!(checked.diagnostics.is_empty(), "{:#?}", checked.diagnostics);
    checked.world
}

fn execute(
    world: &World,
    selected: &[&str],
    limits: Limits,
    optimize: bool,
    entry: &str,
    args: Vec<Value>,
) -> (Result<Value, String>, Report) {
    let config = Config::new(world, selected, limits).unwrap();
    let mut runtime = Runtime::new(world);
    runtime.optimize = optimize;
    let observer = runtime.observe_resources(config).unwrap();
    let result = runtime.run(entry, args);
    let report = observer.snapshot().unwrap();
    (result, report)
}

fn answer(result: Result<Value, String>) {
    assert!(matches!(result.unwrap(), Value::Int(42)));
}

fn actions(report: &Report) -> Vec<Action> {
    report.events().iter().map(|event| event.action()).collect()
}

fn failure(report: &Report, expected: Failure) {
    assert_eq!(report.status(), Status::TraceFailed);
    assert_eq!(report.failure().unwrap().reason(), expected);
}

fn function_span(
    source: &str,
    function: &str,
    needle: &str,
    occurrence: usize,
) -> std::ops::Range<usize> {
    let start = source.find(&format!("fn {function}(")).unwrap();
    let relative = source[start..]
        .match_indices(needle)
        .nth(occurrence)
        .unwrap()
        .0;
    let start = start + relative;
    start..start + needle.len()
}

fn assert_closed(report: &Report) {
    assert_eq!(report.status(), Status::Completed);
    assert!(report.failure().is_none());
    assert!(report.live().is_empty());
}

#[test]
fn actual_construction_and_terminal_patterns_have_original_sites() {
    let world = checked(LIFE);
    for optimize in [false, true] {
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "simple",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
        assert_eq!(
            report.events()[0].site(),
            &function_span(LIFE, "simple", "Ticket {}", 0)
        );
        assert_eq!(
            report.events()[1].site(),
            &function_span(LIFE, "simple", "Ticket {}", 1)
        );
        assert_eq!(report.events()[0].identity(), report.events()[1].identity());
        assert_eq!(report.selected_types(), &["Ticket".to_owned()]);

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "named_constructor",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        let prefix = "let ticket = ";
        let start = function_span(LIFE, "named_constructor", "let ticket = Ticket;", 0).start
            + prefix.len();
        assert_eq!(report.events()[0].site(), &(start..start + "Ticket".len()));
        assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
    }
}

#[test]
fn identities_follow_actual_transfers_borrows_wrappers_and_projection() {
    let world = checked(LIFE);
    for optimize in [false, true] {
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "transfer",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
        assert_eq!(report.events()[0].identity(), report.events()[1].identity());
        assert_eq!(
            report.events()[1].site(),
            &function_span(LIFE, "close", "Ticket {}", 0)
        );

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "shadow",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(
            actions(&report),
            [Action::Mint, Action::Consume, Action::Mint, Action::Consume]
        );
        assert_ne!(report.events()[0].identity(), report.events()[2].identity());
    }
}

#[test]
fn repeated_dynamic_site_keeps_distinct_carriers_and_actual_consume_order() {
    let world = checked(LIFE);
    for optimize in [false, true] {
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "repeated_site",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(
            actions(&report),
            [Action::Mint, Action::Mint, Action::Consume, Action::Consume]
        );
        let events = report.events();
        assert_eq!(events[0].site(), events[1].site());
        assert_eq!(
            events[0].site(),
            &function_span(LIFE, "mint", "Ticket {}", 0)
        );
        assert_ne!(events[0].identity(), events[1].identity());
        assert_eq!(events[2].identity(), events[1].identity());
        assert_eq!(events[3].identity(), events[0].identity());
        assert_eq!(events[2].site(), events[3].site());
    }
}

#[test]
fn selected_kinds_are_exact_and_unselected_linear_values_stay_unobserved() {
    let world = checked(LIFE);
    for optimize in [false, true] {
        let (result, report) = execute(
            &world,
            &["Stamp", "Ticket"],
            Limits::default(),
            optimize,
            "kinds",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(
            actions(&report),
            [Action::Mint, Action::Mint, Action::Consume, Action::Consume]
        );
        assert_eq!(
            report
                .events()
                .iter()
                .map(|event| event.kind())
                .collect::<Vec<_>>(),
            [1, 0, 0, 1]
        );
        assert_ne!(report.events()[0].identity(), report.events()[1].identity());

        let (result, report) = execute(&world, &[], Limits::default(), optimize, "kinds", vec![]);
        answer(result);
        assert_closed(&report);
        assert!(report.events().is_empty());
    }
}

#[test]
fn selected_branch_and_failed_pattern_probe_never_consume_speculatively() {
    let world = checked(LIFE);
    let probe_world = checked(PROBE);
    for optimize in [false, true] {
        for choice in [false, true] {
            let (result, report) = execute(
                &world,
                &["Ticket"],
                Limits::default(),
                optimize,
                "branch",
                vec![Value::Bool(choice)],
            );
            answer(result);
            assert_closed(&report);
            assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
            let expected = if choice {
                function_span(LIFE, "branch", "Ticket {}", 1)
            } else {
                function_span(LIFE, "close", "Ticket {}", 0)
            };
            assert_eq!(report.events()[1].site(), &expected);
        }
        let (result, report) = execute(
            &probe_world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "probe",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
        assert_eq!(
            report.events()[1].site(),
            &function_span(PROBE, "probe", "Ticket {}", 2)
        );
    }
}

#[test]
fn actual_ast_and_ir_optimizers_keep_identity_and_pattern_provenance() {
    for source in [LIFE, PROBE] {
        for ast_optimized in [false, true] {
            let mut parsed = parser::parse(source);
            assert!(parsed.diagnostics.is_empty());
            let original = check::check(&parsed.module);
            assert!(original.diagnostics.is_empty());
            if ast_optimized {
                optimize::optimize(&mut parsed.module, &original.expression_types);
            }
            let checked = check::check(&parsed.module);
            assert!(checked.diagnostics.is_empty());
            let entry = if source == LIFE {
                "repeated_site"
            } else {
                "probe"
            };
            for ir_optimized in [false, true] {
                let (result, report) = execute(
                    &checked.world,
                    &["Ticket"],
                    Limits::default(),
                    ir_optimized,
                    entry,
                    vec![],
                );
                answer(result);
                assert_closed(&report);
                if source == LIFE {
                    assert_eq!(
                        actions(&report),
                        [Action::Mint, Action::Mint, Action::Consume, Action::Consume]
                    );
                    assert_eq!(
                        report.events()[0].site(),
                        &function_span(source, "mint", "Ticket {}", 0)
                    );
                    assert_eq!(
                        report.events()[2].site(),
                        &function_span(source, "close", "Ticket {}", 0)
                    );
                    assert_eq!(report.events()[2].identity(), report.events()[1].identity());
                } else {
                    assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
                    assert_eq!(
                        report.events()[1].site(),
                        &function_span(source, "probe", "Ticket {}", 2)
                    );
                }
            }
        }
    }
}

#[test]
fn owning_handler_returns_consume_but_borrowed_operation_patterns_do_not() {
    let world = checked(HANDLERS);
    for optimize in [false, true] {
        for entry in ["handler_return", "borrowed_handler"] {
            let (result, report) = execute(
                &world,
                &["Ticket"],
                Limits::default(),
                optimize,
                entry,
                vec![],
            );
            answer(result);
            assert_closed(&report);
            assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
            assert_eq!(
                report.events()[1].site(),
                &function_span(HANDLERS, entry, "Ticket {}", 1)
            );
        }
    }
}

#[test]
fn entire_operation_clause_binding_commits_owned_consumes_atomically() {
    let world = checked(HANDLERS);
    for optimize in [false, true] {
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "clause_transaction",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(
            actions(&report),
            [Action::Mint, Action::Mint, Action::Consume, Action::Consume]
        );
        assert_eq!(report.events()[2].identity(), report.events()[0].identity());
        assert_eq!(report.events()[3].identity(), report.events()[1].identity());

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits {
                max_events: 3,
                ..Limits::default()
            },
            optimize,
            "clause_transaction",
            vec![],
        );
        assert!(result.is_err());
        failure(&report, Failure::EventLimit);
        assert_eq!(actions(&report), [Action::Mint, Action::Mint]);
        assert_eq!(report.live().len(), 2);

        // The source checker requires irrefutable operation patterns. Begin
        // with its genuine accepted World, then corrupt only the actual late
        // scalar pattern. Earlier owned arguments must not be consumed during
        // a failed whole-clause binding transaction.
        let mut changed = world.clone();
        let body = changed
            .functions
            .get_mut("clause_transaction")
            .unwrap()
            .body
            .as_mut()
            .unwrap();
        let ExprKind::Block {
            tail: Some(handle), ..
        } = &mut body.kind
        else {
            panic!()
        };
        let ExprKind::Handle { ops, .. } = &mut handle.kind else {
            panic!()
        };
        let clause = ops
            .iter_mut()
            .find(|clause| clause.name == "consume")
            .unwrap();
        assert!(matches!(&clause.pats[2].kind, PatternKind::Bind(name) if name == "expected"));
        clause.pats[2].kind = PatternKind::Literal(Literal::Bool(true));
        let (result, report) = execute(
            &changed,
            &["Ticket"],
            Limits::default(),
            optimize,
            "clause_transaction",
            vec![],
        );
        assert!(result.is_err());
        assert_eq!(report.status(), Status::RuntimeError);
        assert!(report.failure().is_none());
        assert_eq!(actions(&report), [Action::Mint, Action::Mint]);
        assert_eq!(report.live().len(), 2);
    }
}

fn replace_name(expr: &mut Expr, at: usize, old: &str, new: &str) -> usize {
    if expr.span.start == at && matches!(&expr.kind, ExprKind::Name(name) if name == old) {
        expr.kind = ExprKind::Name(new.to_owned());
        return 1;
    }
    match &mut expr.kind {
        ExprKind::Tuple(values) => values
            .iter_mut()
            .map(|expr| replace_name(expr, at, old, new))
            .sum(),
        ExprKind::Struct { fields, .. } => fields
            .iter_mut()
            .map(|(_, expr)| replace_name(expr, at, old, new))
            .sum(),
        ExprKind::Block { statements, tail } => {
            let mut count = 0;
            for statement in statements {
                count += match statement {
                    Statement::Let { value, .. } | Statement::Expr(value) => {
                        replace_name(value, at, old, new)
                    }
                };
            }
            count
                + tail
                    .as_mut()
                    .map_or(0, |expr| replace_name(expr, at, old, new))
        }
        ExprKind::Call { callee, args, .. } => {
            replace_name(callee, at, old, new)
                + args
                    .iter_mut()
                    .map(|arg| replace_name(&mut arg.expr, at, old, new))
                    .sum::<usize>()
        }
        _ => 0,
    }
}

fn mutant(function: &str, context: &str, offset: usize, old: &str, new: &str) -> World {
    // The initial World is always the real accepted parser/checker output.
    // Change actual executable routing while retaining its genuine source spans.
    let mut world = checked(MUTATION);
    let at = function_span(MUTATION, function, context, 0).start + offset;
    let body = world
        .functions
        .get_mut(function)
        .unwrap()
        .body
        .as_mut()
        .unwrap();
    assert_eq!(replace_name(body, at, old, new), 1);
    world
}

#[test]
fn actual_checked_world_carrier_routing_mutants_refuse_aliasing() {
    let valid = checked(MUTATION);
    for optimize in [false, true] {
        let (result, report) = execute(
            &valid,
            &["Ticket"],
            Limits::default(),
            optimize,
            "routing",
            vec![],
        );
        answer(result);
        assert_closed(&report);
        assert_eq!(report.events()[2].identity(), report.events()[0].identity());
        assert_eq!(report.events()[3].identity(), report.events()[1].identity());

        for world in [
            mutant(
                "routing",
                "right: second",
                "right: ".len(),
                "second",
                "first",
            ),
            mutant("routing", "close(right)", "close(".len(), "right", "left"),
        ] {
            let (result, report) = execute(
                &world,
                &["Ticket"],
                Limits::default(),
                optimize,
                "routing",
                vec![],
            );
            assert!(result.is_err());
            failure(&report, Failure::AlreadyConsumed);
            assert_eq!(
                actions(&report),
                [Action::Mint, Action::Mint, Action::Consume]
            );
            assert_eq!(report.events()[2].identity(), report.events()[0].identity());
            assert_eq!(report.live().len(), 1);
            assert_eq!(report.live()[0].identity(), report.events()[1].identity());
        }
        let world = mutant(
            "batch",
            "(first, second)",
            "(first, ".len(),
            "second",
            "first",
        );
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "batch",
            vec![],
        );
        assert!(result.is_err());
        failure(&report, Failure::DuplicateIdentity);
        assert_eq!(actions(&report), [Action::Mint, Action::Mint]);
        assert_eq!(report.live().len(), 2);
    }
}

#[test]
fn actual_returned_carriers_are_validated_without_claiming_exhaustion() {
    let world = checked(LIFE);
    for optimize in [false, true] {
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "returned",
            vec![],
        );
        let value = result.unwrap();
        assert_eq!(report.status(), Status::Completed);
        assert!(report.failure().is_none());
        assert_eq!(actions(&report), [Action::Mint]);
        assert_eq!(report.live().len(), 1);
        let Value::Data {
            resource_identity: Some(identity),
            ..
        } = value
        else {
            panic!("returned owner lost identity")
        };
        assert_eq!(identity, report.live()[0].identity());
        assert_eq!(report.live()[0].mint_site(), report.events()[0].site());
    }
    for (function, context, offset, old, new, reason, prefix) in [
        (
            "duplicated_return",
            "(first, second)",
            "(first, ".len(),
            "second",
            "first",
            Failure::DuplicateIdentity,
            vec![Action::Mint, Action::Mint],
        ),
        (
            "dead_return",
            "close(first);\n    second",
            "close(first);\n    ".len(),
            "second",
            "first",
            Failure::AlreadyConsumed,
            vec![Action::Mint, Action::Mint, Action::Consume],
        ),
    ] {
        for optimize in [false, true] {
            let world = mutant(function, context, offset, old, new);
            let (result, report) = execute(
                &world,
                &["Ticket"],
                Limits::default(),
                optimize,
                function,
                vec![],
            );
            assert!(result.is_err());
            failure(&report, reason);
            assert_eq!(actions(&report), prefix);
            assert!(!report.live().is_empty());
        }
    }
}

#[test]
fn entry_rejects_missing_foreign_and_prior_run_carriers_before_execution() {
    let world = checked(LIFE);
    let mut ordinary = Runtime::new(&world);
    let missing = ordinary.run("returned", vec![]).unwrap();
    assert!(matches!(
        &missing,
        Value::Data {
            resource_identity: None,
            ..
        }
    ));
    let (result, report) = execute(
        &world,
        &["Ticket"],
        Limits::default(),
        true,
        "accept",
        vec![missing],
    );
    assert!(result.is_err());
    failure(&report, Failure::MissingIdentity);
    assert!(report.events().is_empty());

    let config = Config::new(&world, &["Ticket"], Limits::default()).unwrap();
    let mut first = Runtime::new(&world);
    let observer = first.observe_resources(config).unwrap();
    let owned = first.run("returned", vec![]).unwrap();
    let before = observer.snapshot().unwrap();
    assert_eq!(before.live().len(), 1);
    let (result, foreign) = execute(
        &world,
        &["Ticket"],
        Limits::default(),
        true,
        "accept",
        vec![owned.clone()],
    );
    assert!(result.is_err());
    failure(&foreign, Failure::ForeignInstance);
    assert!(foreign.events().is_empty());
    let mut wrong_kind = owned.clone();
    let Value::Data {
        ty, constructor, ..
    } = &mut wrong_kind
    else {
        panic!()
    };
    *ty = "Ghost".to_owned();
    *constructor = "Ghost".to_owned();
    let (result, report) = execute(
        &world,
        &["Ticket"],
        Limits::default(),
        true,
        "accept",
        vec![wrong_kind],
    );
    assert!(result.is_err());
    failure(&report, Failure::WrongKind);
    assert!(report.events().is_empty());
    let mut malformed = owned.clone();
    let Value::Data { fields, .. } = &mut malformed else {
        panic!()
    };
    fields.insert("forged".to_owned(), Value::Int(0));
    let (result, report) = execute(
        &world,
        &["Ticket"],
        Limits::default(),
        true,
        "accept",
        vec![malformed],
    );
    assert!(result.is_err());
    failure(&report, Failure::MalformedCarrier);
    assert!(report.events().is_empty());
    assert!(first.run("accept", vec![owned]).is_err());
    let stale = observer.snapshot().unwrap();
    failure(&stale, Failure::ForeignRun);
    assert!(stale.events().is_empty());
    assert_eq!(before.events().len(), 1);
    assert_eq!(before.live().len(), 1);
    assert_eq!(before.status(), Status::Completed);
}

#[test]
fn repeated_runs_reset_epoch_and_copied_observer_snapshots_are_immutable() {
    let world = checked(LIFE);
    let mut runtime = Runtime::new(&world);
    let observer = runtime
        .observe_resources(Config::new(&world, &["Ticket"], Limits::default()).unwrap())
        .unwrap();
    let other = observer.clone();
    assert_eq!(observer.snapshot().unwrap().status(), Status::Idle);
    answer(runtime.run("simple", vec![]));
    let first = observer.snapshot().unwrap();
    answer(runtime.run("simple", vec![]));
    let second = other.snapshot().unwrap();
    assert_closed(&first);
    assert_closed(&second);
    assert_eq!(first.instance(), second.instance());
    assert_eq!(first.run() + 1, second.run());
    assert_ne!(first.events()[0].identity(), second.events()[0].identity());
    assert_eq!(first.events().len(), 2);
    assert_eq!(first.events()[0].identity().run(), first.run());
    assert_eq!(second.events()[0].identity().instance(), second.instance());
    assert!(
        runtime
            .observe_resources(Config::new(&world, &["Ticket"], Limits::default()).unwrap())
            .is_err()
    );
}

#[test]
fn zero_exact_and_total_mint_capacity_preserve_real_prefixes() {
    let world = checked(CAPACITY);
    for optimize in [false, true] {
        for (limits, reason) in [
            (
                Limits {
                    max_resources: 0,
                    ..Limits::default()
                },
                Failure::ResourceLimit,
            ),
            (
                Limits {
                    max_events: 0,
                    ..Limits::default()
                },
                Failure::EventLimit,
            ),
        ] {
            let (result, report) = execute(&world, &["Ticket"], limits, optimize, "simple", vec![]);
            assert!(result.is_err());
            failure(&report, reason);
            assert!(report.events().is_empty());
            assert!(report.live().is_empty());
        }
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits {
                max_events: 1,
                ..Limits::default()
            },
            optimize,
            "simple",
            vec![],
        );
        assert!(result.is_err());
        failure(&report, Failure::EventLimit);
        assert_eq!(actions(&report), [Action::Mint]);
        assert_eq!(report.live().len(), 1);

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits {
                max_resources: 1,
                max_events: 2,
                ..Limits::default()
            },
            optimize,
            "simple",
            vec![],
        );
        result.unwrap();
        assert_closed(&report);
        assert_eq!(actions(&report), [Action::Mint, Action::Consume]);

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits {
                max_resources: 1,
                ..Limits::default()
            },
            optimize,
            "sequential",
            vec![],
        );
        assert!(result.is_err());
        failure(&report, Failure::ResourceLimit);
        assert_eq!(actions(&report), [Action::Mint, Action::Consume]);
        assert!(report.live().is_empty());

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits {
                max_events: 3,
                ..Limits::default()
            },
            optimize,
            "together",
            vec![],
        );
        assert!(result.is_err());
        failure(&report, Failure::EventLimit);
        assert_eq!(actions(&report), [Action::Mint, Action::Mint]);
        assert_eq!(report.live().len(), 2);

        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits {
                max_resources: 2,
                max_events: 4,
                ..Limits::default()
            },
            optimize,
            "together",
            vec![],
        );
        result.unwrap();
        assert_closed(&report);
        assert_eq!(
            actions(&report),
            [Action::Mint, Action::Mint, Action::Consume, Action::Consume]
        );
        assert_eq!(report.events()[2].identity(), report.events()[0].identity());
        assert_eq!(report.events()[3].identity(), report.events()[1].identity());
    }
}

#[test]
fn runtime_errors_and_fuel_cutoffs_keep_prefix_and_residual_without_cleanup() {
    for optimize in [false, true] {
        let world = mutant(
            "error_after_mint",
            "= ticket;",
            "= ".len(),
            "ticket",
            "missing",
        );
        let (result, report) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "error_after_mint",
            vec![],
        );
        assert!(result.is_err());
        assert_eq!(report.status(), Status::RuntimeError);
        assert!(report.failure().is_none());
        assert_eq!(actions(&report), [Action::Mint]);
        assert_eq!(report.live().len(), 1);

        let world = checked(LIFE);
        let (_, complete) = execute(
            &world,
            &["Ticket"],
            Limits::default(),
            optimize,
            "simple",
            vec![],
        );
        let expected = actions(&complete);
        let mut witnessed_live_prefix = false;
        let mut witnessed_completion = false;
        for fuel in 0..=128 {
            let mut runtime = Runtime::new(&world);
            runtime.optimize = optimize;
            runtime.fuel = fuel;
            let observer = runtime
                .observe_resources(Config::new(&world, &["Ticket"], Limits::default()).unwrap())
                .unwrap();
            let result = runtime.run("simple", vec![]);
            let report = observer.snapshot().unwrap();
            if result.is_ok() {
                assert_closed(&report);
                witnessed_completion = true;
                break;
            }
            assert_eq!(report.status(), Status::FuelExhausted);
            assert!(report.failure().is_none());
            assert!(expected.starts_with(&actions(&report)));
            if actions(&report) == [Action::Mint] {
                assert_eq!(report.live().len(), 1);
                witnessed_live_prefix = true;
            }
        }
        assert!(
            witnessed_live_prefix,
            "no actual fuel cut retained a minted owner"
        );
        assert!(
            witnessed_completion,
            "tiny fixture did not finish within finite fuel sweep"
        );
    }
}

#[test]
fn invalid_profiles_and_cross_world_attachment_refuse_before_execution() {
    let world = checked(
        "linear struct Ticket{} copy struct Copy{} affine struct Affine{} linear struct Full{n:i64} linear struct Generic[T]{} linear enum Choice{Only} fn main(){}",
    );
    for (selection, reason) in [
        (vec!["Ticket", "Ticket"], Failure::DuplicateSelection),
        (vec!["Absent"], Failure::UnknownSelectedType),
        (vec!["Copy"], Failure::UnsupportedSelectedType),
        (vec!["Affine"], Failure::UnsupportedSelectedType),
        (vec!["Full"], Failure::UnsupportedSelectedType),
        (vec!["Generic"], Failure::UnsupportedSelectedType),
        (vec!["Choice"], Failure::UnsupportedSelectedType),
    ] {
        let error = Config::new(&world, &selection, Limits::default()).unwrap_err();
        assert_eq!(error.reason(), reason);
    }
    for limits in [
        Limits {
            max_resources: Limits::HARD.max_resources + 1,
            ..Limits::default()
        },
        Limits {
            max_value_depth: Limits::HARD.max_value_depth + 1,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            Config::new(&world, &["Ticket"], limits)
                .unwrap_err()
                .reason(),
            Failure::InvalidLimits
        );
    }
    let config = Config::new(&world, &["Ticket"], Limits::default()).unwrap();
    for (source, reason) in [
        (
            "affine struct Ticket{} fn main(){}",
            Failure::UnsupportedSelectedType,
        ),
        ("fn main(){}", Failure::UnknownSelectedType),
    ] {
        let changed_world = checked(source);
        let mut runtime = Runtime::new(&changed_world);
        assert_eq!(
            runtime
                .observe_resources(config.clone())
                .err()
                .unwrap()
                .reason(),
            reason
        );
        // A refused attachment does not convert ordinary execution to tracing.
        runtime.run("main", vec![]).unwrap();
    }
    let other_valid = checked("linear struct Ticket{} fn main(){let Ticket{}=Ticket{};}");
    let mut runtime = Runtime::new(&other_valid);
    let observer = runtime.observe_resources(config).unwrap();
    runtime.run("main", vec![]).unwrap();
    assert_closed(&observer.snapshot().unwrap());
}

#[test]
fn entry_value_traversal_caps_and_external_refusal_are_observable() {
    let world = checked(LIFE);
    for (limits, entry, args, reason) in [
        (
            Limits {
                max_value_nodes: 0,
                ..Limits::default()
            },
            "echo",
            vec![Value::Int(42)],
            Failure::ValueNodeLimit,
        ),
        (
            Limits {
                max_value_depth: 1,
                ..Limits::default()
            },
            "nested",
            vec![Value::Tuple(vec![Value::Tuple(vec![Value::Int(42)])])],
            Failure::ValueDepthLimit,
        ),
        (
            Limits::default(),
            "echo",
            vec![Value::Function("mint".to_owned())],
            Failure::UnsupportedValue,
        ),
    ] {
        let (result, report) = execute(&world, &["Ticket"], limits, true, entry, args);
        assert!(result.is_err());
        failure(&report, reason);
        assert!(report.events().is_empty());
    }
    let (result, report) = execute(
        &world,
        &["Ticket"],
        Limits {
            max_value_nodes: 1,
            ..Limits::default()
        },
        true,
        "return_pair",
        vec![],
    );
    assert!(result.is_err());
    failure(&report, Failure::ValueNodeLimit);
    assert_eq!(actions(&report), [Action::Mint, Action::Mint]);
    assert_eq!(report.live().len(), 2);
    let mut runtime = Runtime::new(&world);
    let observer = runtime
        .observe_resources(Config::new(&world, &["Ticket"], Limits::default()).unwrap())
        .unwrap();
    assert!(runtime.begin_external("simple", vec![]).is_err());
    failure(&observer.snapshot().unwrap(), Failure::UnsupportedExternal);
}
