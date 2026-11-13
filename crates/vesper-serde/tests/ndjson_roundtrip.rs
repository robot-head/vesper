//! Independent batch/parallel-column/codec acceptance with exact global errors.
use vesper_serde::event_columns::{EventColumns, ProjectionLimits, project_into};
use vesper_serde::ndjson::{BatchLimits, Error, ErrorKind, EventField, NdjsonBatch};
use vesper_serde::{EncodeLimits, ErrorKind as JsonError, IndexedJson, Limits, Value, encode_into};

fn failed<T>(result: Result<T, Error>) -> Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("unexpected success"),
    }
}
fn parse_batch(input: &[u8]) -> NdjsonBatch<'_> {
    NdjsonBatch::parse(input, BatchLimits::default()).unwrap()
}
const FIRST: &str = "{\"id\":18446744073709551615,\"topic\":\"é🦀\",\"raw_meta\":{\"unknown\":[true,null,18446744073709551615]}}";
const SECOND: &str = "{\"\\u0069d\":9007199254740993,\"to\\u0070ic\":\"line\\n\\uD83E\\uDD80\",\"raw_\\u006deta\":[1,{\"n\":-0}],\"extra\":{\"validated\":true}}";

#[test]
fn actual_unicode_columns_roundtrip_through_scalar_json_without_typed_row_staging() {
    let input = format!("{FIRST}\r\n{SECOND}\n{{\"id\":0,\"topic\":\"\",\"raw_meta\":null}}");
    let batch = parse_batch(input.as_bytes());
    let mut cursor = batch.cursor();
    let mut ids = [77; 5];
    let mut topics = [None; 4];
    let mut raw = [None; 6];
    assert_eq!(
        project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topics,
                raw_meta: &mut raw
            },
            ProjectionLimits::default()
        )
        .unwrap(),
        3
    );
    assert_eq!(cursor.position(), 3);
    assert_eq!(cursor.remaining(), 0);
    assert_eq!(&ids[..3], &[u64::MAX, 9_007_199_254_740_993, 0]);
    assert_eq!(&ids[3..], &[77, 77]);
    assert!(topics[3..].iter().all(Option::is_none));
    assert!(raw[3..].iter().all(Option::is_none));
    assert!(!topics[0].unwrap().is_escaped());
    assert!(topics[1].unwrap().is_escaped());
    let mut encoded = Vec::new();
    for i in 0..3 {
        let mut scratch = [0; 64];
        let topic = topics[i].unwrap().decode(&mut scratch).unwrap();
        assert_eq!(topic.as_str(), ["é🦀", "line\n🦀", ""][i]);
        let fields = [
            ("id", Value::U64(ids[i])),
            ("topic", Value::String(topic.as_str())),
            ("raw_meta", Value::Raw(raw[i].unwrap())),
        ];
        let mut output = [0xd4; 1024];
        let length = encode_into(
            &Value::Object(&fields),
            &mut output,
            EncodeLimits::default(),
        )
        .unwrap();
        assert!(output[length..].iter().all(|byte| *byte == 0xd4));
        let parsed = IndexedJson::parse(&output[..length], Limits::default()).unwrap();
        assert_eq!(
            parsed
                .root()
                .field("id", 4096)
                .unwrap()
                .unwrap()
                .as_u64()
                .unwrap(),
            ids[i]
        );
        assert_eq!(
            parsed
                .root()
                .field("raw_meta", 4096)
                .unwrap()
                .unwrap()
                .raw(),
            raw[i].unwrap().raw()
        );
        encoded.extend_from_slice(&output[..length]);
        encoded.push(b'\n');
    }
    let restored = parse_batch(encoded.as_slice());
    let mut restored_cursor = restored.cursor();
    for (id, expected) in [
        (u64::MAX, "é🦀"),
        (9_007_199_254_740_993, "line\n🦀"),
        (0, ""),
    ] {
        let row = restored_cursor
            .read_event(ProjectionLimits::default())
            .unwrap();
        assert_eq!(row.id, id);
        assert!(row.topic.equals(expected));
    }
}

