use vesper_doc::{links::*, *};

fn target(path: &str, kind: TargetKind) -> LinkTarget {
    LinkTarget {
        path: path.into(),
        kind,
    }
}
fn function(name: &str) -> FunctionDoc {
    FunctionDoc {
        name: name.into(),
        docs: String::new(),
        span: 0..0,
        generics: vec![],
        parameters: vec![],
        result: Type::unit(),
        declared_result: Type::unit(),
        result_mode: Mode::Copy,
        effects: Effects::default(),
        effect_resumptions: vec![],
        unsupported: vec![],
    }
}
fn document(source: &str) -> Document {
    Document {
        schema_version: 1,
        source_bytes: source.len(),
        module_docs: String::new(),
        doc_comments: doc_comments(source),
        functions: vec![function("main")],
        types: vec![],
        effects: vec![],
        traits: vec![],
        conformances: vec![],
        source_conformances: vec![],
        graph: TypestateGraph::default(),
        reachability: vec![],
        limitations: vec![],
        intra_doc_links: LinkReport::default(),
    }
}
fn analyzed(source: &str) -> Document {
    let mut doc = document(source);
    let targets = document_targets(&doc, LinkLimits::default()).unwrap();
    doc.intra_doc_links = analyze(source, &targets, LinkLimits::default()).unwrap();
    assert_eq!(doc.intra_doc_links.status, LinkStatus::Resolved);
    doc
}

