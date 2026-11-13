//! §40.3 layout/typestate records and proof badges from audit reports.
use vesper_doc::{
    Document, FunctionDoc, Mode, Pass, State, Transition, Type, TypestateGraph,
    badges::{self, BadgeLevel},
    visual::{self, ENDIAN_BIG, ENDIAN_LITTLE, ENDIAN_NATIVE, ENDIAN_ZERO_PAD, Symbols},
};

const AUDIT: &str = include_str!("fixtures/proof-audit-ledger.json");

fn wire(fields: &str) -> String {
    format!(
        r#"{{"format":"vesper-wire-layouts","version":1,"declarations":[{{"name":"H","fields":[{fields}]}}],"roots":["H"]}}"#
    )
}

#[test]
fn field_span_record_is_exactly_sixteen_little_endian_bytes() {
    let span = visual::WireFieldSpan {
        byte_offset: 0x0102_0304,
        byte_width: 8,
        cache_line_idx: 0x0a0b,
        endian_tag: ENDIAN_BIG,
        is_hot_tile_col: 0,
        field_sym_id: 0x1122_3344,
    };
    assert_eq!(
        span.to_bytes(),
        [
            4, 3, 2, 1, 8, 0, 0, 0, 0x0b, 0x0a, 2, 0, 0x44, 0x33, 0x22, 0x11
        ]
    );
}

