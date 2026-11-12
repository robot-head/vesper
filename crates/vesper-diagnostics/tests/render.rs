use std::sync::Arc;
use vesper_core::ast::Diagnostic;
use vesper_diagnostics::{Error, Limits, prepare};
use vesper_span::{ByteSpan, LineIndex, Resource};
fn index(source: &str) -> LineIndex {
    LineIndex::new(Arc::from(source), Limits::default().span).unwrap()
}
fn budget(error: Error, resource: Resource) {
    assert_eq!(error, Error::Span(vesper_span::Error::Budget(resource)));
}
#[test]
fn actual_utf8_bytes_become_utf16_ranges_and_all_actual_labels() {
    let text = "😀let 名 = 1;\r\nuse(名);\n";
    let index = index(text);
    let first = text.find('名').unwrap();
    let second = text.rfind('名').unwrap();
    let diagnostics = [
        Diagnostic::new("E0300", "already consumed", second..second + 3)
            .with_related(first..first + 3, "binding declared here")
            .with_related(0..4, "actual earlier event")
            .with_help("Consume exactly once."),
    ];
    let prepared = prepare(&index, &diagnostics, Limits::default()).unwrap();
    let values = prepared.to_lsp("untitled:😀").unwrap();
    let d = &values[0];
    assert_eq!(d["code"], "E0300");
    assert_eq!(d["message"], "already consumed");
    assert_eq!(d["severity"], 1);
    assert_eq!(d["range"]["start"]["line"], 1);
    assert_eq!(d["range"]["start"]["character"], 4);
    assert_eq!(d["range"]["end"]["character"], 5);
    assert_eq!(
        d["relatedInformation"][0]["location"]["range"]["start"]["character"],
        6
    );
    assert_eq!(
        d["relatedInformation"][1]["location"]["range"]["end"]["character"],
        2
    );
    assert_eq!(d["relatedInformation"][0]["location"]["uri"], "untitled:😀");
    assert_eq!(d["data"]["help"], "Consume exactly once.");
    assert!(d.get("fixes").is_none());
    let terminal = String::from_utf8(prepared.terminal("unicode.vsp").unwrap()).unwrap();
    for s in [
        "unicode.vsp:2:5",
        "E0300",
        "already consumed",
        "binding declared here",
        "actual earlier event",
        "Consume exactly once.",
    ] {
        assert!(terminal.contains(s), "missing {s}: {terminal}");
    }
    assert!(!terminal.contains('\x1b'));
    assert_eq!(
        terminal.as_bytes(),
        prepared.terminal("unicode.vsp").unwrap()
    );
    assert_eq!(values, prepared.to_lsp("untitled:😀").unwrap());
}
#[test]
fn empty_legacy_metadata_is_omitted_and_input_mutation_cannot_change_prepared() {
    let index = index("abc");
    let mut data = [Diagnostic::new("E1", "message", 0..1)];
    let prepared = prepare(&index, &data, Limits::default()).unwrap();
    data[0].message = "changed".into();
    data[0].help = Some("new help".into());
    let values = prepared.to_lsp("opaque:x").unwrap();
    assert!(values[0].get("relatedInformation").is_none());
    assert!(values[0].get("data").is_none());
    assert_eq!(values[0]["message"], "message");
    assert!(prepared.data()[0].fixes.is_empty());
    assert_eq!(prepared.source(), "abc");
}
#[test]
fn eof_empty_crlf_lone_terminators_and_no_trailing_terminator() {
    for (source, line, column) in [
        ("", 0, 0),
        ("😀", 0, 2),
        ("a\r\n", 1, 0),
        ("a\r", 1, 0),
        ("a\n", 1, 0),
        ("a\n\n", 2, 0),
    ] {
        let index = index(source);
        let diagnostics = [Diagnostic::new(
            "E0",
            "expected token",
            source.len()..source.len(),
        )];
        let prepared = prepare(&index, &diagnostics, Limits::default()).unwrap();
        let value = prepared.to_lsp("opaque:x").unwrap();
        assert_eq!(value[0]["range"]["start"]["line"], line);
        assert_eq!(value[0]["range"]["start"]["character"], column);
        let terminal = String::from_utf8(prepared.terminal("eof.vsp").unwrap()).unwrap();
        let terminal_col = if source == "😀" { 2 } else { column + 1 };
        assert!(
            terminal.contains(&format!("eof.vsp:{}:{}", line + 1, terminal_col)),
            "{source:?}: {terminal}"
        );
        assert!(terminal.contains("expected token"), "{terminal}");
    }
}
#[test]
fn invalid_primary_related_utf8_crlf_and_reversed_spans_never_clamp() {
    let index = index("a😀\r\nb");
    for span in [
        1..2,
        2..5,
        std::ops::Range { start: 7, end: 6 },
        0..99,
        6..7,
    ] {
        for related in [false, true] {
            let mut diagnostic = Diagnostic::new("E1", "bad", 0..1);
            if related {
                diagnostic = diagnostic.with_related(span.clone(), "cause");
            } else {
                diagnostic.span = span.clone();
            }
            let before = diagnostic.clone();
            assert!(prepare(&index, &[diagnostic.clone()], Limits::default()).is_err());
            assert_eq!(diagnostic, before);
        }
    }
    let valid = [Diagnostic::new("E1", "line ending", 5..7).with_related(7..8, "next line")];
    assert!(prepare(&index, &valid, Limits::default()).is_ok());
}
#[test]
fn every_accepted_byte_boundary_has_the_shared_range() {
    let source = "a😀名\r\nz\r\n";
    let index = index(source);
    for start in 0..=source.len() {
        for end in start..=source.len() {
            let data = [Diagnostic::new("E1", "range", start..end)];
            let expected = index.range(ByteSpan { start, end });
            let actual = prepare(&index, &data, Limits::default());
            match expected {
                Ok(range) => {
                    let out = actual.unwrap().to_lsp("x").unwrap();
                    assert_eq!(out[0]["range"]["start"]["line"], range.start.line);
                    assert_eq!(out[0]["range"]["start"]["character"], range.start.character);
                    assert_eq!(out[0]["range"]["end"]["line"], range.end.line);
                    assert_eq!(out[0]["range"]["end"]["character"], range.end.character);
                }
                Err(e) => assert_eq!(actual.unwrap_err(), Error::Span(e)),
            }
        }
    }
}
#[test]
fn aggregate_limits_are_checked_on_every_label_and_help() {
    let index = index("abcd");
    let data = [Diagnostic::new("E", "msg", 0..1)
        .with_related(1..2, "cause")
        .with_help("help")];
    let defaults = Limits::default();
    for (span, resource) in [
        (
            vesper_span::Limits {
                max_diagnostics: 0,
                ..defaults.span
            },
            Resource::Diagnostics,
        ),
        (
            vesper_span::Limits {
                max_labels: 1,
                ..defaults.span
            },
            Resource::Labels,
        ),
        (
            vesper_span::Limits {
                max_text_bytes: 12,
                ..defaults.span
            },
            Resource::Text,
        ),
        (
            vesper_span::Limits {
                max_metadata_bytes: 1,
                ..defaults.span
            },
            Resource::Metadata,
        ),
        (
            vesper_span::Limits {
                max_work: 1,
                ..defaults.span
            },
            Resource::Work,
        ),
        (
            vesper_span::Limits {
                max_source_bytes: 3,
                ..defaults.span
            },
            Resource::Source,
        ),
    ] {
        budget(
            prepare(&index, &data, Limits { span, ..defaults }).unwrap_err(),
            resource,
        );
    }
    let cap = 1 + 3 + 5 + 4;
    let p = prepare(
        &index,
        &data,
        Limits {
            span: vesper_span::Limits {
                max_text_bytes: cap,
                max_labels: 2,
                max_diagnostics: 1,
                ..defaults.span
            },
            ..defaults
        },
    )
    .unwrap();
    assert_eq!(p.to_lsp("x").unwrap().len(), 1);
    for data in [
        [Diagnostic::new("", "msg", 0..0)],
        [Diagnostic::new("E", "", 0..0)],
    ] {
        assert_eq!(
            prepare(&index, &data, defaults).unwrap_err(),
            Error::Span(vesper_span::Error::InvalidDiagnostic)
        );
    }
}
#[test]
fn exact_lsp_size_accounts_escapes_unicode_related_and_help() {
    let index = index("😀\r\na");
    let text = (0u8..32).map(char::from).collect::<String>() + "\\\"😀";
    let diagnostics = [
        Diagnostic::new("E\"", text.clone(), 0..4)
            .with_related(6..7, &text)
            .with_help(&text),
        Diagnostic::new("E2", "next", 7..7).with_help(""),
    ];
    let limits = Limits::default();
    let out = prepare(&index, &diagnostics, limits)
        .unwrap()
        .to_lsp("opaque:\"\\😀")
        .unwrap();
    let size = serde_json::to_vec(&out).unwrap().len();
    let exact = Limits {
        span: vesper_span::Limits {
            max_output_bytes: size,
            ..limits.span
        },
        ..limits
    };
    assert_eq!(
        prepare(&index, &diagnostics, exact)
            .unwrap()
            .to_lsp("opaque:\"\\😀")
            .unwrap(),
        out
    );
    assert_eq!(
        prepare(
            &index,
            &diagnostics,
            Limits {
                span: vesper_span::Limits {
                    max_output_bytes: size - 1,
                    ..limits.span
                },
                ..limits
            }
        )
        .unwrap()
        .to_lsp("opaque:\"\\😀")
        .unwrap_err(),
        Error::Output
    );
}
#[test]
fn terminal_output_limits_are_exact_without_partial_results() {
    let index = index("a😀b");
    let data = [Diagnostic::new("E1", "oops", 1..5)
        .with_related(0..1, "origin")
        .with_help("help")];
    let limits = Limits::default();
    let bytes = prepare(&index, &data, limits)
        .unwrap()
        .terminal("source.vsp")
        .unwrap();
    let exact = Limits {
        span: vesper_span::Limits {
            max_output_bytes: bytes.len(),
            ..limits.span
        },
        ..limits
    };
    assert_eq!(
        prepare(&index, &data, exact)
            .unwrap()
            .terminal("source.vsp")
            .unwrap(),
        bytes
    );
    assert_eq!(
        prepare(
            &index,
            &data,
            Limits {
                span: vesper_span::Limits {
                    max_output_bytes: bytes.len() - 1,
                    ..limits.span
                },
                ..limits
            }
        )
        .unwrap()
        .terminal("source.vsp")
        .unwrap_err(),
        Error::Output
    );
}
#[test]
fn names_limits_and_empty_diagnostics() {
    let index = index("");
    let p = prepare(&index, &[], Limits::default()).unwrap();
    assert_eq!(p.to_lsp("x").unwrap(), Vec::<serde_json::Value>::new());
    assert!(p.terminal("x").unwrap().is_empty());
    for name in ["", "bad\nname", "bad\0name", "bad\u{85}name"] {
        assert_eq!(p.to_lsp(name).unwrap_err(), Error::Name);
        assert_eq!(p.terminal(name).unwrap_err(), Error::Name);
    }
    let long = "x".repeat(4097);
    assert_eq!(p.to_lsp(&long).unwrap_err(), Error::Name);
    assert_eq!(p.terminal(&long).unwrap_err(), Error::Name);
    assert_eq!(
        Limits {
            max_name_bytes: usize::MAX,
            ..Limits::default()
        }
        .validate()
        .unwrap_err(),
        Error::InvalidLimits
    );
    assert!(
        Limits {
            span: vesper_span::Limits {
                max_labels: usize::MAX,
                ..Limits::default().span
            },
            ..Limits::default()
        }
        .validate()
        .is_err()
    );
}
#[test]
fn terminal_refuses_extra_ariadne_line_separators_but_lsp_is_correct() {
    for separator in ['\x0b', '\x0c', '\u{85}', '\u{2028}', '\u{2029}'] {
        let source = format!("a{separator}b");
        let index = index(&source);
        let start = source.len() - 1;
        let d = [Diagnostic::new("E", "bad", start..source.len())];
        let p = prepare(&index, &d, Limits::default()).unwrap();
        assert_eq!(p.terminal("x").unwrap_err(), Error::UnsupportedLineEnding);
        assert_eq!(p.to_lsp("x").unwrap()[0]["range"]["start"]["line"], 0);
        assert_eq!(p.to_lsp("x").unwrap()[0]["range"]["start"]["character"], 2);
    }
}
#[test]
fn hard_cli_source_and_work_peak_refusal_are_explicit() {
    let source = "x".repeat(1024 * 1024);
    let hard_index = LineIndex::new(Arc::from(source.as_str()), Limits::HARD.span).unwrap();
    let d = [Diagnostic::new("E", "bad", 0..1)];
    budget(
        prepare(&hard_index, &d, Limits::default()).unwrap_err(),
        Resource::Source,
    );
    let p = prepare(
        &hard_index,
        &d,
        Limits {
            span: vesper_span::Limits {
                max_output_bytes: 65536,
                ..Limits::HARD.span
            },
            ..Limits::HARD
        },
    )
    .unwrap();
    assert_eq!(p.to_lsp("x").unwrap().len(), 1);
    assert!(p.terminal("x").is_err()); // full line exceeds bounded output
    let source = "x".repeat(128 * 1024);
    let index = index(&source);
    let data = (0..256)
        .map(|_| Diagnostic::new("E", "bad", 0..1))
        .collect::<Vec<_>>();
    budget(
        prepare(&index, &data, Limits::default()).unwrap_err(),
        Resource::Work,
    );
}

