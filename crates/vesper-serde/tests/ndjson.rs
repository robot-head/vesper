use vesper_serde::{
    Error as JsonError, ErrorKind as JsonErrorKind, IndexedJson, Kind, Limits,
    ndjson::{BatchLimits, BatchStats, Error, ErrorKind, NdjsonBatch},
};

fn parse(input: &[u8]) -> NdjsonBatch<'_> {
    NdjsonBatch::parse(input, BatchLimits::default()).unwrap()
}

#[test]
fn whole_validated_batch_keeps_actual_spans_lines_roots_and_borrowed_input() {
    let input = " \t{\"topic\":\"héllo 😀\"} \r\n[1,2]\ntrue";
    let batch = parse(input.as_bytes());
    assert_eq!(batch.len(), 3);
    assert!(!batch.is_empty());
    let first_end = input.find("\r\n").unwrap();
    let second_start = first_end + 2;
    let second_end = input[second_start..].find('\n').unwrap() + second_start;
    for (ordinal, span, raw, kind) in [
        (0, 0..first_end, "{\"topic\":\"héllo 😀\"}", Kind::Object),
        (1, second_start..second_end, "[1,2]", Kind::Array),
        (2, second_end + 1..input.len(), "true", Kind::Bool),
    ] {
        let record = batch.get(ordinal).unwrap();
        assert_eq!(record.ordinal(), ordinal);
        assert_eq!(record.line(), ordinal + 1);
        assert_eq!(record.payload_span(), span);
        assert_eq!(record.root().kind(), kind);
        assert_eq!(record.root().raw(), raw);
    }
    assert!(batch.get(3).is_none());
    assert!(batch.get(usize::MAX).is_none());
    let topic = batch
        .get(0)
        .unwrap()
        .root()
        .field("topic", 1000)
        .unwrap()
        .unwrap()
        .string()
        .unwrap();
    let decoded = topic.decode(&mut []).unwrap();
    assert_eq!(decoded.as_str(), "héllo 😀");
    assert_eq!(
        decoded.as_str().as_ptr(),
        input.as_ptr().wrapping_add(input.find("héllo").unwrap())
    );
    assert_eq!(batch.stats().records, 3);
}

#[test]
fn empty_and_optional_final_lf_crlf_have_no_virtual_blank_record() {
    let empty = parse(b"");
    assert!(empty.is_empty());
    assert_eq!(empty.stats(), BatchStats::default());
    assert!(empty.get(0).is_none());
    for input in [b"{}".as_slice(), b"{}\n", b"{}\r\n"] {
        let batch = parse(input);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.get(0).unwrap().root().raw(), "{}");
        assert_eq!(batch.get(0).unwrap().payload_span(), 0..2);
    }
}

#[test]
fn every_actual_blank_frame_is_rejected_including_extra_trailing_blanks() {
    for (input, offset, record, line) in [
        (b"\n".as_slice(), 0, 0, 1),
        (b"\r\n", 0, 0, 1),
        (b" \t\r", 0, 0, 1),
        (b"\n{}", 0, 0, 1),
        (b"{}\n\n", 3, 1, 2),
        (b"{}\r\n \t\r\n", 4, 1, 2),
        (b"{}\n \t", 3, 1, 2),
    ] {
        assert_eq!(
            NdjsonBatch::parse(input, BatchLimits::default()).unwrap_err(),
            Error {
                kind: ErrorKind::BlankLine,
                offset,
                record,
                line
            }
        );
    }
}

#[test]
fn only_paired_cr_is_stripped_and_escaped_newlines_do_not_split_records() {
    let input = b"\r0\r\n\t1\r";
    let batch = parse(input);
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.get(0).unwrap().payload_span(), 0..2);
    assert_eq!(batch.get(1).unwrap().payload_span(), 4..7);
    assert_eq!(batch.get(0).unwrap().root().raw(), "0");
    assert_eq!(batch.get(1).unwrap().root().raw(), "1");
    let escaped = parse(b"{\"s\":\"a\\nb\"}\n2");
    assert_eq!(escaped.len(), 2);
    let mut scratch = [0; 3];
    assert_eq!(
        escaped
            .get(0)
            .unwrap()
            .root()
            .field("s", 1000)
            .unwrap()
            .unwrap()
            .string()
            .unwrap()
            .decode(&mut scratch)
            .unwrap()
            .as_str(),
        "a\nb"
    );
    for input in [
        b"{\n\"id\":1}".as_slice(),
        b"{\"s\":\"a\nb\"}",
        b"{\"s\":\"a\rb\"}\n",
    ] {
        assert!(matches!(
            NdjsonBatch::parse(input, BatchLimits::default())
                .unwrap_err()
                .kind,
            ErrorKind::Json(_)
        ));
    }
}

