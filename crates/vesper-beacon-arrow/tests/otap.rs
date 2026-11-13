//! OTAP encode/decode, dictionary deltas, schema reset, transport and PII
//! redaction acceptance tests.

use arrow_ipc::{root_as_message, MessageHeader};
use core::num::NonZeroU64;
use prost::Message;
use std::net::{TcpListener, TcpStream};
use vesper_beacon::*;
use vesper_beacon_arrow::otap::proto::{ArrowPayloadType, BatchArrowRecords, BatchStatus};
use vesper_beacon_arrow::otap::*;

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}

/// Completed records: a parentless OK request, an error child, a cancelled
/// background span and a redacted primary attribute.
fn records() -> Vec<SpanRecord> {
    let pool = SpanPool::<4>::new(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210, nz(10), 1).unwrap();
    let mut ring = TelemetryRing::<8>::default();
    let root = pool
        .start_span(None, SpanSchema::Request, 1_700_000_000_000, 100)
        .unwrap();
    let child = pool
        .start_span(
            Some(root.context()),
            SpanSchema::StorageAppend,
            1_700_000_000_050,
            150,
        )
        .unwrap();
    child
        .finish_err(
            &mut ring,
            175,
            ErrorCode::Storage,
            PrimaryAttribute::ByteCount(4096),
        )
        .unwrap();
    let bg = pool
        .start_span(None, SpanSchema::Background, 1_700_000_000_060, 160)
        .unwrap();
    bg.cancel(&mut ring, 161).unwrap();
    let secret = Secret::new("token");
    root.finish_ok(&mut ring, 400, PrimaryAttribute::Redacted(secret.redact()))
        .unwrap();
    let other = pool
        .start_span(None, SpanSchema::Request, 1_700_000_000_070, 170)
        .unwrap();
    other
        .finish_ok(&mut ring, 171, PrimaryAttribute::ItemCount(3))
        .unwrap();
    ring.records().collect()
}

fn roundtrip(batch: &BatchArrowRecords) -> BatchArrowRecords {
    BatchArrowRecords::decode(batch.encode_to_vec().as_slice()).unwrap()
}

fn check_span(decoded: &DecodedSpan, record: &SpanRecord) {
    assert_eq!(decoded.trace_id, record.context.trace_id());
    assert_eq!(decoded.span_id, record.context.span_id());
    assert_eq!(
        decoded.parent_span_id,
        (record.parent_span_id != 0).then_some(record.parent_span_id)
    );
    assert_eq!(decoded.start_unix_ns, record.start_unix_ns);
    assert_eq!(decoded.duration_ns, record.duration_ns);
    assert_eq!(decoded.flags, Some(u32::from(record.context.trace_flags())));
    assert_eq!(decoded.name, span_name(record.schema));
    match record.status {
        SpanStatus::Ok => {
            assert_eq!(decoded.status_code, Some(schema::STATUS_CODE_OK));
            assert_eq!(decoded.status_message, None);
        }
        SpanStatus::Error(code) => {
            assert_eq!(decoded.status_code, Some(schema::STATUS_CODE_ERROR));
            assert_eq!(decoded.status_message.as_deref(), Some(error_message(code)));
        }
    }
}

/// IPC messages of one payload: (header type, is_delta, dictionary length).
fn messages(bytes: &[u8]) -> Vec<(MessageHeader, bool, i64)> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        assert_eq!(&rest[..4], &[0xff; 4], "continuation marker");
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

#[test]
fn spans_round_trip_with_primary_and_user_attributes() {
    let records = records();
    let mut spans: Vec<OtapSpan> = records.iter().copied().map(OtapSpan::from).collect();
    spans[0]
        .attributes
        .insert("http.method", "POST")
        .unwrap()
        .insert("retry", 2_i64)
        .unwrap()
        .insert("cached", false)
        .unwrap()
        .insert("ratio", 0.25_f64)
        .unwrap();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let batch = roundtrip(&encoder.encode_spans(&spans).unwrap());
    assert_eq!(batch.batch_id, 0);
    assert_eq!(batch.arrow_payloads.len(), 2);
    assert_eq!(
        batch.arrow_payloads[0].r#type,
        ArrowPayloadType::Spans as i32
    );
    assert_eq!(batch.arrow_payloads[0].schema_id, "spans:0");
    assert_eq!(
        batch.arrow_payloads[1].r#type,
        ArrowPayloadType::SpanAttrs as i32
    );
    let mut decoder = OtapDecoder::default();
    let decoded = decoder.decode(&batch).unwrap();
    assert_eq!(decoded.spans.len(), records.len());
    for (d, r) in decoded.spans.iter().zip(&records) {
        check_span(d, r);
    }
    let by_name = |i: usize| -> Vec<(String, DecodedValue)> { decoded.spans[i].attributes.clone() };
    assert_eq!(
        by_name(0),
        vec![
            ("http.method".into(), DecodedValue::Str("POST".into())),
            ("retry".into(), DecodedValue::Int(2)),
            ("cached".into(), DecodedValue::Bool(false)),
            ("ratio".into(), DecodedValue::Double(0.25)),
            ("vesper.byte_count".into(), DecodedValue::Int(4096)),
        ]
    );
    assert_eq!(by_name(1), vec![]);
    assert_eq!(
        by_name(2),
        vec![(
            "vesper.attribute".into(),
            DecodedValue::Str(REDACTED_MARKER.into())
        )]
    );
    assert_eq!(
        by_name(3),
        vec![("vesper.item_count".into(), DecodedValue::Int(3))]
    );
}