#[test]
fn late_unknown_malformed_utf8_and_decoded_duplicates_prevent_batch_publication() {
    let prefix = format!("{FIRST}\r\n{SECOND}\n");
    for tail in [
        r#"{"id":1,"topic":"x","raw_meta":{},"unknown":[1,]}"#,
        r#"{"id":1,"topic":"x","raw_meta":{"n":1,"\u006e":2}}"#,
        r#"{"id":1,"topic":"x","raw_meta":"\uD800"}"#,
    ] {
        let local = IndexedJson::parse(tail.as_bytes(), Limits::default()).unwrap_err();
        let input = prefix.clone() + tail;
        let error = failed(NdjsonBatch::parse(input.as_bytes(), BatchLimits::default()));
        assert_eq!(error.kind, ErrorKind::Json(local.kind));
        assert_eq!(error.offset, prefix.len() + local.offset);
        assert_eq!((error.record, error.line), (2, 3));
    }
    let mut input = prefix.as_bytes().to_vec();
    let tail = b"{\"id\":1,\"topic\":\"\xff\",\"raw_meta\":null}";
    input.extend_from_slice(tail);
    let local = IndexedJson::parse(tail, Limits::default()).unwrap_err();
    let error = failed(NdjsonBatch::parse(&input, BatchLimits::default()));
    assert_eq!(error.kind, ErrorKind::Json(JsonError::Utf8));
    assert_eq!(error.offset, prefix.len() + local.offset);
    assert_eq!((error.record, error.line), (2, 3));
}

#[test]
fn late_schema_errors_preserve_existing_columns_cursor_and_exact_unicode_offsets() {
    for (tail, kind, token) in [
        (
            r#"{"topic":"x","raw_meta":null}"#,
            ErrorKind::MissingField(EventField::Id),
            None,
        ),
        (
            r#"{"id":"bad","raw_meta":null}"#,
            ErrorKind::MissingField(EventField::Topic),
            None,
        ),
        (
            r#"{"id":"bad","topic":0}"#,
            ErrorKind::MissingField(EventField::RawMeta),
            None,
        ),
        (
            r#"{"id":"bad","topic":"x","raw_meta":null}"#,
            ErrorKind::Json(JsonError::Type),
            Some("\"bad\""),
        ),
        (
            r#"{"id":18446744073709551616,"topic":"x","raw_meta":null}"#,
            ErrorKind::Json(JsonError::Number),
            Some("18446744073709551616"),
        ),
        (
            r#"{"id":1e0,"topic":"x","raw_meta":null}"#,
            ErrorKind::Json(JsonError::Number),
            Some("1e0"),
        ),
        (
            r#"{"id":1,"topic":42,"raw_meta":null}"#,
            ErrorKind::Json(JsonError::Type),
            Some("42"),
        ),
    ] {
        let prefix = format!("{FIRST}\r\n");
        let source = prefix.clone() + tail;
        let batch = parse_batch(source.as_bytes());
        let seed = batch
            .cursor()
            .read_event(ProjectionLimits::default())
            .unwrap();
        let mut cursor = batch.cursor();
        let mut ids = [77; 4];
        let mut topics = [Some(seed.topic); 4];
        let mut raw = [Some(seed.raw_meta); 4];
        let error = failed(project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topics,
                raw_meta: &mut raw,
            },
            ProjectionLimits::default(),
        ));
        assert_eq!(error.kind, kind);
        assert_eq!((error.record, error.line), (1, 2));
        assert_eq!(
            error.offset,
            prefix.len() + token.map_or(0, |token| tail.find(token).unwrap())
        );
        assert_eq!(cursor.position(), 0);
        assert_eq!(ids, [77; 4]);
        assert!(
            topics
                .iter()
                .all(|value| value.unwrap().raw_content() == seed.topic.raw_content())
        );
        assert!(
            raw.iter()
                .all(|value| value.unwrap().raw() == seed.raw_meta.raw())
        );
        let first = cursor.read_event(ProjectionLimits::default()).unwrap();
        assert_eq!(first.id, u64::MAX);
        let failure = failed(cursor.read_event(ProjectionLimits::default()));
        assert_eq!(failure, error);
        assert_eq!(cursor.position(), 1);
        assert!(first.topic.equals("é🦀"));
        assert_eq!(first.raw_meta.raw(), seed.raw_meta.raw());
    }
}

