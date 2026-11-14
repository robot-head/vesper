//! Self-checking OTAP export: spans with a redacted PII attribute and a
//! latency histogram, over the framed transport on a loopback TCP socket.
//!
//! `cargo run -p vesper-beacon-arrow --example otap_export -- [DIR]` also
//! writes each payload stream (all its payloads concatenated, plus EOS) to
//! `DIR/<schema_id>.arrows` so another Arrow implementation can read them.

use core::num::NonZeroU64;
use std::collections::BTreeMap;
use std::net::{TcpListener, TcpStream};
use vesper_beacon::*;
use vesper_beacon_arrow::otap::proto::BatchArrowRecords;
use vesper_beacon_arrow::otap::*;

fn main() {
    let dump = std::env::args().nth(1);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut spans = Vec::new();
        let summary = OtapReceiver::default()
            .serve(stream, |batch| spans.extend(batch.spans))
            .expect("serve");
        (summary, spans)
    });

    let pool = SpanPool::<4>::new(0xabc, NonZeroU64::new(1).unwrap(), 1).expect("pool");
    let mut ring = TelemetryRing::<16>::default();
    let mut latency = FastLogHistogram::default();
    let mut batches: Vec<BatchArrowRecords> = Vec::new();
    let mut encoder = TraceEncoder::new(EncoderConfig::default());
    for (i, email) in ["alice@example.com", "bob@example.org", "carol@example.net"]
        .into_iter()
        .enumerate()
    {
        let i = i as u64;
        let span = pool
            .start_span(None, SpanSchema::Request, 1_700_000_000_000 + i, 10 * i)
            .expect("start");
        let child = pool
            .start_span(
                Some(span.context()),
                SpanSchema::StorageAppend,
                0,
                10 * i + 1,
            )
            .expect("start");
        child
            .finish_err(
                &mut ring,
                10 * i + 4,
                ErrorCode::Timeout,
                PrimaryAttribute::None,
            )
            .expect("finish");
        span.finish_ok(&mut ring, 10 * i + 9, PrimaryAttribute::ItemCount(i))
            .expect("finish");
        latency
            .record_with_exemplar(10 * i + 9, NonZeroU64::new(1).unwrap())
            .expect("record");
        let mut spans: Vec<OtapSpan> = ring.records().map(OtapSpan::from).collect();
        spans[1]
            .attributes
            .insert("route", "/login")
            .expect("attribute")
            .insert_pii("user.email", Pii::new(email), &PiiPolicy::Redact)
            .expect("attribute");
        batches.push(encoder.encode_spans(&spans).expect("encode"));
        ring.try_consume_prefix(16, |_| Ok::<_, ()>(()))
            .expect("drain");
    }
    let mut transport = FramedTransport::new(TcpStream::connect(addr).expect("connect"));
    for batch in &batches {
        let status = transport.exchange(batch).expect("exchange");
        assert!(status.is_ok(), "{status:?}");
    }
    drop(transport);
    let (summary, spans) = server.join().expect("server");
    assert_eq!(summary.accepted, 3);
    assert_eq!(spans.len(), 6);
    assert!(spans.iter().all(|s| s
        .attributes
        .iter()
        .all(|(_, v)| !matches!(v, DecodedValue::Str(s) if s.contains('@')))));
    println!(
        "exported {} spans in {} batches; PII redacted",
        spans.len(),
        batches.len()
    );

    let metrics =
        MetricsEncoder::new(EncoderConfig::default()).encode_histograms(&[HistogramPoint {
            name: "vesper.request.latency",
            unit: "ns",
            start_unix_ns: 1_700_000_000_000,
            time_unix_ns: 1_700_000_000_100,
            histogram: &latency,
            attributes: Attributes::new(),
        }]);
    let metrics = metrics.expect("encode histogram");
    let decoded = OtapDecoder::default().decode(&metrics).expect("decode");
    assert_eq!(decoded.histograms[0].count, Some(3));
    batches.push(metrics);

    if let Some(dir) = dump {
        let mut streams: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for batch in &batches {
            for p in &batch.arrow_payloads {
                streams
                    .entry(p.schema_id.clone())
                    .or_default()
                    .extend(&p.record);
            }
        }
        for (id, mut bytes) in streams {
            bytes.extend([0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]);
            let path = std::path::Path::new(&dir).join(format!("{}.arrows", id.replace(':', "_")));
            std::fs::write(&path, bytes).expect("write stream");
            println!("wrote {}", path.display());
        }
    }
}
