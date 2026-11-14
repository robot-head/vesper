//! Wave-seven OTAP columns: resources and scopes with attributes, span kind,
//! trace state, dropped counts, events and links, histogram sum/min/max and
//! data-point attributes, logs; upstream delta and quasi-delta id encodings;
//! dictionaries of the new columns; automatic batch splitting; and the PII
//! boundary across every new attribute set.

use arrow_array::{
    types::UInt8Type, ArrayRef, DictionaryArray, DurationNanosecondArray, FixedSizeBinaryArray,
    Int32Array, RecordBatch, StringArray, TimestampNanosecondArray, UInt16Array, UInt8Array,
};
use arrow_ipc::{root_as_message, writer::StreamWriter, MessageHeader};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use core::num::NonZeroU64;
use prost::Message;
use std::sync::Arc;
use vesper_beacon::*;
use vesper_beacon_arrow::otap::proto::{ArrowPayload, ArrowPayloadType, BatchArrowRecords};
use vesper_beacon_arrow::otap::*;

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}
fn roundtrip(batch: &BatchArrowRecords) -> BatchArrowRecords {
    BatchArrowRecords::decode(batch.encode_to_vec().as_slice()).unwrap()
}
fn record(span_id: u64, parent: u64, start: u64) -> SpanRecord {
    let pool =
        SpanPool::<2>::new(0xaaaa_0000_0000_0000_0000_0000_0000_0001, nz(span_id), 1).unwrap();
    let mut ring = TelemetryRing::<2>::default();
    let span = pool
        .start_span(None, SpanSchema::Request, start, 10)
        .unwrap();
    span.finish_ok(&mut ring, 25, PrimaryAttribute::None)
        .unwrap();
    let mut r = ring.records().next().unwrap();
    r.parent_span_id = parent;
    r
}
fn s(v: &str) -> DecodedValue {
    DecodedValue::Str(v.into())
}
fn messages(bytes: &[u8]) -> Vec<(MessageHeader, bool, i64)> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let len = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let message = root_as_message(&rest[8..8 + len]).unwrap();
        let (delta, rows) = match message.header_as_dictionary_batch() {
            Some(d) => (d.isDelta(), d.data().unwrap().length()),
            None => (false, 0),
        };
        out.push((message.header_type(), delta, rows));
        rest = &rest[8 + len + message.bodyLength() as usize..];
    }
    out
}

fn attrs(pairs: &[(&'static str, &'static str)]) -> Attributes {
    let mut a = Attributes::new();
    for (k, v) in pairs {
        a.insert(k, *v).unwrap();
    }
    a
}

/// Two resources, three scopes, five spans with every new field.
fn trace_groups() -> Vec<ResourceGroup<OtapSpan>> {
    let mut api = Resource {
        attributes: attrs(&[("service.name", "api"), ("deployment.environment", "prod")]),
        schema_url: Some("https://opentelemetry.io/schemas/1.26.0"),
        dropped_attributes_count: 1,
    };
    api.attributes.insert("host.cores", 8i64).unwrap();
    let worker = Resource {
        attributes: attrs(&[("service.name", "worker")]),
        ..Resource::default()
    };
    let http = Scope {
        name: Some("vesper.http"),
        version: Some("0.1.0"),
        attributes: attrs(&[("library.kind", "server")]),
        dropped_attributes_count: 2,
        schema_url: Some("https://opentelemetry.io/schemas/1.25.0"),
    };
    let db = Scope {
        name: Some("vesper.db"),
        ..Scope::default()
    };
    let mut server = OtapSpan::from(record(1, 0, 1_000));
    server.kind = SpanKind::Server;
    server.trace_state = Some(TraceState::parse("vendor=abc, other@tenant=x:1").unwrap());
    server.dropped_attributes_count = 3;
    server.dropped_events_count = 4;
    server.dropped_links_count = 5;
    server.attributes.insert("http.route", "/users").unwrap();
    let mut accepted = SpanEvent::new("accepted", 1_005);
    accepted.attributes.insert("queue", "fast").unwrap();
    accepted.dropped_attributes_count = 6;
    server.events.push(accepted);
    server.events.push(SpanEvent::new("parsed", 1_007));
    let mut link = SpanLink::new(0xbbbb_0000_0000_0000_0000_0000_0000_0002, 77);
    link.trace_state = Some(TraceState::parse("k=v").unwrap());
    link.flags = Some(0x301);
    link.attributes.insert("link.reason", "retry").unwrap();
    link.dropped_attributes_count = 7;
    server.links.push(link);
    let mut client = OtapSpan::from(record(2, 1, 1_010));
    client.kind = SpanKind::Client;
    // Same event name as the server's first event: quasi-delta continues.
    client.events.push(SpanEvent::new("accepted", 1_011));
    client
        .links
        .push(SpanLink::new(0xbbbb_0000_0000_0000_0000_0000_0000_0002, 78));
    let mut query = OtapSpan::from(record(3, 2, 1_020));
    query.kind = SpanKind::Internal;
    query.attributes.insert("http.route", "/users").unwrap();
    let mut job = OtapSpan::from(record(4, 0, 2_000));
    job.kind = SpanKind::Consumer;
    job.attributes.insert("http.route", "/users").unwrap();
    let producer = {
        let mut p = OtapSpan::from(record(5, 4, 2_001));
        p.kind = SpanKind::Producer;
        p
    };
    vec![
        ResourceGroup {
            resource: api,
            scopes: vec![
                ScopeGroup {
                    scope: http.clone(),
                    items: vec![server, client],
                },
                ScopeGroup {
                    scope: db,
                    items: vec![query],
                },
            ],
        },
        ResourceGroup::single(worker, http, vec![job, producer]),
    ]
}