#[test]
fn related_zero_width_eof_labels_are_not_lost() {
    for text in ["", "😀", "x\r\n"] {
        let index = index(text);
        let at = text.len();
        let data = [Diagnostic::new("E", "primary", at..at)
            .with_related(at..at, "actual EOF cause")
            .with_help("EOF help")];
        let prepared = prepare(&index, &data, Limits::default()).unwrap();
        let output = String::from_utf8(prepared.terminal("eof.vsp").unwrap()).unwrap();
        assert!(output.contains("actual EOF cause"), "{text:?}: {output}");
        assert!(output.contains("EOF help"));
        let values = prepared.to_lsp("opaque:eof").unwrap();
        assert_eq!(
            values[0]["range"],
            values[0]["relatedInformation"][0]["location"]["range"]
        );
    }
}

#[test]
fn existing_index_must_fit_renderer_line_profile() {
    let index = index("a\n");
    let d = [Diagnostic::new("E", "message", 0..1)];
    let limits = Limits {
        span: vesper_span::Limits {
            max_lines: 1,
            ..Limits::default().span
        },
        ..Limits::default()
    };
    budget(prepare(&index, &d, limits).unwrap_err(), Resource::Lines);
    let empty = index_empty();
    let limits = Limits {
        span: vesper_span::Limits {
            max_lines: 0,
            ..Limits::default().span
        },
        ..Limits::default()
    };
    budget(prepare(&empty, &[], limits).unwrap_err(), Resource::Lines);
}
fn index_empty() -> LineIndex {
    index("")
}

#[test]
fn combining_marks_count_utf16_units_without_grapheme_guessing() {
    let index = index("😀e\u{301}名");
    let diagnostics =
        [Diagnostic::new("E", "combining mark", 5..7).with_related(4..7, "base and mark")];
    let p = prepare(&index, &diagnostics, Limits::default()).unwrap();
    let out = p.to_lsp("opaque:unicode").unwrap();
    assert_eq!(out[0]["range"]["start"]["character"], 3);
    assert_eq!(out[0]["range"]["end"]["character"], 4);
    assert_eq!(
        out[0]["relatedInformation"][0]["location"]["range"]["start"]["character"],
        2
    );
    let text = String::from_utf8(p.terminal("unicode.vsp").unwrap()).unwrap();
    assert!(text.contains("combining mark"));
    assert!(text.contains("base and mark"));
}