#[test]
fn late_unknown_json_and_unicode_errors_prevent_every_batch_view() {
    for payload in [
        br#"{"known":42,"unknown":[tru]}"#.as_slice(),
        br#"{"known":42,"unknown":{"x":1,"\u0078":2}}"#,
        br#"{"known":42,"unknown":"\uD800"}"#,
        b"{\"unknown\":\"\xff\"}",
        b"[1,]",
        b"1e+",
        b"NaN",
        b"{} trailing",
    ] {
        let input = [b"{}\r\n0\n".as_slice(), payload].concat();
        let expected = IndexedJson::parse(payload, Limits::default()).unwrap_err();
        let error = NdjsonBatch::parse(&input, BatchLimits::default()).unwrap_err();
        assert_eq!(
            error,
            Error {
                kind: ErrorKind::Json(expected.kind),
                offset: 6 + expected.offset,
                record: 2,
                line: 3
            }
        );
    }
}

#[test]
fn record_error_relocation_accepts_eof_and_rejects_out_of_range_diagnostic_data() {
    let input = "0\r\n {\"😀\":\"bad\"} \r\n";
    let batch = parse(input.as_bytes());
    let record = batch.get(1).unwrap();
    let payload = record.payload_span();
    let error = record
        .root()
        .field("😀", 1000)
        .unwrap()
        .unwrap()
        .as_u64()
        .unwrap_err();
    assert_eq!(
        record.json_error(error),
        Error {
            kind: ErrorKind::Json(JsonErrorKind::Type),
            offset: input.find("\"bad\"").unwrap(),
            record: 1,
            line: 2
        }
    );
    for offset in [0, payload.len()] {
        assert_eq!(
            record.json_error(JsonError {
                kind: JsonErrorKind::Syntax,
                offset
            }),
            Error {
                kind: ErrorKind::Json(JsonErrorKind::Syntax),
                offset: payload.start + offset,
                record: 1,
                line: 2
            }
        );
    }
    for offset in [payload.len() + 1, usize::MAX] {
        assert_eq!(
            record.json_error(JsonError {
                kind: JsonErrorKind::Syntax,
                offset
            }),
            Error {
                kind: ErrorKind::Overflow,
                offset: payload.start,
                record: 1,
                line: 2
            }
        );
    }
    assert_eq!(record.root().raw(), "{\"😀\":\"bad\"}");
}

#[test]
fn cursor_views_borrow_the_batch_not_the_mutable_iteration_position() {
    let batch = parse(b"[1,2]\n{\"n\":3}\ntrue");
    let mut cursor = batch.cursor();
    let peeked = cursor.peek().unwrap();
    let retained = peeked.root();
    assert_eq!(cursor.position(), 0);
    assert_eq!(cursor.remaining(), 3);
    assert_eq!(cursor.read_raw().unwrap().root().raw(), "[1,2]");
    assert_eq!(cursor.position(), 1);
    let second = cursor.read_raw().unwrap();
    assert_eq!(cursor.position(), 2);
    assert_eq!(second.ordinal(), 1);
    assert_eq!(
        retained.elements().unwrap().read_raw().unwrap().as_u64(),
        Ok(1)
    );
    assert_eq!(
        second.root().field("n", 1000).unwrap().unwrap().as_u64(),
        Ok(3)
    );
    assert_eq!(peeked.root().raw(), "[1,2]");
    assert_eq!(cursor.read_raw().unwrap().root().as_bool(), Ok(true));
    assert_eq!(cursor.remaining(), 0);
    assert!(cursor.peek().is_none());
}

