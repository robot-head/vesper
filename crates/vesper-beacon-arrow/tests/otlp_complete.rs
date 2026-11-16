//! Wave-nine Beacon completion (OTAP/OTLP side): structured attribute
//! values (maps, slices, bytes) through the PII boundary and CBOR `ser`
//! columns; gauges, sums, exponential histograms and exemplars; Arrow IPC
//! zstd buffer compression; and the HPACK header decoder.

use core::num::NonZeroU64;
use prost::Message;
use vesper_beacon::*;
use vesper_beacon_arrow::otap::proto::{ArrowPayloadType, BatchArrowRecords};
use vesper_beacon_arrow::otap::*;
use vesper_beacon_arrow::otlp::{self, proto as pb};

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}
fn roundtrip(batch: &BatchArrowRecords) -> BatchArrowRecords {
    BatchArrowRecords::decode(batch.encode_to_vec().as_slice()).unwrap()
}
fn str_(v: &str) -> DecodedValue {
    DecodedValue::Str(v.into())
}
fn span_record(span_id: u64) -> SpanRecord {
    let pool =
        SpanPool::<2>::new(0xbeac_0000_0000_0000_0000_0000_0000_0001, nz(span_id), 1).unwrap();
    let mut ring = TelemetryRing::<2>::default();
    pool.start_span(None, SpanSchema::Request, 100, 10)
        .unwrap()
        .finish_ok(&mut ring, 25, PrimaryAttribute::None)
        .unwrap();
    ring.records().next().unwrap()
}
fn payload_types(batch: &BatchArrowRecords) -> Vec<ArrowPayloadType> {
    batch
        .arrow_payloads
        .iter()
        .map(|p| ArrowPayloadType::try_from(p.r#type).unwrap())
        .collect()
}

const SECRET: &str = "alice@example.com";

/// A map with a nested slice, a PII leaf and bytes.
fn structured(policy: &PiiPolicy) -> AttributeValue {
    AttributeValue::map([
        ("region", AttributeValue::public("eu-west")),
        (
            "ports",
            AttributeValue::slice([
                AttributeValue::public(80i64),
                AttributeValue::public(443i64),
                AttributeValue::public(true),
                AttributeValue::public(0.5f64),
            ])
            .unwrap(),
        ),
        ("owner", policy.apply(Pii::new(SECRET.to_owned()))),
        ("digest", AttributeValue::bytes(b"\x00\x01\xfe")),
    ])
    .unwrap()
}
fn decoded_structured() -> DecodedValue {
    DecodedValue::Map(vec![
        ("region".into(), str_("eu-west")),
        (
            "ports".into(),
            DecodedValue::Slice(vec![
                DecodedValue::Int(80),
                DecodedValue::Int(443),
                DecodedValue::Bool(true),
                DecodedValue::Double(0.5),
            ]),
        ),
        ("owner".into(), str_(REDACTED_MARKER)),
        ("digest".into(), DecodedValue::Bytes(vec![0, 1, 0xfe])),
    ])
}

#[test]
fn structured_values_are_bounded_and_checked() {
    let leaf = AttributeValue::public(1i64);
    assert_eq!(
        AttributeValue::map([("a", leaf.clone()), ("a", leaf.clone())]),
        Err(AttributeError::DuplicateKey)
    );
    assert_eq!(
        AttributeValue::map([("", leaf.clone())]),
        Err(AttributeError::EmptyKey)
    );
    let mut deep = leaf.clone();
    for _ in 0..MAX_VALUE_DEPTH - 1 {
        deep = AttributeValue::slice([deep]).unwrap();
    }
    assert_eq!(deep.depth(), MAX_VALUE_DEPTH);
    assert_eq!(AttributeValue::slice([deep]), Err(AttributeError::TooDeep));
    assert_eq!(
        AttributeValue::slice(vec![leaf.clone(); MAX_VALUE_NODES]),
        Err(AttributeError::TooManyValues)
    );
    assert!(AttributeValue::slice(vec![leaf; MAX_VALUE_NODES - 1]).is_ok());
}

#[test]
fn structured_attributes_and_bodies_round_trip_through_otap_and_otlp() {
    let policy = PiiPolicy::Redact;
    let mut span = OtapSpan::from(span_record(1));
    span.attributes
        .insert_value("vesper.test.cfg", structured(&policy))
        .unwrap_err(); // reserved prefix
    span.attributes
        .insert_value("cfg", structured(&policy))
        .unwrap();
    span.attributes
        .insert_value("raw", AttributeValue::bytes(b"xyz"))
        .unwrap();
    // Two equal maps in a row: maps never repeat for the quasi-delta parent.
    let mut span2 = OtapSpan::from(span_record(2));
    span2
        .attributes
        .insert_value("cfg", structured(&policy))
        .unwrap();
    for encoding in [IdEncoding::Transport, IdEncoding::Plain] {
        let mut enc = TraceEncoder::new(EncoderConfig {
            id_encoding: encoding,
            ..EncoderConfig::default()
        });
        let batch = roundtrip(&enc.encode_spans(&[span.clone(), span2.clone()]).unwrap());
        let bytes = batch.encode_to_vec();
        assert!(
            !bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()),
            "PII inside a map crossed the boundary"
        );
        let d = OtapDecoder::default().decode(&batch).unwrap();
        assert_eq!(d.spans.len(), 2);
        let get = |s: &DecodedSpan, k: &str| {
            s.attributes
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get(&d.spans[0], "cfg"), Some(decoded_structured()));
        assert_eq!(
            get(&d.spans[0], "raw"),
            Some(DecodedValue::Bytes(b"xyz".to_vec()))
        );
        assert_eq!(get(&d.spans[1], "cfg"), Some(decoded_structured()));
    }

    // Log bodies carry structured values too.
    let log = LogRecord {
        body: Some(structured(&policy)),
        ..LogRecord::default()
    };
    let bytes_log = LogRecord {
        body: Some(AttributeValue::bytes(b"\xff")),
        ..LogRecord::default()
    };
    let batch = roundtrip(
        &LogsEncoder::new(EncoderConfig::default())
            .encode_logs(&[log.clone(), bytes_log.clone()])
            .unwrap(),
    );
    let d = OtapDecoder::default().decode(&batch).unwrap();
    assert_eq!(d.logs[0].body, Some(decoded_structured()));
    assert_eq!(d.logs[1].body, Some(DecodedValue::Bytes(vec![0xff])));

    // OTLP: kvlist, array and bytes values.
    let req = otlp::logs_request(&[ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        vec![log],
    )]);
    let body = req.resource_logs[0].scope_logs[0].log_records[0]
        .body
        .clone()
        .unwrap();
    let Some(pb::any_value::Value::KvlistValue(kv)) = body.value else {
        panic!("map body");
    };
    assert_eq!(kv.values.len(), 4);
    assert_eq!(kv.values[2].key, "owner");
    assert_eq!(
        kv.values[2].value.as_ref().unwrap().value,
        Some(pb::any_value::Value::StringValue(REDACTED_MARKER.into()))
    );
    assert!(matches!(
        kv.values[1].value.as_ref().unwrap().value,
        Some(pb::any_value::Value::ArrayValue(_))
    ));
    assert_eq!(
        kv.values[3].value.as_ref().unwrap().value,
        Some(pb::any_value::Value::BytesValue(vec![0, 1, 0xfe]))
    );
}