#[test]
fn strict_framing_empty_and_physical_eof_positions_are_not_guessed() {
    for input in [b"0".as_slice(), b"0\n", b"0\r\n", b"\r 0\r", b"\r 0\r\n"] {
        let batch = parse_batch(input);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch.get(0).unwrap().line(), 1);
        let mut cursor = batch.cursor();
        assert_eq!(cursor.read_raw().unwrap().root().as_u64().unwrap(), 0);
        let error = failed(cursor.read_raw());
        assert_eq!(error.kind, ErrorKind::End);
        assert_eq!(error.offset, input.len());
        assert_eq!(error.record, 1);
        assert_eq!(
            error.line,
            input.iter().filter(|byte| **byte == b'\n').count() + 1
        );
        assert_eq!(cursor.position(), 1);
    }
    let empty = parse_batch(b"");
    assert!(empty.is_empty());
    let mut cursor = empty.cursor();
    let mut ids = [77; 1];
    let mut topics = [None; 1];
    let mut raw = [None; 1];
    assert_eq!(
        project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topics,
                raw_meta: &mut raw
            },
            ProjectionLimits::default()
        )
        .unwrap(),
        0
    );
    assert_eq!(ids, [77]);
    assert!(topics[0].is_none() && raw[0].is_none());
    for input in [b"\n".as_slice(), b" \t\r\n", b"0\n\n", b"0\r\n\r\n"] {
        assert_eq!(
            failed(NdjsonBatch::parse(input, BatchLimits::default())).kind,
            ErrorKind::BlankLine
        );
    }
    assert!(matches!(
        failed(NdjsonBatch::parse(b"0\r0", BatchLimits::default())).kind,
        ErrorKind::Json(JsonError::Syntax)
    ));
    assert!(NdjsonBatch::parse(b"{\n\"id\":1}\n", BatchLimits::default()).is_err());
    let source = b"0\r\n";
    let batch = parse_batch(source);
    let record = batch.get(0).unwrap();
    assert_eq!(record.payload_span(), 0..1);
    let eof = record.json_error(vesper_serde::Error {
        kind: JsonError::Syntax,
        offset: 1,
    });
    assert_eq!(eof.offset, 1);
    let invalid = record.json_error(vesper_serde::Error {
        kind: JsonError::Syntax,
        offset: 2,
    });
    assert_eq!(invalid.kind, ErrorKind::Overflow);
    assert_eq!(invalid.offset, 0);
}

#[test]
fn declared_two_pass_work_and_capacity_are_prepaid_before_every_column_write() {
    let source = b"{\"id\":1,\"topic\":\"x\",\"raw_meta\":null}\n";
    let batch = parse_batch(source);
    // Public schema accounting: record1 + fields(217+433+649) + id2 + topic1.
    let schema_work = 1303;
    let mut cursor = batch.cursor();
    assert_eq!(
        failed(cursor.read_event(ProjectionLimits {
            max_work: schema_work,
            ..ProjectionLimits::default()
        }))
        .kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(cursor.position(), 0);
    assert_eq!(
        cursor
            .read_event(ProjectionLimits {
                max_work: schema_work + 1,
                ..ProjectionLimits::default()
            })
            .unwrap()
            .id,
        1
    );
    let expected = 2 * schema_work + 3 + 1;
    for (limits, expected_error) in [
        (
            ProjectionLimits {
                max_work: expected - 1,
                ..ProjectionLimits::default()
            },
            ErrorKind::WorkLimit,
        ),
        (
            ProjectionLimits {
                max_rows: 0,
                ..ProjectionLimits::default()
            },
            ErrorKind::InvalidLimits,
        ),
    ] {
        let mut cursor = batch.cursor();
        let mut ids = [77; 2];
        let mut topics = [None; 2];
        let mut raw = [None; 2];
        assert_eq!(
            failed(project_into(
                &mut cursor,
                EventColumns {
                    ids: &mut ids,
                    topics: &mut topics,
                    raw_meta: &mut raw
                },
                limits
            ))
            .kind,
            expected_error
        );
        assert_eq!(ids, [77; 2]);
        assert!(topics.iter().all(Option::is_none) && raw.iter().all(Option::is_none));
        assert_eq!(cursor.position(), 0);
    }
    let mut cursor = batch.cursor();
    let mut ids = [77; 2];
    let mut topics = [None; 3];
    let mut raw = [None; 4];
    assert_eq!(
        project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topics,
                raw_meta: &mut raw
            },
            ProjectionLimits {
                max_work: expected,
                ..ProjectionLimits::default()
            }
        )
        .unwrap(),
        1
    );
    assert_eq!(ids, [1, 77]);
    assert!(topics[1..].iter().all(Option::is_none) && raw[1..].iter().all(Option::is_none));
    let mut no_ids = [];
    let mut no_topics = [];
    let mut no_raw = [];
    let error = failed(project_into(
        &mut cursor,
        EventColumns {
            ids: &mut no_ids,
            topics: &mut no_topics,
            raw_meta: &mut no_raw,
        },
        ProjectionLimits {
            max_rows: 0,
            ..ProjectionLimits::default()
        },
    ));
    assert_eq!(error.kind, ErrorKind::InvalidLimits);
    assert_eq!(
        (error.offset, error.record, error.line),
        (source.len(), 1, 2)
    );
    let two_rows = [source.as_slice(), source.as_slice()].concat();
    let two = parse_batch(&two_rows);
    let two_work = 4 * schema_work + 6 + 1;
    for allowance in [expected, two_work - 1] {
        let mut cursor = two.cursor();
        let mut ids = [77; 3];
        let mut topics = [None; 3];
        let mut raw = [None; 3];
        let error = failed(project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topics,
                raw_meta: &mut raw,
            },
            ProjectionLimits {
                max_work: allowance,
                ..ProjectionLimits::default()
            },
        ));
        assert_eq!(error.kind, ErrorKind::WorkLimit);
        assert_eq!(ids, [77; 3]);
        assert!(topics.iter().all(Option::is_none) && raw.iter().all(Option::is_none));
        assert_eq!(cursor.position(), 0);
    }
    let mut cursor = two.cursor();
    let mut ids = [77; 3];
    let mut topics = [None; 3];
    let mut raw = [None; 3];
    assert_eq!(
        project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topics,
                raw_meta: &mut raw
            },
            ProjectionLimits {
                max_work: two_work,
                ..ProjectionLimits::default()
            },
        )
        .unwrap(),
        2
    );
    assert_eq!(ids, [1, 1, 77]);
    assert!(topics[2].is_none() && raw[2].is_none());
    let source = format!("{FIRST}\n{SECOND}");
    let batch = parse_batch(source.as_bytes());
    for (limits, capacity, kind) in [
        (
            ProjectionLimits {
                max_rows: 1,
                ..ProjectionLimits::default()
            },
            0,
            ErrorKind::RecordLimit,
        ),
        (ProjectionLimits::default(), 1, ErrorKind::OutputTooSmall),
    ] {
        let mut cursor = batch.cursor();
        let mut ids = [77; 3];
        let mut topics = [None; 3];
        let mut raw = [None; 3];
        let error = failed(project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids[..capacity],
                topics: &mut topics,
                raw_meta: &mut raw,
            },
            limits,
        ));
        assert_eq!(error.kind, kind);
        assert_eq!(ids, [77; 3]);
        assert!(topics.iter().all(Option::is_none) && raw.iter().all(Option::is_none));
        assert_eq!(cursor.position(), 0);
    }
}