#[test]
fn empty_batches_and_missing_attribute_payloads_round_trip() {
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let mut decoder = OtapDecoder::default();
    let empty = encoder.encode_spans(&[]).unwrap();
    assert_eq!(empty.arrow_payloads.len(), 1);
    assert!(decoder.decode(&roundtrip(&empty)).unwrap().spans.is_empty());
    let records = records();
    let plain = OtapSpan::from(SpanRecord {
        attribute: PrimaryAttribute::None,
        ..records[1]
    });
    let batch = encoder.encode_spans(&[plain]).unwrap();
    assert_eq!(batch.batch_id, 1);
    assert_eq!(
        batch.arrow_payloads.len(),
        1,
        "no SPAN_ATTRS without attributes"
    );
    let decoded = decoder.decode(&batch).unwrap();
    check_span(&decoded.spans[0], &records[1]);
}

#[test]
fn later_batches_carry_only_delta_dictionaries_and_no_schema() {
    let records = records();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let mut decoder = OtapDecoder::default();
    // Batch 0: names {request}; batch 1: request again (no dictionary
    // message at all); batch 2: adds storage_append and background.
    let first = encoder.encode_spans(&[records[2].into()]).unwrap();
    let same = encoder.encode_spans(&[records[2].into()]).unwrap();
    let grown = encoder
        .encode_spans(&[records[0].into(), records[1].into(), records[2].into()])
        .unwrap();
    let kinds = |b: &BatchArrowRecords| messages(&b.arrow_payloads[0].record);
    use MessageHeader as H;
    assert_eq!(
        kinds(&first),
        vec![
            (H::Schema, false, 0),
            (H::DictionaryBatch, false, 1), // name: request
            (H::DictionaryBatch, false, 0), // status_message: still empty
            (H::RecordBatch, false, 0)
        ],
        "first payload: schema, full dictionaries, batch"
    );
    assert_eq!(kinds(&same), vec![(H::RecordBatch, false, 0)]);
    assert_eq!(
        kinds(&grown),
        vec![
            (H::DictionaryBatch, true, 2), // name: +storage_append, +background
            (H::DictionaryBatch, true, 2), // status_message: +storage, +cancelled
            (H::RecordBatch, false, 0)
        ]
    );
    for batch in [&first, &same, &grown] {
        assert_eq!(batch.arrow_payloads[0].schema_id, "spans:0");
    }
    // Attribute strings: the redacted marker first, then nothing new.
    let attrs = |b: &BatchArrowRecords| messages(&b.arrow_payloads[1].record);
    assert_eq!(attrs(&first)[0].0, H::Schema);
    assert!(attrs(&same).iter().all(|m| m.0 != H::Schema));
    for batch in [first, same, grown.clone()] {
        decoder.decode(&roundtrip(&batch)).unwrap();
    }
    let decoded = decoder
        .decode(&encoder.encode_spans(&[records[0].into()]).unwrap())
        .unwrap();
    assert_eq!(decoded.spans[0].name, "vesper.storage_append");
    // A fresh decoder cannot resolve a delta-only payload: typed error.
    let mut late = OtapDecoder::default();
    assert!(late.decode(&grown).is_err());
}

