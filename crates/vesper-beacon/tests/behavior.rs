use core::num::NonZeroU64;
use vesper_beacon::*;

const HEADER: &[u8; 55] = b"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}
fn pool<const N: usize>() -> SpanPool<N> {
    SpanPool::new(99, nz(1), 1).unwrap()
}

#[test]
fn actual_header_bytes_and_all_opaque_flags_roundtrip() {
    let p = parse_traceparent_55(HEADER).unwrap();
    assert_eq!(p.trace_id(), 0x4bf92f3577b34da6a3ce929d0e0e4736);
    assert_eq!(p.span_id(), 0x00f067aa0ba902b7);
    assert!(p.sampled());
    assert_eq!(&p.to_traceparent(), HEADER);
    for flags in 0..=255 {
        let ctx = W3cSpanContext::new(u128::MAX, u64::MAX, flags).unwrap();
        assert_eq!(parse_traceparent_55(&ctx.to_traceparent()).unwrap(), ctx);
        assert_eq!(ctx.sampled(), flags & 1 == 1);
    }
    let unknown = W3cSpanContext::new(1, 1, 0xfe).unwrap();
    assert!(!unknown.sampled());
    assert_eq!(
        parse_traceparent_55(&unknown.to_traceparent())
            .unwrap()
            .trace_flags(),
        0xfe
    );
}

#[test]
fn lengths_separators_lowercase_version_and_nonzero_are_strict() {
    for len in 0..55 {
        assert_eq!(
            parse_traceparent_55(&HEADER[..len]),
            Err(TraceError::InvalidLength { actual: len })
        );
    }
    assert_eq!(
        parse_traceparent_55(&[0; 56]),
        Err(TraceError::InvalidLength { actual: 56 })
    );
    for offset in [2, 35, 52] {
        let mut h = *HEADER;
        h[offset] = b' ';
        assert_eq!(
            parse_traceparent_55(&h),
            Err(TraceError::MissingHyphen { offset })
        );
    }
    for offset in (0..55).filter(|p| ![2, 35, 52].contains(p)) {
        for bad in [b'A', b'G', b'/', 0, 0xff, b' '] {
            let mut h = *HEADER;
            h[offset] = bad;
            assert_eq!(
                parse_traceparent_55(&h),
                Err(TraceError::InvalidHex { offset })
            );
        }
    }
    for version in [b"01", b"fe", b"ff"] {
        let mut h = *HEADER;
        h[..2].copy_from_slice(version);
        assert!(matches!(
            parse_traceparent_55(&h),
            Err(TraceError::UnsupportedVersion { .. })
        ));
    }
    let mut h = *HEADER;
    h[3..35].fill(b'0');
    assert_eq!(parse_traceparent_55(&h), Err(TraceError::ZeroTraceId));
    h = *HEADER;
    h[36..52].fill(b'0');
    assert_eq!(parse_traceparent_55(&h), Err(TraceError::ZeroSpanId));
    assert_eq!(W3cSpanContext::new(0, 1, 0), Err(TraceError::ZeroTraceId));
    assert_eq!(W3cSpanContext::new(1, 0, 0), Err(TraceError::ZeroSpanId));
    assert!(SpanPool::<1>::new(0, nz(1), 0).is_err());
}

#[test]
fn explicit_parent_and_error_attribution_preserve_columns() {
    let pool = pool::<2>();
    let incoming = parse_traceparent_55(HEADER).unwrap();
    let root = pool
        .start_span(Some(incoming), SpanSchema::Request, 1000, 100)
        .unwrap();
    let root_context = root.context();
    let child = pool
        .start_span(Some(root_context), SpanSchema::StorageAppend, 1001, 101)
        .unwrap();
    assert_eq!(child.context().trace_id(), incoming.trace_id());
    assert_eq!(child.context().trace_flags(), incoming.trace_flags());
    assert_ne!(child.context().span_id(), root_context.span_id());
    let batch = {
        let mut queue = TelemetryRing::<2>::default();
        let secret = Secret::new("sensitive");
        child
            .finish_err(
                &mut queue,
                120,
                ErrorCode::Storage,
                PrimaryAttribute::Redacted(secret.redact()),
            )
            .unwrap();
        root.finish_ok(&mut queue, 130, PrimaryAttribute::ByteCount(100))
            .unwrap();
        queue.drain::<2>()
    };
    assert_eq!(pool.unclosed_count(), 0);
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.trace_ids(), &[incoming.trace_id(); 2]);
    assert_eq!(
        batch.parent_span_ids(),
        &[root_context.span_id(), incoming.span_id()]
    );
    assert_eq!(batch.flags(), &[1; 2]);
    assert_eq!(batch.start_unix_ns(), &[1001, 1000]);
    assert_eq!(batch.duration_ns(), &[19, 30]);
    assert_eq!(
        batch.schemas(),
        &[SpanSchema::StorageAppend, SpanSchema::Request]
    );
    assert_eq!(
        batch.statuses(),
        &[SpanStatus::Error(ErrorCode::Storage), SpanStatus::Ok]
    );
    assert!(matches!(
        batch.attributes()[0],
        PrimaryAttribute::Redacted(_)
    ));
    assert_eq!(batch.attributes()[1], PrimaryAttribute::ByteCount(100));
}

