use serde_json::Value as Json;
use std::sync::Arc;
use vesper::{
    check, interface,
    lexer::{self, TokenKind},
    lsp::actions::*,
    parser,
    tooling::lint::ConfigProvenance,
};
use vesper_lint::policy::{Policy, PolicyLimits, Rule, Severity};
use vesper_span::{ByteSpan, Limits, LineIndex, Position, Range};
const URI: &str = "untitled:actions/🦀";
fn index(source: &str) -> LineIndex {
    LineIndex::new(Arc::from(source), Limits::default()).unwrap()
}
fn range(index: &LineIndex, start: usize, end: usize) -> Range {
    index.range(ByteSpan { start, end }).unwrap()
}
fn selection(index: &LineIndex, needle: &str) -> Range {
    let start = index.text().find(needle).unwrap();
    range(index, start, start + needle.len())
}
fn apply_json(source: &str, version: i32, edit: &Json) -> String {
    let changes = edit["documentChanges"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["textDocument"]["uri"], URI);
    assert_eq!(changes[0]["textDocument"]["version"], version);
    let edits = changes[0]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    let decode = |p: &Json| Position {
        line: p["line"].as_u64().unwrap().try_into().unwrap(),
        character: p["character"].as_u64().unwrap().try_into().unwrap(),
    };
    let r = Range {
        start: decode(&edits[0]["range"]["start"]),
        end: decode(&edits[0]["range"]["end"]),
    };
    let bytes = index(source).span(r).unwrap();
    let mut out = source.to_owned();
    out.replace_range(
        bytes.start..bytes.end,
        edits[0]["newText"].as_str().unwrap(),
    );
    out
}
fn checked(source: &str) -> check::Checked {
    let parsed = parser::parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let result = check::check(&parsed.module);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    result
}
fn compatible(before: &str, after: &str) {
    let before = interface::export(&checked(before).world, "@tests/actions").unwrap();
    let after = interface::export(&checked(after).world, "@tests/actions").unwrap();
    let report = vesper_semver::compare(&before, &after);
    assert!(report.is_compatible(), "{report:?}");
}
fn evidence(source: &str) -> Vec<(TokenKind, String)> {
    let (tokens, errors) = lexer::lex(source);
    assert!(errors.is_empty());
    tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Eof))
        .map(|t| (t.kind, source[t.span].to_owned()))
        .collect()
}
const TWO: &str = "// Unicode 🦀 é\neffect Audit{fn event();}\nfn selected(x:u64)->u64 eff[/* keep 🦀 */ Audit]{let x=x;{let x=x;x}}\nfn other()->u64 eff[Audit]{42}\n";
#[test]
fn selected_quick_fix_is_versioned_utf16_and_leaves_other_function_unchanged() {
    let idx = index(TWO);
    let doc = ActionDocument::new(URI, 7, TWO, &idx).unwrap();
    let selection = selection(&idx, "Audit]{let");
    let actions = code_actions(
        &doc,
        doc.stamp(),
        selection,
        ActionRequest::QuickFix,
        &Policy::default(),
        ConfigProvenance::Default,
    )
    .unwrap();
    assert_eq!(actions.len(), 1);
    let wire = actions[0].to_json();
    assert_eq!(wire["kind"], "quickfix");
    let after = apply_json(TWO, 7, &wire["edit"]);
    assert!(after.contains("fn other()->u64 eff[Audit]{42}"));
    assert!(after.contains("/* keep 🦀 */"));
    assert!(after.contains("let x=x;{let x=x;x}"));
    assert!(!after.contains("Audit]{let"));
    compatible(TWO, &after);
    assert_eq!(
        wire["data"]["document"]["source_blake3"],
        blake3::hash(TWO.as_bytes()).to_hex().to_string()
    );
    assert_eq!(
        wire["data"]["candidate_blake3"],
        blake3::hash(after.as_bytes()).to_hex().to_string()
    );
    assert!(wire["data"]["semver"]["classification"].is_string());
    assert_eq!(wire["data"]["policy"]["provenance"]["kind"], "default");
    let remaining = vesper::tooling::lint::prepare_source(&after, false, false).unwrap();
    assert_eq!(remaining.report.diagnostics.len(), 1);
    assert_eq!(remaining.report.diagnostics[0].finding.function, "other");
}
#[test]
fn explicit_fix_all_has_separate_kind_and_removes_both_only_after_actual_checks() {
    let idx = index(TWO);
    let doc = ActionDocument::new(URI, 8, TWO, &idx).unwrap();
    let actions = code_actions(
        &doc,
        doc.stamp(),
        selection(&idx, "Audit]{let"),
        ActionRequest::FixAllUnusedEffects,
        &Policy::default(),
        ConfigProvenance::Default,
    )
    .unwrap();
    assert_eq!(actions.len(), 1);
    let wire = actions[0].to_json();
    assert_eq!(wire["kind"], "source.fixAll.vesper");
    let after = apply_json(TWO, 8, &wire["edit"]);
    compatible(TWO, &after);
    assert!(!after.contains("eff[Audit]"));
    assert!(after.contains("/* keep 🦀 */"));
    assert!(
        vesper::tooling::lint::prepare_source(&after, false, false)
            .unwrap()
            .report
            .diagnostics
            .is_empty()
    );
}
#[test]
fn canonical_formatting_conserves_comments_tokens_unicode_and_is_idempotent() {
    let source = "// Unicode 🦀 é\r\nfn main( )->i64{/* keep */let x=1;// next\n x}";
    let idx = index(source);
    let doc = ActionDocument::new(URI, 3, source, &idx).unwrap();
    let formatted = format_document(&doc, doc.stamp()).unwrap();
    assert_eq!(formatted.edits.len(), 1);
    assert_eq!(formatted.edits[0].range, range(&idx, 0, source.len()));
    let after = apply_json(source, 3, &formatted.workspace_edit_json());
    assert_eq!(evidence(source), evidence(&after));
    assert!(after.contains("let x = 1;"));
    assert!(after.lines().any(|line| line.starts_with("    ")));
    assert_eq!(formatted.to_json().as_array().unwrap().len(), 1);
    checked(&after);
    let next = index(&after);
    let doc = ActionDocument::new(URI, 4, &after, &next).unwrap();
    assert!(format_document(&doc, doc.stamp()).unwrap().edits.is_empty());
    assert_eq!(formatted.provenance["indent_spaces"], 4);
    assert_eq!(formatted.provenance["line_width"], 100);
}
#[test]
fn stale_versions_source_hashes_and_mismatched_indexes_never_offer_edits() {
    let idx = index(TWO);
    let doc = ActionDocument::new(URI, 9, TWO, &idx).unwrap();
    let stamp = doc.stamp();
    for stale in [
        SourceStamp {
            version: 8,
            ..stamp
        },
        SourceStamp {
            source_hash: [0; 32],
            ..stamp
        },
    ] {
        assert_eq!(
            format_document(&doc, stale).unwrap_err(),
            ActionError::StaleDocument
        );
        assert_eq!(
            code_actions(
                &doc,
                stale,
                selection(&idx, "Audit]{let"),
                ActionRequest::QuickFix,
                &Policy::default(),
                ConfigProvenance::Default
            )
            .unwrap_err(),
            ActionError::StaleDocument
        );
    }
    assert!(matches!(
        ActionDocument::new(URI, 9, "different", &idx),
        Err(ActionError::InvalidDocument(_))
    ));
    assert!(matches!(
        ActionDocument::new("", 9, TWO, &idx),
        Err(ActionError::InvalidDocument(_))
    ));
}
#[test]
fn strict_ranges_reject_surrogate_interiors_reversed_and_nonexistent_positions() {
    let idx = index(TWO);
    let doc = ActionDocument::new(URI, 1, TWO, &idx).unwrap();
    let bads = [
        Range {
            start: Position {
                line: 0,
                character: 12,
            },
            end: Position {
                line: 0,
                character: 12,
            },
        },
        Range {
            start: Position {
                line: 3,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 0,
            },
        },
        Range {
            start: Position {
                line: 999,
                character: 0,
            },
            end: Position {
                line: 999,
                character: 0,
            },
        },
    ];
    for r in bads {
        assert!(matches!(
            code_actions(
                &doc,
                doc.stamp(),
                r,
                ActionRequest::QuickFix,
                &Policy::default(),
                ConfigProvenance::Default
            ),
            Err(ActionError::InvalidRange(_))
        ));
    }
}
#[test]
fn allow_policy_suppresses_edits_and_effective_policy_identity_is_recorded() {
    let idx = index(TWO);
    let doc = ActionDocument::new(URI, 1, TWO, &idx).unwrap();
    let policy = Policy::from_rules(
        &[Rule {
            name: "unused_effect_capability".into(),
            severity: Severity::Allow,
        }],
        PolicyLimits::default(),
    )
    .unwrap();
    for request in [ActionRequest::QuickFix, ActionRequest::FixAllUnusedEffects] {
        assert!(
            code_actions(
                &doc,
                doc.stamp(),
                selection(&idx, "Audit]{let"),
                request,
                &policy,
                ConfigProvenance::Default
            )
            .unwrap()
            .is_empty()
        );
    }
    let provenance = ConfigProvenance::Explicit {
        path: "in-memory:policy".into(),
        bytes: 12,
        blake3: "0".repeat(64),
    };
    let action = code_actions(
        &doc,
        doc.stamp(),
        selection(&idx, "Audit]{let"),
        ActionRequest::QuickFix,
        &Policy::default(),
        provenance,
    )
    .unwrap()
    .remove(0)
    .to_json();
    assert_eq!(
        action["data"]["policy"]["provenance"]["path"],
        "in-memory:policy"
    );
    assert_eq!(
        action["data"]["policy_rules_blake3"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
}
#[test]
fn invalid_opaque_and_linear_error_source_returns_truthful_unavailable() {
    for source in ["fn main(){", "fn main(){m!{ /* raw */ token }}"] {
        let idx = index(source);
        let doc = ActionDocument::new(URI, 1, source, &idx).unwrap();
        assert!(matches!(
            format_document(&doc, doc.stamp()),
            Err(ActionError::FormatterUnavailable(_))
        ));
    }
    for source in [
        "linear struct Ticket{value:u64} effect Spare{fn no();} fn leak(sink t:Ticket) eff[Spare]{}",
        "effect E{fn act();} fn recur() eff[E]{recur()}",
    ] {
        let idx = index(source);
        let doc = ActionDocument::new(URI, 1, source, &idx).unwrap();
        assert!(matches!(
            code_actions(
                &doc,
                doc.stamp(),
                range(&idx, 0, source.len()),
                ActionRequest::FixAllUnusedEffects,
                &Policy::default(),
                ConfigProvenance::Default
            ),
            Err(ActionError::LintUnavailable(_))
        ));
    }
}
#[test]
fn absent_actual_findings_do_not_become_client_asserted_fixes() {
    // Forged diagnostics/fix/verified JSON is deliberately outside the API: the
    // sole source of findings is current source+validated policy. The dispatcher
    // integration is responsible for checking context.diagnostics is ignored.
    let source = "fn main()->u64{42}";
    let idx = index(source);
    let doc = ActionDocument::new(URI, 1, source, &idx).unwrap();
    assert!(
        code_actions(
            &doc,
            doc.stamp(),
            range(&idx, 0, source.len()),
            ActionRequest::QuickFix,
            &Policy::default(),
            ConfigProvenance::Default
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn editor_source_and_formatted_output_limits_apply_before_offering_an_edit() {
    let code = "\nfn main(){0}";
    let source = format!(
        "//{}{}",
        "x".repeat(MAX_SOURCE_BYTES - 2 - code.len()),
        code
    );
    assert_eq!(source.len(), MAX_SOURCE_BYTES);
    let idx = index(&source);
    let doc = ActionDocument::new(URI, 1, &source, &idx).unwrap();
    assert!(matches!(
        format_document(&doc, doc.stamp()),
        Err(ActionError::FormatterUnavailable(_))
    ));
    let oversized = format!("{source} ");
    let permissive = LineIndex::new(
        Arc::from(oversized.as_str()),
        Limits {
            max_source_bytes: MAX_SOURCE_BYTES + 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        ActionDocument::new(URI, 1, &oversized, &permissive),
        Err(ActionError::InvalidDocument(_))
    ));
}

#[test]
fn direct_policy_provenance_is_bounded_before_lint_preparation() {
    let source = "fn main()->u64{42}";
    let idx = index(source);
    let doc = ActionDocument::new(URI, 1, source, &idx).unwrap();
    for provenance in [
        ConfigProvenance::Explicit {
            path: "x".repeat(MAX_URI_BYTES + 1),
            bytes: 0,
            blake3: "0".repeat(64),
        },
        ConfigProvenance::Explicit {
            path: "in-memory".into(),
            bytes: 65537,
            blake3: "0".repeat(64),
        },
        ConfigProvenance::Explicit {
            path: "in-memory".into(),
            bytes: 1,
            blake3: "not a hash".into(),
        },
    ] {
        assert!(matches!(
            code_actions(
                &doc,
                doc.stamp(),
                range(&idx, 0, source.len()),
                ActionRequest::QuickFix,
                &Policy::default(),
                provenance
            ),
            Err(ActionError::InvalidPolicyProvenance(_))
        ));
    }
}

#[test]
fn utf16_caret_selects_the_finding_with_an_exclusive_end() {
    let idx = index(TWO);
    let doc = ActionDocument::new(URI, 4, TWO, &idx).unwrap();
    let start = TWO.find("Audit]{let").unwrap();
    for (at, expected) in [(start, 1), (start + "Audit".len(), 0)] {
        let actions = code_actions(
            &doc,
            doc.stamp(),
            range(&idx, at, at),
            ActionRequest::QuickFix,
            &Policy::default(),
            ConfigProvenance::Default,
        )
        .unwrap();
        assert_eq!(actions.len(), expected);
    }
}
