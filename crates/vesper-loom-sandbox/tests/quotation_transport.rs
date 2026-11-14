use vesper_loom_sandbox::{
    quote_transport as q,
    transport::{Delimiter, Severity, TokenKind as K},
};
fn limits() -> q::Limits {
    q::Limits::default()
}
fn packed(text: &[u8]) -> q::PackedResult {
    q::PackedResult {
        items: vec![q::PackedItem::Generated(q::PackedGenerated {
            kind: K::Ident,
            text: q::Slice {
                offset: 0,
                len: text.len() as u32,
            },
            reported_span: None,
            claimed_def: None,
        })],
        text: text.to_vec(),
        ..Default::default()
    }
}
fn invocation() -> q::Invocation {
    q::Invocation {
        protocol_version: q::PROTOCOL_VERSION,
        kind: q::MacroKind::ProcMacro,
        entry: "quote".into(),
        args: vec![],
        slots: vec![q::InputSlot {
            category: q::Category::Expr,
            tokens: vec![q::InputToken {
                kind: K::Ident,
                text: "x".into(),
                reported_span: None,
            }],
        }],
    }
}
#[test]
fn genuine_packed_claims_and_current_input_ordinals() {
    let mut p = packed(b"xnote");
    if let q::PackedItem::Generated(t) = &mut p.items[0] {
        t.text.len = 1;
        t.claimed_def = Some(vesper_loom_sandbox::transport::DefId {
            package: 7,
            item: 9,
        });
    }
    p.items.push(q::PackedItem::InputSlot(0));
    p.diagnostics.push(q::PackedDiagnostic {
        severity: Severity::Note,
        message: q::Slice { offset: 1, len: 4 },
        reported_span: None,
    });
    let out = limits().unpack(&p, 1, false).unwrap();
    assert_eq!(out.items.len(), 2);
    assert_eq!(out.diagnostics[0].message, "note");
    assert!(matches!(out.items[1], q::Item::InputSlot(0)));
    limits().check_outcome(&out, 1, false).unwrap();
    assert_eq!(limits().unpack(&p, 0, false).unwrap_err(), q::Error::Slot);
    assert_eq!(
        limits().unpack(&p, 1, true).unwrap_err(),
        q::Error::ItemsInFailure
    );
}
#[test]
fn alias_bombs_are_charged_before_per_item_copy() {
    let mut p = packed(b"abcdefgh");
    p.items.push(p.items[0].clone());
    let before = p.clone();
    let l = q::Limits {
        max_copied_output_text_bytes: 8,
        ..limits()
    };
    assert_eq!(
        l.unpack(&p, 0, false).unwrap_err(),
        q::Error::Budget(q::Resource::Text)
    );
    assert_eq!(p, before);
    let mut p = q::PackedResult {
        text: b"note".to_vec(),
        ..Default::default()
    };
    let d = q::PackedDiagnostic {
        severity: Severity::Note,
        message: q::Slice { offset: 0, len: 4 },
        reported_span: None,
    };
    p.diagnostics = vec![d.clone(), d];
    assert_eq!(
        q::Limits {
            max_diagnostic_text_bytes: 4,
            ..limits()
        }
        .unpack(&p, 0, false)
        .unwrap_err(),
        q::Error::Budget(q::Resource::DiagnosticText)
    );
}
#[test]
fn hostile_slices_utf8_counts_and_work_fail_closed() {
    let mut p = packed(b"x");
    if let q::PackedItem::Generated(t) = &mut p.items[0] {
        t.text.offset = u32::MAX;
        t.text.len = u32::MAX;
    }
    assert_eq!(limits().unpack(&p, 0, false).unwrap_err(), q::Error::Slice);
    assert_eq!(
        limits().unpack(&packed(&[0xff]), 0, false).unwrap_err(),
        q::Error::Utf8
    );
    assert_eq!(
        q::Limits {
            max_output_items: 0,
            ..limits()
        }
        .unpack(&packed(b"x"), 0, false)
        .unwrap_err(),
        q::Error::Budget(q::Resource::Items)
    );
    assert_eq!(
        q::Limits {
            max_work: 1,
            ..limits()
        }
        .unpack(&packed(b"x"), 0, false)
        .unwrap_err(),
        q::Error::Budget(q::Resource::Work)
    );
    let p = q::PackedResult {
        items: vec![q::PackedItem::InputSlot(0); 2],
        ..Default::default()
    };
    assert_eq!(
        q::Limits {
            max_slot_occurrences: 1,
            ..limits()
        }
        .unpack(&p, 1, false)
        .unwrap_err(),
        q::Error::Budget(q::Resource::References)
    );
}
#[test]
fn lexical_kind_text_and_unicode_escape_coherence() {
    for s in ["x", "@pkg/path", "naïve", "true", "foo-bar"] {
        assert!(q::valid_token(K::Ident, s), "{s}");
    }
    for s in ["", "x y", "0foo", "x::y", "a.b"] {
        assert!(!q::valid_token(K::Ident, s), "{s}");
    }
    for s in [
        "0",
        "1_000",
        "0xffff_ffff_ffff_ffff",
        "18446744073709551615",
    ] {
        assert!(q::valid_token(K::IntLiteral, s), "{s}");
    }
    for s in [
        "",
        "_",
        "0x",
        "-1",
        "18446744073709551616",
        "0x10000000000000000",
        "1.0",
    ] {
        assert!(!q::valid_token(K::IntLiteral, s), "{s}");
    }
    for s in [
        r#""x""#,
        r#""\uD83D\uDE00""#,
        r#""\b\t\n\f\r\\\"\/""#,
        "\"é😀\"",
    ] {
        assert!(q::valid_token(K::StringLiteral, s), "{s}");
    }
    for s in [
        r#""\uD800""#,
        r#""\uDC00""#,
        r#""\uD800\u1234""#,
        r#""\q""#,
        "\"x\n\"",
    ] {
        assert!(!q::valid_token(K::StringLiteral, s), "{s}");
    }
    assert!(!q::valid_token(K::Open(Delimiter::Paren), "["));
    assert!(!q::valid_token(K::Punct, "("));
    let s = r#""\uD83D\uDE00""#;
    for end in 0..s.len() {
        assert!(!q::valid_token(K::StringLiteral, &s[..end]));
    }
}
#[test]
fn invocation_caps_and_balanced_inputs() {
    let mut inv = invocation();
    limits().check_invocation(&inv).unwrap();
    inv.slots[0].tokens.push(q::InputToken {
        kind: K::Close(Delimiter::Brace),
        text: "}".into(),
        reported_span: None,
    });
    assert_eq!(
        limits().check_invocation(&inv).unwrap_err(),
        q::Error::Delimiters
    );
    let inv = invocation();
    for l in [
        q::Limits {
            max_slots: 0,
            ..limits()
        },
        q::Limits {
            max_input_tokens: 0,
            ..limits()
        },
        q::Limits {
            max_input_text_bytes: 0,
            ..limits()
        },
        q::Limits {
            max_work: 0,
            ..limits()
        },
    ] {
        assert!(l.check_invocation(&inv).is_err());
    }
    let mut inv = invocation();
    inv.protocol_version = 2;
    assert_eq!(
        limits().check_invocation(&inv).unwrap_err(),
        q::Error::Protocol
    );
    assert_eq!(
        q::Limits {
            max_record_bytes: usize::MAX,
            ..limits()
        }
        .validate()
        .unwrap_err(),
        q::Error::InvalidLimits
    );
}
#[test]
fn failed_guest_can_return_only_bounded_diagnostics() {
    let p = q::PackedResult {
        diagnostics: vec![q::PackedDiagnostic {
            severity: Severity::Error,
            message: q::Slice { offset: 0, len: 3 },
            reported_span: None,
        }],
        text: b"bad".to_vec(),
        ..Default::default()
    };
    let out = limits().unpack(&p, 0, true).unwrap();
    assert!(out.items.is_empty());
    assert_eq!(out.diagnostics[0].severity, Severity::Error);
}
