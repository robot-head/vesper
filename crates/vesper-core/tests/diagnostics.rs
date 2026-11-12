use vesper_core::ast::{Diagnostic, DiagnosticLabel};

#[test]
fn legacy_json_decodes_and_reencodes_the_exact_original_shape() {
    let legacy =
        r#"{"code":"E0301","message":"unchanged compiler message","span":{"start":5,"end":9}}"#;
    let diagnostic: Diagnostic = serde_json::from_str(legacy).unwrap();
    assert_eq!(
        diagnostic,
        Diagnostic::new("E0301", "unchanged compiler message", 5..9)
    );
    assert!(diagnostic.related.is_empty());
    assert_eq!(diagnostic.help, None);
    assert_eq!(serde_json::to_string(&diagnostic).unwrap(), legacy);
}

#[test]
fn explicit_empty_metadata_normalizes_to_legacy_shape() {
    let explicit =
        r#"{"code":"E1","message":"m","span":{"start":0,"end":0},"related":[],"help":null}"#;
    let diagnostic: Diagnostic = serde_json::from_str(explicit).unwrap();
    assert_eq!(
        serde_json::to_string(&diagnostic).unwrap(),
        r#"{"code":"E1","message":"m","span":{"start":0,"end":0}}"#
    );
    let value = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 3);
    assert!(value.get("related").is_none());
    assert!(value.get("help").is_none());
}

#[test]
fn rich_unicode_metadata_roundtrips_exact_utf8_bytes_and_label_order() {
    let source = "α 🦀\r\nlet tx = café;\r\nconsume(tx)";
    let binding = source.find("tx").unwrap();
    let operation = source.find("consume").unwrap();
    let crab = source.find('🦀').unwrap();
    let diagnostic = Diagnostic::new("E0303", "不能重复消费 🦀", operation..source.len())
        .with_related(binding..binding + 2, "binding introduced here: café")
        .with_related(crab..crab + '🦀'.len_utf8(), "supplied Unicode label 🦀")
        .with_help("保留所有路径上的线性义务；no invented method");
    let bytes = serde_json::to_vec(&diagnostic).unwrap();
    let decoded: Diagnostic = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, diagnostic);
    assert_eq!(&source[decoded.span.clone()], "consume(tx)");
    assert_eq!(&source[decoded.related[0].span.clone()], "tx");
    assert_eq!(&source[decoded.related[1].span.clone()], "🦀");
    assert_eq!(decoded.related[0].message, "binding introduced here: café");
    assert_eq!(decoded.related[1].message, "supplied Unicode label 🦀");
    // These are UTF-8 byte offsets, not character counts or UTF-16 columns.
    assert_eq!(crab, 3);
    assert_eq!(decoded.related[1].span, 3..7);
    assert!(binding > source[..binding].chars().count());
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
}

#[test]
fn builders_append_related_preserve_primary_and_replace_only_help() {
    let diagnostic = Diagnostic::new("E-original", String::from("original"), 10..12)
        .with_related(2..4, "first")
        .with_help("old help")
        .with_related(0..0, String::from("second"))
        .with_help(String::from("new help"));
    assert_eq!(diagnostic.code, "E-original");
    assert_eq!(diagnostic.message, "original");
    assert_eq!(diagnostic.span, 10..12);
    assert_eq!(
        diagnostic.related,
        vec![
            DiagnosticLabel {
                span: 2..4,
                message: "first".into()
            },
            DiagnosticLabel {
                span: 0..0,
                message: "second".into()
            }
        ]
    );
    assert_eq!(diagnostic.help.as_deref(), Some("new help"));
}

#[test]
fn metadata_is_independently_optional_and_empty_help_is_supplied_data() {
    let related = Diagnostic::new("E", "m", 1..2).with_related(0..1, "cause");
    let value = serde_json::to_value(&related).unwrap();
    assert_eq!(value["related"].as_array().unwrap().len(), 1);
    assert!(value.get("help").is_none());
    let decoded: Diagnostic = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, related);
    let help = Diagnostic::new("E", "m", 1..2).with_help("");
    let value = serde_json::to_value(&help).unwrap();
    assert_eq!(value["help"], "");
    assert!(value.get("related").is_none());
    assert_eq!(serde_json::from_value::<Diagnostic>(value).unwrap(), help);
}

#[test]
fn clone_and_equality_include_all_metadata_without_shared_mutation() {
    let original = Diagnostic::new("E", "m", 1..2)
        .with_related(0..1, "origin")
        .with_help("help");
    let mut cloned = original.clone();
    assert_eq!(cloned, original);
    cloned.related[0].message.push('!');
    assert_ne!(cloned, original);
    assert_eq!(original.related[0].message, "origin");
    cloned = original.clone();
    cloned.related[0].span = 0..0;
    assert_ne!(cloned, original);
    cloned = original.clone();
    cloned.help = None;
    assert_ne!(cloned, original);
}

#[test]
fn malformed_new_metadata_is_not_silently_discarded() {
    for metadata in [
        r#""related":null"#,
        r#""related":{}"#,
        r#""related":[{"span":{"start":0,"end":1}}]"#,
        r#""help":7"#,
    ] {
        let bytes =
            format!(r#"{{"code":"E","message":"m","span":{{"start":0,"end":1}},{metadata}}}"#);
        assert!(
            serde_json::from_str::<Diagnostic>(&bytes).is_err(),
            "{bytes}"
        );
    }
}