#[test]
fn dictionary_growth_past_the_limit_resets_the_stream() {
    let key = || PiiPolicy::Fingerprint(FingerprintKey::new([7; 32]));
    let mut encoder = TraceEncoder::new(EncoderConfig {
        max_dictionary_entries: 3,
        ..EncoderConfig::default()
    });
    let mut decoder = OtapDecoder::default();
    let base = records()[1];
    let span = |email: &str| {
        let mut s = OtapSpan::from(base);
        s.attributes
            .insert_pii("user.email", Pii::new(email.to_owned()), &key())
            .unwrap();
        s
    };
    // byte_count is an int, so `str` holds only fingerprints: 3 fit.
    let mut ids = Vec::new();
    for email in ["a@x", "b@x", "c@x", "d@x", "a@x"] {
        let batch = encoder.encode_spans(&[span(email)]).unwrap();
        let attrs = &batch.arrow_payloads[1];
        ids.push(attrs.schema_id.clone());
        let decoded = decoder.decode(&roundtrip(&batch)).unwrap();
        let DecodedValue::Str(fp) = &decoded.spans[0].attributes[0].1 else {
            panic!("fingerprint is a string")
        };
        assert!(fp.starts_with(FINGERPRINT_PREFIX) && fp.len() == FINGERPRINT_PREFIX.len() + 17);
        assert!(!fp.contains(email));
    }
    assert_eq!(ids[..3], ["span_attrs:1", "span_attrs:1", "span_attrs:1"]);
    assert_eq!(
        ids[3], "span_attrs:2",
        "fourth distinct value resets the stream"
    );
    assert_eq!(ids[4], "span_attrs:2");
    assert_eq!(encoder.resets(), 1);
    // The decoder dropped the replaced stream: one spans + one attrs stream.
    assert_eq!(decoder.stream_count(), 2);
    // A single batch needing more than the limit is a typed error and
    // resets every stream.
    let too_many: Vec<OtapSpan> = ["p", "q", "r", "s"].iter().map(|e| span(e)).collect();
    assert!(matches!(
        encoder.encode_spans(&too_many),
        Err(EncodeError::DictionaryOverflow { column: "str" })
    ));
    let next = encoder.encode_spans(&[span("z")]).unwrap();
    assert_eq!(next.arrow_payloads[0].schema_id, "spans:3");
    assert_eq!(
        messages(&next.arrow_payloads[0].record)[0].0,
        MessageHeader::Schema
    );
    decoder.decode(&next).unwrap();
}

#[test]
fn values_outside_the_signed_otlp_domain_are_rejected() {
    let mut r = records()[3];
    r.attribute = PrimaryAttribute::ItemCount(u64::MAX);
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    assert!(matches!(
        encoder.encode_spans(&[r.into()]),
        Err(EncodeError::ValueOutOfRange {
            column: "vesper.item_count"
        })
    ));
    r.attribute = PrimaryAttribute::None;
    r.start_unix_ns = u64::MAX;
    assert!(matches!(
        encoder.encode_spans(&[r.into()]),
        Err(EncodeError::ValueOutOfRange {
            column: "start_time_unix_nano"
        })
    ));
    let mut small = TraceEncoder::new(EncoderConfig {
        max_rows: 1,
        ..EncoderConfig::default()
    });
    let two = [records()[0].into(), records()[1].into()];
    assert!(matches!(
        small.encode_spans(&two),
        Err(EncodeError::RowLimit)
    ));
}

// ---------------------------------------------------------------- redaction

const EMAIL: &str = "alice.sensitive@example.com";
const TOKEN: &str = "Bearer sk_live_51HxQ_secret_token";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn pii_span(email: &str, token: &str, policy: &PiiPolicy) -> OtapSpan {
    let mut span = OtapSpan::from(records()[0]);
    span.attributes
        .insert("route", "/login")
        .unwrap()
        .insert_pii("user.email", Pii::new(email.to_owned()), policy)
        .unwrap()
        .insert_pii("auth", Secret::new(token.as_bytes().to_vec()), policy)
        .unwrap();
    span
}

#[test]
fn pii_bytes_never_reach_an_exported_batch() {
    for policy in [
        PiiPolicy::Redact,
        PiiPolicy::Fingerprint(FingerprintKey::new([42; 32])),
    ] {
        let mut encoder = TraceEncoder::new(EncoderConfig::default());
        let mut wire = Vec::new();
        for _ in 0..3 {
            let batch = encoder
                .encode_spans(&[pii_span(EMAIL, TOKEN, &policy)])
                .unwrap();
            wire.extend(batch.encode_to_vec());
        }
        for needle in [EMAIL, TOKEN, "alice", "sk_live", "example.com"] {
            assert!(
                !contains(&wire, needle.as_bytes()),
                "{needle} leaked under {policy:?}"
            );
        }
        // The stored attribute set itself never held the raw value either.
        let span = pii_span(EMAIL, TOKEN, &policy);
        assert!(!format!("{:?}", span.attributes).contains("alice"));
        assert!(contains(&wire, b"/login"), "public attribute exported");
    }
}

#[test]
fn redacted_export_is_byte_identical_for_different_pii() {
    let run = |email: &str, token: &str| -> Vec<u8> {
        let mut encoder = TraceEncoder::new(EncoderConfig::default());
        let mut out = Vec::new();
        for _ in 0..2 {
            out.extend(
                encoder
                    .encode_spans(&[pii_span(email, token, &PiiPolicy::Redact)])
                    .unwrap()
                    .encode_to_vec(),
            );
        }
        out
    };
    assert_eq!(run(EMAIL, TOKEN), run("bob@example.org", "x"));
    let decoded = OtapDecoder::default()
        .decode(
            &TraceEncoder::new(EncoderConfig::default())
                .encode_spans(&[pii_span(EMAIL, TOKEN, &PiiPolicy::Redact)])
                .unwrap(),
        )
        .unwrap();
    let attrs = &decoded.spans[0].attributes;
    assert_eq!(
        attrs[1],
        (
            "user.email".into(),
            DecodedValue::Str(REDACTED_MARKER.into())
        )
    );
    assert_eq!(
        attrs[2],
        ("auth".into(), DecodedValue::Str(REDACTED_MARKER.into()))
    );
}

