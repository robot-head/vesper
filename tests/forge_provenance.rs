//! Macro provenance (COMP-03): every expanded artifact attributes each IR
//! function to source or to its macro invocations, diagnostics about expanded
//! code carry their macro origin, and Lean extraction emits kernel-visible
//! markers for expanded definitions.
use vesper::forge_frontend::{
    self as forge, CompileConfig, CompileLimits, MapKind, Stage,
    provenance::{self, FunctionOrigin, ProvenanceError, ProvenanceTable},
};

const MIXED: &str = include_str!("fixtures/source_macros/provenance_mixed.ves");
const KEEP: &str = include_str!("fixtures/source_macros/keep.ves");

fn expand(source: &str) -> forge::ExpandedChecked {
    forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn every_ir_function_has_exactly_its_macro_origins() {
    let artifact = expand(MIXED);
    let table = artifact.provenance();
    table
        .verify(Some((
            artifact.original_source(),
            artifact.expanded_source(),
        )))
        .unwrap();
    let ir = artifact.ir().functions.keys().cloned().collect::<Vec<_>>();
    let recorded = table
        .functions
        .iter()
        .map(|f| f.function.clone())
        .collect::<Vec<_>>();
    assert_eq!(ir, recorded, "one provenance entry per IR function");
    assert_eq!(
        table.function("plain").unwrap().origin,
        FunctionOrigin::Source
    );
    assert!(table.function("plain").unwrap().expansions.is_empty());
    for (function, r#macro) in [("doubled", "twice"), ("main", "keep")] {
        let entry = table.function(function).unwrap();
        assert_eq!(entry.origin, FunctionOrigin::Expanded);
        assert_eq!(entry.expansions.len(), 1);
        let record = &table.expansions[entry.expansions[0] as usize];
        assert_eq!(record.r#macro, r#macro);
        assert_eq!(record.host_function, function);
        let invocation = &MIXED[record.invocation.start..record.invocation.end];
        assert!(
            invocation.starts_with(&format!("{macro}!(")),
            "{invocation}"
        );
        for range in &record.generated {
            assert!(
                artifact
                    .expanded_source()
                    .get(range.start..range.end)
                    .is_some()
            );
        }
    }
    // `twice` generates the `wrapping_add(..)` call; every generated range of
    // the source map is attributed to exactly one expansion record.
    let generated_in_map = artifact
        .source_map()
        .iter()
        .filter(|s| s.kind == MapKind::Generated)
        .count();
    let recorded_generated = table
        .expansions
        .iter()
        .map(|e| e.generated.len())
        .sum::<usize>();
    assert_eq!(generated_in_map, recorded_generated);
    assert!(recorded_generated > 0);
    assert_eq!(
        table.source_blake3,
        blake3::hash(MIXED.as_bytes()).to_hex().as_str()
    );
    assert_eq!(
        table.expanded_blake3,
        blake3::hash(artifact.expanded_source().as_bytes())
            .to_hex()
            .as_str()
    );
}

#[test]
fn provenance_is_deterministic_and_serializes_round_trip() {
    let a = expand(MIXED);
    let b = expand(MIXED);
    assert_eq!(a.provenance(), b.provenance());
    let text = serde_json::to_string(a.provenance()).unwrap();
    let back: ProvenanceTable = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, a.provenance());
    back.verify(None).unwrap();
}

#[test]
fn tampered_or_incomplete_tables_are_refused() {
    let artifact = expand(MIXED);
    let sources = Some((artifact.original_source(), artifact.expanded_source()));
    let good = artifact.provenance().clone();

    let mut renamed = good.clone();
    renamed.expansions[0].r#macro = "other".into();
    assert!(matches!(
        renamed.verify(None),
        Err(ProvenanceError::Mismatch(_))
    ));

    let mut stripped = good.clone();
    let main = stripped
        .functions
        .iter_mut()
        .find(|f| f.function == "main")
        .unwrap();
    main.origin = FunctionOrigin::Source;
    main.expansions.clear();
    assert!(matches!(
        stripped.verify(None),
        Err(ProvenanceError::Incomplete(_))
    ));

    let mut orphan = good.clone();
    orphan.functions.retain(|f| f.function != "doubled");
    assert!(matches!(
        orphan.verify(None),
        Err(ProvenanceError::Incomplete(_))
    ));

    let mut redigested = good.clone();
    redigested.digest = "0".repeat(64);
    assert!(matches!(
        redigested.verify(None),
        Err(ProvenanceError::Mismatch(_))
    ));

    assert!(matches!(
        good.verify(Some((KEEP, artifact.expanded_source()))),
        Err(ProvenanceError::Mismatch(_))
    ));
    let mut schema = good.clone();
    schema.schema = "vesper.forge.provenance/0".into();
    assert_eq!(schema.verify(None), Err(ProvenanceError::Schema));
    good.verify(sources).unwrap();
}

#[test]
fn check_diagnostics_in_generated_code_name_their_macro_invocation() {
    let source = "macro bad($x: expr) -> expr {\n    quote {\n        wrapping_add($x, true)\n    }\n}\nfn main() -> i64 {\n    let n = 42;\n    bad!(n)\n}\n";
    let failure = forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
        .unwrap_err();
    assert_eq!(failure.stage(), Stage::Check);
    assert_eq!(failure.origins().len(), failure.diagnostics().len());
    let origin = failure.origins()[0].as_ref().expect("diagnostic origin");
    assert_eq!(origin.kind, MapKind::Generated);
    assert_eq!(origin.r#macro.as_deref(), Some("bad"));
    assert_eq!(
        &source[origin.original.start..origin.original.end],
        "bad!(n)"
    );
    let expanded = failure.expanded_source().unwrap();
    assert_eq!(
        &expanded[origin.expanded.start..origin.expanded.end],
        "true"
    );
}

#[test]
fn check_diagnostics_in_ordinary_code_keep_their_source_span() {
    let source = "macro keep($x: expr) -> expr {\n    quote {\n        $x\n    }\n}\nfn other() -> i64 {\n    true\n}\nfn main() -> i64 {\n    let n = 42;\n    keep!(n)\n}\n";
    let failure = forge::expand_source(source, &CompileConfig::default(), CompileLimits::default())
        .unwrap_err();
    assert_eq!(failure.stage(), Stage::Check);
    let origin = failure.origins()[0].as_ref().expect("diagnostic origin");
    assert_eq!(origin.kind, MapKind::Source);
    assert_eq!(origin.r#macro, None);
    assert_eq!(&source[origin.original.start..origin.original.end], "true");
}

#[test]
fn lean_extraction_marks_exactly_the_expanded_definitions() {
    let artifact = expand(MIXED);
    let lean = provenance::extract_lean(&artifact).unwrap();
    let table = artifact.provenance();
    let owners = lean
        .marked
        .iter()
        .map(|(_, owner)| owner.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        owners,
        ["doubled", "main"].into_iter().collect(),
        "only expanded functions are marked"
    );
    for (declaration, owner) in &lean.marked {
        let digest = &table.function(owner).unwrap().digest;
        let marker = format!(
            "def «{declaration}{}»  : _root_.«String» := \"{digest}\"",
            provenance::MARKER_SUFFIX
        );
        assert!(lean.text.contains(&marker), "missing {marker}");
    }
    assert!(lean.text.contains(&format!(
        "def «{}»  : _root_.«String» := \"{}\"",
        provenance::MODULE_MARKER,
        table.digest
    )));
    assert!(!lean.text.contains("«plain$macro_provenance»"));
}

#[test]
fn lean_owner_follows_extractor_companion_names() {
    let functions = ["a", "a_b", "main"].into_iter().collect();
    assert_eq!(provenance::lean_owner("main", &functions), Some("main"));
    assert_eq!(
        provenance::lean_owner("main$fuel", &functions),
        Some("main")
    );
    assert_eq!(
        provenance::lean_owner("main$contract", &functions),
        Some("main")
    );
    assert_eq!(
        provenance::lean_owner("$block_a_b_3", &functions),
        Some("a_b")
    );
    assert_eq!(provenance::lean_owner("$block_a_0", &functions), Some("a"));
    assert_eq!(provenance::lean_owner("$block_a_x", &functions), None);
    assert_eq!(provenance::lean_owner("HandlerCPS", &functions), None);
    assert_eq!(provenance::lean_owner("$arith_simps", &functions), None);
}

#[test]
fn diagnostic_origin_uses_half_open_segments() {
    use forge::{ByteRange, SourceMapSegment};
    use vesper::ast::Span;
    let seg =
        |e: (usize, usize), o: (usize, usize), kind: MapKind, m: Option<&str>| SourceMapSegment {
            expanded: ByteRange {
                start: e.0,
                end: e.1,
            },
            original: ByteRange {
                start: o.0,
                end: o.1,
            },
            kind,
            r#macro: m.map(str::to_owned),
        };
    // `let n = ` (source, binder) then `true` (source) then `;` at EOF.
    let map = vec![
        seg((0, 8), (100, 108), MapKind::Source, None),
        seg((8, 12), (200, 204), MapKind::Source, None),
        seg((12, 13), (300, 301), MapKind::Source, None),
    ];
    let at_boundary = provenance::origin_of(&map, &Span { start: 8, end: 12 }).unwrap();
    assert_eq!(
        at_boundary.original,
        ByteRange {
            start: 200,
            end: 204
        }
    );
    let empty_at_boundary = provenance::origin_of(&map, &Span { start: 8, end: 8 }).unwrap();
    assert_eq!(empty_at_boundary.original.start, 200);
    // End of file has no successor and falls back to the last segment.
    let eof = provenance::origin_of(&map, &Span { start: 13, end: 13 }).unwrap();
    assert_eq!(eof.original.start, 301);
    // A generated segment that ends where a span starts does not claim it.
    let map = vec![
        seg((0, 8), (50, 57), MapKind::Generated, Some("m")),
        seg((8, 12), (200, 204), MapKind::Source, None),
    ];
    let origin = provenance::origin_of(&map, &Span { start: 8, end: 12 }).unwrap();
    assert_eq!(origin.kind, MapKind::Source);
    assert_eq!(origin.r#macro, None);
}