/// An attribute payload with a hand-written `ser` cell.
fn ser_attrs_batch(ty: u8, ser: &[u8]) -> BatchArrowRecords {
    use arrow_array::{
        types::UInt8Type, BinaryArray, DictionaryArray, RecordBatch, StringArray, UInt16Array,
        UInt8Array,
    };
    use std::sync::Arc;
    let mut enc = TraceEncoder::new(EncoderConfig::default());
    let mut batch = enc.encode_spans(&[OtapSpan::from(span_record(1))]).unwrap();
    let schema = Arc::new(schema::attrs16_schema());
    let key = DictionaryArray::<UInt8Type>::try_new(
        UInt8Array::from(vec![0u8]),
        Arc::new(StringArray::from(vec!["k"])),
    )
    .unwrap();
    let str_col = DictionaryArray::<arrow_array::types::UInt16Type>::try_new(
        UInt16Array::from(vec![None::<u16>]),
        Arc::new(StringArray::from(Vec::<&str>::new())),
    )
    .unwrap();
    let rb = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt16Array::from(vec![0u16])),
            Arc::new(key),
            Arc::new(UInt8Array::from(vec![ty])),
            Arc::new(str_col),
            Arc::new(arrow_array::Int64Array::from(vec![None::<i64>])),
            Arc::new(arrow_array::Float64Array::from(vec![None::<f64>])),
            Arc::new(arrow_array::BooleanArray::from(vec![None::<bool>])),
            Arc::new(BinaryArray::from(vec![None::<&[u8]>])),
            Arc::new(BinaryArray::from(vec![Some(ser)])),
        ],
    )
    .unwrap();
    let mut w = arrow_ipc::writer::StreamWriter::try_new(Vec::new(), &schema).unwrap();
    w.write(&rb).unwrap();
    batch.arrow_payloads.push(proto::ArrowPayload {
        schema_id: "attrs-handmade".into(),
        r#type: ArrowPayloadType::SpanAttrs as i32,
        record: {
            let mut v = w.into_inner().unwrap();
            v.truncate(v.len() - 8);
            v
        },
    });
    batch
}