#[test]
fn fingerprints_are_keyed_and_stable() {
    let fp = |key: u8, email: &str| {
        let policy = PiiPolicy::Fingerprint(FingerprintKey::new([key; 32]));
        let mut attrs = SpanAttributes::new();
        attrs
            .insert_pii("user.email", Pii::new(email.to_owned()), &policy)
            .unwrap();
        let value = attrs.iter().next().unwrap().1;
        value.export_str().unwrap()
    };
    assert_eq!(fp(1, EMAIL), fp(1, EMAIL));
    assert_ne!(fp(1, EMAIL), fp(2, EMAIL), "different keys");
    assert_ne!(fp(1, EMAIL), fp(1, "bob@example.org"), "different values");
}

#[test]
fn attribute_keys_are_validated_before_storage() {
    let mut attrs = SpanAttributes::new();
    assert_eq!(
        attrs.insert("", 1_i64).unwrap_err(),
        AttributeError::EmptyKey
    );
    assert_eq!(
        attrs.insert("vesper.item_count", 1_i64).unwrap_err(),
        AttributeError::ReservedKey
    );
    attrs.insert("k", 1_i64).unwrap();
    assert_eq!(
        attrs
            .insert_pii("k", Pii::new(EMAIL), &PiiPolicy::Redact)
            .unwrap_err(),
        AttributeError::DuplicateKey
    );
    let long: &'static str = Box::leak("x".repeat(MAX_KEY_BYTES + 1).into_boxed_str());
    assert_eq!(
        attrs.insert(long, 1_i64).unwrap_err(),
        AttributeError::KeyTooLong
    );
    const KEYS: [&str; MAX_ATTRIBUTES_PER_SPAN] = {
        let mut k = [""; MAX_ATTRIBUTES_PER_SPAN];
        let names = [
            "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "a9", "b0", "b1", "b2", "b3",
            "b4", "b5", "b6", "b7", "b8", "b9", "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7",
            "c8", "c9", "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "e0", "e1",
            "e2", "e3", "e4", "e5", "e6", "e7", "e8", "e9", "f0", "f1", "f2", "f3", "f4", "f5",
            "f6", "f7", "f8", "f9", "g0", "g1", "g2", "g3",
        ];
        let mut i = 0;
        while i < MAX_ATTRIBUTES_PER_SPAN {
            k[i] = names[i];
            i += 1;
        }
        k
    };
    for key in &KEYS[..MAX_ATTRIBUTES_PER_SPAN - 1] {
        attrs.insert(key, true).unwrap();
    }
    assert_eq!(attrs.len(), MAX_ATTRIBUTES_PER_SPAN);
    assert_eq!(
        attrs.insert("one_more", true).unwrap_err(),
        AttributeError::TooManyAttributes
    );
}

// ---------------------------------------------------------------- metrics