#[test]
fn backpressure_returns_same_owner_retry_is_exactly_once_and_cancel_is_explicit() {
    let pool = pool::<2>();
    let mut queue = TelemetryRing::<1>::default();
    let a = pool.start_span(None, SpanSchema::Request, 0, 10).unwrap();
    let id_a = a.context().span_id();
    a.finish_ok(&mut queue, 20, PrimaryAttribute::None).unwrap();
    let b = pool.start_span(None, SpanSchema::Request, 0, 30).unwrap();
    let id_b = b.context().span_id();
    let failed = b
        .finish_err(&mut queue, 40, ErrorCode::Timeout, PrimaryAttribute::None)
        .unwrap_err();
    assert_eq!(failed.error, FinishError::QueueFull);
    assert_eq!(failed.span.context().span_id(), id_b);
    assert_eq!(pool.unclosed_count(), 1);
    assert_eq!(queue.drain::<1>().span_ids(), &[id_a]);
    failed
        .span
        .finish_err(&mut queue, 40, ErrorCode::Timeout, PrimaryAttribute::None)
        .unwrap();
    let owned = queue.drain::<8>();
    assert_eq!(owned.span_ids(), &[id_b]);
    assert_eq!(owned.statuses(), &[SpanStatus::Error(ErrorCode::Timeout)]);
    assert!(queue.drain::<8>().is_empty());
    let c = pool
        .start_span(None, SpanSchema::Background, 0, 10)
        .unwrap();
    let mut full = TelemetryRing::<0>::default();
    let failed = c
        .finish_ok(&mut full, 9, PrimaryAttribute::None)
        .unwrap_err();
    assert_eq!(failed.error, FinishError::EndBeforeStart);
    let failed = failed
        .span
        .finish_ok(&mut full, 10, PrimaryAttribute::None)
        .unwrap_err();
    assert_eq!(failed.error, FinishError::QueueFull);
    let id_c = failed.span.context().span_id();
    let failed = failed.span.cancel(&mut full, 9).unwrap_err();
    assert_eq!(failed.error, FinishError::EndBeforeStart);
    let failed = failed.span.cancel(&mut full, 10).unwrap_err();
    assert_eq!(failed.error, FinishError::QueueFull);
    assert_eq!(failed.span.context().span_id(), id_c);
    assert_eq!(pool.unclosed_count(), 1);
    failed.span.cancel(&mut queue, 10).unwrap();
    let cancellation = queue.drain::<2>();
    assert_eq!(cancellation.span_ids(), &[id_c]);
    assert_eq!(
        cancellation.statuses(),
        &[SpanStatus::Error(ErrorCode::Cancelled)]
    );
    assert!(queue.drain::<2>().is_empty());
    assert!(id_c > id_b && id_b > id_a);
    assert_eq!(pool.unclosed_count(), 0);
    assert!(full.drain::<1>().is_empty());
}

#[test]
fn abandoned_spans_stay_observable_and_capacity_is_bounded() {
    let pool = pool::<1>();
    {
        let abandoned = pool.start_span(None, SpanSchema::Request, 0, 0).unwrap();
        assert_eq!(abandoned.context().span_id(), 1);
    }
    assert_eq!(pool.unclosed_count(), 1);
    assert_eq!(
        pool.start_span(None, SpanSchema::Request, 0, 0)
            .unwrap_err(),
        StartError::PoolFull
    );
    assert_eq!(
        SpanPool::<0>::new(1, nz(1), 0)
            .unwrap()
            .start_span(None, SpanSchema::Request, 0, 0)
            .unwrap_err(),
        StartError::PoolFull
    );
}

