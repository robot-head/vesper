use std::sync::Arc;
use vesper_span::*;
fn index(s: &str) -> LineIndex {
    LineIndex::new(Arc::from(s), Limits::default()).unwrap()
}
fn p(line: u32, character: u32) -> Position {
    Position { line, character }
}
fn b(start: usize, end: usize) -> ByteSpan {
    ByteSpan { start, end }
}
fn e(start: usize, end: usize, expected: &str, replacement: &str) -> SourceEdit {
    SourceEdit {
        span: b(start, end),
        expected: expected.into(),
        replacement: replacement.into(),
    }
}
#[test]
fn utf8_utf16_and_strict_interiors() {
    let i = index("aé🦀中");
    for (byte, character) in [(0, 0), (1, 1), (3, 2), (7, 4), (10, 5)] {
        assert_eq!(i.position(byte), Ok(p(0, character)));
        assert_eq!(i.offset(p(0, character)), Ok(byte));
    }
    for byte in [2, 4, 5, 6, 8, 9] {
        assert_eq!(i.position(byte), Err(Error::Utf8Boundary));
    }
    assert_eq!(i.offset(p(0, 3)), Err(Error::SurrogateInterior));
    assert_eq!(i.offset(p(0, 6)), Err(Error::CharacterOutOfBounds));
    assert_eq!(i.position(11), Err(Error::ByteOutOfBounds));
    assert_eq!(i.offset(p(u32::MAX, u32::MAX)), Err(Error::LineOutOfBounds));
}
#[test]
fn newlines_and_eof_exact() {
    let i = index("a\r\nb\rc\n\r\n");
    for (byte, pos) in [
        (0, p(0, 0)),
        (1, p(0, 1)),
        (3, p(1, 0)),
        (4, p(1, 1)),
        (5, p(2, 0)),
        (6, p(2, 1)),
        (7, p(3, 0)),
        (9, p(4, 0)),
    ] {
        assert_eq!(i.position(byte), Ok(pos));
        assert_eq!(i.offset(pos), Ok(byte));
    }
    assert_eq!(i.position(2), Err(Error::LineEndingInterior));
    assert_eq!(i.position(8), Err(Error::LineEndingInterior));
    assert_eq!(i.offset(p(3, 1)), Err(Error::CharacterOutOfBounds));
    assert_eq!(index("").position(0), Ok(p(0, 0)));
    assert_eq!(index("\r").position(1), Ok(p(1, 0)));
    assert_eq!(index("\n").position(1), Ok(p(1, 0)));
}
#[test]
fn all_small_unicode_strings_have_exact_coordinate_roundtrips() {
    let alphabet = ["a", "é", "🦀", "\r", "\n"];
    for a in alphabet {
        for c in alphabet {
            for d in alphabet {
                let s = format!("{a}{c}{d}");
                let i = index(&s);
                for byte in 0..=s.len() {
                    match i.position(byte) {
                        Ok(pos) => assert_eq!(i.offset(pos), Ok(byte)),
                        Err(Error::Utf8Boundary | Error::LineEndingInterior) => {}
                        other => panic!("{s:?} {byte} {other:?}"),
                    }
                }
            }
        }
    }
}
#[test]
fn ranges_validate_both_endpoints_and_order() {
    let i = index("a🦀\r\nz");
    let span = b(1, 7);
    assert_eq!(i.span(i.range(span).unwrap()), Ok(span));
    assert_eq!(i.range(b(3, 1)), Err(Error::ReversedRange));
    assert_eq!(i.range(b(0, 6)), Err(Error::LineEndingInterior));
    assert_eq!(
        i.span(Range {
            start: p(1, 0),
            end: p(0, 0)
        }),
        Err(Error::ReversedRange)
    );
    assert_eq!(
        i.span(Range {
            start: p(0, 0),
            end: p(0, 2)
        }),
        Err(Error::SurrogateInterior)
    );
}
#[test]
fn configured_index_budgets_are_checked() {
    for (limits, error) in [
        (
            Limits {
                max_source_bytes: 1,
                ..Limits::default()
            },
            Error::Budget(Resource::Source),
        ),
        (
            Limits {
                max_lines: 1,
                ..Limits::default()
            },
            Error::Budget(Resource::Lines),
        ),
        (
            Limits {
                max_metadata_bytes: 0,
                ..Limits::default()
            },
            Error::Budget(Resource::Metadata),
        ),
        (
            Limits {
                max_work: 3,
                ..Limits::default()
            },
            Error::Budget(Resource::Work),
        ),
    ] {
        assert_eq!(LineIndex::new(Arc::from("a\n"), limits).unwrap_err(), error);
    }
    assert_eq!(
        Limits {
            max_source_bytes: usize::MAX,
            ..Limits::default()
        }
        .validate(),
        Err(Error::InvalidLimits)
    );
    assert!(
        LineIndex::new(
            Arc::from("a\n"),
            Limits {
                max_source_bytes: 2,
                max_lines: 2,
                max_metadata_bytes: 32,
                max_work: 4,
                ..Limits::default()
            }
        )
        .is_ok()
    );
}
#[test]
fn unordered_edits_are_sorted_and_snapshot_bound() {
    let i = index("a🦀\r\nbc");
    let input = [e(8, 9, "c", "Z"), e(1, 5, "🦀", "é"), e(7, 8, "b", "")];
    let edits = SourceEdits::new(&i, &input, Limits::default()).unwrap();
    assert_eq!(edits.source(), i.text());
    assert_eq!(edits.apply().unwrap(), "aé\r\nZ");
    assert_eq!(edits.edits()[0], input[1]);
    assert_eq!(input[0].span, b(8, 9));
    assert_eq!(i.text(), "a🦀\r\nbc");
    assert_eq!(
        SourceEdits::new(&index("x🦀\r\nbc"), &[e(0, 1, "a", "b")], Limits::default()).unwrap_err(),
        Error::SourceMismatch
    );
    assert_eq!(
        SourceEdits::new(&index(""), &[e(0, 0, "", "🦀")], Limits::default())
            .unwrap()
            .apply()
            .unwrap(),
        "🦀"
    );
}
#[test]
fn collisions_and_invalid_edits_leave_source_unchanged() {
    let i = index("abcd");
    for edits in [
        vec![e(0, 2, "ab", "x"), e(1, 3, "bc", "y")],
        vec![e(1, 1, "", "x"), e(1, 1, "", "y")],
        vec![e(0, 1, "a", "x"), e(1, 1, "", "y")],
        vec![e(1, 1, "", "x"), e(1, 2, "b", "y")],
    ] {
        assert_eq!(
            SourceEdits::new(&i, &edits, Limits::default()).unwrap_err(),
            Error::OverlappingEdits
        );
    }
    assert_eq!(
        SourceEdits::new(&i, &[e(2, 1, "", "x")], Limits::default()).unwrap_err(),
        Error::ReversedRange
    );
    assert_eq!(
        SourceEdits::new(&i, &[e(0, 9, "abcd", "x")], Limits::default()).unwrap_err(),
        Error::ByteOutOfBounds
    );
    assert_eq!(
        SourceEdits::new(&i, &[e(0, 1, "x", "z")], Limits::default()).unwrap_err(),
        Error::SourceMismatch
    );
    assert_eq!(i.text(), "abcd");
}
#[test]
fn edit_budget_preflight_and_empty_edits() {
    let i = index("ab");
    let edits = [e(0, 1, "a", "xyz")];
    for (limits, error) in [
        (
            Limits {
                max_edits: 0,
                ..Limits::default()
            },
            Resource::Edits,
        ),
        (
            Limits {
                max_output_bytes: 3,
                ..Limits::default()
            },
            Resource::Output,
        ),
        (
            Limits {
                max_text_bytes: 3,
                ..Limits::default()
            },
            Resource::Text,
        ),
        (
            Limits {
                max_metadata_bytes: 1,
                ..Limits::default()
            },
            Resource::Metadata,
        ),
        (
            Limits {
                max_work: 1,
                ..Limits::default()
            },
            Resource::Work,
        ),
    ] {
        assert_eq!(
            SourceEdits::new(&i, &edits, limits).unwrap_err(),
            Error::Budget(error)
        );
        assert_eq!(i.text(), "ab");
    }
    assert_eq!(
        SourceEdits::new(
            &i,
            &edits,
            Limits {
                max_output_bytes: 4,
                ..Limits::default()
            }
        )
        .unwrap()
        .apply()
        .unwrap(),
        "xyzb"
    );
    assert_eq!(
        SourceEdits::new(&i, &[], Limits::default())
            .unwrap()
            .apply()
            .unwrap(),
        "ab"
    );
}
fn diagnostic() -> DiagnosticData {
    DiagnosticData {
        code: "E1".into(),
        message: "actual supplied message".into(),
        severity: Severity::Warning,
        labels: vec![
            Label {
                span: b(0, 1),
                message: "primary".into(),
                primary: true,
            },
            Label {
                span: b(1, 5),
                message: "secondary".into(),
                primary: false,
            },
        ],
        help: Some("help".into()),
        fixes: vec![Fix {
            id: "replace".into(),
            title: "replace a".into(),
            edits: vec![e(0, 1, "a", "b")],
        }],
    }
}
#[test]
fn multispan_data_is_checked_and_retains_metadata() {
    let i = index("a🦀");
    let data = diagnostic();
    let checked = Diagnostics::new(&i, std::slice::from_ref(&data), Limits::default()).unwrap();
    assert_eq!(checked.source(), i.text());
    assert_eq!(checked.data(), &[data]);
    let fix = &checked.data()[0].fixes[0];
    assert_eq!(
        SourceEdits::new(&i, &fix.edits, Limits::default())
            .unwrap()
            .apply()
            .unwrap(),
        "b🦀"
    );
}
#[test]
fn diagnostic_invalid_labels_fixes_and_aggregate_budgets() {
    let i = index("a🦀");
    let mut d = diagnostic();
    d.labels[1].span = b(2, 5);
    assert_eq!(
        Diagnostics::new(&i, &[d], Limits::default()).unwrap_err(),
        Error::Utf8Boundary
    );
    let mut d = diagnostic();
    d.labels[0].primary = false;
    assert_eq!(
        Diagnostics::new(&i, &[d], Limits::default()).unwrap_err(),
        Error::InvalidDiagnostic
    );
    let mut d = diagnostic();
    d.fixes[0].edits[0].expected = "z".into();
    assert_eq!(
        Diagnostics::new(&i, &[d], Limits::default()).unwrap_err(),
        Error::SourceMismatch
    );
    for (limits, resource) in [
        (
            Limits {
                max_diagnostics: 0,
                ..Limits::default()
            },
            Resource::Diagnostics,
        ),
        (
            Limits {
                max_labels: 1,
                ..Limits::default()
            },
            Resource::Labels,
        ),
        (
            Limits {
                max_fixes: 0,
                ..Limits::default()
            },
            Resource::Fixes,
        ),
        (
            Limits {
                max_edits: 0,
                ..Limits::default()
            },
            Resource::Edits,
        ),
        (
            Limits {
                max_text_bytes: 1,
                ..Limits::default()
            },
            Resource::Text,
        ),
        (
            Limits {
                max_metadata_bytes: 1,
                ..Limits::default()
            },
            Resource::Metadata,
        ),
    ] {
        assert_eq!(
            Diagnostics::new(&i, &[diagnostic()], limits).unwrap_err(),
            Error::Budget(resource)
        );
    }
    assert_eq!(
        Diagnostics::new(
            &i,
            &[diagnostic(), diagnostic()],
            Limits {
                max_labels: 3,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Labels)
    );
}

#[test]
fn maximal_integer_endpoints_and_invalid_configuration_do_not_wrap() {
    let i = index("🦀\r\n");
    assert_eq!(i.position(usize::MAX), Err(Error::ByteOutOfBounds));
    assert_eq!(i.range(b(0, usize::MAX)), Err(Error::ByteOutOfBounds));
    assert_eq!(i.offset(p(0, u32::MAX)), Err(Error::CharacterOutOfBounds));
    assert_eq!(
        SourceEdits::new(
            &i,
            &[],
            Limits {
                max_work: usize::MAX,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::InvalidLimits
    );
    assert_eq!(
        Diagnostics::new(
            &i,
            &[],
            Limits {
                max_metadata_bytes: usize::MAX,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::InvalidLimits
    );
    assert_eq!(
        LineIndex::new(
            Arc::from(""),
            Limits {
                max_lines: 0,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Lines)
    );
}

#[test]
fn edits_reject_utf8_and_crlf_interiors() {
    let i = index("🦀\r\n");
    assert_eq!(
        SourceEdits::new(&i, &[e(1, 1, "", "x")], Limits::default()).unwrap_err(),
        Error::Utf8Boundary
    );
    assert_eq!(
        SourceEdits::new(&i, &[e(5, 5, "", "x")], Limits::default()).unwrap_err(),
        Error::LineEndingInterior
    );
    assert_eq!(
        SourceEdits::new(&i, &[e(4, 6, "\r\n", "\n")], Limits::default())
            .unwrap()
            .apply()
            .unwrap(),
        "🦀\n"
    );
}

#[test]
fn diagnostics_work_and_edits_are_aggregate_not_per_fix() {
    let i = index("a🦀");
    let d = diagnostic();
    let one = Diagnostics::new(
        &i,
        std::slice::from_ref(&d),
        Limits {
            max_work: 150,
            ..Limits::default()
        },
    );
    assert!(one.is_ok());
    assert_eq!(
        Diagnostics::new(
            &i,
            &[d.clone(), d.clone()],
            Limits {
                max_work: 150,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Work)
    );
    assert_eq!(
        Diagnostics::new(
            &i,
            &[d.clone(), d],
            Limits {
                max_edits: 1,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Edits)
    );
    for severity in [
        Severity::Error,
        Severity::Warning,
        Severity::Information,
        Severity::Hint,
    ] {
        let mut d = diagnostic();
        d.severity = severity;
        assert_eq!(
            Diagnostics::new(&i, &[d], Limits::default())
                .unwrap()
                .data()[0]
                .severity,
            severity
        );
    }
}

#[test]
fn diagnostic_input_is_preserved_after_late_failure() {
    let i = index("a🦀");
    let data = vec![diagnostic(), diagnostic()];
    let before = data.clone();
    assert_eq!(
        Diagnostics::new(
            &i,
            &data,
            Limits {
                max_metadata_bytes: 1,
                ..Limits::default()
            }
        )
        .unwrap_err(),
        Error::Budget(Resource::Metadata)
    );
    assert_eq!(data, before);
    assert_eq!(i.text(), "a🦀");
}
