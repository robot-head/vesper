use core::num::NonZeroU64;
use vesper_beacon::*;

fn main() {
    let incoming = b"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let parent = parse_traceparent_55(incoming).unwrap();
    assert_eq!(&parent.to_traceparent(), incoming);
    let pool = SpanPool::<2>::new(1, NonZeroU64::new(1).unwrap(), 0).unwrap();
    let mut queue = TelemetryRing::<1>::default();
    let span = pool
        .start_span(Some(parent), SpanSchema::StorageAppend, 1_000, 100)
        .unwrap();
    let child = pool
        .start_span(Some(span.context()), SpanSchema::Background, 1_010, 110)
        .unwrap();
    let secret = Secret::new("never exported credential");
    child
        .finish_err(
            &mut queue,
            120,
            ErrorCode::Storage,
            PrimaryAttribute::Redacted(secret.redact()),
        )
        .unwrap();
    let retry = span
        .finish_ok(&mut queue, 130, PrimaryAttribute::ItemCount(7))
        .unwrap_err();
    assert_eq!(retry.error, FinishError::QueueFull);
    let child_columns = queue.drain::<16>();
    retry
        .span
        .finish_ok(&mut queue, 130, PrimaryAttribute::ItemCount(7))
        .unwrap();
    let root_columns = queue.drain::<16>();
    assert_eq!(child_columns.parent_span_ids(), root_columns.span_ids());
    assert_eq!(
        child_columns.statuses(),
        &[SpanStatus::Error(ErrorCode::Storage)]
    );
    assert_eq!(pool.unclosed_count(), 0);
    let mut histogram = FastLogHistogram::default();
    histogram
        .record_with_exemplar(
            root_columns.duration_ns()[0],
            NonZeroU64::new(root_columns.span_ids()[0]).unwrap(),
        )
        .unwrap();
    println!(
        "completed {} spans; histogram count={}, bytes={}",
        child_columns.len() + root_columns.len(),
        histogram.total_count(),
        FastLogHistogram::FOOTPRINT_BYTES
    );
}
