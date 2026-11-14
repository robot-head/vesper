//! Actual sealed database/compiler snapshots through the pure preview codec.
use serde_json::Value;
use vesper::{
    database::{Database, SourceFile, macro_preview::preview},
    forge_frontend::Stage,
    lsp::macro_preview::{self, Document, Error, Limits},
};

const KEEP: &str = include_str!("fixtures/source_macros/keep.ves");
const LINEAR: &str = include_str!("fixtures/source_macros/linear_duplicate.ves");

fn rendered(source: &str) -> Value {
    let db = Database::default();
    let file = SourceFile::new(&db, source.to_owned());
    let snapshot = preview(&db, file);
    let document = Document::new("untitled:macro", 7, source, snapshot).unwrap();
    macro_preview::render(&document, Limits::default()).unwrap()
}

#[test]
fn checked_expanded_result_is_the_actual_artifact_with_byte_provenance() {
    let db = Database::default();
    let file = SourceFile::new(&db, KEEP.to_owned());
    let snapshot = preview(&db, file);
    let expanded = snapshot.expanded().unwrap();
    let document =
        Document::new("file:///this/path/need/not/exist.ves", -3, KEEP, snapshot).unwrap();
    let value = macro_preview::render(&document, Limits::default()).unwrap();
    assert_eq!(value["schema"], macro_preview::SCHEMA);
    assert_eq!(value["status"], "expanded_checked");
    assert_eq!(value["scope"], "expanded_source_only");
    assert_eq!(value["tier"], 1);
    assert_eq!(value["uri"], "file:///this/path/need/not/exist.ves");
    assert_eq!(value["version"], -3);
    assert_eq!(value["range_unit"], "utf8_bytes");
    assert_eq!(value["package"], "@local/source");
    assert_eq!(value["expanded_source"], expanded.expanded_source());
    assert_eq!(value["source_bytes"], KEEP.len());
    assert_eq!(value["expanded_bytes"], expanded.expanded_source().len());
    assert_eq!(value["macro_definitions"], expanded.macro_definitions());
    assert_eq!(value["invocations"], expanded.invocations());
    assert_eq!(
        value["source_blake3"],
        blake3::hash(KEEP.as_bytes()).to_hex().as_str()
    );
    assert_eq!(
        value["expanded_blake3"],
        blake3::hash(expanded.expanded_source().as_bytes())
            .to_hex()
            .as_str()
    );
    assert_eq!(
        value["source_map"],
        serde_json::to_value(expanded.source_map()).unwrap()
    );
    assert_eq!(
        value["audits"],
        serde_json::to_value(expanded.audits()).unwrap()
    );
    assert!(!value["source_map"].as_array().unwrap().is_empty());
    assert!(
        value["audits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|audit| audit["scope"] == "expanded_function")
    );
    for key in [
        "World",
        "world",
        "checked_source",
        "approved",
        "diagnostics",
    ] {
        assert!(value.get(key).is_none(), "unexpected {key}");
    }
    assert!(serde_json::to_vec(&value).unwrap().len() <= Limits::default().max_output_bytes);
}

#[test]
fn exact_text_association_rejects_same_length_changes_and_accepts_only_uri_labels() {
    let db = Database::default();
    let file = SourceFile::new(&db, KEEP.to_owned());
    let snapshot = preview(&db, file);
    let changed = KEEP.replace("42", "41");
    assert_eq!(changed.len(), KEEP.len());
    assert!(matches!(
        Document::new("untitled:x", 1, &changed, snapshot),
        Err(Error::SourceMismatch)
    ));
    assert!(matches!(
        Document::new("", 1, KEEP, snapshot),
        Err(Error::InvalidDocument(_))
    ));
    let long_uri = "x".repeat(4097);
    assert!(matches!(
        Document::new(&long_uri, 1, KEEP, snapshot),
        Err(Error::InvalidDocument(_))
    ));
    // Relative, control-containing, and non-file labels are encoded as data.
    for uri in ["../not-a-path", "custom:\u{0}\n😀"] {
        let document = Document::new(uri, 99, KEEP, snapshot).unwrap();
        let result = macro_preview::render(&document, Limits::default()).unwrap();
        assert_eq!(result["uri"], uri);
        assert_eq!(result["version"], 99);
        assert_eq!(
            result["source_blake3"],
            blake3::hash(KEEP.as_bytes()).to_hex().as_str()
        );
    }
}

#[test]
fn real_linear_check_failure_keeps_only_candidate_diagnostics() {
    let db = Database::default();
    let file = SourceFile::new(&db, LINEAR.to_owned());
    let snapshot = preview(&db, file);
    let failure = snapshot.expanded().unwrap_err();
    assert_eq!(failure.stage(), Stage::Check);
    assert!(failure.diagnostics().iter().any(|d| d.code == "E0303"));
    let document = Document::new("untitled:linear", 4, LINEAR, snapshot).unwrap();
    let value = macro_preview::render(&document, Limits::default()).unwrap();
    assert_eq!(value["status"], "rejected");
    assert_eq!(value["stage"], "check");
    assert_eq!(value["diagnostic_source"], "expanded");
    assert_eq!(value["expanded_source"], failure.expanded_source().unwrap());
    assert_eq!(
        value["diagnostics"],
        serde_json::to_value(failure.diagnostics()).unwrap()
    );
    assert_eq!(value["source_map"], serde_json::json!([]));
    assert_eq!(value["audits"], serde_json::json!([]));
    for key in [
        "expanded_blake3",
        "source_bytes",
        "expanded_bytes",
        "macro_definitions",
        "invocations",
        "package",
        "approved",
        "world",
    ] {
        assert!(value.get(key).is_none(), "rejection contains {key}");
    }
}

#[test]
fn original_parse_and_pre_candidate_failures_report_truthful_diagnostic_spaces() {
    let malformed = "macro keep($x: expr)->expr { quote { $x } } fn main( {";
    let parse = rendered(malformed);
    assert_eq!(parse["status"], "rejected");
    assert_eq!(parse["stage"], "parse");
    assert_eq!(parse["diagnostic_source"], "original");
    assert!(parse["expanded_source"].is_null());
    let unsupported =
        rendered("macro keep($x: expr)->expr { quote { $x } } fn main[T](in x:i64)->i64{keep!(x)}");
    assert_eq!(unsupported["status"], "rejected");
    assert_eq!(unsupported["stage"], "unsupported");
    assert_eq!(unsupported["diagnostic_source"], "none");
    assert!(unsupported["expanded_source"].is_null());
    for value in [parse, unsupported] {
        assert_eq!(value["scope"], "expanded_source_only");
        assert_eq!(value["source_map"], serde_json::json!([]));
        assert_eq!(value["audits"], serde_json::json!([]));
        assert!(value.get("expanded_blake3").is_none());
    }
}

#[test]
fn repeated_antiquotation_retains_distinct_actual_expanded_ranges() {
    let source = include_str!("fixtures/source_macros/repeated.ves");
    let value = rendered(source);
    assert_eq!(value["status"], "expanded_checked");
    let maps = value["source_map"].as_array().unwrap();
    let original = source.find("twice!(n)").unwrap() + "twice!(".len();
    let spans: std::collections::BTreeSet<_> = maps
        .iter()
        .filter(|entry| entry["original"] == serde_json::json!({"start":original,"end":original+1}))
        .map(|entry| {
            (
                entry["expanded"]["start"].as_u64().unwrap(),
                entry["expanded"]["end"].as_u64().unwrap(),
            )
        })
        .collect();
    assert!(
        spans.len() >= 2,
        "repeated argument must retain distinct expanded anchors"
    );
    let spans: Vec<_> = spans.into_iter().collect();
    assert!(spans.iter().all(|(start, end)| start < end));
    assert!(spans.windows(2).all(|pair| pair[0].1 <= pair[1].0));
}

#[test]
fn invalid_and_tight_resource_profiles_return_no_value() {
    let db = Database::default();
    let file = SourceFile::new(&db, KEEP.to_owned());
    let snapshot = preview(&db, file);
    let document = Document::new("untitled:limits", 1, KEEP, snapshot).unwrap();
    let default = Limits::default();
    for limits in [
        Limits {
            max_source_bytes: 0,
            ..default
        },
        Limits {
            max_source_bytes: 128 * 1024 + 1,
            ..default
        },
        Limits {
            max_uri_bytes: 0,
            ..default
        },
        Limits {
            max_uri_bytes: 4097,
            ..default
        },
        Limits {
            max_output_bytes: 0,
            ..default
        },
        Limits {
            max_output_bytes: 1024 * 1024 + 1,
            ..default
        },
        Limits {
            max_work: 0,
            ..default
        },
        Limits {
            max_work: 32 * 1024 * 1024 + 1,
            ..default
        },
    ] {
        assert_eq!(
            macro_preview::render(&document, limits),
            Err(Error::InvalidLimits)
        );
    }
    for limits in [
        Limits {
            max_source_bytes: KEEP.len() - 1,
            ..default
        },
        Limits {
            max_uri_bytes: 1,
            ..default
        },
        Limits {
            max_output_bytes: 1,
            ..default
        },
        Limits {
            max_work: 1,
            ..default
        },
    ] {
        assert!(matches!(
            macro_preview::render(&document, limits),
            Err(Error::Resource(_))
        ));
    }
    let ordinary = macro_preview::render(&document, default).unwrap();
    let bytes = serde_json::to_vec(&ordinary).unwrap().len();
    assert!(matches!(
        macro_preview::render(
            &document,
            Limits {
                max_output_bytes: bytes - 1,
                ..default
            }
        ),
        Err(Error::Resource("output bytes"))
    ));
    assert_eq!(
        macro_preview::render(
            &document,
            Limits {
                max_output_bytes: bytes,
                ..default
            }
        )
        .unwrap(),
        ordinary
    );
}

#[test]
fn oversize_legacy_snapshot_cannot_be_associated_or_hash_promoted() {
    let db = Database::default();
    let source = " ".repeat(128 * 1024 + 1);
    let file = SourceFile::new(&db, source.clone());
    let snapshot = preview(&db, file);
    assert!(snapshot.source().is_none());
    assert!(snapshot.source_identity().blake3().is_none());
    assert!(matches!(
        Document::new("untitled:large", 1, &source, snapshot),
        Err(Error::Resource("source bytes"))
    ));
    assert!(matches!(
        Document::new("untitled:large", 1, "", snapshot),
        Err(Error::SourceMismatch)
    ));
}