#[test]
fn aggregate_budgets_cover_all_records_without_fresh_per_line_allowances() {
    let input = b"0\n0\n0";
    let successful = parse_batch(input);
    let stats = successful.stats();
    assert_eq!((stats.records, stats.tokens), (3, 3));
    for (limits, allowed) in [
        (
            BatchLimits {
                max_input_bytes: input.len() - 1,
                ..BatchLimits::default()
            },
            &[ErrorKind::InputLimit][..],
        ),
        (
            BatchLimits {
                max_records: 2,
                ..BatchLimits::default()
            },
            &[ErrorKind::RecordLimit][..],
        ),
        (
            BatchLimits {
                max_metadata_bytes: stats.metadata_bytes - 1,
                ..BatchLimits::default()
            },
            &[ErrorKind::MetadataLimit][..],
        ),
        (
            BatchLimits {
                max_tokens: 2,
                ..BatchLimits::default()
            },
            &[
                ErrorKind::TokenLimit,
                ErrorKind::Json(JsonError::TokenLimit),
                ErrorKind::Json(JsonError::AllocationLimit),
            ][..],
        ),
        (
            BatchLimits {
                max_index_bytes: stats.index_bytes - 1,
                ..BatchLimits::default()
            },
            &[
                ErrorKind::IndexLimit,
                ErrorKind::Json(JsonError::AllocationLimit),
            ][..],
        ),
        (
            BatchLimits {
                max_work: stats.work - 1,
                ..BatchLimits::default()
            },
            &[ErrorKind::WorkLimit, ErrorKind::Json(JsonError::WorkLimit)][..],
        ),
    ] {
        let error = failed(NdjsonBatch::parse(input, limits));
        assert!(allowed.contains(&error.kind), "{error:?}");
    }
    let exact = NdjsonBatch::parse(
        input,
        BatchLimits {
            max_input_bytes: input.len(),
            max_records: 3,
            max_tokens: stats.tokens,
            max_index_bytes: stats.index_bytes,
            max_metadata_bytes: stats.metadata_bytes,
            max_work: stats.work,
            ..BatchLimits::default()
        },
    )
    .unwrap();
    assert_eq!(exact.stats().work, stats.work);
    assert_eq!(exact.stats().index_bytes, stats.index_bytes);
    assert_eq!(
        failed(NdjsonBatch::parse(
            b"",
            BatchLimits {
                max_tokens: 0,
                ..BatchLimits::default()
            }
        ))
        .kind,
        ErrorKind::InvalidLimits
    );
}
