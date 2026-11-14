use vesper_serde::{
    ErrorKind as JsonError, IndexedJson, JsonStr, JsonString, Kind, Limits, Node,
    event_columns::{EventColumns, ProjectionLimits, project_into},
    ndjson::{BatchLimits, ErrorKind, EventField, NdjsonBatch},
};

const SIMPLE: &str = "{\"id\":7,\"topic\":\"t\",\"raw_meta\":null}";
fn batch(source: &str) -> NdjsonBatch<'_> {
    NdjsonBatch::parse(source.as_bytes(), BatchLimits::default()).unwrap()
}
fn topics<'a, const N: usize>(
    values: [Option<JsonString<'a>>; N],
) -> [Option<(&'a str, *const u8, bool, usize)>; N] {
    values.map(|value| {
        value.map(|s| {
            (
                s.raw_content(),
                s.raw_content().as_ptr(),
                s.is_escaped(),
                s.decoded_len(),
            )
        })
    })
}
fn metadata<'a, const N: usize>(
    values: [Option<Node<'_, 'a>>; N],
) -> [Option<(&'a str, *const u8, Kind)>; N] {
    values.map(|value| value.map(|n| (n.raw(), n.raw().as_ptr(), n.kind())))
}

#[test]
fn successful_rows_keep_batch_borrows_after_cursor_advances_and_topic_scratch_is_explicit() {
    let source = concat!(
        "{\"id\":18446744073709551615,\"topic\":\"é🦀\",\"raw_meta\":{\"nested\":[1,true]}}\r\n",
        "{\"id\":2,\"topic\":\"a\\uD83E\\uDD80b\",\"raw_meta\":[null,\"x\"]}\n",
    );
    let doc = batch(source);
    let mut cursor = doc.cursor();
    let first = cursor.read_event(ProjectionLimits::default()).unwrap();
    assert_eq!(cursor.position(), 1);
    let second = cursor.read_event(ProjectionLimits::default()).unwrap();
    assert_eq!(cursor.position(), 2);
    assert_eq!(first.id, u64::MAX);
    assert_eq!(first.raw_meta.raw(), r#"{"nested":[1,true]}"#);
    assert_eq!(second.id, 2);
    assert_eq!(second.raw_meta.raw(), r#"[null,"x"]"#);
    let direct = first.topic.decode(&mut []).unwrap();
    assert!(matches!(direct, JsonStr::Borrowed(_)));
    assert_eq!(direct.as_str(), "é🦀");
    let offset = source.find("é🦀").unwrap();
    assert_eq!(direct.as_str().as_ptr(), source[offset..].as_ptr());
    assert!(second.topic.is_escaped());
    let mut tiny = [0xa5; 5];
    assert_eq!(
        second.topic.decode(&mut tiny).unwrap_err().kind,
        JsonError::ScratchTooSmall
    );
    assert_eq!(tiny, [0xa5; 5]);
    let mut scratch = [0xa5; 10];
    let decoded = second.topic.decode(&mut scratch).unwrap();
    assert!(matches!(decoded, JsonStr::Scratch(_)));
    assert_eq!(decoded.as_str(), "a🦀b");
    assert!(scratch[6..].iter().all(|&b| b == 0xa5));
    let error = cursor.read_event(ProjectionLimits::default()).unwrap_err();
    assert_eq!(
        (error.kind, error.offset, error.record, error.line),
        (ErrorKind::End, source.len(), 2, 3)
    );
    assert_eq!(cursor.position(), 2);
    assert_eq!(first.topic.raw_content(), "é🦀");
}

#[test]
fn escaped_equivalent_required_keys_and_unknown_validated_subtrees_match_once() {
    let source = r#"{"unknown":{"a":[null,true,"\uD83E\uDD80"]},"to\u0070ic":"ok","raw_\u006deta":1e999999,"\u0069d":42}"#;
    let doc = batch(source);
    let mut cursor = doc.cursor();
    let row = cursor.read_event(ProjectionLimits::default()).unwrap();
    assert_eq!(row.id, 42);
    assert!(row.topic.equals("ok"));
    assert_eq!(row.raw_meta.raw(), "1e999999");
    assert_eq!(cursor.remaining(), 0);
}

#[test]
fn every_required_presence_subset_has_fixed_error_precedence_before_conversion() {
    for mask in 0_u8..7 {
        let mut fields = vec![];
        // These types are deliberately invalid: missing presence must win.
        if mask & 1 != 0 {
            fields.push(r#""id":"wrong""#);
        }
        if mask & 2 != 0 {
            fields.push(r#""topic":false"#);
        }
        if mask & 4 != 0 {
            fields.push(r#""raw_meta":null"#);
        }
        let source = format!(" \t{{{}}}\n", fields.join(","));
        let doc = batch(&source);
        let mut cursor = doc.cursor();
        let error = cursor.read_event(ProjectionLimits::default()).unwrap_err();
        let field = if mask & 1 == 0 {
            EventField::Id
        } else if mask & 2 == 0 {
            EventField::Topic
        } else {
            EventField::RawMeta
        };
        assert_eq!(error.kind, ErrorKind::MissingField(field), "{mask}");
        assert_eq!((error.offset, error.record, error.line), (0, 0, 1));
        assert_eq!(cursor.position(), 0);
    }
}

#[test]
fn actual_id_and_topic_errors_retain_global_value_offsets_and_cursor_for_retry() {
    for (id, kind) in [
        ("-1", JsonError::Number),
        ("-0", JsonError::Number),
        ("18446744073709551616", JsonError::Number),
        ("1e0", JsonError::Number),
        ("1.0", JsonError::Number),
        ("\"not-id\"", JsonError::Type),
        ("false", JsonError::Type),
        ("[]", JsonError::Type),
        ("null", JsonError::Type),
    ] {
        let source = format!("{SIMPLE}\r\n  {{\"id\":{id},\"topic\":\"ok\",\"raw_meta\":null}}\n");
        let doc = batch(&source);
        let mut cursor = doc.cursor();
        cursor.read_raw().unwrap();
        let error = cursor.read_event(ProjectionLimits::default()).unwrap_err();
        let expected = SIMPLE.len() + 2 + 2 + r#"{"id":"#.len();
        assert_eq!(
            (error.kind, error.offset, error.record, error.line),
            (ErrorKind::Json(kind), expected, 1, 2),
            "{id}"
        );
        assert_eq!(cursor.position(), 1);
        assert_eq!(
            cursor
                .read_raw()
                .unwrap()
                .root()
                .field("id", 4096)
                .unwrap()
                .unwrap()
                .raw(),
            id
        );
    }
    for topic in ["false", "0", "[]", "{}", "null"] {
        let source = format!("{SIMPLE}\n{{\"id\":42,\"topic\":{topic},\"raw_meta\":null}}");
        let doc = batch(&source);
        let mut cursor = doc.cursor();
        cursor.read_raw().unwrap();
        let error = cursor.read_event(ProjectionLimits::default()).unwrap_err();
        let offset = SIMPLE.len() + 1 + r#"{"id":42,"topic":"#.len();
        assert_eq!(
            (error.kind, error.offset, error.record, error.line),
            (ErrorKind::Json(JsonError::Type), offset, 1, 2)
        );
        assert_eq!(cursor.position(), 1);
    }
    for value in ["null", "true", "42", "[]", "\"object?\""] {
        let source = format!(" \t{value}\n");
        let doc = batch(&source);
        let mut cursor = doc.cursor();
        let error = cursor.read_event(ProjectionLimits::default()).unwrap_err();
        assert_eq!(
            (error.kind, error.offset),
            (ErrorKind::Json(JsonError::Type), 2)
        );
        assert_eq!(cursor.position(), 0);
    }
}

#[test]
fn successful_projection_accepts_all_raw_metadata_and_preserves_mismatched_suffixes() {
    let metadata_inputs = [
        "null",
        "true",
        "1e999999",
        "\"é🦀\"",
        "[0,false]",
        "{\"x\":42}",
    ];
    let source = metadata_inputs
        .iter()
        .enumerate()
        .map(|(i, raw)| format!("{{\"id\":{i},\"topic\":\"event\",\"raw_meta\":{raw}}}\n"))
        .collect::<String>();
    let doc = batch(&source);
    let mut cursor = doc.cursor();
    let sentinel = IndexedJson::parse(br#""sentinel""#, Limits::default()).unwrap();
    let topic = sentinel.root().string().unwrap();
    let mut ids = [999; 7];
    let mut topic_slots = [Some(topic); 8];
    let mut raw_slots = [Some(sentinel.root()); 9];
    let written = project_into(
        &mut cursor,
        EventColumns {
            ids: &mut ids,
            topics: &mut topic_slots,
            raw_meta: &mut raw_slots,
        },
        ProjectionLimits::default(),
    )
    .unwrap();
    assert_eq!(written, 6);
    assert_eq!(cursor.position(), 6);
    assert_eq!(&ids[..6], &[0, 1, 2, 3, 4, 5]);
    assert_eq!(ids[6], 999);
    assert!(topic_slots[..6].iter().all(|v| v.unwrap().equals("event")));
    assert!(
        topic_slots[6..]
            .iter()
            .all(|v| v.unwrap().raw_content().as_ptr() == topic.raw_content().as_ptr())
    );
    for (node, raw) in raw_slots[..6].iter().zip(metadata_inputs) {
        assert_eq!(node.unwrap().raw(), raw);
    }
    assert!(
        raw_slots[6..]
            .iter()
            .all(|v| v.unwrap().raw().as_ptr() == sentinel.root().raw().as_ptr())
    );
}

#[test]
fn late_schema_failures_preserve_every_initialized_slot_and_cursor() {
    for tail in [
        r#"{"id":8,"topic":"valid"}"#,
        r#"{"id":1e0,"topic":"valid","raw_meta":null}"#,
        r#"{"id":8,"topic":false,"raw_meta":null}"#,
        r#"[1,2,3]"#,
    ] {
        let source = format!("{SIMPLE}\n{tail}\n");
        let doc = batch(&source);
        let mut cursor = doc.cursor();
        let sentinel = IndexedJson::parse(br#""retained""#, Limits::default()).unwrap();
        let mut ids = [91, 92, 93, 94];
        let before_ids = ids;
        let mut topic_slots = [
            Some(sentinel.root().string().unwrap()),
            None,
            Some(sentinel.root().string().unwrap()),
            None,
        ];
        let before_topics = topics(topic_slots);
        let mut raw_slots = [Some(sentinel.root()), None, Some(sentinel.root()), None];
        let before_raw = metadata(raw_slots);
        let error = project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topic_slots,
                raw_meta: &mut raw_slots,
            },
            ProjectionLimits::default(),
        )
        .unwrap_err();
        assert_eq!((error.record, error.line), (1, 2), "{tail}");
        assert_eq!(cursor.position(), 0);
        assert_eq!(ids, before_ids);
        assert_eq!(topics(topic_slots), before_topics);
        assert_eq!(metadata(raw_slots), before_raw);
    }
}

#[test]
fn count_precedes_capacity_and_each_short_column_preserves_all_outputs() {
    let source = format!("{SIMPLE}\n{SIMPLE}\n");
    let doc = batch(&source);
    let sentinel = IndexedJson::parse(br#""old""#, Limits::default()).unwrap();
    for (capacity, limits, expected) in [
        (
            [0, 0, 0],
            ProjectionLimits {
                max_rows: 1,
                ..Default::default()
            },
            ErrorKind::RecordLimit,
        ),
        (
            [1, 3, 3],
            ProjectionLimits::default(),
            ErrorKind::OutputTooSmall,
        ),
        (
            [3, 1, 3],
            ProjectionLimits::default(),
            ErrorKind::OutputTooSmall,
        ),
        (
            [3, 3, 1],
            ProjectionLimits::default(),
            ErrorKind::OutputTooSmall,
        ),
    ] {
        let mut cursor = doc.cursor();
        let mut ids = [44; 3];
        let mut topic_slots = [Some(sentinel.root().string().unwrap()); 3];
        let before_topics = topics(topic_slots);
        let mut raw_slots = [Some(sentinel.root()); 3];
        let before_raw = metadata(raw_slots);
        let error = project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids[..capacity[0]],
                topics: &mut topic_slots[..capacity[1]],
                raw_meta: &mut raw_slots[..capacity[2]],
            },
            limits,
        )
        .unwrap_err();
        assert_eq!(
            (error.kind, error.offset, error.record, error.line),
            (expected, 0, 0, 1)
        );
        assert_eq!(cursor.position(), 0);
        assert_eq!(ids, [44; 3]);
        assert_eq!(topics(topic_slots), before_topics);
        assert_eq!(metadata(raw_slots), before_raw);
    }
}

#[test]
fn exact_declared_read_and_two_pass_projection_work_include_writes_and_commit() {
    // Approved schema policy: record1; keys (217+433+649); id2; topic1.
    const SCHEMA: usize = 1303;
    let doc = batch(SIMPLE);
    for (work, succeeds) in [(SCHEMA, false), (SCHEMA + 1, true)] {
        let mut cursor = doc.cursor();
        let result = cursor.read_event(ProjectionLimits {
            max_work: work,
            ..Default::default()
        });
        if succeeds {
            assert_eq!(result.unwrap().id, 7);
            assert_eq!(cursor.position(), 1);
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::WorkLimit);
            assert_eq!(cursor.position(), 0);
        }
    }
    let source = format!("{SIMPLE}\n{SIMPLE}\n");
    let doc = batch(&source);
    const EXACT: usize = SCHEMA * 2 * 2 + 3 * 2 + 1;
    for (work, succeeds) in [(SCHEMA * 2 + 7, false), (EXACT - 1, false), (EXACT, true)] {
        let mut cursor = doc.cursor();
        let mut ids = [99; 3];
        let mut topic_slots = [None; 3];
        let mut raw_slots = [None; 3];
        let result = project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topic_slots,
                raw_meta: &mut raw_slots,
            },
            ProjectionLimits {
                max_work: work,
                ..Default::default()
            },
        );
        if succeeds {
            assert_eq!(result.unwrap(), 2);
            assert_eq!(ids, [7, 7, 99]);
            assert_eq!(cursor.position(), 2);
            assert!(topic_slots[2].is_none() && raw_slots[2].is_none());
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                (error.kind, error.record, error.line),
                (ErrorKind::WorkLimit, 1, 2)
            );
            assert_eq!(ids, [99; 3]);
            assert!(topic_slots.iter().all(Option::is_none));
            assert!(raw_slots.iter().all(Option::is_none));
            assert_eq!(cursor.position(), 0);
        }
    }
}

#[test]
fn escaped_unknown_key_comparisons_share_the_same_finite_work_stage() {
    let source = r#"{"\u0069d":7,"topic":"t","raw_meta":null,"\u0075nknown":[]}"#;
    let doc = batch(source);
    // Independent cost from actual validated raw key widths, never fresh lookup allowances.
    let schema = 1
        + doc
            .get(0)
            .unwrap()
            .root()
            .fields()
            .unwrap()
            .map(|(k, _)| 3 * 24 * (k.raw_content().len() + 1) + 1)
            .sum::<usize>()
        + 2
        + 1;
    for (work, succeeds) in [(schema, false), (schema + 1, true)] {
        let mut cursor = doc.cursor();
        let result = cursor.read_event(ProjectionLimits {
            max_work: work,
            ..Default::default()
        });
        assert_eq!(result.is_ok(), succeeds);
        assert_eq!(cursor.position(), usize::from(succeeds));
    }
}

#[test]
fn configuration_is_validated_even_at_eof_and_relocated_to_current_record() {
    let source = format!("{SIMPLE}\r\n{SIMPLE}\n");
    let doc = batch(&source);
    for limits in [
        ProjectionLimits {
            max_rows: 0,
            ..Default::default()
        },
        ProjectionLimits {
            max_rows: ProjectionLimits::HARD.max_rows + 1,
            ..Default::default()
        },
        ProjectionLimits {
            max_work: 0,
            ..Default::default()
        },
        ProjectionLimits {
            max_work: ProjectionLimits::HARD.max_work + 1,
            ..Default::default()
        },
    ] {
        let standalone = limits.validate().unwrap_err();
        assert_eq!(
            (
                standalone.kind,
                standalone.offset,
                standalone.record,
                standalone.line
            ),
            (ErrorKind::InvalidLimits, 0, 0, 1)
        );
        for position in 0..=2 {
            let mut cursor = doc.cursor();
            for _ in 0..position {
                cursor.read_raw().unwrap();
            }
            let offset = doc
                .get(position)
                .map(|r| r.payload_span().start)
                .unwrap_or(source.len());
            let line = if position < 2 { position + 1 } else { 3 };
            let error = cursor.read_event(limits).unwrap_err();
            assert_eq!(
                (error.kind, error.offset, error.record, error.line),
                (ErrorKind::InvalidLimits, offset, position, line)
            );
            assert_eq!(cursor.position(), position);
            let mut ids = [7; 3];
            let mut topic_slots = [None; 3];
            let mut raw_slots = [None; 3];
            let error = project_into(
                &mut cursor,
                EventColumns {
                    ids: &mut ids,
                    topics: &mut topic_slots,
                    raw_meta: &mut raw_slots,
                },
                limits,
            )
            .unwrap_err();
            assert_eq!(
                (error.kind, error.offset, error.record, error.line),
                (ErrorKind::InvalidLimits, offset, position, line)
            );
            assert_eq!(ids, [7; 3]);
            assert!(topic_slots.iter().all(Option::is_none));
            assert!(raw_slots.iter().all(Option::is_none));
            assert_eq!(cursor.position(), position);
        }
    }
}

#[test]
fn empty_projection_and_remaining_after_raw_read_keep_exact_scope() {
    let doc = batch("");
    let mut cursor = doc.cursor();
    assert_eq!(
        project_into(
            &mut cursor,
            EventColumns {
                ids: &mut [],
                topics: &mut [],
                raw_meta: &mut []
            },
            ProjectionLimits {
                max_rows: 1,
                max_work: 1
            }
        )
        .unwrap(),
        0
    );
    assert_eq!(cursor.position(), 0);
    let source = format!("null\n{SIMPLE}\n");
    let doc = batch(&source);
    let mut cursor = doc.cursor();
    cursor.read_raw().unwrap();
    let mut ids = [99; 2];
    let mut topic_slots = [None; 2];
    let mut raw_slots = [None; 2];
    assert_eq!(
        project_into(
            &mut cursor,
            EventColumns {
                ids: &mut ids,
                topics: &mut topic_slots,
                raw_meta: &mut raw_slots
            },
            ProjectionLimits {
                max_rows: 1,
                ..Default::default()
            }
        )
        .unwrap(),
        1
    );
    assert_eq!(ids, [7, 99]);
    assert_eq!(cursor.position(), 2);
    assert!(topic_slots[1].is_none() && raw_slots[1].is_none());
}
