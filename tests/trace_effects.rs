//! Source Beacon (design §22): `Trace` rules, `Telemetrable` coherence and
//! the evaluator's Beacon host. Accepted fixtures list their expected span
//! forest in `// span: ` header lines; rejected ones start with
//! `// expect: E0xxx`.
use std::path::{Path, PathBuf};
use vesper::trace_effects::host::{self, TraceConfig, TraceRunError};
use vesper::trace_effects::{self, PRELUDE_NAMES};
use vesper::{check, parser};
use vesper_beacon::{PrimaryAttribute, parse_traceparent_55};

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/trace_effects")
        .join(kind);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ves"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
}

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    check::check(&p.module)
}

fn codes(c: &check::Checked) -> Vec<&str> {
    c.diagnostics.iter().map(|d| d.code.as_str()).collect()
}

fn expected_tree(source: &str) -> String {
    source
        .lines()
        .take_while(|l| l.starts_with("// span: "))
        .map(|l| format!("{}\n", &l["// span: ".len()..]))
        .collect()
}

#[test]
fn accepted_fixtures_check_and_export_expected_span_trees() {
    let files = fixtures("accept");
    assert_eq!(files.len(), 3);
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let c = checked(&source);
        assert!(
            c.diagnostics.is_empty(),
            "{}: {:?}",
            path.display(),
            c.diagnostics
        );
        let run = host::run_traced(&c, "main", vec![], TraceConfig::default())
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let expected = expected_tree(&source);
        assert!(!expected.is_empty(), "{}", path.display());
        assert_eq!(run.tree(), expected, "{}", path.display());
    }
}

#[test]
fn rejected_fixtures_report_their_typed_diagnostic() {
    let files = fixtures("reject");
    assert_eq!(files.len(), 14);
    for path in files {
        let source = std::fs::read_to_string(&path).unwrap();
        let expected = source
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("// expect: "))
            .expect("rejected fixtures start with `// expect: E0xxx`")
            .trim();
        let c = checked(&source);
        assert!(
            codes(&c).contains(&expected),
            "{}: expected {expected}, got {:?}",
            path.display(),
            c.diagnostics
        );
        for d in &c.diagnostics {
            assert!(
                d.span.end <= source.len() && d.span.start <= d.span.end,
                "{}: {d:?}",
                path.display()
            );
        }
    }
}

#[test]
fn every_new_rule_has_a_rejected_fixture() {
    let mut seen = std::collections::BTreeSet::new();
    for path in fixtures("reject") {
        let source = std::fs::read_to_string(&path).unwrap();
        seen.insert(source.lines().next().unwrap()[11..].trim().to_string());
    }
    for code in [
        "E0309", "E0401", "E0402", "E0750", "E0751", "E0752", "E0753", "E0754", "E0756", "E0757",
        "E0758",
    ] {
        assert!(seen.contains(code), "no fixture for {code}");
    }
}

fn fixture(kind: &str, name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/trace_effects")
            .join(kind)
            .join(name),
    )
    .unwrap()
}

#[test]
fn exported_spans_link_parents_within_one_trace() {
    let c = checked(&fixture("accept", "with_parent.ves"));
    let config = TraceConfig::default();
    let run = host::run_traced(&c, "main", vec![], config).unwrap();
    assert_eq!(run.value.to_string(), "1");
    assert_eq!(run.spans.len(), 4);
    let by_name = |n: &str| run.spans.iter().find(|s| s.name == n).unwrap();
    assert!(run.spans.iter().all(|s| s.trace_id == config.trace_id));
    assert_eq!(by_name("main").parent_span_id, 0);
    assert_eq!(by_name("job").parent_span_id, by_name("main").span_id);
    assert_eq!(by_name("inner").parent_span_id, by_name("job").span_id);
    assert_eq!(by_name("explicit").parent_span_id, by_name("main").span_id);
    // Children finish before parents, and every duration is the logical
    // clock distance between start and finish.
    let finish_order: Vec<_> = run.spans.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(finish_order, ["inner", "job", "explicit", "main"]);
    assert!(run.spans.iter().all(|s| s.duration_ns > 0));
}

#[test]
fn an_imported_traceparent_becomes_the_root_parent() {
    let header = b"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
    let parent = parse_traceparent_55(header).unwrap();
    let c = checked(&fixture("accept", "nested_calls.ves"));
    let config = TraceConfig {
        parent: Some(parent),
        ..TraceConfig::default()
    };
    let run = host::run_traced(&c, "main", vec![], config).unwrap();
    assert_eq!(run.value.to_string(), "4");
    assert!(run.spans.iter().all(|s| s.trace_id == parent.trace_id()));
    let main = run.spans.iter().find(|s| s.name == "main").unwrap();
    assert_eq!(main.parent_span_id, parent.span_id());
    assert_eq!(main.flags, parent.trace_flags());
    // The exported tree still has `main` as its root.
    assert_eq!(
        run.tree(),
        expected_tree(&fixture("accept", "nested_calls.ves"))
    );
}