fn check_traces(decoded: &DecodedBatch) {
    assert_eq!(decoded.resources.len(), 2);
    assert_eq!(decoded.scopes.len(), 3);
    let api = &decoded.resources[0];
    assert_eq!(
        api.attributes,
        vec![
            ("service.name".into(), s("api")),
            ("deployment.environment".into(), s("prod")),
            ("host.cores".into(), DecodedValue::Int(8)),
        ]
    );
    assert_eq!(
        api.schema_url.as_deref(),
        Some("https://opentelemetry.io/schemas/1.26.0")
    );
    assert_eq!(api.dropped_attributes_count, 1);
    assert_eq!(
        decoded.resources[1].attributes,
        vec![("service.name".into(), s("worker"))]
    );
    assert_eq!(decoded.resources[1].schema_url, None);
    let http = &decoded.scopes[0];
    assert_eq!(http.name.as_deref(), Some("vesper.http"));
    assert_eq!(http.version.as_deref(), Some("0.1.0"));
    assert_eq!(http.attributes, vec![("library.kind".into(), s("server"))]);
    assert_eq!(http.dropped_attributes_count, 2);
    assert_eq!(
        http.schema_url.as_deref(),
        Some("https://opentelemetry.io/schemas/1.25.0")
    );
    assert_eq!(decoded.scopes[1].name.as_deref(), Some("vesper.db"));
    assert_eq!(decoded.scopes[1].version, None);
    assert_eq!(decoded.scopes[2], decoded.scopes[0]);

    let spans = &decoded.spans;
    assert_eq!(spans.len(), 5);
    let placement: Vec<(usize, usize)> = spans.iter().map(|s| (s.resource, s.scope)).collect();
    assert_eq!(placement, [(0, 0), (0, 0), (0, 1), (1, 2), (1, 2)]);
    let kinds: Vec<Option<i32>> = spans.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [Some(2), Some(3), Some(1), Some(5), Some(4)]);
    let server = &spans[0];
    assert_eq!(
        server.trace_state.as_deref(),
        Some("vendor=abc,other@tenant=x:1")
    );
    assert_eq!(
        (
            server.dropped_attributes_count,
            server.dropped_events_count,
            server.dropped_links_count
        ),
        (3, 4, 5)
    );
    assert_eq!(server.attributes, vec![("http.route".into(), s("/users"))]);
    assert_eq!(
        server.events,
        vec![
            DecodedEvent {
                name: "accepted".into(),
                time_unix_ns: Some(1_005),
                attributes: vec![("queue".into(), s("fast"))],
                dropped_attributes_count: 6,
            },
            DecodedEvent {
                name: "parsed".into(),
                time_unix_ns: Some(1_007),
                attributes: vec![],
                dropped_attributes_count: 0,
            },
        ]
    );
    assert_eq!(
        server.links,
        vec![DecodedLink {
            trace_id: 0xbbbb_0000_0000_0000_0000_0000_0000_0002,
            span_id: 77,
            trace_state: Some("k=v".into()),
            flags: Some(0x301),
            attributes: vec![("link.reason".into(), s("retry"))],
            dropped_attributes_count: 7,
        }]
    );
    let client = &spans[1];
    assert_eq!(client.parent_span_id, Some(1));
    assert_eq!(client.events.len(), 1);
    assert_eq!(client.events[0].name, "accepted");
    assert_eq!(client.links[0].span_id, 78);
    assert!(client.attributes.is_empty());
    // Equal key/value on consecutive spans exercises quasi-delta runs.
    for i in [2, 3] {
        assert_eq!(
            spans[i].attributes,
            vec![("http.route".into(), s("/users"))]
        );
    }
    assert!(spans[4].events.is_empty() && spans[4].links.is_empty());
}