#[test]
fn end_errors_keep_position_and_report_the_physical_eof_line() {
    for (input, records, eof_line) in [
        (b"".as_slice(), 0, 1),
        (b"0", 1, 1),
        (b"0\n", 1, 2),
        (b"0\r\n", 1, 2),
        (b"0\n1", 2, 2),
        (b"0\n1\n", 2, 3),
    ] {
        let batch = parse(input);
        let mut cursor = batch.cursor();
        for _ in 0..records {
            cursor.read_raw().unwrap();
        }
        for _ in 0..2 {
            assert_eq!(
                cursor.read_raw().unwrap_err(),
                Error {
                    kind: ErrorKind::End,
                    offset: input.len(),
                    record: records,
                    line: eof_line
                }
            );
            assert_eq!(cursor.position(), records);
        }
    }
}

#[test]
fn actual_stats_are_aggregate_and_all_exact_boundaries_are_admitted() {
    let input = b"0\n0\n0";
    let batch = parse(input);
    let stats = batch.stats();
    let one = IndexedJson::parse(b"0", Limits::default()).unwrap();
    assert_eq!(stats.records, 3);
    assert_eq!(stats.tokens, one.token_count() * 3);
    assert_eq!(stats.index_bytes, one.index_capacity_bytes() * 3);
    assert!(stats.metadata_bytes > 0);
    assert_eq!(
        stats.work,
        input.len() * 3 + stats.records * 32 + stats.metadata_bytes + one.work_used() * 3
    );
    let exact = BatchLimits {
        max_input_bytes: input.len(),
        max_records: stats.records,
        max_tokens: stats.tokens,
        max_index_bytes: stats.index_bytes,
        max_metadata_bytes: stats.metadata_bytes,
        max_work: stats.work,
        ..Default::default()
    };
    assert_eq!(NdjsonBatch::parse(input, exact).unwrap().stats(), stats);
    for (limits, kind, offset, record, line) in [
        (
            BatchLimits {
                max_input_bytes: input.len() - 1,
                ..exact
            },
            ErrorKind::InputLimit,
            0,
            0,
            1,
        ),
        (
            BatchLimits {
                max_records: 2,
                ..exact
            },
            ErrorKind::RecordLimit,
            0,
            0,
            1,
        ),
        (
            BatchLimits {
                max_tokens: stats.tokens - 1,
                ..exact
            },
            ErrorKind::TokenLimit,
            4,
            2,
            3,
        ),
        (
            BatchLimits {
                max_index_bytes: stats.index_bytes - 1,
                ..exact
            },
            ErrorKind::Json(JsonErrorKind::AllocationLimit),
            4,
            2,
            3,
        ),
        (
            BatchLimits {
                max_metadata_bytes: stats.metadata_bytes - 1,
                ..exact
            },
            ErrorKind::MetadataLimit,
            0,
            0,
            1,
        ),
        (
            BatchLimits {
                max_work: stats.work - 1,
                ..exact
            },
            ErrorKind::Json(JsonErrorKind::WorkLimit),
            4,
            2,
            3,
        ),
    ] {
        assert_eq!(
            NdjsonBatch::parse(input, limits).unwrap_err(),
            Error {
                kind,
                offset,
                record,
                line
            }
        );
    }
}

