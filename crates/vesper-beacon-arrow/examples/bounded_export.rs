use arrow2::{
    array::PrimitiveArray,
    io::ipc::read::{read_stream_metadata, StreamReader, StreamState},
};
use std::{io::Cursor, num::NonZeroU64};
use vesper_beacon::*;
use vesper_beacon_arrow::*;
fn main() {
    let pool = SpanPool::<1>::new(u128::MAX, NonZeroU64::new(u64::MAX).unwrap(), 0xff).unwrap();
    let mut ring = TelemetryRing::<1>::default();
    pool.start_span(None, SpanSchema::Background, u64::MAX, 0)
        .unwrap()
        .cancel(&mut ring, u64::MAX)
        .unwrap();
    let plan = plan_ring(&ring, 1, ExportLimits::default()).unwrap();
    let too_small = ExportLimits {
        max_output_bytes: plan.output_reservation - 1,
        ..ExportLimits::default()
    };
    assert!(matches!(
        export_ring(&mut ring, 1, too_small),
        Err(ExportError::OutputLimit)
    ));
    assert_eq!(ring.len(), 1); // Record remains recoverable after failed preflight.
    let bytes = export_ring(&mut ring, 1, ExportLimits::default()).unwrap();
    assert!(ring.is_empty());
    let mut input = Cursor::new(&bytes);
    let metadata = read_stream_metadata(&mut input).unwrap();
    assert_eq!(metadata.schema.metadata["vesper.schema"], SCHEMA_VERSION);
    let mut reader = StreamReader::new(input, metadata, None);
    let StreamState::Some(batch) = reader.next().unwrap().unwrap() else {
        panic!("missing batch")
    };
    assert_eq!(batch.len(), 1);
    for (column, expected) in [
        (0, u64::MAX),
        (1, u64::MAX),
        (2, u64::MAX),
        (3, 0),
        (4, u64::MAX),
        (5, u64::MAX),
        (11, 0),
    ] {
        assert_eq!(
            batch.arrays()[column]
                .as_any()
                .downcast_ref::<PrimitiveArray<u64>>()
                .unwrap()
                .value(0),
            expected
        );
    }
    for (column, expected) in [(6, 0xff), (7, 2), (8, 1), (9, 4), (10, 0)] {
        assert_eq!(
            batch.arrays()[column]
                .as_any()
                .downcast_ref::<PrimitiveArray<u8>>()
                .unwrap()
                .value(0),
            expected
        );
    }
    assert!(reader.next().is_none());
    println!(
        "independent Arrow reader verified 12 columns, 1 cancelled span, {} IPC bytes",
        bytes.len()
    );
}
