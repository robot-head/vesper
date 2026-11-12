use arrow2::{
    array::{Array, PrimitiveArray},
    datatypes::DataType,
    io::ipc::read::{read_stream_metadata, StreamReader, StreamState},
};
use core::num::NonZeroU64;
use std::io::Cursor;
use vesper_beacon::*;
use vesper_beacon_arrow::*;

fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}
fn limits() -> ExportLimits {
    ExportLimits {
        max_rows: MAX_ROWS,
        max_attribute_bytes: MAX_ROWS * 10,
        max_intermediate_bytes: 16 * 1024 * 1024,
        max_output_bytes: 8 * 1024 * 1024,
    }
}
fn verify(bytes: &[u8], records: &[SpanRecord]) {
    verify_buffer_descriptors(bytes, records.len());
    let mut input = Cursor::new(bytes);
    let metadata = read_stream_metadata(&mut input).unwrap();
    assert_eq!(metadata.schema.metadata["vesper.schema"], SCHEMA_VERSION);
    assert_eq!(
        metadata.schema.metadata["vesper.schema_codes"],
        "0=request,1=storage_append,2=background"
    );
    assert_eq!(
        metadata.schema.metadata["vesper.status_codes"],
        "0=ok,1=error"
    );
    assert_eq!(
        metadata.schema.metadata["vesper.redacted_marker"],
        "[REDACTED]"
    );
    assert_eq!(
        metadata.schema.metadata["vesper.error_codes"],
        "0=none,1=invalid_input,2=storage,3=timeout,4=cancelled"
    );
    assert_eq!(
        metadata.schema.metadata["vesper.attribute_codes"],
        "0=none,1=item_count,2=byte_count,3=redacted;inactive_value=0"
    );
    let names = [
        "trace_id_hi",
        "trace_id_lo",
        "span_id",
        "parent_span_id",
        "start_unix_ns",
        "duration_ns",
        "trace_flags",
        "schema",
        "status",
        "error_code",
        "attribute_kind",
        "attribute_value",
    ];
    for (i, field) in metadata.schema.fields.iter().enumerate() {
        assert_eq!(field.name, names[i]);
        assert!(!field.is_nullable);
        assert_eq!(
            field.data_type,
            if i < 6 || i == 11 {
                DataType::UInt64
            } else {
                DataType::UInt8
            }
        );
    }
    assert_eq!(metadata.schema.fields.len(), 12);
    let mut reader = StreamReader::new(input, metadata, None);
    let StreamState::Some(chunk) = reader.next().unwrap().unwrap() else {
        panic!("missing record batch")
    };
    assert_eq!(chunk.len(), records.len());
    assert_eq!(chunk.arrays().len(), 12);
    for (row, record) in records.iter().enumerate() {
        let (status, error) = match record.status {
            SpanStatus::Ok => (0, 0),
            SpanStatus::Error(code) => (
                1,
                match code {
                    ErrorCode::InvalidInput => 1,
                    ErrorCode::Storage => 2,
                    ErrorCode::Timeout => 3,
                    ErrorCode::Cancelled => 4,
                },
            ),
        };
        let (attribute, value) = match record.attribute {
            PrimaryAttribute::None => (0, 0),
            PrimaryAttribute::ItemCount(n) => (1, n),
            PrimaryAttribute::ByteCount(n) => (2, n),
            PrimaryAttribute::Redacted(_) => (3, 0),
        };
        let expected_wide = [
            (record.context.trace_id() >> 64) as u64,
            record.context.trace_id() as u64,
            record.context.span_id(),
            record.parent_span_id,
            record.start_unix_ns,
            record.duration_ns,
            value,
        ];
        for (i, expected) in [0, 1, 2, 3, 4, 5, 11].into_iter().zip(expected_wide) {
            let column = chunk.arrays()[i]
                .as_any()
                .downcast_ref::<PrimitiveArray<u64>>()
                .unwrap();
            assert_eq!(column.null_count(), 0);
            assert_eq!(column.value(row), expected);
        }
        let schema = match record.schema {
            SpanSchema::Request => 0,
            SpanSchema::StorageAppend => 1,
            SpanSchema::Background => 2,
        };
        for (i, expected) in (6..11).zip([
            record.context.trace_flags(),
            schema,
            status,
            error,
            attribute,
        ]) {
            let column = chunk.arrays()[i]
                .as_any()
                .downcast_ref::<PrimitiveArray<u8>>()
                .unwrap();
            assert_eq!(column.null_count(), 0);
            assert_eq!(column.value(row), expected);
        }
    }
    assert!(
        reader.next().is_none(),
        "expected explicit EOS, no trailing batches"
    );
}
fn sample_ring() -> TelemetryRing<8> {
    let pool = SpanPool::<1>::new(u128::MAX, nz(1), 0xfe).unwrap();
    let mut ring = TelemetryRing::default();
    let attributes = [
        PrimaryAttribute::None,
        PrimaryAttribute::ItemCount(u64::MAX),
        PrimaryAttribute::ByteCount(0),
        PrimaryAttribute::Redacted(Secret::new("must not leak").redact()),
        PrimaryAttribute::ItemCount(0),
        PrimaryAttribute::ByteCount(u64::MAX),
    ];
    for (i, attribute) in attributes.into_iter().enumerate() {
        let parent = if i == 0 {
            None
        } else {
            Some(W3cSpanContext::new(u128::MAX, u64::MAX, 0xff).unwrap())
        };
        let span = pool
            .start_span(
                parent,
                [
                    SpanSchema::Request,
                    SpanSchema::StorageAppend,
                    SpanSchema::Background,
                ][i % 3],
                u64::MAX - i as u64,
                0,
            )
            .unwrap();
        match i {
            0 => span.finish_ok(&mut ring, u64::MAX, attribute).unwrap(),
            1 => span
                .finish_err(&mut ring, 1, ErrorCode::InvalidInput, attribute)
                .unwrap(),
            2 => span
                .finish_err(&mut ring, 2, ErrorCode::Storage, attribute)
                .unwrap(),
            3 => span
                .finish_err(&mut ring, 3, ErrorCode::Timeout, attribute)
                .unwrap(),
            4 => span.cancel(&mut ring, 4).unwrap(),
            _ => span.finish_ok(&mut ring, 5, attribute).unwrap(),
        }
    }
    ring
}
#[test]
fn independent_arrow2_reader_compares_every_column_and_metadata() {
    let mut ring = sample_ring();
    let records: Vec<_> = ring.records().collect();
    let bytes = export_ring(&mut ring, usize::MAX, limits()).unwrap();
    assert!(ring.is_empty());
    verify(&bytes, &records);
    assert!(!bytes.windows(13).any(|b| b == b"must not leak"));
}
#[test]
fn empty_partial_owned_batch_and_fifo_wraparound() {
    let mut ring = sample_ring();
    let expected: Vec<_> = ring.records().collect();
    let empty = export_ring(&mut ring, 0, limits()).unwrap();
    verify(&empty, &[]);
    assert_eq!(ring.len(), 6);
    let partial = export_ring(&mut ring, 2, limits()).unwrap();
    verify(&partial, &expected[..2]);
    let batch = ring.drain::<2>();
    let bytes = export_batch(&batch, limits()).unwrap();
    verify(&bytes, &expected[2..4]);
    assert_eq!(batch.len(), 2); // Export borrows and preserves batch on success.
    let rest = export_ring(&mut ring, 8, limits()).unwrap();
    verify(&rest, &expected[4..]);
    verify(&export_ring(&mut ring, 8, limits()).unwrap(), &[]);
    let pool = SpanPool::<1>::new(1, nz(u64::MAX), 0).unwrap();
    pool.start_span(None, SpanSchema::Request, 0, 0)
        .unwrap()
        .cancel(&mut ring, 0)
        .unwrap();
    let expected: Vec<_> = ring.records().collect();
    verify(&export_ring(&mut ring, 8, limits()).unwrap(), &expected);
    assert_eq!(expected[0].context.span_id(), u64::MAX);
}
#[test]
fn all_preflight_errors_preserve_records_for_retry_and_exact_reservations_pass() {
    let mut ring = sample_ring();
    let expected: Vec<_> = ring.records().collect();
    let plan = plan_ring(&ring, 8, limits()).unwrap();
    assert_eq!(plan.rows, 6);
    assert_eq!(plan.values_bytes, 6 * 61);
    assert_eq!(plan.attribute_bytes, 34); // none+8+8+10+cancel-none+8
    for (bad, kind) in [
        (
            ExportLimits {
                max_rows: 5,
                ..limits()
            },
            "rows",
        ),
        (
            ExportLimits {
                max_attribute_bytes: 33,
                ..limits()
            },
            "attribute",
        ),
        (
            ExportLimits {
                max_intermediate_bytes: plan.intermediate_reservation - 1,
                ..limits()
            },
            "intermediate",
        ),
        (
            ExportLimits {
                max_output_bytes: plan.output_reservation - 1,
                ..limits()
            },
            "output",
        ),
    ] {
        let error = export_ring(&mut ring, 8, bad).unwrap_err();
        assert!(matches!(
            (kind, error),
            ("rows", ExportError::RowLimit)
                | ("attribute", ExportError::AttributeLimit)
                | ("intermediate", ExportError::IntermediateLimit)
                | ("output", ExportError::OutputLimit)
        ));
        assert_eq!(ring.records().collect::<Vec<_>>(), expected);
    }
    let exact = ExportLimits {
        max_rows: 6,
        max_attribute_bytes: 34,
        max_intermediate_bytes: plan.intermediate_reservation,
        max_output_bytes: plan.output_reservation,
    };
    let bytes = export_ring(&mut ring, 8, exact).unwrap();
    assert!(bytes.len() <= plan.output_reservation);
    verify(&bytes, &expected);
    assert!(ring.is_empty());
    let mut ring = sample_ring();
    let batch = ring.drain::<8>();
    assert!(export_batch(
        &batch,
        ExportLimits {
            max_rows: 0,
            ..limits()
        }
    )
    .is_err());
    verify(&export_batch(&batch, limits()).unwrap(), &expected);
}
#[test]
fn bitmap_alignment_and_hard_row_boundary_interoperate() {
    for count in [0, 1, 7, 8, 9, 63, 64, 65] {
        let pool = SpanPool::<1>::new(1, nz(1), 1).unwrap();
        let mut ring = TelemetryRing::<65>::default();
        for _ in 0..count {
            pool.start_span(None, SpanSchema::Request, 0, 0)
                .unwrap()
                .finish_ok(&mut ring, 0, PrimaryAttribute::None)
                .unwrap();
        }
        let records: Vec<_> = ring.records().collect();
        let plan = plan_ring(&ring, usize::MAX, limits()).unwrap();
        let bytes = export_ring(&mut ring, usize::MAX, limits()).unwrap();
        assert!(bytes.len() <= plan.output_reservation);
        verify(&bytes, &records);
    }
    // Fixed-capacity core rings are caller-owned. A larger test thread stack
    // accommodates the maximum-size input without changing core allocation policy.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let pool = SpanPool::<1>::new(1, nz(1), 1).unwrap();
            let mut ring = TelemetryRing::<MAX_ROWS>::default();
            for _ in 0..MAX_ROWS {
                pool.start_span(None, SpanSchema::Request, 0, 0)
                    .unwrap()
                    .finish_ok(&mut ring, 0, PrimaryAttribute::None)
                    .unwrap();
            }
            let records: Vec<_> = ring.records().collect();
            let plan = plan_ring(&ring, usize::MAX, limits()).unwrap();
            let bytes = export_ring(&mut ring, usize::MAX, limits()).unwrap();
            assert!(bytes.len() <= plan.output_reservation);
            verify(&bytes, &records);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn zero_capacity_inputs_are_valid_empty_arrow_streams() {
    let mut ring = TelemetryRing::<0>::default();
    let limits = ExportLimits {
        max_rows: 0,
        max_attribute_bytes: 0,
        ..limits()
    };
    verify(&export_ring(&mut ring, usize::MAX, limits).unwrap(), &[]);
    let batch = ColumnBatch::<0>::default();
    verify(&export_batch(&batch, limits).unwrap(), &[]);
}

// Additional descriptor checks against the pinned standard writer. This is not
// a general-purpose untrusted Arrow validator or a Grammar implementation.
fn verify_buffer_descriptors(bytes: &[u8], rows: usize) {
    assert_eq!(&bytes[..4], &[0xff; 4]);
    let schema_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    assert!(schema_len <= METADATA_ALLOWANCE);
    let offset = 8 + schema_len;
    assert_eq!(&bytes[offset..offset + 4], &[0xff; 4]);
    let message_len =
        u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
    assert!(message_len <= METADATA_ALLOWANCE);
    let body_start = offset + 8 + message_len;
    let message = arrow_ipc::root_as_message(&bytes[offset + 8..body_start]).unwrap();
    let batch = message.header_as_record_batch().unwrap();
    assert_eq!(batch.length(), rows as i64);
    let nodes = batch.nodes().unwrap();
    let buffers = batch.buffers().unwrap();
    assert_eq!(nodes.len(), 12);
    assert_eq!(buffers.len(), 24);
    let body_len = usize::try_from(message.bodyLength()).unwrap();
    assert!(body_len <= rows * 61 + 12 * rows.div_ceil(8) + 24 * 63);
    assert!(body_start.checked_add(body_len).unwrap() <= bytes.len());
    for i in 0..12 {
        assert_eq!(nodes.get(i).length(), rows as i64);
        assert_eq!(nodes.get(i).null_count(), 0);
        for (descriptor, expected_len) in [
            (buffers.get(i * 2), rows.div_ceil(8)),
            (
                buffers.get(i * 2 + 1),
                rows * if i < 6 || i == 11 { 8 } else { 1 },
            ),
        ] {
            let offset = usize::try_from(descriptor.offset()).unwrap();
            let len = usize::try_from(descriptor.length()).unwrap();
            assert_eq!(offset % 64, 0);
            assert_eq!(len, expected_len);
            assert!(offset.checked_add(len).unwrap() <= body_len);
        }
    }
}