#[test]
fn ser_cells_from_upstream_style_cbor_decode_and_malformed_ones_are_typed_errors() {
    // Upstream writes indefinite-length maps and arrays (fxamacker/cbor
    // `StartIndefiniteMap`): {"a": [1, "x", null]}.
    let upstream = [0xbf, 0x61, b'a', 0x9f, 0x01, 0x61, b'x', 0xf6, 0xff, 0xff];
    let d = OtapDecoder::default()
        .decode(&ser_attrs_batch(schema::value_type::MAP, &upstream))
        .unwrap();
    assert_eq!(
        d.spans[0].attributes,
        vec![(
            "k".to_string(),
            DecodedValue::Map(vec![(
                "a".into(),
                DecodedValue::Slice(vec![DecodedValue::Int(1), str_("x"), DecodedValue::Empty])
            )])
        )]
    );
    let deep: Vec<u8> = std::iter::repeat_n(0x81u8, 200).chain([0x01]).collect();
    let bad: [(&str, u8, Vec<u8>); 6] = [
        ("truncated", schema::value_type::MAP, vec![0xbf, 0x61]),
        (
            "trailing bytes",
            schema::value_type::SLICE,
            vec![0x80, 0x00],
        ),
        ("map tagged as slice", schema::value_type::SLICE, vec![0xa0]),
        (
            "integer key",
            schema::value_type::MAP,
            vec![0xa1, 0x01, 0x01],
        ),
        ("too deep", schema::value_type::SLICE, deep),
        // A byte string claiming 2^63 bytes.
        (
            "huge declared length",
            schema::value_type::SLICE,
            vec![0x81, 0x5b, 0x80, 0, 0, 0, 0, 0, 0, 0],
        ),
    ];
    for (what, ty, cell) in bad {
        let err = OtapDecoder::default()
            .decode(&ser_attrs_batch(ty, &cell))
            .unwrap_err();
        assert!(
            matches!(err, DecodeError::BadSerializedValue),
            "{what}: {err:?}"
        );
    }
}

fn exemplar(value: NumberValue, span: u64) -> Exemplar {
    let mut e = Exemplar::new(1_000, value);
    e.span_id = Some(span);
    e.trace_id = Some(0xabc);
    e
}

fn all_kinds<'a>(h: &'a FastLogHistogram, exp: &ExponentialHistogram) -> Vec<MetricPoint<'a>> {
    let policy = PiiPolicy::Redact;
    let mut gauge = NumberPoint::gauge("queue.depth", "1", 50, NumberValue::Int(7));
    gauge.attributes.insert("queue", "fast").unwrap();
    gauge.exemplars.push(exemplar(NumberValue::Int(7), 1));
    let gauge2 = NumberPoint::gauge("queue.depth", "1", 60, NumberValue::Int(9));
    let mut sum = NumberPoint::sum(
        "bytes.sent",
        "By",
        true,
        Temporality::Delta,
        10,
        60,
        NumberValue::Double(1.5),
    );
    sum.description = Some("bytes sent");
    let mut e = exemplar(NumberValue::Double(1.5), 2);
    e.filtered_attributes
        .insert_pii("user", Pii::new(SECRET.to_owned()), &policy)
        .unwrap();
    sum.exemplars.push(e.clone());
    // Same value twice: the exemplar parent id is delta-encoded.
    sum.exemplars.push(e);
    let mut hp =
        HistogramPoint::new("rpc.duration", "ns", 1, 60, h).with_bin_exemplars(55, Some(9));
    hp.attributes.insert("rpc", "Export").unwrap();
    let mut ep = ExpHistogramPoint::from_histogram("rpc.size", "By", 1, 60, exp);
    ep.exemplars.push(exemplar(NumberValue::Int(4096), 3));
    ep.attributes.insert("rpc", "Export").unwrap();
    vec![
        gauge.into(),
        gauge2.into(),
        sum.into(),
        hp.into(),
        ep.into(),
    ]
}