#[test]
fn wire_input_compiles_with_the_bridge_checker_and_flattens_nested_structs() {
    let text = include_str!("../examples/package/wire.json");
    let layouts = visual::compile_wire_input(text).unwrap();
    let mut symbols = Symbols::default();
    let d = visual::wire_diagram(&layouts[0], &mut symbols).unwrap();
    assert_eq!((d.name.as_str(), d.bytes), ("Record", 80));
    let named: Vec<_> = d
        .spans
        .iter()
        .map(|s| {
            (
                symbols.name(s.field_sym_id).unwrap(),
                s.byte_offset,
                s.byte_width,
                s.cache_line_idx,
                s.endian_tag,
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            ("header.length", 0, 4, 0, ENDIAN_BIG),
            ("header.api_key", 4, 2, 0, ENDIAN_BIG),
            ("header.api_version", 6, 2, 0, ENDIAN_BIG),
            ("header.correlation_id", 8, 4, 0, ENDIAN_BIG),
            ("header.flags", 12, 1, 0, ENDIAN_NATIVE),
            ("header.reserved", 13, 3, 0, ENDIAN_ZERO_PAD),
            ("key", 16, 16, 0, ENDIAN_NATIVE),
            ("offset", 32, 8, 0, ENDIAN_LITTLE),
            ("payload", 40, 40, 0, ENDIAN_NATIVE),
        ]
    );
    // Contiguous, hole-free coverage of every byte.
    let mut end = 0;
    for s in &d.spans {
        assert_eq!(s.byte_offset, end);
        end += s.byte_width;
    }
    assert_eq!(end, d.bytes);
    let svg = visual::layout_svg(&d, &symbols).unwrap();
    assert!(svg.starts_with("<svg") && svg.contains("L1") && svg.contains("header.length @0+4 be"));
}

#[test]
fn oversized_layouts_omit_only_their_diagram() {
    let mut symbols = Symbols::default();
    let sym = symbols.intern("huge").unwrap();
    let diagram = visual::LayoutDiagram {
        name: "Huge".into(),
        source: visual::LayoutSource::Native,
        bytes: 65 * 64,
        alignment: 8,
        spans: vec![visual::WireFieldSpan {
            byte_offset: 0,
            byte_width: 65 * 64,
            cache_line_idx: 0,
            endian_tag: ENDIAN_NATIVE,
            is_hot_tile_col: 0,
            field_sym_id: sym,
        }],
    };
    assert!(visual::layout_svg(&diagram, &symbols).is_err());
    let visuals = visual::Visuals {
        symbols,
        layouts: vec![diagram],
        ..Default::default()
    };
    let doc = document(&[]);
    let html =
        visual::render_codex_html(&doc, &doc, &visuals, &[], 0, vesper_doc::Limits::default())
            .unwrap();
    let html = String::from_utf8(html).unwrap();
    assert!(html.contains("Diagram omitted: Huge: layout exceeds 64 cache lines"));
    assert!(html.contains("<td>huge</td>"));
}

#[test]
fn wire_input_rejects_implicit_padding_unknown_types_and_wrong_formats() {
    let padded = wire(r#"{"name":"a","type":"u8"},{"name":"b","type":"u32"}"#);
    let error = visual::compile_wire_input(&padded).unwrap_err();
    assert!(error.contains("ZeroPad"), "{error}");
    for bad in [
        wire(r#"{"name":"a","type":"u24be"}"#),
        wire(r#"{"name":"a","type":"u8le"}"#),
        wire(r#"{"name":"a","type":"Missing"}"#),
        wire(r#"{"name":"a","type":"bytes[x]"}"#),
        wire(r#"{"name":"a","type":"u8","extra":1}"#),
        r#"{"format":"other","version":1,"declarations":[],"roots":["H"]}"#.into(),
        r#"{"format":"vesper-wire-layouts","version":1,"declarations":[],"roots":[]}"#.into(),
    ] {
        assert!(visual::compile_wire_input(&bad).is_err(), "{bad}");
    }
}

fn tx() -> Type {
    Type::named("Tx")
}
fn function(name: &str, pass: Pass, resumption: Option<Mode>) -> FunctionDoc {
    serde_json::from_value(serde_json::json!({
        "name": name, "docs": "", "span": {"start":0,"end":0}, "generics": [],
        "parameters": [{"name":"tx","pass":pass,"selector":{"kind":"All"},"ty":tx(),"mode":"Linear"}],
        "result": Type::named("u64"), "declared_result": Type::named("u64"), "result_mode": "Copy",
        "effects": {"instances":[],"tail":null},
        "effect_resumptions": [resumption], "unsupported": []
    }))
    .unwrap_or_else(|e| panic!("FunctionDoc shape changed: {e}"))
}

#[test]
fn typestate_edges_use_section_forty_three_tags_and_terminal_sentinel() {
    let identity = vesper_doc::type_identity(&tx()).unwrap();
    let graph = TypestateGraph {
        states: vec![State {
            identity: identity.clone(),
            ty: tx(),
        }],
        edges: vec![
            Transition {
                from: identity.clone(),
                function: "deposit".into(),
                parameter: 0,
                pass: Pass::Inout,
                to: Some(identity.clone()),
            },
            Transition {
                from: identity.clone(),
                function: "commit".into(),
                parameter: 0,
                pass: Pass::Sink,
                to: None,
            },
        ],
        omitted: vec![],
    };
    let functions = [
        function("deposit", Pass::Inout, None),
        function("commit", Pass::Sink, Some(Mode::Linear)),
    ];
    let mut symbols = Symbols::default();
    let (states, edges) = visual::linear_edges(&graph, &functions, &mut symbols).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(edges[0].pass_mode_tag, visual::PASS_INOUT_STEP);
    assert_eq!(edges[0].to_typestate_opt, states[0]);
    assert!(!edges[0].resumption_is_linear);
    assert_eq!(edges[1].pass_mode_tag, visual::PASS_TERMINAL_SINK);
    assert_eq!(edges[1].to_typestate_opt, 0);
    assert!(edges[1].resumption_is_linear);
    assert_eq!(symbols.name(edges[1].method_sym), Some("commit"));
    let mut borrowed = graph.clone();
    borrowed.edges[0].pass = Pass::In;
    assert!(visual::linear_edges(&borrowed, &functions, &mut symbols).is_err());
}

fn document(items: &[(&str, &str)]) -> Document {
    let functions: Vec<_> = items
        .iter()
        .map(|(name, docs)| {
            let mut f = function(name, Pass::Sink, None);
            f.docs = (*docs).into();
            f
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "schema_version": 1, "source_bytes": 0, "module_docs": "", "doc_comments": [],
        "functions": functions, "types": [], "effects": [], "traits": [], "conformances": [],
        "source_conformances": [], "graph": {"states":[],"edges":[],"omitted":[]},
        "reachability": [], "limitations": []
    }))
    .unwrap()
}

#[test]
fn badges_come_only_from_audit_reports() {
    let audit = badges::parse_audit(AUDIT).unwrap();
    let doc = document(&[
        ("anchored", "Commits.\n@proof(Codex.Ledger.commit_total)"),
        ("missing", "@proof(Codex.Ledger.not_audited)"),
        (
            "plain",
            "No anchor; prose claims of being verified grant nothing.",
        ),
    ]);
    let none = badges::badges(&doc, &[]).unwrap();
    assert!(none.iter().all(|b| b.level == BadgeLevel::Unverified));
    let with = badges::badges(&doc, std::slice::from_ref(&audit)).unwrap();
    assert_eq!(with[0].level, BadgeLevel::Proved);
    assert_eq!(with[0].evidence[0].module, "CodexLedger");
    assert_eq!(with[1].level, BadgeLevel::Unverified);
    assert!(with[1].reason.contains("not in any supplied proof audit"));
    assert_eq!(with[2].level, BadgeLevel::Unverified);

    // Allowed axioms downgrade proved to audited.
    let mut axioms: serde_json::Value = serde_json::from_str(AUDIT).unwrap();
    axioms["allowed_axioms"] = serde_json::json!(["propext"]);
    axioms["theorems"][0]["axioms"] = serde_json::json!(["propext"]);
    let audited = badges::parse_audit(&axioms.to_string()).unwrap();
    assert_eq!(
        badges::badges(&doc, &[audited]).unwrap()[0].level,
        BadgeLevel::Audited
    );

    // A rejected audit grants nothing.
    let mut rejected = axioms.clone();
    rejected["allowed_axioms"] = serde_json::json!([]);
    rejected["forbidden_axioms"] = serde_json::json!(["propext"]);
    rejected["accepted"] = serde_json::json!(false);
    let rejected = badges::parse_audit(&rejected.to_string()).unwrap();
    let b = &badges::badges(&doc, std::slice::from_ref(&rejected)).unwrap()[0];
    assert_eq!(b.level, BadgeLevel::Unverified);
    assert!(b.reason.contains("rejected"));
    // Report order never lets a rejected audit mask accepted evidence.
    for audits in [[rejected.clone(), audit.clone()], [audit.clone(), rejected]] {
        assert_eq!(
            badges::badges(&doc, &audits).unwrap()[0].level,
            BadgeLevel::Proved
        );
    }
}

#[test]
fn forged_or_inconsistent_audit_reports_are_rejected() {
    let base: serde_json::Value = serde_json::from_str(AUDIT).unwrap();
    let mut cases = Vec::new();
    let mut sorry = base.clone();
    sorry["theorems"][0]["axioms"] = serde_json::json!(["sorryAx"]);
    cases.push(sorry); // accepted=true with a forbidden axiom
    let mut allowed_sorry = base.clone();
    allowed_sorry["allowed_axioms"] = serde_json::json!(["sorryAx"]);
    cases.push(allowed_sorry);
    let mut error = base.clone();
    error["status"] = serde_json::json!("error");
    cases.push(error);
    let mut extra = base.clone();
    extra["verified"] = serde_json::json!(true);
    cases.push(extra);
    let mut duplicate = base.clone();
    let theorem = duplicate["theorems"][0].clone();
    duplicate["theorems"].as_array_mut().unwrap().push(theorem);
    cases.push(duplicate);
    for case in cases {
        assert!(badges::parse_audit(&case.to_string()).is_err(), "{case}");
    }
}
