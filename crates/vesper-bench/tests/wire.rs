use vesper_bench::wire::{
    AvailabilityMask, PMU_SAMPLE_WIRE_BYTES, PmuField, PmuSample, PmuSampleWire, WireError,
    backend_bound_bp,
};

#[test]
fn layout_is_exactly_one_padding_free_cache_line() {
    assert_eq!(PMU_SAMPLE_WIRE_BYTES, 64);
    let mut next = 0;
    for field in PmuField::ALL {
        let (offset, width) = field.span();
        assert_eq!(offset, next, "{} leaves a gap", field.name());
        assert_eq!(offset % width, 0, "{} is misaligned", field.name());
        next = offset + width;
    }
    assert_eq!(next, 64);
}

#[test]
fn encoding_is_little_endian_and_round_trips() {
    let wire = PmuSampleWire {
        ref_cycles: 0x0102_0304_0506_0708,
        instructions_retired: 2,
        l1d_read_misses: 3,
        llc_misses: 4,
        branch_mispredicts: 5,
        simd_vec_uops_retired: 6,
        arena_bump_bytes: u64::MAX,
        tma_backend_bound_slots_bp: 10_000,
        icache_rand_offset_bytes: 4032,
    };
    let bytes = wire.encode().unwrap();
    assert_eq!(&bytes[..8], &[8, 7, 6, 5, 4, 3, 2, 1]);
    assert_eq!(&bytes[48..56], &[0xff; 8]);
    assert_eq!(&bytes[56..60], &10_000u32.to_le_bytes());
    assert_eq!(&bytes[60..64], &4032u32.to_le_bytes());
    assert_eq!(PmuSampleWire::decode(&bytes).unwrap(), wire);
}

#[test]
fn malformed_records_are_typed_errors() {
    assert_eq!(PmuSampleWire::decode(&[0; 63]), Err(WireError::Length(63)));
    assert_eq!(PmuSampleWire::decode(&[0; 65]), Err(WireError::Length(65)));
    let mut bytes = [0u8; 64];
    bytes[56..60].copy_from_slice(&10_001u32.to_le_bytes());
    assert_eq!(
        PmuSampleWire::decode(&bytes),
        Err(WireError::BasisPointsOutOfRange(10_001))
    );
    for bad in [4096u32, 65, 1] {
        let mut bytes = [0u8; 64];
        bytes[60..64].copy_from_slice(&bad.to_le_bytes());
        assert_eq!(
            PmuSampleWire::decode(&bytes),
            Err(WireError::LayoutOffsetOutOfRange(u64::from(bad)))
        );
    }
    let wire = PmuSampleWire {
        icache_rand_offset_bytes: 100,
        ..PmuSampleWire::default()
    };
    assert!(wire.encode().is_err());
}

#[test]
fn unavailable_fields_are_never_encoded_as_measured_zeroes() {
    let mut sample = PmuSample::new();
    sample.set(PmuField::InstructionsRetired, 42).unwrap();
    sample.set(PmuField::ArenaBumpBytes, 0).unwrap();
    let Err(WireError::Unavailable(missing)) = sample.to_wire() else {
        panic!("a partial sample must not encode as a complete record");
    };
    assert_eq!(missing.len(), 7);
    assert!(!missing.contains(&PmuField::InstructionsRetired));
    let (wire, mask) = sample.to_wire_masked().unwrap();
    assert_eq!(wire.instructions_retired, 42);
    assert!(mask.contains(PmuField::ArenaBumpBytes));
    assert!(!mask.contains(PmuField::RefCycles));
    assert_eq!(PmuSample::from_wire_masked(&wire, mask), sample);
    assert!(AvailabilityMask::new(1 << 9).is_err());
    assert_eq!(
        sample.set(PmuField::TmaBackendBoundSlotsBp, 10_001),
        Err(WireError::BasisPointsOutOfRange(10_001))
    );
    assert_eq!(
        sample.set(PmuField::IcacheRandOffsetBytes, u64::from(u32::MAX) + 1),
        Err(WireError::FieldOverflow(PmuField::IcacheRandOffsetBytes))
    );
    for field in PmuField::ALL {
        sample.set(field, 0).unwrap();
    }
    assert_eq!(sample.to_wire().unwrap(), PmuSampleWire::default());
}

#[test]
fn backend_bound_share_is_exact_and_checked() {
    assert_eq!(backend_bound_bp(1, 3), Some(3333));
    assert_eq!(backend_bound_bp(5, 5), Some(10_000));
    assert_eq!(backend_bound_bp(u64::MAX, u64::MAX), Some(10_000));
    assert_eq!(backend_bound_bp(1, 0), None);
    assert_eq!(backend_bound_bp(6, 5), None);
}