#[test]
fn changing_a_secret_does_not_change_the_exported_spans() {
    let source = fixture("accept", "telemetry.ves");
    let other = source.replace("value: 9", "value: 1234567");
    assert_ne!(source, other);
    let a = host::run_traced(&checked(&source), "main", vec![], TraceConfig::default()).unwrap();
    let b = host::run_traced(&checked(&other), "main", vec![], TraceConfig::default()).unwrap();
    assert_eq!(a.spans, b.spans);
    assert!(matches!(
        a.spans[1].attribute,
        PrimaryAttribute::Redacted(_)
    ));
}

#[test]
fn host_rejects_out_of_vocabulary_operations_with_typed_errors() {
    let run = |body: &str| {
        let c = checked(&format!("fn main() -> u64 eff [Trace] {{ {body} }}"));
        assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
        host::run_traced(&c, "main", vec![], TraceConfig::default())
    };
    assert_eq!(
        run("let s = Trace::start_span(9, \"x\"); Trace::emit_ok(s, 0); 0").unwrap_err(),
        TraceRunError::UnknownSchema(9)
    );
    assert_eq!(
        run("let s = Trace::start_span(0, \"x\"); Trace::emit_err(s, 8, \"m\"); 0").unwrap_err(),
        TraceRunError::UnknownErrorCode(8)
    );
    let bad_parent = "let p = SpanContext { trace_id_hi: 0, trace_id_lo: 0, span_id: 5, flags: 0 }; \
        let s = Trace::start_child(p, 0, \"x\"); Trace::emit_ok(s, 0); 0";
    assert_eq!(run(bad_parent).unwrap_err(), TraceRunError::InvalidContext);
    assert!(run("let s = Trace::start_span(0, \"x\"); Trace::emit_ok(s, 0); 0").is_ok());
}

#[test]
fn a_finished_span_cannot_be_finished_again_through_a_handler() {
    // A test handler that re-sends the same span to the host twice: the
    // checker allows minting in handler clauses, the host refuses a span it
    // does not hold open.
    let source = "fn twice() -> u64 eff [Trace] {
        let parent = Trace::current_ctx();
        handle Trace in {
            let s = Trace::start_span(0, \"a\");
            Trace::emit_ok(s, 1);
            0
        } with {
            return v => v,
            current_ctx(resume) => resume(parent),
            start_span(schema, name, resume) => resume(Trace::start_child(parent, schema, name)),
            start_child(p, schema, name, resume) => resume(Trace::start_child(p, schema, name)),
            emit_ok(span, attr, resume) => {
                let ActiveSpan { ctx, parent_span_id, start_mono_ns, schema_id } = span;
                Trace::emit_ok(ActiveSpan { ctx: ctx, parent_span_id: parent_span_id, start_mono_ns: start_mono_ns, schema_id: schema_id }, attr);
                Trace::emit_ok(ActiveSpan { ctx: ctx, parent_span_id: parent_span_id, start_mono_ns: start_mono_ns, schema_id: schema_id }, attr);
                resume(())
            },
            emit_err(span, code, msg, resume) => { Trace::emit_err(span, code, msg); resume(()) },
            emit_attr(span, attr, resume) => { Trace::emit_attr(span, attr); resume(()) },
        }
    }
    fn main() -> u64 eff [Trace] { twice() }";
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let err = host::run_traced(&c, "main", vec![], TraceConfig::default()).unwrap_err();
    assert!(matches!(err, TraceRunError::UnknownSpan(_)), "{err:?}");
}

