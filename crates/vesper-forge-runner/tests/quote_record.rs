use vesper_forge_runner::{
    quote_key,
    quote_record::{Error, RawRecord},
};
use vesper_loom_sandbox::{
    quote_transport as q,
    transport::{Diagnostic, Severity, TokenKind},
};
fn record() -> RawRecord {
    RawRecord {
        key: [7; 32],
        fuel_consumed: 19,
        peak_memory_bytes: 65536,
        outcome: q::Outcome {
            items: vec![
                q::Item::Generated(q::GeneratedToken {
                    kind: TokenKind::IntLiteral,
                    text: "42".into(),
                    reported_span: None,
                    claimed_def: None,
                }),
                q::Item::InputSlot(0),
            ],
            diagnostics: vec![Diagnostic {
                severity: Severity::Note,
                message: "😀note".into(),
                span: None,
            }],
        },
        stdout: b"out".to_vec(),
        stderr: vec![],
    }
}
#[test]
fn actual_wire_record_roundtrip_every_truncation_and_canonical_tail() {
    let record = record();
    let bytes = record.encode(q::Limits::default(), 1, 1024).unwrap();
    assert_eq!(&bytes[..8], b"VFRQ\x01\0\0\0");
    assert_eq!(
        RawRecord::decode(&bytes, q::Limits::default(), 1, 1024).unwrap(),
        record
    );
    for end in 0..bytes.len() {
        assert!(
            RawRecord::decode(&bytes[..end], q::Limits::default(), 1, 1024).is_err(),
            "{end}"
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        RawRecord::decode(&trailing, q::Limits::default(), 1, 1024).unwrap_err(),
        Error::NonCanonical
    );
    let mut wrong = bytes.clone();
    wrong[4] = 2;
    assert_eq!(
        RawRecord::decode(&wrong, q::Limits::default(), 1, 1024).unwrap_err(),
        Error::Version
    );
    assert!(RawRecord::decode(&bytes, q::Limits::default(), 0, 1024).is_err());
}
#[test]
fn independent_single_slot_vector_and_unused_metadata_are_canonical() {
    let mut bytes = b"VFRQ\x01\0\0\0".to_vec();
    bytes.extend([0; 32]);
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes.extend([1, 0, 0, 0, 0]);
    let decoded = RawRecord::decode(&bytes, q::Limits::default(), 1, 0).unwrap();
    assert_eq!(decoded.outcome.items, vec![q::Item::InputSlot(0)]);
    assert_eq!(decoded.encode(q::Limits::default(), 1, 0).unwrap(), bytes);
    let mut bad = record().encode(q::Limits::default(), 1, 1024).unwrap();
    bad[76] = 1; // first generated span payload nonzero while has-span=0
    assert_eq!(
        RawRecord::decode(&bad, q::Limits::default(), 1, 1024).unwrap_err(),
        Error::NonCanonical
    );
}
#[test]
fn malformed_tags_counts_utf8_and_budget_fail_before_output() {
    let bytes = record().encode(q::Limits::default(), 1, 1024).unwrap();
    for offset in [72, 73, 74, 75] {
        let mut bad = bytes.clone();
        bad[offset] = 255;
        assert!(RawRecord::decode(&bad, q::Limits::default(), 1, 1024).is_err());
    }
    let mut count = bytes.clone();
    count[56..60].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(RawRecord::decode(&count, q::Limits::default(), 1, 1024).is_err());
    assert!(
        RawRecord::decode(
            &bytes,
            q::Limits {
                max_work: 1,
                ..q::Limits::default()
            },
            1,
            1024
        )
        .is_err()
    );
    assert!(
        RawRecord::decode(
            &bytes,
            q::Limits {
                max_record_bytes: bytes.len() - 1,
                ..q::Limits::default()
            },
            1,
            1024
        )
        .is_err()
    );
    assert!(RawRecord::decode(&bytes, q::Limits::default(), 1, 2).is_err());
    let mut utf8 = bytes.clone();
    utf8[101] = 0xff;
    assert_eq!(
        RawRecord::decode(&utf8, q::Limits::default(), 1, 1024).unwrap_err(),
        Error::Transport(q::Error::Utf8)
    );
    let mut error = record();
    error.outcome.diagnostics[0].severity = Severity::Error;
    assert_eq!(
        error.encode(q::Limits::default(), 1, 1024).unwrap_err(),
        Error::ErrorDiagnostic
    );
}
fn invocation() -> q::Invocation {
    q::Invocation {
        protocol_version: 1,
        kind: q::MacroKind::ProcMacro,
        entry: "identity".into(),
        args: vec![],
        slots: vec![q::InputSlot {
            category: q::Category::Expr,
            tokens: vec![q::InputToken {
                kind: TokenKind::Ident,
                text: "x".into(),
                reported_span: None,
            }],
        }],
    }
}
#[test]
fn invocation_encoding_binds_order_boundaries_profile_and_size() {
    let mut inv = invocation();
    let bytes = quote_key::canonical_invocation(&inv, q::Limits::default()).unwrap();
    assert_eq!(&bytes[..8], b"VFQI\x01\0\0\0");
    let mut other = inv.clone();
    other.slots.push(other.slots[0].clone());
    assert_ne!(
        bytes,
        quote_key::canonical_invocation(&other, q::Limits::default()).unwrap()
    );
    inv.args.push("x".into());
    assert_ne!(
        bytes,
        quote_key::canonical_invocation(&inv, q::Limits::default()).unwrap()
    );
    assert!(
        quote_key::canonical_invocation(
            &inv,
            q::Limits {
                max_invocation_bytes: 1,
                ..q::Limits::default()
            }
        )
        .is_err()
    );
}