#[test]
fn gauges_sums_histograms_and_exponential_histograms_round_trip_with_exemplars() {
    let mut h = FastLogHistogram::default();
    for v in [3, 30, 300] {
        h.record_with_exemplar(v, nz(v)).unwrap();
    }
    let mut exp = ExponentialHistogram::default();
    for v in [0, 1, 2, 3, 1000, 4096, 1 << 40] {
        exp.record(v).unwrap();
    }
    let points = all_kinds(&h, &exp);
    for encoding in [IdEncoding::Transport, IdEncoding::Plain] {
        let mut enc = MetricsEncoder::new(EncoderConfig {
            id_encoding: encoding,
            ..EncoderConfig::default()
        });
        let batch = roundtrip(&enc.encode_points(&points).unwrap());
        let bytes = batch.encode_to_vec();
        assert!(!bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
        let types = payload_types(&batch);
        for t in [
            ArrowPayloadType::UnivariateMetrics,
            ArrowPayloadType::NumberDataPoints,
            ArrowPayloadType::NumberDpAttrs,
            ArrowPayloadType::NumberDpExemplars,
            ArrowPayloadType::NumberDpExemplarAttrs,
            ArrowPayloadType::HistogramDataPoints,
            ArrowPayloadType::HistogramDpAttrs,
            ArrowPayloadType::HistogramDpExemplars,
            ArrowPayloadType::ExpHistogramDataPoints,
            ArrowPayloadType::ExpHistogramDpAttrs,
            ArrowPayloadType::ExpHistogramDpExemplars,
        ] {
            assert!(types.contains(&t), "{t:?} missing from {types:?}");
        }
        let d = OtapDecoder::default().decode(&batch).unwrap();
        // Numbers: the two gauge points share a metric row; the sum has its own.
        assert_eq!(d.numbers.len(), 3);
        let g = &d.numbers[0];
        assert_eq!(
            (g.name.as_str(), g.metric_type, g.value),
            (
                "queue.depth",
                schema::METRIC_TYPE_GAUGE,
                Some(NumberValue::Int(7))
            )
        );
        assert_eq!(g.attributes, vec![("queue".into(), str_("fast"))]);
        assert_eq!(g.exemplars.len(), 1);
        assert_eq!(g.exemplars[0].span_id, Some(1));
        assert_eq!(g.exemplars[0].trace_id, Some(0xabc));
        assert_eq!(d.numbers[1].value, Some(NumberValue::Int(9)));
        assert_eq!(d.numbers[1].time_unix_ns, 60);
        let s = &d.numbers[2];
        assert_eq!(s.metric_type, schema::METRIC_TYPE_SUM);
        assert_eq!(s.aggregation_temporality, Some(schema::TEMPORALITY_DELTA));
        assert_eq!(s.is_monotonic, Some(true));
        assert_eq!(s.description.as_deref(), Some("bytes sent"));
        assert_eq!(s.start_unix_ns, Some(10));
        assert_eq!(s.exemplars.len(), 2);
        assert_eq!(
            s.exemplars[1].filtered_attributes,
            vec![("user".into(), str_(REDACTED_MARKER))]
        );
        // Histogram with bin exemplars.
        let hist = &d.histograms[0];
        assert_eq!(hist.count, Some(3));
        assert_eq!(hist.exemplars.len(), 3);
        assert_eq!(hist.exemplars[0].value, NumberValue::Int(3));
        assert_eq!(hist.exemplars[0].span_id, Some(3));
        assert_eq!(hist.exemplars[0].trace_id, Some(9));
        // Exponential histogram.
        let e = &d.exp_histograms[0];
        assert_eq!(e.count, 7);
        assert_eq!(e.zero_count, 1);
        assert_eq!(e.scale, exp.scale());
        assert_eq!(e.positive.offset, exp.offset());
        assert_eq!(e.positive.counts, exp.positive_counts());
        assert_eq!(e.sum, Some(exp.sum() as f64));
        assert_eq!(e.min, Some(0.0));
        assert_eq!(e.max, Some((1u64 << 40) as f64));
        assert_eq!(
            e.aggregation_temporality,
            Some(schema::TEMPORALITY_CUMULATIVE)
        );
        assert_eq!(e.exemplars[0].value, NumberValue::Int(4096));
        assert_eq!(e.attributes, vec![("rpc".into(), str_("Export"))]);
        // Five points, four metric rows.
        assert_eq!(batch.arrow_payloads.len(), types.len());
    }

    // OTLP shapes.
    let req = otlp::metric_points_request(&[ResourceGroup::single(
        Resource::default(),
        Scope::default(),
        points,
    )]);
    let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;
    assert_eq!(metrics.len(), 4);
    let Some(pb::metric::Data::Gauge(g)) = &metrics[0].data else {
        panic!("gauge")
    };
    assert_eq!(g.data_points.len(), 2);
    assert_eq!(
        g.data_points[0].value,
        Some(pb::number_data_point::Value::AsInt(7))
    );
    assert_eq!(g.data_points[0].exemplars[0].span_id, 1u64.to_be_bytes());
    let Some(pb::metric::Data::Sum(s)) = &metrics[1].data else {
        panic!("sum")
    };
    assert!(s.is_monotonic);
    assert_eq!(s.aggregation_temporality, schema::TEMPORALITY_DELTA);
    assert_eq!(metrics[1].description, "bytes sent");
    let Some(pb::metric::Data::Histogram(hh)) = &metrics[2].data else {
        panic!("histogram")
    };
    assert_eq!(hh.data_points[0].exemplars.len(), 3);
    let Some(pb::metric::Data::ExponentialHistogram(eh)) = &metrics[3].data else {
        panic!("exponential histogram")
    };
    let p = &eh.data_points[0];
    assert_eq!((p.count, p.zero_count, p.scale), (7, 1, exp.scale()));
    assert_eq!(
        p.positive.as_ref().unwrap().bucket_counts,
        exp.positive_counts()
    );
    // Round trip through protobuf bytes.
    let back = pb::ExportMetricsServiceRequest::decode(req.encode_to_vec().as_slice()).unwrap();
    assert_eq!(back, req);
}

#[test]
fn invalid_exponential_points_and_mismatched_kinds_are_typed_errors() {
    let exp = ExponentialHistogram::default();
    let mut bad = ExpHistogramPoint::from_histogram("x", "", 1, 2, &exp);
    bad.count = 5;
    let mut enc = MetricsEncoder::new(EncoderConfig::default());
    assert!(matches!(
        enc.encode_points(&[bad.clone().into()]),
        Err(EncodeError::InvalidPoint("count"))
    ));
    bad.count = 0;
    bad.scale = 21;
    assert!(matches!(
        enc.encode_points(&[bad.into()]),
        Err(EncodeError::InvalidPoint("scale"))
    ));

    // A number point whose metric row says histogram: rewrite metric_type.
    use arrow_array::{RecordBatch, UInt8Array};
    use std::sync::Arc;
    let p = NumberPoint::gauge("g", "", 1, NumberValue::Int(1));
    let batch = MetricsEncoder::new(EncoderConfig::default())
        .encode_points(&[p.into()])
        .unwrap();
    let mut reader =
        arrow_ipc::reader::StreamReader::try_new(batch.arrow_payloads[0].record.as_slice(), None)
            .unwrap();
    let rb = reader.next().unwrap().unwrap();
    let i = rb.schema().index_of("metric_type").unwrap();
    let mut cols = rb.columns().to_vec();
    cols[i] = Arc::new(UInt8Array::from(vec![schema::METRIC_TYPE_HISTOGRAM]));
    let rb = RecordBatch::try_new(rb.schema(), cols).unwrap();
    let mut w = arrow_ipc::writer::StreamWriter::try_new(Vec::new(), &rb.schema()).unwrap();
    w.write(&rb).unwrap();
    let mut forged = batch.clone();
    forged.arrow_payloads[0].record = {
        let mut v = w.into_inner().unwrap();
        v.truncate(v.len() - 8);
        v
    };
    forged.arrow_payloads[0].schema_id = "forged".into();
    assert!(matches!(
        OtapDecoder::default().decode(&forged),
        Err(DecodeError::PointKindMismatch(
            schema::METRIC_TYPE_HISTOGRAM
        ))
    ));
}

#[test]
fn zstd_ipc_buffers_round_trip_and_are_bounded() {
    let mut h = FastLogHistogram::default();
    for v in 0..2000u64 {
        h.record_with_exemplar(v * 7, nz(v + 1)).unwrap();
    }
    let mut exp = ExponentialHistogram::default();
    exp.record(5).unwrap();
    let points = all_kinds(&h, &exp);
    let mut spans = Vec::new();
    for i in 0..200 {
        let mut s = OtapSpan::from(span_record(i + 1));
        s.attributes
            .insert("http.route", "/a/long/route/value/repeated")
            .unwrap();
        spans.push(s);
    }
    let config = EncoderConfig {
        ipc_compression: IpcCompression::Zstd,
        ..EncoderConfig::default()
    };
    let plain = TraceEncoder::new(EncoderConfig::default())
        .encode_spans(&spans)
        .unwrap();
    let mut enc = TraceEncoder::new(config);
    let mut dec = OtapDecoder::default();
    for _ in 0..2 {
        let zipped = enc.encode_spans(&spans).unwrap();
        // The IPC body really is compressed.
        assert!(zipped.encoded_len() < plain.encoded_len());
        let d = dec.decode(&roundtrip(&zipped)).unwrap();
        assert_eq!(d.spans.len(), 200);
        assert_eq!(
            d.spans[199].attributes,
            vec![("http.route".into(), str_("/a/long/route/value/repeated"))]
        );
    }
    let m = MetricsEncoder::new(config).encode_points(&points).unwrap();
    let d = OtapDecoder::default().decode(&m).unwrap();
    assert_eq!(d.histograms[0].count, Some(2000));
    assert_eq!(d.numbers.len(), 3);

    // The decompressed-bytes limit is enforced before decompression.
    let zipped = TraceEncoder::new(config).encode_spans(&spans).unwrap();
    let mut small = OtapDecoder::new(DecodeLimits {
        max_decompressed_bytes: 1024,
        ..DecodeLimits::default()
    });
    assert!(matches!(
        small.decode(&zipped),
        Err(DecodeError::DecompressedTooLarge)
    ));
    // Corrupted compressed data is a typed error, never a panic.
    let mut broken = zipped.clone();
    for p in &mut broken.arrow_payloads {
        let n = p.record.len();
        for b in &mut p.record[n / 2..n.saturating_sub(16)] {
            *b ^= 0x5a;
        }
    }
    assert!(OtapDecoder::default().decode(&broken).is_err());
}

#[test]
fn hpack_headers_decode_in_stream_order_with_a_shared_dynamic_table() {
    let mut encoder = loona_hpack::Encoder::new();
    let first = encoder.encode([
        (
            &b"traceparent"[..],
            &b"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"[..],
        ),
        (&b"x-tenant"[..], &b"vesper"[..]),
    ]);
    // The second block refers to the first's dynamic-table entries.
    let second = encoder.encode([(&b"x-tenant"[..], &b"vesper"[..])]);
    assert!(second.len() < 4, "second block should be an index");
    let limits = DecodeLimits {
        headers: HeaderPolicy::Decode {
            max_bytes: 4096,
            max_table_size: DEFAULT_HPACK_TABLE_BYTES,
        },
        ..DecodeLimits::default()
    };
    let mut dec = OtapDecoder::new(limits);
    let mut enc = TraceEncoder::new(EncoderConfig::default());
    let mut batch = enc.encode_spans(&[OtapSpan::from(span_record(1))]).unwrap();
    batch.headers = first;
    let d = dec.decode(&batch).unwrap();
    assert_eq!(
        d.header_fields,
        vec![
            (
                "traceparent".to_string(),
                b"00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_vec()
            ),
            ("x-tenant".to_string(), b"vesper".to_vec()),
        ]
    );
    let mut batch = enc.encode_spans(&[OtapSpan::from(span_record(2))]).unwrap();
    batch.headers = second.clone();
    let d = dec.decode(&batch).unwrap();
    assert_eq!(
        d.header_fields,
        vec![("x-tenant".to_string(), b"vesper".to_vec())]
    );

    // A fresh decoder lacks the table: a typed error.
    let mut fresh = OtapDecoder::new(limits);
    let mut batch = TraceEncoder::new(EncoderConfig::default())
        .encode_spans(&[OtapSpan::from(span_record(1))])
        .unwrap();
    batch.headers = second;
    assert!(matches!(
        fresh.decode(&batch),
        Err(DecodeError::BadHeaders(_))
    ));
    // A table size update above the limit.
    let mut tiny = HeaderDecoder::new(100, 4096);
    assert!(tiny.decode(&[0x3f, 0xe1, 0x1f]).is_err()); // size update to 4096
                                                        // Decoded size bound: repeated indexed references are not copied past it.
    let mut amp = loona_hpack::Encoder::new();
    let big = vec![b'v'; 3000];
    let block = amp.encode([(&b"k"[..], &big[..])]);
    let mut d = HeaderDecoder::new(DEFAULT_HPACK_TABLE_BYTES, 4096);
    d.decode(&block).unwrap();
    let repeat: Vec<u8> = std::iter::repeat_n(0xbe, 100).collect(); // index 62
    assert!(matches!(
        d.decode(&repeat),
        Err(DecodeError::HeadersTooLarge)
    ));
}

#[test]
fn hpack_decoder_never_panics_on_random_blocks() {
    // A fixed xorshift stream: random blocks and mutated valid blocks.
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut enc = loona_hpack::Encoder::new();
    let valid = enc.encode([(&b"grpc-timeout"[..], &b"1S"[..]), (&b"a"[..], &b"b"[..])]);
    for round in 0..20_000 {
        let mut d = HeaderDecoder::new(DEFAULT_HPACK_TABLE_BYTES, 1 << 16);
        let len = (next() % 48) as usize;
        let block: Vec<u8> = if round % 2 == 0 {
            (0..len).map(|_| next() as u8).collect()
        } else {
            let mut b = valid.clone();
            let i = (next() as usize) % b.len();
            b[i] ^= next() as u8;
            b
        };
        let _ = d.decode(&block);
    }
}

#[test]
fn emitted_metric_schemas_follow_upstream() {
    // Column names and types of upstream NumberDataPointSchema,
    // EHistogramDataPointSchema and ExemplarSchema (otel-arrow go v0.60.3).
    let check = |ours: arrow_schema::Schema, reference: &[(&str, &str)]| {
        let names: Vec<String> = ours
            .fields()
            .iter()
            .map(|f| format!("{}:{}", f.name(), f.data_type()))
            .collect();
        let expected: Vec<String> = reference.iter().map(|(n, t)| format!("{n}:{t}")).collect();
        assert_eq!(names, expected);
    };
    check(
        schema::number_points_schema(),
        &[
            ("id", "UInt32"),
            ("parent_id", "UInt16"),
            ("start_time_unix_nano", "Timestamp(Nanosecond, None)"),
            ("time_unix_nano", "Timestamp(Nanosecond, None)"),
            ("int_value", "Int64"),
            ("double_value", "Float64"),
            ("flags", "UInt32"),
        ],
    );
    check(
        schema::exemplars_schema(),
        &[
            ("id", "UInt32"),
            ("parent_id", "UInt32"),
            ("time_unix_nano", "Timestamp(Nanosecond, None)"),
            ("int_value", "Int64"),
            ("double_value", "Float64"),
            ("span_id", "FixedSizeBinary(8)"),
            ("trace_id", "FixedSizeBinary(16)"),
        ],
    );
    let exp: Vec<String> = schema::exp_histogram_points_schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    assert_eq!(
        exp,
        [
            "id",
            "parent_id",
            "start_time_unix_nano",
            "time_unix_nano",
            "count",
            "sum",
            "scale",
            "zero_count",
            "positive",
            "negative",
            "flags",
            "min",
            "max",
            "zero_threshold"
        ]
    );
}
