//! Flight ring: wire sizes, overwrite-oldest retention against a reference
//! model, typed rejection of bad input, snapshot slots.

use vesper_chrono::wire::{
    self, ChronoLayouts, EFFECT_SNAPSHOT, HEADER_BYTES, OP_TAG_DIGEST, ObligationHopWire,
};
use vesper_chrono::{EventHeader, FlightRecorder, RecordError, RecorderConfig, TraceCursor};
use vesper_ffi::{Declaration, Endian, Field, Integer, LayoutErrorKind, Registry, WireType};

#[test]
fn wire_layout_sizes_and_roundtrips() {
    let l = ChronoLayouts::new();
    assert_eq!(l.header.size(), 32, "design-stated 32-byte header");
    assert_eq!(l.cursor.size(), wire::TRACE_CURSOR_BYTES);
    assert_eq!(l.hop.size(), wire::OBLIGATION_HOP_BYTES);
    assert_eq!(l.inout.size(), wire::INOUT_WRITE_BYTES);
    assert_eq!(l.obligation.size(), wire::OBLIGATION_RECORD_BYTES);
    let offsets: Vec<_> = l.header.fields().iter().map(|f| f.offset()).collect();
    assert_eq!(offsets, [0, 8, 16, 24, 26, 28]);

    let header = EventHeader {
        lamport_seq: u64::MAX,
        fiber_id: 7,
        ipt_tsc_anchor: 0x0102_0304_0506_0708,
        effect_id: 0xABCD,
        op_tag: 0x8001,
        payload_byte_len: 99,
    };
    let mut bytes = [0u8; 32];
    header.encode(&l, &mut bytes).unwrap();
    assert_eq!(&bytes[16..24], &0x0102_0304_0506_0708u64.to_le_bytes());
    assert_eq!(EventHeader::decode(&l, &bytes).unwrap(), header);
    assert!(EventHeader::decode(&l, &bytes[..31]).is_err());

    let cursor = TraceCursor {
        epoch_id: 3,
        lamport_seq: 9,
    };
    let mut c = [0u8; 12];
    cursor.encode(&l, &mut c).unwrap();
    assert_eq!(TraceCursor::decode(&l, &c).unwrap(), cursor);

    let hop = ObligationHopWire {
        lamport_seq: 1,
        fiber_id: 2,
        source_span_id: 3,
        hop_kind_tag: 3,
    };
    let mut h = [0u8; 24];
    hop.encode(&l, &mut h).unwrap();
    assert_eq!(ObligationHopWire::decode(&l, &h).unwrap(), hop);
    h[22] = 1; // explicit padding must stay zero
    assert!(ObligationHopWire::decode(&l, &h).is_err());
}

/// The size correction: the design's `ObligationHop` fields with natural
/// alignment would need 3 implicit tail bytes, which wire layouts reject.
#[test]
fn natively_aligned_obligation_hop_is_rejected_without_explicit_padding() {
    let n = |bits| WireType::Integer(Integer::unsigned(bits, Endian::Native));
    let registry = Registry::new([Declaration::new(
        "ObligationHop",
        vec![
            Field::new("lamport_seq", n(64)),
            Field::new("fiber_id", n(64)),
            Field::new("source_span_id", n(32)),
            Field::new("hop_kind_tag", n(8)),
        ],
    )])
    .unwrap();
    let err = registry.compile("ObligationHop").unwrap_err();
    assert!(
        matches!(err.kind, LayoutErrorKind::ImplicitPadding { .. }),
        "{err}"
    );
}