#[test]
fn histograms_round_trip_with_explicit_bounds() {
    let mut latency = FastLogHistogram::default();
    for v in [0, 3, 15, 16, 1_000, 1_000, 123_456_789] {
        latency.record_with_exemplar(v, nz(1)).unwrap();
    }
    let empty = FastLogHistogram::default();
    let mut encoder = MetricsEncoder::new(EncoderConfig::default());
    let points = [
        HistogramPoint {
            name: "vesper.append.latency",
            unit: "ns",
            start_unix_ns: 10,
            time_unix_ns: 20,
            histogram: &latency,
        },
        HistogramPoint {
            name: "vesper.idle",
            unit: "",
            start_unix_ns: 10,
            time_unix_ns: 20,
            histogram: &empty,
        },
    ];
    let mut decoder = OtapDecoder::default();
    for round in 0..2 {
        let batch = roundtrip(&encoder.encode_histograms(&points).unwrap());
        assert_eq!(batch.batch_id, round);
        let types: Vec<i32> = batch.arrow_payloads.iter().map(|p| p.r#type).collect();
        assert_eq!(
            types,
            [
                ArrowPayloadType::UnivariateMetrics as i32,
                ArrowPayloadType::HistogramDataPoints as i32
            ]
        );
        let decoded = decoder.decode(&batch).unwrap();
        let h = &decoded.histograms[0];
        assert_eq!(h.name, "vesper.append.latency");
        assert_eq!(h.unit.as_deref(), Some("ns"));
        assert_eq!(
            (h.start_unix_ns, h.time_unix_ns, h.count),
            (Some(10), 20, Some(7))
        );
        let last = FastLogHistogram::bucket_index(123_456_789);
        assert_eq!(h.bucket_counts.len(), last + 1);
        for (i, c) in h.bucket_counts.iter().enumerate() {
            assert_eq!(Some(*c), latency.count(i));
        }
        for (i, b) in h.explicit_bounds.iter().enumerate() {
            assert_eq!(*b, FastLogHistogram::bin_range(i).unwrap().upper as f64);
        }
        let idle = &decoded.histograms[1];
        assert_eq!((idle.unit.clone(), idle.count), (None, Some(0)));
        assert_eq!(
            (idle.bucket_counts.clone(), idle.explicit_bounds.len()),
            (vec![0], 0)
        );
    }
}

// ---------------------------------------------------------------- transport

#[test]
fn framed_tcp_export_commits_the_ring_only_after_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut receiver = OtapReceiver::default();
        let mut seen = Vec::new();
        let summary = receiver.serve(stream, |b| seen.push(b)).unwrap();
        (summary, seen)
    });
    let stream = TcpStream::connect(addr).unwrap();
    let mut exporter = TraceExporter::new(
        TraceEncoder::new(EncoderConfig::default()),
        FramedTransport::new(stream),
    );
    let pool = SpanPool::<2>::new(9, nz(1), 1).unwrap();
    let mut ring = TelemetryRing::<4>::default();
    for i in 0..3 {
        pool.start_span(None, SpanSchema::Request, i, i)
            .unwrap()
            .finish_ok(&mut ring, i + 5, PrimaryAttribute::ItemCount(i))
            .unwrap();
    }
    let expected: Vec<SpanRecord> = ring.records().collect();
    assert_eq!(exporter.export_ring(&mut ring, 2).unwrap(), 2);
    assert_eq!(ring.len(), 1);
    assert_eq!(exporter.export_ring(&mut ring, 8).unwrap(), 1);
    assert!(ring.is_empty());
    // A batch the receiver cannot decode: forged delta-only payload of a
    // stream it never saw. The exporter reports the rejection and resets.
    let mut forged = TraceEncoder::new(EncoderConfig::default());
    forged.encode_spans(&[expected[0].into()]).unwrap();
    let mut bad = forged.encode_spans(&[expected[0].into()]).unwrap();
    bad.arrow_payloads[0].schema_id = "spans:999".into();
    bad.batch_id = 2;
    let status = exporter.transport_mut().exchange(&bad).unwrap();
    assert!(!status.is_ok());
    assert_eq!(status.batch_id, 2);
    drop(exporter);
    let (summary, seen) = server.join().unwrap();
    assert_eq!(
        summary,
        ServeSummary {
            accepted: 2,
            rejected: 1
        }
    );
    let spans: Vec<DecodedSpan> = seen.into_iter().flat_map(|b| b.spans).collect();
    assert_eq!(spans.len(), 3);
    for (d, r) in spans.iter().zip(&expected) {
        check_span(d, r);
    }
}

struct FailingTransport {
    fail: bool,
    inner: LoopbackTransport<fn(DecodedBatch)>,
}
impl BatchTransport for FailingTransport {
    fn exchange(&mut self, batch: &BatchArrowRecords) -> Result<BatchStatus, TransportError> {
        if self.fail {
            return Err(TransportError::UnexpectedEof);
        }
        self.inner.exchange(batch)
    }
}

#[test]
fn transport_failure_keeps_records_and_restarts_streams() {
    let mut exporter = TraceExporter::new(
        TraceEncoder::new(EncoderConfig::default()),
        FailingTransport {
            fail: false,
            inner: LoopbackTransport {
                decoder: OtapDecoder::default(),
                sink: |_| {},
            },
        },
    );
    let pool = SpanPool::<1>::new(5, nz(1), 0).unwrap();
    let mut ring = TelemetryRing::<2>::default();
    let push = |ring: &mut TelemetryRing<2>, schema| {
        pool.start_span(None, schema, 1, 1)
            .unwrap()
            .finish_ok(ring, 2, PrimaryAttribute::None)
            .unwrap();
    };
    push(&mut ring, SpanSchema::Request);
    exporter.export_ring(&mut ring, 1).unwrap();
    push(&mut ring, SpanSchema::Background);
    let before: Vec<SpanRecord> = ring.records().collect();
    exporter.transport_mut().fail = true;
    assert!(matches!(
        exporter.export_ring(&mut ring, 1),
        Err(ExportFailure::Transport(TransportError::UnexpectedEof))
    ));
    assert_eq!(
        ring.records().collect::<Vec<_>>(),
        before,
        "record kept for retry"
    );
    // The lost batch carried a delta the receiver never applied; the retry
    // must start a fresh stream that the receiver can decode on its own.
    exporter.transport_mut().fail = false;
    assert_eq!(exporter.export_ring(&mut ring, 1).unwrap(), 1);
    assert!(ring.is_empty());
    assert_eq!(exporter.encoder().resets(), 1);
}