#[test]
fn every_new_trace_column_round_trips_in_both_id_encodings() {
    for encoding in [IdEncoding::Transport, IdEncoding::Plain] {
        let mut encoder = TraceEncoder::new(EncoderConfig {
            id_encoding: encoding,
            ..EncoderConfig::default()
        });
        let batch = encoder.encode(&trace_groups()).unwrap();
        let types: Vec<ArrowPayloadType> = batch
            .arrow_payloads
            .iter()
            .map(|p| ArrowPayloadType::try_from(p.r#type).unwrap())
            .collect();
        use ArrowPayloadType as T;
        assert_eq!(
            types,
            [
                T::Spans,
                T::ResourceAttrs,
                T::ScopeAttrs,
                T::SpanAttrs,
                T::SpanEvents,
                T::SpanEventAttrs,
                T::SpanLinks,
                T::SpanLinkAttrs
            ]
        );
        let decoded = OtapDecoder::default().decode(&roundtrip(&batch)).unwrap();
        check_traces(&decoded);
    }
}

/// Reads one column of the single record batch in `payload`.
fn column(payload: &ArrowPayload, name: &str) -> ArrayRef {
    let reader = arrow_ipc::reader::StreamReader::try_new(
        std::io::Cursor::new(payload.record.clone()),
        None,
    )
    .unwrap();
    let batches: Vec<RecordBatch> = reader.map(Result::unwrap).collect();
    batches[0].column_by_name(name).unwrap().clone()
}
fn u_values(a: &ArrayRef) -> Vec<Option<u64>> {
    use arrow_array::cast::AsArray;
    match a.data_type() {
        DataType::UInt16 => a
            .as_primitive::<arrow_array::types::UInt16Type>()
            .iter()
            .map(|v| v.map(u64::from))
            .collect(),
        DataType::UInt32 => a
            .as_primitive::<arrow_array::types::UInt32Type>()
            .iter()
            .map(|v| v.map(u64::from))
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn transport_ids_are_upstream_delta_and_quasi_delta() {
    let batch = TraceEncoder::new(EncoderConfig::default())
        .encode(&trace_groups())
        .unwrap();
    let by = |t: ArrowPayloadType| {
        batch
            .arrow_payloads
            .iter()
            .find(|p| p.r#type == t as i32)
            .unwrap()
    };
    let spans = by(ArrowPayloadType::Spans);
    // Span ids 0..5 delta-encoded; no `encoding` metadata (Go ignores it).
    assert_eq!(
        u_values(&column(spans, "id")),
        [Some(0), Some(1), Some(1), Some(1), Some(1)]
    );
    use arrow_array::cast::AsArray;
    let resource = column(spans, "resource");
    let rid = u_values(resource.as_struct().column_by_name("id").unwrap());
    assert_eq!(rid, [Some(0), Some(0), Some(0), Some(1), Some(0)]);
    let scope = column(spans, "scope");
    let sid = u_values(scope.as_struct().column_by_name("id").unwrap());
    assert_eq!(sid, [Some(0), Some(0), Some(1), Some(1), Some(0)]);
    // SPAN_ATTRS: (0, route=/users), (2, route=/users), (3, route=/users):
    // equal key and value continue as deltas 2 and 1.
    let attrs = by(ArrowPayloadType::SpanAttrs);
    assert_eq!(
        u_values(&column(attrs, "parent_id")),
        [Some(0), Some(2), Some(1)]
    );
    // Events: (span 0, accepted), (0, parsed), (1, accepted): parent ids are
    // absolute on a name change.
    let events = by(ArrowPayloadType::SpanEvents);
    assert_eq!(
        u_values(&column(events, "parent_id")),
        [Some(0), Some(0), Some(1)]
    );
    assert_eq!(u_values(&column(events, "id")), [Some(0), Some(1), Some(1)]);
    // Links share a trace id: the second parent is a delta of 1.
    let links = by(ArrowPayloadType::SpanLinks);
    assert_eq!(u_values(&column(links, "parent_id")), [Some(0), Some(1)]);
    let schema = schema::spans_schema();
    assert!(schema.field_with_name("id").unwrap().metadata().is_empty());
    let plain = schema::spans_schema_with(IdEncoding::Plain);
    assert_eq!(
        plain.field_with_name("id").unwrap().metadata()[schema::ENCODING_KEY],
        "plain"
    );
}

fn ipc(batch: &RecordBatch) -> Vec<u8> {
    let mut w = StreamWriter::try_new(Vec::new(), &batch.schema()).unwrap();
    w.write(batch).unwrap();
    w.into_inner().unwrap()[..].to_vec()
}
fn strip_eos(mut v: Vec<u8>) -> Vec<u8> {
    v.truncate(v.len() - 8);
    v
}

/// A batch laid out the way upstream's Go producer lays it out: attributes
/// sorted by key and value across spans (so quasi-delta runs skip parents),
/// dictionary-encoded `kind` and duration, and no `encoding` metadata.
#[test]
fn upstream_style_sorted_quasi_delta_and_dictionary_columns_decode() {
    let n = 3;
    let spans_schema = Schema::new(vec![
        Field::new("id", DataType::UInt16, true),
        Field::new(
            "start_time_unix_nano",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "duration_time_unix_nano",
            DataType::Dictionary(
                Box::new(DataType::UInt8),
                Box::new(DataType::Duration(TimeUnit::Nanosecond)),
            ),
            false,
        ),
        Field::new("trace_id", DataType::FixedSizeBinary(16), false),
        Field::new("span_id", DataType::FixedSizeBinary(8), false),
        Field::new("name", DataType::Utf8, false),
        Field::new(
            "kind",
            DataType::Dictionary(Box::new(DataType::UInt8), Box::new(DataType::Int32)),
            true,
        ),
    ]);
    let durations = DictionaryArray::<UInt8Type>::try_new(
        UInt8Array::from(vec![0, 0, 1]),
        Arc::new(DurationNanosecondArray::from(vec![5, 9])),
    )
    .unwrap();
    let kinds = DictionaryArray::<UInt8Type>::try_new(
        UInt8Array::from(vec![Some(0), None, Some(1)]),
        Arc::new(Int32Array::from(vec![2, 3])),
    )
    .unwrap();
    let spans = RecordBatch::try_new(
        Arc::new(spans_schema),
        vec![
            // ids 0, 1, 3 (delta 0, 1, 2)
            Arc::new(UInt16Array::from(vec![0, 1, 2])),
            Arc::new(TimestampNanosecondArray::from(vec![10, 20, 30]).with_timezone("UTC")),
            Arc::new(durations),
            Arc::new(FixedSizeBinaryArray::try_from_iter((0..n).map(|i| [i as u8; 16])).unwrap()),
            Arc::new(
                FixedSizeBinaryArray::try_from_iter((0..n).map(|i| [i as u8 + 1; 8])).unwrap(),
            ),
            Arc::new(StringArray::from(vec!["a", "b", "c"])),
            Arc::new(kinds),
        ],
    )
    .unwrap();
    // Attributes sorted by (key, value, parent): env=prod for spans 0 and 3,
    // then env=test for span 1, then zone=1 for spans 0, 1, 3.
    let attrs_schema = Schema::new(vec![
        Field::new("parent_id", DataType::UInt16, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("type", DataType::UInt8, false),
        Field::new("str", DataType::Utf8, true),
        Field::new("int", DataType::Int64, true),
    ]);
    let attrs = RecordBatch::try_new(
        Arc::new(attrs_schema),
        vec![
            Arc::new(UInt16Array::from(vec![0, 3, 1, 0, 1, 2])),
            Arc::new(StringArray::from(vec![
                "env", "env", "env", "zone", "zone", "zone",
            ])),
            Arc::new(UInt8Array::from(vec![1, 1, 1, 2, 2, 2])),
            Arc::new(StringArray::from(vec![
                Some("prod"),
                Some("prod"),
                Some("test"),
                None,
                None,
                None,
            ])),
            Arc::new(arrow_array::Int64Array::from(vec![
                None,
                None,
                None,
                Some(1),
                Some(1),
                Some(1),
            ])),
        ],
    )
    .unwrap();
    let batch = BatchArrowRecords {
        batch_id: 4,
        arrow_payloads: vec![
            ArrowPayload {
                schema_id: "s".into(),
                r#type: ArrowPayloadType::Spans as i32,
                record: strip_eos(ipc(&spans)),
            },
            ArrowPayload {
                schema_id: "a".into(),
                r#type: ArrowPayloadType::SpanAttrs as i32,
                record: strip_eos(ipc(&attrs)),
            },
        ],
        headers: vec![],
    };
    let decoded = OtapDecoder::default().decode(&batch).unwrap();
    let got: Vec<(String, Option<i32>, u64, DecodedAttributes)> = decoded
        .spans
        .iter()
        .map(|s| (s.name.clone(), s.kind, s.duration_ns, s.attributes.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "a".into(),
                Some(2),
                5,
                vec![
                    ("env".into(), s("prod")),
                    ("zone".into(), DecodedValue::Int(1))
                ]
            ),
            (
                "b".into(),
                None,
                5,
                vec![
                    ("env".into(), s("test")),
                    ("zone".into(), DecodedValue::Int(1))
                ]
            ),
            (
                "c".into(),
                Some(3),
                9,
                vec![
                    ("env".into(), s("prod")),
                    ("zone".into(), DecodedValue::Int(1))
                ]
            ),
        ]
    );
    // The same parent ids read as plain ids dangle (span 2 does not exist).
    let mut plain = batch.clone();
    let attrs_plain = RecordBatch::try_new(
        Arc::new(
            attrs
                .schema()
                .as_ref()
                .clone()
                .with_metadata(Default::default()),
        ),
        attrs.columns().to_vec(),
    )
    .unwrap();
    let mut fields: Vec<Field> = attrs_plain
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    fields[0] = fields[0]
        .clone()
        .with_metadata([("encoding".to_owned(), "plain".to_owned())].into());
    let attrs_plain = RecordBatch::try_new(
        Arc::new(Schema::new(fields)),
        attrs_plain.columns().to_vec(),
    )
    .unwrap();
    plain.arrow_payloads[1].record = strip_eos(ipc(&attrs_plain));
    plain.arrow_payloads[1].schema_id = "b".into();
    assert!(matches!(
        OtapDecoder::default().decode(&plain),
        Err(DecodeError::DanglingParent(_))
    ));
}

#[test]
fn new_dictionary_columns_send_deltas_and_reset() {
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let mut decoder = OtapDecoder::default();
    let groups = trace_groups();
    let first = encoder.encode(&groups).unwrap();
    // The same resources and scopes again: no dictionary messages anywhere.
    let same = encoder.encode(&groups).unwrap();
    for p in &same.arrow_payloads {
        assert_eq!(
            messages(&p.record),
            vec![(MessageHeader::RecordBatch, false, 0)],
            "{}",
            p.schema_id
        );
    }
    // A new scope version and resource attribute value: deltas only for the
    // grown dictionaries (scope.version in SPANS, str in RESOURCE_ATTRS).
    let mut grown = groups.clone();
    grown[1].scopes[0].scope.version = Some("0.2.0");
    grown[1].resource.attributes = attrs(&[("service.name", "worker-2")]);
    let delta = encoder.encode(&grown).unwrap();
    let spans = messages(&delta.arrow_payloads[0].record);
    assert_eq!(
        spans,
        vec![
            (MessageHeader::DictionaryBatch, true, 1),
            (MessageHeader::RecordBatch, false, 0)
        ]
    );
    let res = messages(&delta.arrow_payloads[1].record);
    assert_eq!(
        res,
        vec![
            (MessageHeader::DictionaryBatch, true, 1),
            (MessageHeader::RecordBatch, false, 0)
        ]
    );
    for b in [&first, &same, &delta] {
        decoder.decode(&roundtrip(b)).unwrap();
    }
    let d = decoder.decode(&encoder.encode(&grown).unwrap()).unwrap();
    assert_eq!(d.scopes[2].version.as_deref(), Some("0.2.0"));
    assert_eq!(d.resources[1].attributes[0].1, s("worker-2"));
    // Growth past the configured limit resets just that stream.
    let mut small = TraceEncoder::new(EncoderConfig {
        max_dictionary_entries: 3,
        ..EncoderConfig::default()
    });
    let mut decoder = OtapDecoder::default();
    let mut ids = Vec::new();
    for version in ["1", "2", "3", "4"] {
        let mut g = groups.clone();
        g[0].scopes[0].scope.version = Some(version);
        g[1].scopes[0].scope.version = Some(version);
        let b = small.encode(&g).unwrap();
        ids.push(b.arrow_payloads[0].schema_id.clone());
        let d = decoder.decode(&b).unwrap();
        assert_eq!(d.scopes[0].version.as_deref(), Some(version));
    }
    assert_eq!(ids[0], ids[2]);
    assert_ne!(ids[2], ids[3]);
}

#[test]
fn histograms_carry_sum_min_max_attributes_and_share_metric_rows() {
    let mut latency = FastLogHistogram::default();
    for v in [3, 1_000, 123_456] {
        latency.record_with_exemplar(v, nz(1)).unwrap();
    }
    let mut other = FastLogHistogram::default();
    other.record_with_exemplar(7, nz(2)).unwrap();
    let empty = FastLogHistogram::default();
    let point = |h, route: &'static str| {
        let mut p = HistogramPoint::new("http.server.duration", "ns", 1, 2, h);
        p.attributes.insert("http.route", route).unwrap();
        p
    };
    let resource = Resource {
        attributes: attrs(&[("service.name", "api")]),
        ..Resource::default()
    };
    let scope = Scope {
        name: Some("vesper.metrics"),
        ..Scope::default()
    };
    let groups = [ResourceGroup::single(
        resource,
        scope,
        vec![
            point(&latency, "/a"),
            point(&other, "/b"),
            HistogramPoint::new("vesper.idle", "", 1, 2, &empty),
        ],
    )];
    for encoding in [IdEncoding::Transport, IdEncoding::Plain] {
        let mut encoder = MetricsEncoder::new(EncoderConfig {
            id_encoding: encoding,
            ..EncoderConfig::default()
        });
        let batch = roundtrip(&encoder.encode(&groups).unwrap());
        // Two metric rows: the two http points share one.
        let metrics = &batch.arrow_payloads[0];
        assert_eq!(metrics.r#type, ArrowPayloadType::UnivariateMetrics as i32);
        assert_eq!(column(metrics, "name").len(), 2);
        let d = OtapDecoder::default().decode(&batch).unwrap();
        assert_eq!(
            d.resources[0].attributes,
            vec![("service.name".into(), s("api"))]
        );
        assert_eq!(d.scopes[0].name.as_deref(), Some("vesper.metrics"));
        let h = &d.histograms;
        assert_eq!(h.len(), 3);
        assert_eq!(
            (h[0].sum, h[0].min, h[0].max),
            (Some(124_459.0), Some(3.0), Some(123_456.0))
        );
        assert_eq!(h[0].attributes, vec![("http.route".into(), s("/a"))]);
        assert_eq!(h[1].attributes, vec![("http.route".into(), s("/b"))]);
        assert_eq!(
            (h[1].sum, h[1].min, h[1].max),
            (Some(7.0), Some(7.0), Some(7.0))
        );
        assert_eq!(h[1].name, "http.server.duration");
        assert_eq!(
            (h[2].name.as_str(), h[2].min, h[2].max, h[2].sum),
            ("vesper.idle", None, None, Some(0.0))
        );
        assert!(h[2].attributes.is_empty());
        assert_eq!(
            h[0].aggregation_temporality,
            Some(schema::TEMPORALITY_CUMULATIVE)
        );
        assert_eq!(h[0].count, Some(3));
    }
}

#[test]
fn logs_round_trip_with_bodies_and_attributes() {
    let policy = PiiPolicy::Redact;
    let mut a = LogRecord {
        time_unix_ns: Some(100),
        observed_time_unix_ns: Some(101),
        trace_id: Some(0x1234),
        span_id: Some(0x56),
        severity: Severity::ERROR,
        severity_text: Some("ERROR"),
        flags: 1,
        event_name: Some("vesper.login_failed"),
        dropped_attributes_count: 2,
        ..LogRecord::default()
    };
    a.set_body("login failed");
    a.attributes.insert("attempt", 3i64).unwrap();
    let mut b = LogRecord {
        severity: Severity::INFO,
        ..LogRecord::default()
    };
    b.set_body(2.5f64);
    let mut c = LogRecord::default();
    c.set_body_pii(Pii::new("bob@example.com".to_owned()), &policy);
    c.attributes
        .insert_pii(
            "user.email",
            Pii::new("bob@example.com".to_owned()),
            &policy,
        )
        .unwrap();
    let d = LogRecord::default();
    let resource = Resource {
        attributes: attrs(&[("service.name", "auth")]),
        ..Resource::default()
    };
    let groups = [ResourceGroup::single(
        resource,
        Scope::default(),
        vec![a, b, c, d],
    )];
    let mut encoder = LogsEncoder::new(EncoderConfig::default());
    let batch = roundtrip(&encoder.encode(&groups).unwrap());
    let bytes = batch.encode_to_vec();
    assert!(!bytes.windows(15).any(|w| w == b"bob@example.com"));
    let out = OtapDecoder::default().decode(&batch).unwrap();
    assert_eq!(
        out.resources[0].attributes,
        vec![("service.name".into(), s("auth"))]
    );
    let l = &out.logs;
    assert_eq!(l.len(), 4);
    assert_eq!(l[0].time_unix_ns, Some(100));
    assert_eq!(l[0].observed_time_unix_ns, Some(101));
    assert_eq!(l[0].trace_id, Some(0x1234));
    assert_eq!(l[0].span_id, Some(0x56));
    assert_eq!(l[0].severity_number, Some(17));
    assert_eq!(l[0].severity_text.as_deref(), Some("ERROR"));
    assert_eq!(l[0].body, Some(s("login failed")));
    assert_eq!(
        l[0].attributes,
        vec![("attempt".into(), DecodedValue::Int(3))]
    );
    assert_eq!(l[0].flags, Some(1));
    assert_eq!(l[0].event_name.as_deref(), Some("vesper.login_failed"));
    assert_eq!(l[0].dropped_attributes_count, 2);
    assert_eq!(l[1].body, Some(DecodedValue::Double(2.5)));
    assert_eq!(l[2].body, Some(s(REDACTED_MARKER)));
    assert_eq!(
        l[2].attributes,
        vec![("user.email".into(), s(REDACTED_MARKER))]
    );
    assert_eq!(
        (l[3].body.clone(), l[3].trace_id, l[3].time_unix_ns),
        (None, None, None)
    );
}

#[test]
fn batches_split_automatically_at_the_attribute_row_limit() {
    // 2,000 spans x 40 attributes = 80,000 attribute rows > 65,536.
    const KEYS: [&str; 40] = [
        "k00", "k01", "k02", "k03", "k04", "k05", "k06", "k07", "k08", "k09", "k10", "k11", "k12",
        "k13", "k14", "k15", "k16", "k17", "k18", "k19", "k20", "k21", "k22", "k23", "k24", "k25",
        "k26", "k27", "k28", "k29", "k30", "k31", "k32", "k33", "k34", "k35", "k36", "k37", "k38",
        "k39",
    ];
    let spans: Vec<OtapSpan> = (0..2_000u64)
        .map(|i| {
            let mut span = OtapSpan::from(record(i + 1, 0, i));
            for (j, k) in KEYS.iter().enumerate() {
                span.attributes
                    .insert(k, (i * 100 + j as u64) as i64)
                    .unwrap();
            }
            span
        })
        .collect();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    assert!(matches!(
        encoder.encode_spans(&spans),
        Err(EncodeError::AttributeRowLimit)
    ));
    let batches = encoder.encode_spans_split(&spans).unwrap();
    assert!(batches.len() >= 2);
    let mut decoder = OtapDecoder::default();
    let mut decoded = Vec::new();
    for b in &batches {
        decoded.extend(decoder.decode(b).unwrap().spans);
    }
    assert_eq!(decoded.len(), 2_000);
    for (i, d) in decoded.iter().enumerate() {
        assert_eq!(d.start_unix_ns, i as u64);
        assert_eq!(d.attributes.len(), 40);
        assert_eq!(d.attributes[39].1, DecodedValue::Int(i as i64 * 100 + 39));
    }
    // Batch ids are consecutive.
    let ids: Vec<i64> = batches.iter().map(|b| b.batch_id).collect();
    assert_eq!(ids, (0..batches.len() as i64).collect::<Vec<_>>());
    // Logs and metrics split the same way at the main-row limit.
    let logs: Vec<LogRecord> = (0..5).map(|_| LogRecord::default()).collect();
    let mut small = LogsEncoder::new(EncoderConfig {
        max_rows: 2,
        ..EncoderConfig::default()
    });
    assert_eq!(small.encode_logs_split(&logs).unwrap().len(), 3);
}

const EMAIL: &[u8] = b"carol.private@example.org";
const TOKEN: &[u8] = b"sk_live_secret_0123456789";

fn sensitive_everywhere(policy: &PiiPolicy) -> Vec<ResourceGroup<OtapSpan>> {
    let email = || Pii::new(String::from_utf8(EMAIL.to_vec()).unwrap());
    let token = || Secret::new(TOKEN.to_vec());
    let mut resource = Resource::default();
    resource
        .attributes
        .insert_pii("host.owner", email(), policy)
        .unwrap();
    let mut scope = Scope::default();
    scope
        .attributes
        .insert_pii("scope.key", token(), policy)
        .unwrap();
    let mut span = OtapSpan::from(record(9, 0, 5));
    span.attributes
        .insert_pii("user.email", email(), policy)
        .unwrap();
    let mut event = SpanEvent::new("login", 6);
    event
        .attributes
        .insert_pii("event.email", email(), policy)
        .unwrap();
    span.events.push(event);
    let mut link = SpanLink::new(1, 2);
    link.attributes
        .insert_pii("link.token", token(), policy)
        .unwrap();
    span.links.push(link);
    vec![ResourceGroup::single(resource, scope, vec![span])]
}

#[test]
fn pii_never_reaches_resource_scope_event_link_point_or_log_columns() {
    let contains = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    for policy in [
        PiiPolicy::Redact,
        PiiPolicy::Fingerprint(FingerprintKey::new([3; 32])),
    ] {
        let batch = TraceEncoder::new(EncoderConfig::default())
            .encode(&sensitive_everywhere(&policy))
            .unwrap();
        let bytes = batch.encode_to_vec();
        assert!(!contains(&bytes, EMAIL) && !contains(&bytes, TOKEN));
        let d = OtapDecoder::default().decode(&batch).unwrap();
        let exported: Vec<&DecodedValue> = d.resources[0]
            .attributes
            .iter()
            .chain(&d.scopes[0].attributes)
            .chain(&d.spans[0].attributes)
            .chain(&d.spans[0].events[0].attributes)
            .chain(&d.spans[0].links[0].attributes)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(exported.len(), 5);
        for v in exported {
            let DecodedValue::Str(v) = v else {
                panic!("string form")
            };
            match policy {
                PiiPolicy::Redact => assert_eq!(v, REDACTED_MARKER),
                PiiPolicy::Fingerprint(_) => assert!(v.starts_with(FINGERPRINT_PREFIX)),
            }
        }
        // Data-point and log attributes and log bodies.
        let h = FastLogHistogram::default();
        let mut p = HistogramPoint::new("m", "", 0, 1, &h);
        p.attributes
            .insert_pii(
                "dp.email",
                Pii::new(String::from_utf8(EMAIL.to_vec()).unwrap()),
                &policy,
            )
            .unwrap();
        let mb = MetricsEncoder::new(EncoderConfig::default())
            .encode_histograms(&[p])
            .unwrap();
        assert!(!contains(&mb.encode_to_vec(), EMAIL));
        let mut log = LogRecord::default();
        log.set_body_pii(Secret::new(TOKEN.to_vec()), &policy);
        log.attributes
            .insert_pii(
                "log.email",
                Pii::new(String::from_utf8(EMAIL.to_vec()).unwrap()),
                &policy,
            )
            .unwrap();
        let lb = LogsEncoder::new(EncoderConfig::default())
            .encode_logs(&[log])
            .unwrap();
        let lbytes = lb.encode_to_vec();
        assert!(!contains(&lbytes, EMAIL) && !contains(&lbytes, TOKEN));
    }
    // Redacted output is byte-identical for different sensitive values.
    let a = TraceEncoder::new(EncoderConfig::default())
        .encode(&sensitive_everywhere(&PiiPolicy::Redact))
        .unwrap();
    let b = {
        let mut g = sensitive_everywhere(&PiiPolicy::Redact);
        let mut r = Resource::default();
        r.attributes
            .insert_pii(
                "host.owner",
                Pii::new("someone.else@example.net".to_owned()),
                &PiiPolicy::Redact,
            )
            .unwrap();
        g[0].resource = r;
        TraceEncoder::new(EncoderConfig::default())
            .encode(&g)
            .unwrap()
    };
    assert_eq!(a.encode_to_vec(), b.encode_to_vec());
}

#[test]
fn trace_state_follows_the_w3c_grammar() {
    assert_eq!(
        TraceState::parse("a=1 , b@c=2").unwrap().as_str(),
        "a=1,b@c=2"
    );
    assert_eq!(TraceState::parse("").unwrap().as_str(), "");
    for bad in [
        "A=1", "a", "a=", "a=1,a=2", "a=b=c", "@a=1", "a@=1", "a@b@c=1", "a=\u{7f}",
    ] {
        assert!(TraceState::parse(bad).is_err(), "{bad:?}");
    }
    let many: Vec<String> = (0..33).map(|i| format!("k{i}=v")).collect();
    assert_eq!(
        TraceState::parse(&many.join(",")),
        Err(TraceStateError::TooManyMembers)
    );
    assert_eq!(
        TraceState::parse(&"a".repeat(600)),
        Err(TraceStateError::TooLong)
    );
}

#[test]
fn malformed_related_payloads_are_typed_errors() {
    let batch = TraceEncoder::new(EncoderConfig::default())
        .encode(&trace_groups())
        .unwrap();
    let without = |t: ArrowPayloadType| {
        let mut b = batch.clone();
        b.arrow_payloads.retain(|p| p.r#type != t as i32);
        b
    };
    // Event attributes without events: their parents dangle.
    assert!(matches!(
        OtapDecoder::default().decode(&without(ArrowPayloadType::SpanEvents)),
        Err(DecodeError::DanglingParent("span_event_attrs.parent_id"))
    ));
    // Related payloads without the main record.
    assert!(matches!(
        OtapDecoder::default().decode(&without(ArrowPayloadType::Spans)),
        Err(DecodeError::MissingMainRecord)
    ));
    // Two signals in one batch.
    let mut mixed = batch.clone();
    let logs = LogsEncoder::new(EncoderConfig::default())
        .encode_logs(&[LogRecord::default()])
        .unwrap();
    mixed.arrow_payloads.push(logs.arrow_payloads[0].clone());
    assert!(matches!(
        OtapDecoder::default().decode(&mixed),
        Err(DecodeError::MixedSignals)
    ));
    // Delta ids that overflow u16.
    let ids = Schema::new(vec![
        Field::new("id", DataType::UInt16, true),
        Field::new(
            "start_time_unix_nano",
            DataType::Timestamp(TimeUnit::Nanosecond, None),
            false,
        ),
        Field::new(
            "duration_time_unix_nano",
            DataType::Duration(TimeUnit::Nanosecond),
            false,
        ),
        Field::new("trace_id", DataType::FixedSizeBinary(16), false),
        Field::new("span_id", DataType::FixedSizeBinary(8), false),
        Field::new("name", DataType::Utf8, false),
    ]);
    let rb = RecordBatch::try_new(
        Arc::new(ids),
        vec![
            Arc::new(UInt16Array::from(vec![60_000, 60_000])),
            Arc::new(TimestampNanosecondArray::from(vec![1, 2])),
            Arc::new(DurationNanosecondArray::from(vec![1, 2])),
            Arc::new(
                FixedSizeBinaryArray::try_from_iter([[1u8; 16], [2; 16]].into_iter()).unwrap(),
            ),
            Arc::new(FixedSizeBinaryArray::try_from_iter([[1u8; 8], [2; 8]].into_iter()).unwrap()),
            Arc::new(StringArray::from(vec!["a", "b"])),
        ],
    )
    .unwrap();
    let overflow = BatchArrowRecords {
        batch_id: 0,
        arrow_payloads: vec![ArrowPayload {
            schema_id: "x".into(),
            r#type: ArrowPayloadType::Spans as i32,
            record: strip_eos(ipc(&rb)),
        }],
        headers: vec![],
    };
    assert!(matches!(
        OtapDecoder::default().decode(&overflow),
        Err(DecodeError::IdOverflow("id"))
    ));
    // Unknown `encoding` metadata on an id column.
    let mut fields: Vec<Field> = rb
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    fields[0] = fields[0]
        .clone()
        .with_metadata([("encoding".to_owned(), "zigzag".to_owned())].into());
    let rb2 = RecordBatch::try_new(Arc::new(Schema::new(fields)), rb.columns().to_vec()).unwrap();
    let mut bad = overflow.clone();
    bad.arrow_payloads[0].record = strip_eos(ipc(&rb2));
    assert!(matches!(
        OtapDecoder::default().decode(&bad),
        Err(DecodeError::BadIdEncoding("id"))
    ));
}