#[test]
fn traced_entries_must_carry_exactly_trace() {
    let c = checked(
        "fn main() -> u64 { 0 } fn t() -> u64 eff [Trace] { let x = Trace::current_ctx(); x.span_id }",
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert!(matches!(
        host::run_traced(&c, "main", vec![], TraceConfig::default()),
        Err(TraceRunError::Entry(_))
    ));
    let run = host::run_traced(&c, "t", vec![], TraceConfig::default()).unwrap();
    assert_eq!(run.value.to_string(), "0");
    assert!(run.spans.is_empty());
    assert!(matches!(
        host::run_traced(
            &checked("fn main() -> u64 { 0 }"),
            "main",
            vec![],
            TraceConfig::default()
        ),
        Err(TraceRunError::NotBeacon)
    ));
}

#[test]
fn the_prelude_is_added_only_for_undeclared_beacon_names() {
    let plain = parser::parse("fn main() -> u64 { 0 }").module;
    assert!(!trace_effects::with_prelude(&plain).active());
    let uses = parser::parse("fn main() -> u64 eff [Trace] { 0 }").module;
    let beacon = trace_effects::with_prelude(&uses);
    assert!(beacon.active());
    assert_eq!(beacon.module().items.len(), uses.items.len() + 4);
    // A module that declares its own Trace keeps it, and no Beacon rule or
    // host applies.
    let own = "linear effect Trace { fn tick() -> u64; }
        fn main() -> u64 eff [Trace] { Trace::tick() }";
    let module = parser::parse(own).module;
    assert!(!trace_effects::with_prelude(&module).active());
    let c = checked(own);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert!(!trace_effects::active(&c.world));
    assert_eq!(
        host::run_traced(&c, "main", vec![], TraceConfig::default()).unwrap_err(),
        TraceRunError::NotBeacon
    );
    assert_eq!(PRELUDE_NAMES.len(), 5);
}

#[test]
fn telemetrable_without_beacon_declarations_gets_the_trait() {
    let c = checked(
        "copy struct P { x: u64 }
         impl Telemetrable for P { fn telemetry_attr(in self) -> TelemetryAttr { TelemetryAttr::Bytes(self.x) } }
         fn main() -> u64 eff [Trace] {
             let p = P { x: 3 };
             let s = Trace::start_span(2, \"p\");
             Trace::emit_attr(s, p.telemetry_attr());
             0
         }",
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let run = host::run_traced(&c, "main", vec![], TraceConfig::default()).unwrap();
    assert_eq!(run.tree(), "p [background] ok bytes=3\n");
}

#[test]
fn generic_and_blanket_escapes_are_rejected() {
    let c = checked(
        "fn id[T](sink x: T) -> T { x }
         fn main() -> u64 eff [Trace] { let s = id(Trace::start_span(0, \"a\")); Trace::emit_ok(s, 0); 0 }",
    );
    assert!(codes(&c).contains(&"E0750"), "{:?}", c.diagnostics);
    let c = checked(
        "impl [T] Telemetrable for T { fn telemetry_attr(in self) -> TelemetryAttr { TelemetryAttr::NoAttr } }
         fn main() -> u64 { 0 }",
    );
    assert!(codes(&c).contains(&"E0757"), "{:?}", c.diagnostics);
}

#[test]
fn context_rules_accept_propagation_and_reject_drops() {
    // Passing a context on, or reading it, is propagation.
    let ok = checked(
        "fn f(sink ctx: SpanContext) -> u64 eff [Trace] { ctx.span_id }
         fn main() -> u64 eff [Trace] { let c = Trace::current_ctx(); f(c) }",
    );
    assert!(ok.diagnostics.is_empty(), "{:?}", ok.diagnostics);
    for body in [
        "let _ = Trace::current_ctx(); 0",
        "let c = Trace::current_ctx(); 0",
        "Trace::current_ctx(); 0",
    ] {
        let c = checked(&format!("fn main() -> u64 eff [Trace] {{ {body} }}"));
        assert_eq!(codes(&c), ["E0753"], "{body}: {:?}", c.diagnostics);
    }
    // A top-level Trace handler (no enclosing trace) may start a root.
    let root = checked(
        "fn main() -> u64 {
            handle Trace in { 0 } with {
                return v => v,
                current_ctx(resume) => resume(SpanContext { trace_id_hi: 0, trace_id_lo: 1, span_id: 0, flags: 0 }),
                start_span(schema, name, resume) => resume(ActiveSpan { ctx: SpanContext { trace_id_hi: 0, trace_id_lo: 1, span_id: 1, flags: 0 }, parent_span_id: 0, start_mono_ns: 0, schema_id: schema }),
                start_child(p, schema, name, resume) => resume(ActiveSpan { ctx: p, parent_span_id: p.span_id, start_mono_ns: 0, schema_id: schema }),
                emit_ok(span, attr, resume) => { let ActiveSpan { ctx, parent_span_id, start_mono_ns, schema_id } = span; resume(()) },
                emit_err(span, code, msg, resume) => { let ActiveSpan { ctx, parent_span_id, start_mono_ns, schema_id } = span; resume(()) },
                emit_attr(span, attr, resume) => { let ActiveSpan { ctx, parent_span_id, start_mono_ns, schema_id } = span; resume(()) },
            }
        }",
    );
    assert!(root.diagnostics.is_empty(), "{:?}", root.diagnostics);
}

#[test]
fn a_handler_that_swallows_a_host_span_is_reported_unclosed() {
    // The clause may destructure the span (handlers implement Trace), but
    // the host started it and never sees it finish.
    let source = "fn swallow() -> u64 eff [Trace] {
        let parent = Trace::current_ctx();
        handle Trace in {
            let s = Trace::start_span(0, \"a\");
            Trace::emit_ok(s, 1);
            0
        } with {
            return v => v,
            current_ctx(resume) => resume(parent),
            start_span(schema, name, resume) => resume(Trace::start_child(parent, schema, name)),
            start_child(p, schema, name, resume) => resume(Trace::start_child(p, schema, name)),
            emit_ok(span, attr, resume) => {
                let ActiveSpan { ctx, parent_span_id, start_mono_ns, schema_id } = span;
                resume(())
            },
            emit_err(span, code, msg, resume) => { Trace::emit_err(span, code, msg); resume(()) },
            emit_attr(span, attr, resume) => { Trace::emit_attr(span, attr); resume(()) },
        }
    }
    fn main() -> u64 eff [Trace] { swallow() }";
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    assert_eq!(
        host::run_traced(&c, "main", vec![], TraceConfig::default()).unwrap_err(),
        TraceRunError::UnclosedSpans(1)
    );
}