#[test]
fn exact_utf8_crlf_repeated_and_backticked_occurrences_keep_original_byte_spans() {
    let source = "//! 😀 [main], [`main`], [main]\r\nfn main(){}\r\n";
    let report = analyze(
        source,
        &[target("main", TargetKind::Function)],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(report.status, LinkStatus::Resolved);
    assert_eq!(report.source_bytes, source.len());
    assert_eq!(report.references.len(), 3);
    let mut end = 0;
    for (reference, expected) in report
        .references
        .iter()
        .zip(["[main]", "[`main`]", "[main]"])
    {
        assert!(reference.span.start >= end);
        assert_eq!(&source[reference.span.clone()], expected);
        assert_eq!(
            &source[reference.span.start + 1..reference.span.end - 1],
            reference.display
        );
        assert_eq!(reference.anchor, anchor_id(TargetKind::Function, "main"));
        end = reference.span.end;
    }
    validate_report(
        &report,
        &[target("main", TargetKind::Function)],
        LinkLimits::default(),
    )
    .unwrap();
}

#[test]
fn only_real_doc_tokens_participate_and_prose_stays_literal() {
    let source = "// [Missing]\n//// [Missing]\n/* /// [Missing] */\nfn main(){let text=\"/// [Missing]\";}\n//! [T-CallProj], [two words], [], [42], [main]\n";
    let report = analyze(
        source,
        &[target("main", TargetKind::Function)],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(report.status, LinkStatus::Resolved);
    assert_eq!(report.references.len(), 1);
}

#[test]
fn matching_inline_runs_escapes_images_and_explicit_markdown_are_not_references() {
    let source = r#"//! \[Missing], `a [Missing] b`, `` a ` [Missing] b ` ``, ![Missing](url), ![Missing]
//! [Missing](https://example.invalid/a([Missing]))
//! [Missing][id]
//! [id]: https://example.invalid
//! [main]
fn main(){}
"#;
    let report = analyze(
        source,
        &[target("main", TargetKind::Function)],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(
        report.status,
        LinkStatus::Resolved,
        "{:?}",
        report.diagnostics
    );
    assert_eq!(report.references.len(), 1);
}

#[test]
fn multiline_markdown_destinations_titles_labels_and_reference_style_skip_inner_brackets() {
    let source = r#"//! [guide](https://example.invalid/ "
//! [Missing]) [Missing]")
//! [guide](<https://example.invalid/([Missing])>
//! "quoted ) [Missing]")
//! [multi [Missing]
//! line [Missing]](url)
//! [guide]
//! [reference]
//! [main]
fn main(){}
"#;
    let report = analyze(
        source,
        &[target("main", TargetKind::Function)],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(
        report.status,
        LinkStatus::Resolved,
        "{:?}",
        report.diagnostics
    );
    assert_eq!(report.references.len(), 1);
    assert_eq!(report.references[0].display, "main");
}

#[test]
fn apostrophes_inside_bare_uris_do_not_hide_following_real_unknown_links() {
    for tail in [" [Missing]\n", "\n//! [Missing]\n"] {
        let source = format!("//! [manual](https://example.invalid/o'reilly){tail}fn main(){{}}\n");
        let report = analyze(&source, &[], LinkLimits::default()).unwrap();
        assert_eq!(report.status, LinkStatus::Rejected);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].code, "unknown");
        assert_eq!(&source[report.diagnostics[0].span.clone()], "[Missing]");
    }
}

#[test]
fn code_spans_inside_explicit_link_and_image_labels_protect_their_closing_brackets() {
    for prefix in ["", "!"] {
        let source = format!(
            "//! {prefix}[outer `]` [Missing]](https://example.invalid/) [main]\nfn main(){{}}\n"
        );
        let report = analyze(
            &source,
            &[target("main", TargetKind::Function)],
            LinkLimits::default(),
        )
        .unwrap();
        assert_eq!(
            report.status,
            LinkStatus::Resolved,
            "{:?}",
            report.diagnostics
        );
        assert_eq!(report.references.len(), 1);
        assert_eq!(report.references[0].display, "main");
    }
}

#[test]
fn both_fence_kinds_and_multiline_inline_code_obey_contiguous_doc_groups() {
    let source = "//! ```vesper\n//! [Missing]\n//! ````\n//! ~~~~text\n//! [Missing]\n//! ~~~~\n//! `cross\n//! [Missing] line` [main]\nfn main(){}\n";
    let report = analyze(
        source,
        &[target("main", TargetKind::Function)],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(report.status, LinkStatus::Resolved);
    assert_eq!(report.references.len(), 1);
    for break_text in ["\n\n", "\n// ordinary\n", "\nfn other(){}\n"] {
        let source = format!("//! ```{break_text}//! [main]\nfn main(){{}}\n");
        let report = analyze(
            &source,
            &[target("main", TargetKind::Function)],
            LinkLimits::default(),
        )
        .unwrap();
        assert_eq!(report.status, LinkStatus::Resolved);
        assert_eq!(report.references.len(), 1, "{break_text:?}");
    }
}

#[test]
fn unmatched_inline_delimiter_is_literal_and_does_not_hide_unknown_reference() {
    let report = analyze(
        "//! `unclosed [Missing]\nfn main(){}",
        &[],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(report.status, LinkStatus::Rejected);
    assert_eq!(report.diagnostics[0].code, "unknown");
}

#[test]
fn genuine_same_path_namespaces_are_ambiguous_without_namespace_guessing() {
    let source = "//! [Channel], [Absent], [Self::open], [@pkg/type::open], [Vec[T]]\nfn main(){}";
    let targets = [
        target("Channel", TargetKind::Type),
        target("Channel", TargetKind::Effect),
    ];
    let report = analyze(source, &targets, LinkLimits::default()).unwrap();
    assert_eq!(report.status, LinkStatus::Rejected);
    assert_eq!(
        report
            .diagnostics
            .iter()
            .map(|d| d.code.as_str())
            .collect::<Vec<_>>(),
        [
            "ambiguous",
            "unknown",
            "unsupported",
            "unsupported",
            "unsupported"
        ]
    );
    for diagnostic in &report.diagnostics {
        assert!(source[diagnostic.span.clone()].starts_with('['));
        assert!(source[diagnostic.span.clone()].ends_with(']'));
    }
    validate_report(&report, &targets, LinkLimits::default()).unwrap();
}

#[test]
fn target_inventory_is_deterministic_and_duplicate_descriptors_are_invalid() {
    let source = "//! [z], [a]\n";
    let targets = [
        target("z", TargetKind::Function),
        target("a", TargetKind::Function),
    ];
    let a = analyze(source, &targets, LinkLimits::default()).unwrap();
    let b = analyze(
        source,
        &[targets[1].clone(), targets[0].clone()],
        LinkLimits::default(),
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a.targets[0].path, "a");
    assert_eq!(
        analyze(
            source,
            &[targets[0].clone(), targets[0].clone()],
            LinkLimits::default()
        )
        .unwrap_err()
        .code,
        "invalid"
    );
}

#[test]
fn every_limit_rejects_zero_or_above_hard_before_analysis() {
    let h = LinkLimits::HARD;
    for mutate in [
        |l: &mut LinkLimits, n| l.max_source_bytes = n,
        |l: &mut LinkLimits, n| l.max_targets = n,
        |l: &mut LinkLimits, n| l.max_references = n,
        |l: &mut LinkLimits, n| l.max_path_bytes = n,
        |l: &mut LinkLimits, n| l.max_path_segments = n,
        |l: &mut LinkLimits, n| l.max_metadata_bytes = n,
        |l: &mut LinkLimits, n| l.max_work = n,
    ] {
        let mut zero = LinkLimits::default();
        mutate(&mut zero, 0);
        assert_eq!(analyze("", &[], zero).unwrap_err().code, "configuration");
        let mut excessive = h;
        mutate(&mut excessive, usize::MAX);
        assert_eq!(
            analyze("", &[], excessive).unwrap_err().code,
            "configuration"
        );
    }
}

#[test]
fn declared_source_target_reference_path_segment_metadata_and_work_limits_refuse() {
    let defaults = LinkLimits::default();
    let cases = [
        LinkLimits {
            max_source_bytes: 1,
            ..defaults
        },
        LinkLimits {
            max_targets: 1,
            ..defaults
        },
        LinkLimits {
            max_references: 1,
            ..defaults
        },
        LinkLimits {
            max_path_bytes: 1,
            ..defaults
        },
        LinkLimits {
            max_path_segments: 1,
            ..defaults
        },
        LinkLimits {
            max_metadata_bytes: 64,
            ..defaults
        },
        LinkLimits {
            max_work: 1,
            ..defaults
        },
    ];
    let source = "//! [Tx::close], [main]\nfn main(){}";
    let targets = [
        target("Tx::close", TargetKind::InherentMethod),
        target("main", TargetKind::Function),
    ];
    for limits in cases {
        assert_eq!(
            analyze(source, &targets, limits).unwrap_err().code,
            "resource",
            "{limits:?}"
        );
    }
    let report = analyze(
        "//! [WayTooLong]\n",
        &[],
        LinkLimits {
            max_path_bytes: 3,
            ..defaults
        },
    );
    assert_eq!(report.unwrap_err().code, "resource");
    let report = analyze(
        "//! [A::B::C]\n",
        &[],
        LinkLimits {
            max_path_segments: 2,
            ..defaults
        },
    );
    assert_eq!(report.unwrap_err().code, "resource");
}

#[test]
fn lexer_failure_and_dense_token_resource_refusal_are_explicit() {
    assert_eq!(
        analyze("fn main(){§}", &[], LinkLimits::default())
            .unwrap_err()
            .code,
        "lexical"
    );
    let source = "{}".repeat(10_000);
    assert_eq!(
        analyze(&source, &[], LinkLimits::default())
            .unwrap_err()
            .code,
        "resource"
    );
}

#[test]
fn source_methods_on_builtin_owners_and_nested_effect_operations_have_real_targets() {
    let mut doc = document("fn main(){}");
    doc.functions.push(function("i64::value"));
    doc.effects.push(EffectDoc {
        name: "Clock".into(),
        docs: String::new(),
        generics: vec![],
        linear: false,
        operations: vec![function("read"), function("Clock::tick")],
    });
    let targets = document_targets(&doc, LinkLimits::default()).unwrap();
    assert!(targets.contains(&target("i64::value", TargetKind::InherentMethod)));
    assert!(targets.contains(&target("Clock::read", TargetKind::EffectOperation)));
    assert!(targets.contains(&target("Clock::tick", TargetKind::EffectOperation)));
    assert!(!targets.iter().any(|t| t.path == "i64"));
}

#[test]
fn anchors_encode_entire_utf8_bytes_and_never_accept_url_syntax() {
    assert_eq!(
        anchor_id(TargetKind::Type, "é<\""),
        "vesper-doc-type-c3a93c22"
    );
    assert_ne!(
        anchor_id(TargetKind::Type, "Same"),
        anchor_id(TargetKind::Effect, "Same")
    );
    assert_eq!(
        analyze(
            "",
            &[target("javascript:alert", TargetKind::Function)],
            LinkLimits::default()
        )
        .unwrap_err()
        .code,
        "invalid"
    );
}

#[test]
fn typed_html_has_one_actual_anchor_for_every_link_and_escapes_all_other_text() {
    let doc = analyzed("//! <script> '& [main], [`main`] 😀\nfn main(){}");
    let html = String::from_utf8(doc.html(Limits::default()).unwrap()).unwrap();
    let id = anchor_id(TargetKind::Function, "main");
    assert_eq!(html.matches(&format!("id=\"{id}\"")).count(), 1);
    assert_eq!(html.matches(&format!("href=\"#{id}\"")).count(), 2);
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt; &#39;&amp;"));
    assert!(html.contains("unverified"));
    assert_eq!(
        doc.html(Limits::default()).unwrap(),
        doc.html(Limits::default()).unwrap()
    );
}

#[test]
fn required_typed_anchor_section_survives_deep_json_overview_refusal() {
    let doc = analyzed("//! [main]\nfn main(){}");
    let mut deep = serde_json::Value::Null;
    for _ in 0..150 {
        deep = serde_json::json!({"child":deep});
    }
    let html =
        String::from_utf8(render_document_html(&doc, &deep, Limits::default()).unwrap()).unwrap();
    let id = anchor_id(TargetKind::Function, "main");
    assert_eq!(html.matches(&format!("id=\"{id}\"")).count(), 1);
    assert_eq!(html.matches(&format!("href=\"#{id}\"")).count(), 1);
}

#[test]
fn typed_renderer_refuses_modified_anchor_target_display_spans_and_source_identity() {
    let doc = analyzed("//! 😀 [main]\nfn main(){}");
    let mut variants = vec![];
    let mut changed = doc.clone();
    changed.intra_doc_links.references[0].anchor = "javascript:alert(1)".into();
    variants.push(changed);
    let mut changed = doc.clone();
    changed.intra_doc_links.references[0].target.path = "other".into();
    variants.push(changed);
    let mut changed = doc.clone();
    changed.intra_doc_links.references[0].display = "other".into();
    variants.push(changed);
    let mut changed = doc.clone();
    changed.intra_doc_links.references[0].span = 5..11;
    variants.push(changed);
    let mut changed = doc.clone();
    changed.intra_doc_links.source_bytes += 1;
    variants.push(changed);
    let mut changed = doc.clone();
    changed
        .intra_doc_links
        .targets
        .push(target("other", TargetKind::Function));
    variants.push(changed);
    let mut changed = doc.clone();
    changed.functions.clear();
    variants.push(changed);
    let mut changed = doc.clone();
    changed.doc_comments[0].text = "😀 [none]".into();
    variants.push(changed);
    for changed in variants {
        assert!(changed.html(Limits::default()).is_err());
    }
}

#[test]
fn malformed_public_comment_metadata_cannot_panic_or_publish_links() {
    let doc = analyzed("//! [main]\nfn main(){}");
    for span in [
        usize::MAX..usize::MAX,
        Span {
            start: 100,
            end: 99,
        },
        0..1,
        0..usize::MAX,
    ] {
        let mut changed = doc.clone();
        changed.doc_comments[0].span = span;
        assert!(changed.html(Limits::default()).is_err());
    }
    let mut changed = doc.clone();
    changed.doc_comments.push(changed.doc_comments[0].clone());
    assert!(changed.html(Limits::default()).is_err());
}

#[test]
fn rejected_or_nonempty_not_analyzed_metadata_refuses_typed_rendering() {
    let mut doc = document("//! [Missing]\nfn main(){}");
    doc.intra_doc_links =
        analyze("//! [Missing]\nfn main(){}", &[], LinkLimits::default()).unwrap();
    assert_eq!(doc.intra_doc_links.status, LinkStatus::Rejected);
    assert!(doc.html(Limits::default()).is_err());
    doc.intra_doc_links = LinkReport::default();
    doc.intra_doc_links.source_bytes = 1;
    assert!(doc.html(Limits::default()).is_err());
}

#[test]
fn legacy_escaped_rendering_remains_available_without_analyzed_links() {
    let doc = document("//! [main] <img>\nfn main(){}");
    let html = String::from_utf8(doc.html(Limits::default()).unwrap()).unwrap();
    assert!(!html.contains("href="));
    assert!(!html.contains("<img>"));
    assert_eq!(
        doc.html(Limits::default()).unwrap(),
        render_html(&doc, Limits::default()).unwrap()
    );
    let mut json = serde_json::to_value(&doc).unwrap();
    json.as_object_mut().unwrap().remove("intra_doc_links");
    assert_eq!(
        serde_json::from_value::<Document>(json)
            .unwrap()
            .intra_doc_links,
        LinkReport::default()
    );
}

#[test]
fn link_dtos_deny_unknown_fields_and_typed_rendering_retains_output_budget() {
    let doc = analyzed("//! [main]\nfn main(){}");
    let mut json = serde_json::to_value(&doc.intra_doc_links).unwrap();
    json["checked"] = serde_json::json!(true);
    assert!(serde_json::from_value::<LinkReport>(json).is_err());
    let mut json = serde_json::to_value(&doc.intra_doc_links.references[0]).unwrap();
    json["approved"] = serde_json::json!(true);
    assert!(serde_json::from_value::<LinkReference>(json).is_err());
    assert!(
        doc.html(Limits {
            max_output_bytes: 16,
            ..Limits::default()
        })
        .is_err()
    );
}