#[test]
fn ids_never_wrap_or_repeat_and_imported_self_parent_is_skipped() {
    let pool = SpanPool::<1>::new(1, nz(u64::MAX), 0).unwrap();
    let final_span = pool.start_span(None, SpanSchema::Request, 0, 0).unwrap();
    let mut queue = TelemetryRing::<1>::default();
    final_span.cancel(&mut queue, 0).unwrap();
    assert_eq!(queue.drain::<1>().span_ids(), &[u64::MAX]);
    assert_eq!(
        pool.start_span(None, SpanSchema::Request, 0, 0)
            .unwrap_err(),
        StartError::IdExhausted
    );
    let pool = SpanPool::<1>::new(1, nz(5), 0).unwrap();
    let parent = W3cSpanContext::new(1, 5, 0xfe).unwrap();
    let child = pool
        .start_span(Some(parent), SpanSchema::Request, 0, 0)
        .unwrap();
    assert_eq!(child.context().span_id(), 6);
    assert_eq!(child.context().trace_flags(), 0xfe);
    child.cancel(&mut queue, 0).unwrap();
    assert_eq!(queue.drain::<1>().span_ids(), &[6]);
    pool.start_span(None, SpanSchema::Request, 0, 0)
        .unwrap()
        .cancel(&mut queue, 0)
        .unwrap();
    assert_eq!(queue.drain::<1>().span_ids(), &[7]);
    let pool = SpanPool::<1>::new(1, nz(u64::MAX), 0).unwrap();
    let parent = W3cSpanContext::new(1, u64::MAX, 0).unwrap();
    assert_eq!(
        pool.start_span(Some(parent), SpanSchema::Request, 0, 0)
            .unwrap_err(),
        StartError::IdExhausted
    );
    assert_eq!(pool.unclosed_count(), 0);
}

#[test]
fn drain_wraparound_partial_zero_and_fifo_ownership() {
    let pool = pool::<1>();
    let mut ring = TelemetryRing::<3>::default();
    for _ in 0..3 {
        pool.start_span(None, SpanSchema::Request, 0, 0)
            .unwrap()
            .finish_ok(&mut ring, 1, PrimaryAttribute::ItemCount(1))
            .unwrap();
    }
    assert!(ring.drain::<0>().is_empty());
    assert_eq!(ring.len(), 3);
    assert_eq!(ring.drain::<1>().span_ids(), &[1]);
    pool.start_span(None, SpanSchema::Request, 0, 0)
        .unwrap()
        .finish_ok(&mut ring, 1, PrimaryAttribute::None)
        .unwrap();
    let batch = ring.drain::<5>();
    assert_eq!(batch.span_ids(), &[2, 3, 4]);
    assert!(ring.is_empty());
    assert_eq!(pool.unclosed_count(), 0);
}

#[test]
fn redaction_marker_does_not_depend_on_payload() {
    let a = Secret::new("password A");
    let b = Secret::new([0xff_u8; 100]);
    assert_eq!(a.redact(), b.redact());
    assert_eq!(a.redact().as_bytes(), b"[REDACTED]");
    assert_eq!(a.declassify(), "password A");
}

#[test]
fn cancellation_cannot_bypass_full_queue_or_duplicate_records() {
    let pool = pool::<1>();
    let mut ring = TelemetryRing::<1>::default();
    pool.start_span(None, SpanSchema::Request, 0, 0)
        .unwrap()
        .finish_ok(&mut ring, 1, PrimaryAttribute::None)
        .unwrap();
    let cancelled = pool.start_span(None, SpanSchema::Request, 0, 2).unwrap();
    let context = cancelled.context();
    let failed = cancelled.cancel(&mut ring, 3).unwrap_err();
    assert_eq!(failed.error, FinishError::QueueFull);
    assert_eq!(failed.span.context(), context);
    assert_eq!(pool.unclosed_count(), 1);
    let existing = ring.drain::<2>();
    assert_eq!(existing.span_ids(), &[1]);
    assert_eq!(existing.statuses(), &[SpanStatus::Ok]);
    failed.span.cancel(&mut ring, 3).unwrap();
    assert_eq!(pool.unclosed_count(), 0);
    let owned = ring.drain::<2>();
    assert_eq!(owned.span_ids(), &[context.span_id()]);
    assert_eq!(owned.statuses(), &[SpanStatus::Error(ErrorCode::Cancelled)]);
    assert_eq!(owned.duration_ns(), &[1]);
    assert!(ring.drain::<2>().is_empty());
}