#[test]
fn framing_rejects_oversized_compressed_and_truncated_frames() {
    let mut big = vec![0u8];
    big.extend_from_slice(&(DEFAULT_MAX_FRAME as u32 + 1).to_be_bytes());
    assert!(matches!(
        read_frame(&mut big.as_slice(), DEFAULT_MAX_FRAME),
        Err(TransportError::FrameTooLarge { .. })
    ));
    assert!(matches!(
        read_frame(&mut [1u8, 0, 0, 0, 0].as_slice(), 16),
        Err(TransportError::CompressedFrame)
    ));
    assert!(matches!(
        read_frame(&mut [7u8, 0, 0, 0, 0].as_slice(), 16),
        Err(TransportError::BadCompressionFlag(7))
    ));
    assert!(matches!(
        read_frame(&mut [0u8, 0, 0].as_slice(), 16),
        Err(TransportError::UnexpectedEof)
    ));
    assert!(matches!(
        read_frame(&mut [0u8, 0, 0, 0, 4, 1, 2].as_slice(), 16),
        Err(TransportError::UnexpectedEof)
    ));
    assert!(read_frame(&mut [].as_slice(), 16).unwrap().is_none());
    let mut out = Vec::new();
    write_frame(&mut out, &BatchStatus::ok(7)).unwrap();
    let body = read_frame(&mut out.as_slice(), 16).unwrap().unwrap();
    assert_eq!(
        BatchStatus::decode(body.as_slice()).unwrap(),
        BatchStatus::ok(7)
    );
}

// ---------------------------------------------------------------- untrusted input

fn sample_batches() -> Vec<BatchArrowRecords> {
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let records = records();
    let mut spans: Vec<OtapSpan> = records.iter().copied().map(OtapSpan::from).collect();
    spans[1].attributes.insert("k", "v").unwrap();
    vec![
        encoder.encode_spans(&spans[..2]).unwrap(),
        encoder.encode_spans(&spans).unwrap(),
    ]
}

#[test]
fn malformed_batches_are_typed_errors() {
    let good = sample_batches();
    let mut d = OtapDecoder::default();
    let mut b = good[0].clone();
    b.headers = vec![1];
    assert!(matches!(d.decode(&b), Err(DecodeError::HeadersUnsupported)));
    let mut b = good[0].clone();
    b.arrow_payloads[0].r#type = 99;
    assert!(matches!(
        d.decode(&b),
        Err(DecodeError::UnknownPayloadType(99))
    ));
    let mut b = good[0].clone();
    b.arrow_payloads[0].r#type = ArrowPayloadType::Logs as i32;
    assert!(matches!(
        d.decode(&b),
        Err(DecodeError::UnsupportedPayloadType(_))
    ));
    let mut b = good[0].clone();
    b.arrow_payloads.push(b.arrow_payloads[0].clone());
    assert!(matches!(
        d.decode(&b),
        Err(DecodeError::DuplicatePayloadType(_))
    ));
    let mut b = good[0].clone();
    b.arrow_payloads[0].schema_id.clear();
    assert!(matches!(d.decode(&b), Err(DecodeError::BadSchemaId)));
    let mut b = good[0].clone();
    b.arrow_payloads.remove(0);
    assert!(matches!(d.decode(&b), Err(DecodeError::MissingMainRecord)));
    let mut b = good[0].clone();
    let len = b.arrow_payloads[0].record.len();
    b.arrow_payloads[0].record.truncate(len - 3);
    assert!(d.decode(&b).is_err());
    let mut tight = OtapDecoder::new(DecodeLimits {
        max_payload_bytes: 64,
        ..DecodeLimits::default()
    });
    assert!(matches!(
        tight.decode(&good[0]),
        Err(DecodeError::PayloadTooLarge)
    ));
    let mut few = OtapDecoder::new(DecodeLimits {
        max_rows: 1,
        ..DecodeLimits::default()
    });
    assert!(matches!(few.decode(&good[0]), Err(DecodeError::RowLimit)));
    // A schema id reused for another payload type.
    let mut d = OtapDecoder::default();
    d.decode(&good[0]).unwrap();
    let mut b = good[1].clone();
    b.arrow_payloads[1].schema_id = b.arrow_payloads[0].schema_id.clone();
    assert!(matches!(d.decode(&b), Err(DecodeError::PayloadTypeChanged)));
    assert_eq!(d.stream_count(), 0, "errors drop all stream state");
}