#[test]
fn individually_valid_records_cannot_replenish_index_token_or_work_allowances() {
    let input = b"0\n0\n0";
    let one = IndexedJson::parse(b"0", Limits::default()).unwrap();
    let batch = parse(input);
    assert_eq!(
        NdjsonBatch::parse(
            input,
            BatchLimits {
                max_index_bytes: one.index_capacity_bytes(),
                ..Default::default()
            }
        )
        .unwrap_err(),
        Error {
            kind: ErrorKind::IndexLimit,
            offset: 2,
            record: 1,
            line: 2
        }
    );
    assert_eq!(
        NdjsonBatch::parse(
            input,
            BatchLimits {
                max_tokens: 1,
                ..Default::default()
            }
        )
        .unwrap_err(),
        Error {
            kind: ErrorKind::TokenLimit,
            offset: 2,
            record: 1,
            line: 2
        }
    );
    let overhead = batch.stats().work - one.work_used() * 3;
    let error = NdjsonBatch::parse(
        input,
        BatchLimits {
            max_work: overhead + one.work_used(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        error,
        Error {
            kind: ErrorKind::WorkLimit,
            offset: 2,
            record: 1,
            line: 2
        }
    );
}

#[test]
fn per_record_input_depth_string_token_index_and_work_guards_are_kept() {
    for (payload, record_limits) in [
        (
            b"[[1]]".as_slice(),
            Limits {
                max_depth: 1,
                ..Default::default()
            },
        ),
        (
            b"\"ab\"",
            Limits {
                max_string_bytes: 1,
                ..Default::default()
            },
        ),
        (
            b"[\"a\",\"bb\"]",
            Limits {
                max_total_string_bytes: 2,
                ..Default::default()
            },
        ),
        (
            b"[1,2]",
            Limits {
                max_tokens: 2,
                ..Default::default()
            },
        ),
        (
            b"[1]",
            Limits {
                max_index_bytes: IndexedJson::parse(b"0", Limits::default())
                    .unwrap()
                    .index_capacity_bytes(),
                ..Default::default()
            },
        ),
        (
            b"true",
            Limits {
                max_work: 2,
                ..Default::default()
            },
        ),
    ] {
        let mut record = BatchLimits::default().record;
        record.max_depth = record.max_depth.min(record_limits.max_depth);
        record.max_string_bytes = record.max_string_bytes.min(record_limits.max_string_bytes);
        record.max_total_string_bytes = record
            .max_total_string_bytes
            .min(record_limits.max_total_string_bytes);
        record.max_tokens = record.max_tokens.min(record_limits.max_tokens);
        record.max_index_bytes = record.max_index_bytes.min(record_limits.max_index_bytes);
        record.max_work = record.max_work.min(record_limits.max_work);
        let local = IndexedJson::parse(payload, record).unwrap_err();
        let input = [b"0\n".as_slice(), payload].concat();
        let error = NdjsonBatch::parse(
            &input,
            BatchLimits {
                record,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error,
            Error {
                kind: ErrorKind::Json(local.kind),
                offset: 2 + local.offset,
                record: 1,
                line: 2
            }
        );
    }
    let error = NdjsonBatch::parse(
        b"0\n123",
        BatchLimits {
            record: Limits {
                max_input_bytes: 2,
                ..BatchLimits::default().record
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        error,
        Error {
            kind: ErrorKind::InputLimit,
            offset: 2,
            record: 1,
            line: 2
        }
    );
}

#[test]
fn zero_and_above_hard_settings_fail_even_for_empty_input() {
    type Field = fn(&mut BatchLimits) -> &mut usize;
    let fields: [Field; 13] = [
        |l| &mut l.max_input_bytes,
        |l| &mut l.max_records,
        |l| &mut l.max_tokens,
        |l| &mut l.max_index_bytes,
        |l| &mut l.max_metadata_bytes,
        |l| &mut l.max_work,
        |l| &mut l.record.max_input_bytes,
        |l| &mut l.record.max_depth,
        |l| &mut l.record.max_tokens,
        |l| &mut l.record.max_string_bytes,
        |l| &mut l.record.max_total_string_bytes,
        |l| &mut l.record.max_work,
        |l| &mut l.record.max_index_bytes,
    ];
    for field in fields {
        let mut limits = BatchLimits::default();
        *field(&mut limits) = 0;
        assert_eq!(
            NdjsonBatch::parse(b"", limits).unwrap_err(),
            Error {
                kind: ErrorKind::InvalidLimits,
                offset: 0,
                record: 0,
                line: 1
            }
        );
        let mut hard = BatchLimits::HARD;
        *field(&mut limits) = *field(&mut hard) + 1;
        assert_eq!(
            limits.validate().unwrap_err().kind,
            ErrorKind::InvalidLimits
        );
    }
    BatchLimits::HARD.validate().unwrap();
}

#[test]
fn every_tail_truncation_keeps_existing_complete_json_validation() {
    let payload = br#"{"a":[null,true,{"b":"\uD83E\uDD80"}]}"#;
    for end in 1..payload.len() {
        let tail = &payload[..end];
        let local = IndexedJson::parse(tail, Limits::default()).unwrap_err();
        let input = [b"42\r\n".as_slice(), tail].concat();
        assert_eq!(
            NdjsonBatch::parse(&input, BatchLimits::default()).unwrap_err(),
            Error {
                kind: ErrorKind::Json(local.kind),
                offset: 4 + local.offset,
                record: 1,
                line: 2
            }
        );
    }
}