fn tiny() -> RecorderConfig {
    RecorderConfig {
        ring_bytes: 512,
        max_records: 10,
        max_payload: 64,
        snapshot_slots: 3,
        snapshot_image_bytes: 16,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn overwrite_oldest_matches_reference_model() {
    for seed in 1..=20u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut rec = FlightRecorder::new(tiny()).unwrap();
        let mut written: Vec<(u64, Vec<u8>)> = Vec::new();
        for step in 0..500u64 {
            let len = (rng.next() % 65) as usize;
            let payload: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let seq = rec
                .record_effect(rng.next() % 4, step, 1, (rng.next() % 3) as u16, &payload)
                .unwrap();
            written.push((seq, payload));
            let retained: Vec<_> = rec
                .events()
                .map(|e| (e.lamport_seq, e.payload.to_vec()))
                .collect();
            // A nonempty suffix of everything written, newest last.
            assert!(!retained.is_empty());
            assert_eq!(retained[..], written[written.len() - retained.len()..]);
            assert!(retained.len() <= tiny().max_records);
            let bytes: usize = retained.iter().map(|(_, p)| HEADER_BYTES + p.len()).sum();
            assert!(bytes as u64 <= rec.bytes_used() && rec.bytes_used() <= 512);
            assert_eq!(rec.first_retained_seq(), retained[0].0);
            assert_eq!(rec.evicted_records() + retained.len() as u64, seq);
            // Records are only lost to capacity: with the record limit not
            // reached, the retained bytes plus the evicted predecessor plus a
            // maximal wrap gap exceed the ring.
            if retained.len() < tiny().max_records && retained.len() < written.len() {
                let older = &written[written.len() - retained.len() - 1];
                assert!(bytes + HEADER_BYTES + older.1.len() + 2 * (HEADER_BYTES + 64) > 512);
            }
        }
    }
}

#[test]
fn malformed_input_is_a_typed_error() {
    let mut rec = FlightRecorder::new(tiny()).unwrap();
    assert_eq!(
        rec.record_effect(0, 0, 0xFFF0, 0, b"x"),
        Err(RecordError::ReservedEffectId(0xFFF0))
    );
    assert_eq!(
        rec.record_effect(0, 0, 1, OP_TAG_DIGEST, b"x"),
        Err(RecordError::OpTagOutOfRange(OP_TAG_DIGEST))
    );
    assert_eq!(
        rec.record_effect(0, 0, 1, 0, &[0; 65]),
        Err(RecordError::PayloadTooLarge { len: 65, max: 64 })
    );
    rec.record_effect(0, 10, 1, 0, b"x").unwrap();
    assert_eq!(
        rec.record_effect(0, 9, 1, 0, b"x"),
        Err(RecordError::TimeWentBackwards {
            last_ns: 10,
            got_ns: 9
        })
    );
    assert!(
        rec.record_obligation_hop(0, 10, 1, vesper_chrono::HopKind::Leak, 0)
            .is_err(),
        "hop for an obligation never minted"
    );
    assert_eq!(
        rec.capture_snapshot(10, &[0; 17]),
        Err(RecordError::ImageTooLarge { len: 17, max: 16 })
    );
    assert!(matches!(
        FlightRecorder::new(RecorderConfig {
            ring_bytes: 64,
            ..tiny()
        }),
        Err(RecordError::InvalidConfig(_))
    ));
    assert!(matches!(
        FlightRecorder::new(RecorderConfig {
            max_records: 0,
            ..tiny()
        }),
        Err(RecordError::InvalidConfig(_))
    ));
    assert_eq!(rec.len(), 1, "failed records leave no trace");
}

#[test]
fn digest_records_and_snapshot_slots() {
    let mut rec = FlightRecorder::new(tiny()).unwrap();
    rec.record_effect_digest(1, 0, 5, 2, b"a long secret outcome")
        .unwrap();
    let ev = rec.events().next().unwrap();
    assert!(ev.digest);
    assert_eq!(ev.op_tag, 2);
    assert_eq!(
        ev.payload,
        blake3::hash(b"a long secret outcome").as_bytes()
    );
    for i in 0..5u8 {
        let epoch = rec.capture_snapshot(u64::from(i), &[i; 4]).unwrap();
        assert_eq!(epoch, u32::from(i) + 1);
    }
    let snaps = rec.snapshots();
    assert_eq!(
        snaps.iter().map(|s| s.epoch_id).collect::<Vec<_>>(),
        [3, 4, 5],
        "oldest snapshot slots overwritten"
    );
    for s in &snaps {
        let marker = rec
            .events()
            .find(|e| e.lamport_seq == s.start_lamport_seq)
            .unwrap();
        assert_eq!(marker.effect_id, EFFECT_SNAPSHOT);
        assert_eq!(marker.payload, s.epoch_id.to_le_bytes());
        assert_eq!(s.image, [(s.epoch_id - 1) as u8; 4]);
    }
}