/// Raise with `BEACON_FUZZ_CASES` for a longer local run.
fn fuzz_cases() -> usize {
    std::env::var("BEACON_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000)
}

#[test]
fn corrupted_payload_bytes_never_panic() {
    #[allow(non_snake_case)]
    let FUZZ_CASES = fuzz_cases();
    let good = sample_batches();
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut rejected = 0;
    for _ in 0..FUZZ_CASES {
        let mut batches = good.clone();
        let which = (next() % 2) as usize;
        let payload = (next() % batches[which].arrow_payloads.len() as u64) as usize;
        let record = &mut batches[which].arrow_payloads[payload].record;
        match next() % 4 {
            0 => {
                let i = (next() % record.len() as u64) as usize;
                record[i] ^= 1 << (next() % 8);
            }
            3 => {
                for _ in 0..1 + next() % 8 {
                    let i = (next() % record.len() as u64) as usize;
                    record[i] = next() as u8;
                }
            }
            1 => {
                let i = (next() % record.len() as u64) as usize;
                record[i] = next() as u8;
            }
            _ => {
                let keep = (next() % record.len() as u64) as usize;
                record.truncate(keep);
            }
        }
        let mut decoder = OtapDecoder::default();
        for batch in &batches {
            match decoder.decode(batch) {
                Ok(_) => {}
                Err(DecodeError::DecoderPanic) => panic!("Arrow panic reached the guard"),
                Err(_) => {
                    rejected += 1;
                    break;
                }
            }
        }
    }
    assert!(
        rejected > FUZZ_CASES / 3,
        "most corruptions are detected: {rejected}"
    );
}

#[test]
fn protobuf_garbage_is_a_typed_error() {
    for bytes in [
        &[0xff_u8, 0xff, 0xff][..],
        &[0x0a, 0x05, 0x01],
        &[0x12, 0x80],
    ] {
        assert!(BatchArrowRecords::decode(bytes).is_err());
    }
}

// ---------------------------------------------------------------- independent readers

/// The concatenated `SPANS` payloads of one stream are a standard Arrow IPC
/// stream. arrow2 0.18, an independent implementation, reads a stream's first
/// payload (schema, full dictionaries, batch). It does not implement delta
/// dictionaries and says so with a typed error on the second payload.
#[test]
fn arrow2_reads_the_first_payload_and_rejects_deltas_it_lacks() {
    use arrow2::io::ipc::read::{read_stream_metadata, StreamReader, StreamState};
    let records = records();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    let first = encoder
        .encode_spans(&[records[2].into(), records[3].into()])
        .unwrap();
    let second = encoder
        .encode_spans(&[records[0].into(), records[1].into()])
        .unwrap();
    let eos = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0];
    let read = |bytes: Vec<u8>| {
        let mut input = std::io::Cursor::new(bytes);
        let metadata = read_stream_metadata(&mut input).unwrap();
        let fields: Vec<String> = metadata
            .schema
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let mut names = Vec::new();
        for state in StreamReader::new(input, metadata, None) {
            let StreamState::Some(chunk) = state? else {
                panic!("waiting on a complete stream")
            };
            let dict = chunk.arrays()[7]
                .as_any()
                .downcast_ref::<arrow2::array::DictionaryArray<u8>>()
                .unwrap();
            let values = dict
                .values()
                .as_any()
                .downcast_ref::<arrow2::array::Utf8Array<i32>>()
                .unwrap();
            for key in dict.keys().values_iter() {
                names.push(values.value(*key as usize).to_owned());
            }
        }
        Ok::<_, arrow2::error::Error>((fields, names))
    };
    let mut stream = first.arrow_payloads[0].record.clone();
    stream.extend(eos);
    let (fields, names) = read(stream.clone()).unwrap();
    assert_eq!(
        fields,
        [
            "id",
            "start_time_unix_nano",
            "duration_time_unix_nano",
            "trace_id",
            "span_id",
            "parent_span_id",
            "flags",
            "name",
            "status"
        ]
    );
    assert_eq!(names, ["vesper.request", "vesper.request"]);
    stream.truncate(stream.len() - 8);
    stream.extend(&second.arrow_payloads[0].record);
    stream.extend(eos);
    assert!(matches!(
        read(stream),
        Err(arrow2::error::Error::NotYetImplemented(_))
    ));
}

// ---------------------------------------------------------------- reference schema

/// Upstream column tables transcribed from open-telemetry/otel-arrow `main`
/// on 2026-11-13: `go/pkg/otel/traces/arrow/traces.go` (`TracesSchema`),
/// `status.go` (`StatusDT`), `go/pkg/otel/common/arrow/attributes_16.go`
/// (`AttrsSchema16`), `go/pkg/otel/metrics/arrow/metrics.go`
/// (`MetricsSchema`) and `histogram_dp.go` (`HistogramDataPointSchema`).
/// `(name, type, required)`; `string` admits plain or dictionary-encoded
/// utf8, since upstream picks the encoding adaptively.
type Columns = &'static [(&'static str, &'static str, bool)];
const REFERENCE: [(&str, Columns); 4] = [
    (
        "SPANS",
        &[
            ("id", "u16", false),
            ("resource", "struct", false),
            ("scope", "struct", false),
            ("schema_url", "string", false),
            ("start_time_unix_nano", "timestamp_ns", true),
            ("duration_time_unix_nano", "duration_ns", true),
            ("trace_id", "fsb16", true),
            ("span_id", "fsb8", true),
            ("trace_state", "string", false),
            ("parent_span_id", "fsb8", false),
            ("flags", "u32", false),
            ("name", "string", true),
            ("kind", "i32", false),
            ("dropped_attributes_count", "u32", false),
            ("dropped_events_count", "u32", false),
            ("dropped_links_count", "u32", false),
            ("status", "struct{code:i32,status_message:string}", false),
        ],
    ),
    (
        "SPAN_ATTRS",
        &[
            ("parent_id", "u16", true),
            ("key", "string", true),
            ("type", "u8", true),
            ("str", "string", false),
            ("int", "i64", false),
            ("double", "f64", false),
            ("bool", "bool", false),
            ("bytes", "binary", false),
            ("ser", "binary", false),
        ],
    ),
    (
        "UNIVARIATE_METRICS",
        &[
            ("id", "u16", true),
            ("resource", "struct", false),
            ("scope", "struct", false),
            ("schema_url", "string", false),
            ("metric_type", "u8", true),
            ("name", "string", true),
            ("description", "string", false),
            ("unit", "string", false),
            ("aggregation_temporality", "i32", false),
            ("is_monotonic", "bool", false),
        ],
    ),
    (
        "HISTOGRAM_DATA_POINTS",
        &[
            ("id", "u32", false),
            ("parent_id", "u16", true),
            ("start_time_unix_nano", "timestamp_ns", false),
            ("time_unix_nano", "timestamp_ns", true),
            ("count", "u64", false),
            ("sum", "f64", false),
            ("bucket_counts", "list<u64>", false),
            ("explicit_bounds", "list<f64>", false),
            ("flags", "u32", false),
            ("min", "f64", false),
            ("max", "f64", false),
        ],
    ),
];

fn token(t: &arrow_schema::DataType) -> String {
    use arrow_schema::{DataType as D, TimeUnit};
    match t {
        D::UInt8 => "u8".into(),
        D::UInt16 => "u16".into(),
        D::UInt32 => "u32".into(),
        D::UInt64 => "u64".into(),
        D::Int32 => "i32".into(),
        D::Int64 => "i64".into(),
        D::Float64 => "f64".into(),
        D::Boolean => "bool".into(),
        D::Utf8 => "string".into(),
        D::Dictionary(_, v) if **v == D::Utf8 => "string".into(),
        D::Binary => "binary".into(),
        D::FixedSizeBinary(n) => format!("fsb{n}"),
        D::Timestamp(TimeUnit::Nanosecond, None) => "timestamp_ns".into(),
        D::Duration(TimeUnit::Nanosecond) => "duration_ns".into(),
        D::List(f) => format!("list<{}>", token(f.data_type())),
        D::Struct(fields) => format!(
            "struct{{{}}}",
            fields
                .iter()
                .map(|f| format!("{}:{}", f.name(), token(f.data_type())))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => format!("{other:?}"),
    }
}

#[test]
fn emitted_schemas_are_subsets_of_the_upstream_otap_schemas() {
    let ours = [
        schema::spans_schema(),
        schema::span_attrs_schema(),
        schema::metrics_schema(),
        schema::histogram_points_schema(),
    ];
    for ((payload, reference), ours) in REFERENCE.iter().zip(ours) {
        for field in ours.fields() {
            let (_, ty, _) = reference
                .iter()
                .find(|(name, _, _)| name == field.name())
                .unwrap_or_else(|| panic!("{payload}.{} is not an upstream column", field.name()));
            assert_eq!(&token(field.data_type()), ty, "{payload}.{}", field.name());
        }
        // Every column upstream requires is present and non-nullable.
        for (name, _, required) in reference.iter() {
            if *required {
                let field = ours
                    .field_with_name(name)
                    .unwrap_or_else(|_| panic!("{payload}.{name} required upstream"));
                assert!(
                    !field.is_nullable() || *name == "id",
                    "{payload}.{name} must not be nullable"
                );
            }
        }
        // Reference order is preserved for the columns we emit.
        let positions: Vec<usize> = ours
            .fields()
            .iter()
            .map(|f| {
                reference
                    .iter()
                    .position(|(n, _, _)| n == f.name())
                    .unwrap()
            })
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "{payload} column order"
        );
    }
}
