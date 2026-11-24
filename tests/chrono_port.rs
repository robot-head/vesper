//! Differential verification of `@vesper/chrono` (`crates/vesper-chrono/vesper/`):
//! wire record encoding/decoding, snapshot-epoch timeline index & O(log N)
//! reverse `inout` watchpoints, and bounded flight-recorder ring with
//! overwrite-oldest eviction against `vesper_chrono::{wire, timeline, ring}`.
mod std_io_support;

use std_io_support::{MODES, Temp, agree};
use vesper::{ffi_lowering, packages};
use vesper_chrono::ring::{FlightRecorder, RecorderConfig};
use vesper_chrono::timeline::{ChronoTimelineIndex, InOutWriteEvent, IngestOrder, SnapshotEpoch};
use vesper_chrono::wire::{
    ChronoLayouts, EventHeader, InoutWriteRecord, ObligationHopWire, ObligationRecord, TraceCursor,
};

fn check_chrono_driver(source: &str) -> ffi_lowering::ForeignChecked {
    let dir = Temp::new("chrono-port");
    let chrono_pkg = std::fs::canonicalize("crates/vesper-chrono/vesper").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/chrono-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper/chrono\" = {{ version = \"^0.1\", path = {:?} }}\n",
            chrono_pkg
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.ves"), source).unwrap();
    let assembled = packages::load(dir.path()).expect("load @test/chrono-driver");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &ffi_lowering::Imports::new())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    h
}

fn mix_u64(h: u64, v: u64) -> u64 {
    let mut out = h;
    for b in v.to_le_bytes() {
        out ^= u64::from(b);
        out = out.wrapping_mul(1099511628211);
    }
    out
}

fn mix_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    h = mix_u64(h, bytes.len() as u64);
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    h
}

const FNV_PREAMBLE: &str = r#"
fn mix_u64(h: u64, v: u64) -> u64 {
    let mut out = h;
    let mut i: u64 = 0;
    loop {
        if i >= 8 { break; }
        let b = wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8))) & 255;
        out = wrapping_mul(out ^ b, 1099511628211);
        i = wrapping_add(i, 1);
    }
    out
}

fn mix_bytes(h: u64, in bytes: Slice[u8]) -> u64 {
    let mut out = mix_u64(h, bytes.len());
    let mut i: u64 = 0;
    loop {
        if i >= bytes.len() { break; }
        let b = match bytes.try_get(i) { Option::Some(v) => v, Option::None => 0 };
        out = wrapping_mul(out ^ widen_u64(b), 1099511628211);
        i = wrapping_add(i, 1);
    }
    out
}

fn mix_bool(h: u64, b: bool) -> u64 {
    mix_u64(h, if b { 1 } else { 0 })
}
"#;

#[test]
fn wire_records_roundtrip_and_padding_match_rust() {
    let layouts = ChronoLayouts::new();
    let hdr = EventHeader {
        lamport_seq: 0x0102_0304_0506_0708,
        fiber_id: 99,
        ipt_tsc_anchor: 1_234_567_890,
        effect_id: 42,
        op_tag: 0x8007,
        payload_byte_len: 17,
    };
    let mut hdr_bytes = [0u8; 32];
    hdr.encode(&layouts, &mut hdr_bytes).unwrap();
    let hdr_dec = EventHeader::decode(&layouts, &hdr_bytes).unwrap();

    let cur = TraceCursor {
        epoch_id: 12,
        lamport_seq: 456_789,
    };
    let mut cur_bytes = [0u8; 12];
    cur.encode(&layouts, &mut cur_bytes).unwrap();
    let cur_dec = TraceCursor::decode(&layouts, &cur_bytes).unwrap();

    let io = InoutWriteRecord {
        place_hash: 0x0011_2233_4455_6677,
        source_span_id: 77,
    };
    let mut io_bytes = [0u8; 16];
    io.encode(&layouts, &mut io_bytes).unwrap();
    let io_dec = InoutWriteRecord::decode(&layouts, &io_bytes).unwrap();

    let obl = ObligationRecord {
        obligation_id: 500,
        aux: 42,
        source_span_id: 19,
        hop_kind_tag: 2,
    };
    let mut obl_bytes = [0u8; 24];
    obl.encode(&layouts, &mut obl_bytes).unwrap();
    let obl_dec = ObligationRecord::decode(&layouts, &obl_bytes).unwrap();

    let hop = ObligationHopWire {
        lamport_seq: 888,
        fiber_id: 7,
        source_span_id: 33,
        hop_kind_tag: 1,
    };
    let mut hop_bytes = [0u8; 24];
    hop.encode(&layouts, &mut hop_bytes).unwrap();
    let hop_dec = ObligationHopWire::decode(&layouts, &hop_bytes).unwrap();

    let mut bad_io = io_bytes;
    bad_io[14] = 1;
    let rust_bad_io_err = InoutWriteRecord::decode(&layouts, &bad_io).is_err();
    let rust_short_hdr_err = EventHeader::decode(&layouts, &hdr_bytes[..31]).is_err();

    let h_hdr = mix_u64(
        mix_u64(
            mix_u64(
                mix_u64(
                    mix_u64(
                        mix_u64(mix_bytes(fnv1a(b"h"), &hdr_bytes), hdr_dec.lamport_seq),
                        hdr_dec.fiber_id,
                    ),
                    hdr_dec.ipt_tsc_anchor,
                ),
                u64::from(hdr_dec.effect_id),
            ),
            u64::from(hdr_dec.op_tag),
        ),
        u64::from(hdr_dec.payload_byte_len),
    );
    let h_cur = mix_u64(
        mix_u64(
            mix_bytes(fnv1a(b"c"), &cur_bytes),
            u64::from(cur_dec.epoch_id),
        ),
        cur_dec.lamport_seq,
    );
    let h_io = mix_u64(
        mix_u64(mix_bytes(fnv1a(b"i"), &io_bytes), io_dec.place_hash),
        u64::from(io_dec.source_span_id),
    );
    let h_obl = mix_u64(
        mix_u64(
            mix_u64(
                mix_u64(mix_bytes(fnv1a(b"o"), &obl_bytes), obl_dec.obligation_id),
                obl_dec.aux,
            ),
            u64::from(obl_dec.source_span_id),
        ),
        u64::from(obl_dec.hop_kind_tag),
    );
    let h_hop = mix_u64(
        mix_u64(
            mix_u64(
                mix_u64(mix_bytes(fnv1a(b"p"), &hop_bytes), hop_dec.lamport_seq),
                hop_dec.fiber_id,
            ),
            u64::from(hop_dec.source_span_id),
        ),
        u64::from(hop_dec.hop_kind_tag),
    );
    let mut h = fnv1a(b"wire");
    h = mix_u64(h, h_hdr);
    h = mix_u64(h, h_cur);
    h = mix_u64(h, h_io);
    h = mix_u64(h, h_obl);
    h = mix_u64(h, h_hop);
    h = mix_u64(h, u64::from(rust_bad_io_err));
    h = mix_u64(h, u64::from(rust_short_hdr_err));

    let source = format!(
        r#"use @vesper/chrono::wire::{{
    EventHeader, InoutWriteRecord, ObligationHopWire, ObligationRecord, TraceCursor,
    decode_event_header, decode_inout_write, decode_obligation_hop, decode_obligation_record,
    decode_trace_cursor, encode_event_header, encode_inout_write, encode_obligation_hop,
    encode_obligation_record, encode_trace_cursor,
}};
{FNV_PREAMBLE}
fn fnv_ch(c: u8) -> u64 {{
    wrapping_mul(14695981039346656037 ^ widen_u64(c), 1099511628211)
}}

pub fn main() -> u64 {{
    let hdr = EventHeader {{
        lamport_seq: 72623859790382856,
        fiber_id: 99,
        ipt_tsc_anchor: 1234567890,
        effect_id: 42,
        op_tag: 32775,
        payload_byte_len: 17,
    }};
    let hdr_bytes = encode_event_header(hdr);
    let hdr_dec = match decode_event_header(in hdr_bytes.as_slice()) {{
        Option::Some(v) => v,
        Option::None => hdr,
    }};
    let cur = TraceCursor {{ epoch_id: 12, lamport_seq: 456789 }};
    let cur_bytes = encode_trace_cursor(cur);
    let cur_dec = match decode_trace_cursor(in cur_bytes.as_slice()) {{
        Option::Some(v) => v,
        Option::None => cur,
    }};
    let io = InoutWriteRecord {{ place_hash: 4822678189205111, source_span_id: 77 }};
    let io_bytes = encode_inout_write(io);
    let io_dec = match decode_inout_write(in io_bytes.as_slice()) {{
        Option::Some(v) => v,
        Option::None => io,
    }};
    let obl = ObligationRecord {{ obligation_id: 500, aux: 42, source_span_id: 19, hop_kind_tag: 2 }};
    let obl_bytes = encode_obligation_record(obl);
    let obl_dec = match decode_obligation_record(in obl_bytes.as_slice()) {{
        Option::Some(v) => v,
        Option::None => obl,
    }};
    let hop = ObligationHopWire {{ lamport_seq: 888, fiber_id: 7, source_span_id: 33, hop_kind_tag: 1 }};
    let hop_bytes = encode_obligation_hop(hop);
    let hop_dec = match decode_obligation_hop(in hop_bytes.as_slice()) {{
        Option::Some(v) => v,
        Option::None => hop,
    }};

    let mut short_hdr: ChunkSeq[u8] = ChunkSeq::new();
    let mut k: u64 = 0;
    loop {{
        if k >= 31 {{ break; }}
        short_hdr.push(match hdr_bytes.as_slice().try_get(k) {{ Option::Some(b) => b, Option::None => 0 }});
        k = wrapping_add(k, 1);
    }}
    let short_hdr_err = match decode_event_header(in short_hdr.as_slice()) {{
        Option::Some(_) => false,
        Option::None => true,
    }};

    let mut h_hdr = mix_bytes(fnv_ch(104), in hdr_bytes.as_slice());
    h_hdr = mix_u64(h_hdr, hdr_dec.lamport_seq);
    h_hdr = mix_u64(h_hdr, hdr_dec.fiber_id);
    h_hdr = mix_u64(h_hdr, hdr_dec.ipt_tsc_anchor);
    h_hdr = mix_u64(h_hdr, widen_u64(widen_u32(hdr_dec.effect_id)));
    h_hdr = mix_u64(h_hdr, widen_u64(widen_u32(hdr_dec.op_tag)));
    h_hdr = mix_u64(h_hdr, widen_u64(hdr_dec.payload_byte_len));

    let mut h_cur = mix_bytes(fnv_ch(99), in cur_bytes.as_slice());
    h_cur = mix_u64(h_cur, widen_u64(cur_dec.epoch_id));
    h_cur = mix_u64(h_cur, cur_dec.lamport_seq);

    let mut h_io = mix_bytes(fnv_ch(105), in io_bytes.as_slice());
    h_io = mix_u64(h_io, io_dec.place_hash);
    h_io = mix_u64(h_io, widen_u64(io_dec.source_span_id));

    let mut h_obl = mix_bytes(fnv_ch(111), in obl_bytes.as_slice());
    h_obl = mix_u64(h_obl, obl_dec.obligation_id);
    h_obl = mix_u64(h_obl, obl_dec.aux);
    h_obl = mix_u64(h_obl, widen_u64(obl_dec.source_span_id));
    h_obl = mix_u64(h_obl, widen_u64(widen_u32(obl_dec.hop_kind_tag)));

    let mut h_hop = mix_bytes(fnv_ch(112), in hop_bytes.as_slice());
    h_hop = mix_u64(h_hop, hop_dec.lamport_seq);
    h_hop = mix_u64(h_hop, hop_dec.fiber_id);
    h_hop = mix_u64(h_hop, widen_u64(hop_dec.source_span_id));
    h_hop = mix_u64(h_hop, widen_u64(widen_u32(hop_dec.hop_kind_tag)));

    let mut bad_io = io_bytes;
    let _ = bad_io.try_set(14, 1);
    let bad_io_err = match decode_inout_write(in bad_io.as_slice()) {{
        Option::Some(_) => false,
        Option::None => true,
    }};

    if h_hdr != {h_hdr} {{ return 1; }}
    if h_cur != {h_cur} {{ return 2; }}
    if h_io != {h_io} {{ return 3; }}
    if h_obl != {h_obl} {{ return 4; }}
    if h_hop != {h_hop} {{ return 5; }}
    if !bad_io_err {{ return 6; }}
    if !short_hdr_err {{ return 7; }}

    let mut h: u64 = 14695981039346656037;
    let tag = "wire";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, h_hdr);
    h = mix_u64(h, h_cur);
    h = mix_u64(h, h_io);
    h = mix_u64(h, h_obl);
    h = mix_u64(h, h_hop);
    h = mix_bool(h, bad_io_err);
    h = mix_bool(h, short_hdr_err);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn timeline_index_and_watchpoint_search_match_rust() {
    let raw_writes = vec![
        InOutWriteEvent {
            lamport_seq: 30,
            fiber_id: 2,
            place_hash: 0xAA,
            span_id: 3,
        },
        InOutWriteEvent {
            lamport_seq: 10,
            fiber_id: 1,
            place_hash: 0xAA,
            span_id: 1,
        },
        InOutWriteEvent {
            lamport_seq: 20,
            fiber_id: 1,
            place_hash: 0xBB,
            span_id: 2,
        },
        InOutWriteEvent {
            lamport_seq: 50,
            fiber_id: 3,
            place_hash: 0xAA,
            span_id: 5,
        },
        InOutWriteEvent {
            lamport_seq: 40,
            fiber_id: 2,
            place_hash: 0xBB,
            span_id: 4,
        },
    ];
    let mut idx =
        ChronoTimelineIndex::from_writes(raw_writes, IngestOrder::SortWithReason("out of order"))
            .unwrap();
    let moved = idx.ordering_notes()[0].moved as u64;
    idx.record_snapshot(SnapshotEpoch {
        epoch_id: 1,
        start_lamport_seq: 15,
        arena_cow_offset: 100,
    })
    .unwrap();
    idx.record_snapshot(SnapshotEpoch {
        epoch_id: 2,
        start_lamport_seq: 35,
        arena_cow_offset: 200,
    })
    .unwrap();

    let dup_err = idx
        .record_inout_write(InOutWriteEvent {
            lamport_seq: 50,
            fiber_id: 1,
            place_hash: 0xAA,
            span_id: 9,
        })
        .is_err();
    let ooo_err = idx
        .record_inout_write(InOutWriteEvent {
            lamport_seq: 45,
            fiber_id: 1,
            place_hash: 0xAA,
            span_id: 9,
        })
        .is_err();
    let snap_err = idx
        .record_snapshot(SnapshotEpoch {
            epoch_id: 2,
            start_lamport_seq: 60,
            arena_cow_offset: 300,
        })
        .is_err();

    let (q1, p1) = idx.find_last_writer_before_probed(0xAA, 45);
    let (s1, w1) = q1.unwrap();
    let (q2, _) = idx.find_last_writer_before_probed(0xAA, 10);
    let (q3, p3) = idx.find_last_writer_before_probed(0xBB, 99);
    let (s3, w3) = q3.unwrap();

    assert!(
        p1 <= 5 && p3 <= 5,
        "expected O(log N) probes: p1={p1}, p3={p3}"
    );
    let mut h = fnv1a(b"timeline");
    h = mix_u64(h, moved);
    h = mix_u64(h, idx.write_count() as u64);
    h = mix_u64(h, u64::from(dup_err));
    h = mix_u64(h, u64::from(ooo_err));
    h = mix_u64(h, u64::from(snap_err));
    h = mix_u64(h, u64::from(s1.unwrap().epoch_id));
    h = mix_u64(h, w1.lamport_seq);
    h = mix_u64(h, u64::from(q2.is_some()));
    h = mix_u64(h, u64::from(s3.unwrap().epoch_id));
    h = mix_u64(h, w3.lamport_seq);

    let source = format!(
        r#"use @vesper/chrono::timeline::{{
    InOutWriteEvent, SnapshotEpoch, find_last_writer_before_probed, new_index,
    record_inout_write, record_snapshot, sort_writes_stable,
}};
{FNV_PREAMBLE}
pub fn main() -> u64 {{
    let mut ws: ChunkSeq[InOutWriteEvent] = ChunkSeq::new();
    ws.push(InOutWriteEvent {{ lamport_seq: 30, fiber_id: 2, place_hash: 170, span_id: 3 }});
    ws.push(InOutWriteEvent {{ lamport_seq: 10, fiber_id: 1, place_hash: 170, span_id: 1 }});
    ws.push(InOutWriteEvent {{ lamport_seq: 20, fiber_id: 1, place_hash: 187, span_id: 2 }});
    ws.push(InOutWriteEvent {{ lamport_seq: 50, fiber_id: 3, place_hash: 170, span_id: 5 }});
    ws.push(InOutWriteEvent {{ lamport_seq: 40, fiber_id: 2, place_hash: 187, span_id: 4 }});
    let moved = sort_writes_stable(inout ws);

    let mut idx = new_index();
    let mut i: u64 = 0;
    loop {{
        if i >= ws.len() {{ break; }}
        let ev = match ws.as_slice().try_get(i) {{
            Option::Some(v) => v,
            Option::None => InOutWriteEvent {{ lamport_seq: 0, fiber_id: 0, place_hash: 0, span_id: 0 }},
        }};
        let _ = record_inout_write(inout idx, ev);
        i = wrapping_add(i, 1);
    }}
    let _ = record_snapshot(inout idx, SnapshotEpoch {{ epoch_id: 1, start_lamport_seq: 15, arena_cow_offset: 100 }});
    let _ = record_snapshot(inout idx, SnapshotEpoch {{ epoch_id: 2, start_lamport_seq: 35, arena_cow_offset: 200 }});

    let dup_err = record_inout_write(inout idx, InOutWriteEvent {{ lamport_seq: 50, fiber_id: 1, place_hash: 170, span_id: 9 }}) != 0;
    let ooo_err = record_inout_write(inout idx, InOutWriteEvent {{ lamport_seq: 45, fiber_id: 1, place_hash: 170, span_id: 9 }}) != 0;
    let snap_err = record_snapshot(inout idx, SnapshotEpoch {{ epoch_id: 2, start_lamport_seq: 60, arena_cow_offset: 300 }}) != 0;

    let q1 = find_last_writer_before_probed(in idx, 170, 45);
    let q2 = find_last_writer_before_probed(in idx, 170, 10);
    let q3 = find_last_writer_before_probed(in idx, 187, 99);

    let mut h: u64 = 14695981039346656037;
    let tag = "timeline";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, moved);
    h = mix_u64(h, idx.write_count);
    h = mix_bool(h, dup_err);
    h = mix_bool(h, ooo_err);
    h = mix_bool(h, snap_err);
    h = mix_u64(h, widen_u64(q1.snapshot.epoch_id));
    h = mix_u64(h, q1.write.lamport_seq);
    h = mix_bool(h, q2.found);
    h = mix_u64(h, widen_u64(q3.snapshot.epoch_id));
    h = mix_u64(h, q3.write.lamport_seq);
    if q1.probes > 5 || q3.probes > 5 {{ return 0; }}
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn flight_recorder_wrap_eviction_and_snapshots_match_rust() {
    let cfg = RecorderConfig {
        ring_bytes: 160,
        max_records: 16,
        max_payload: 32,
        snapshot_slots: 2,
        snapshot_image_bytes: 16,
    };
    let mut rec = FlightRecorder::new(cfg).unwrap();
    let s1 = rec.record_effect(1, 100, 1, 2, b"alpha").unwrap();
    let s2 = rec.record_inout_write(1, 110, 0x1234, 7).unwrap();
    let o1 = rec.mint_obligation(2, 120, 8, None).unwrap();
    let e1 = rec.capture_snapshot(130, b"img1").unwrap();
    let s3 = rec
        .record_effect(2, 140, 3, 4, b"beta-wrap-1234567890")
        .unwrap();
    let e2 = rec.capture_snapshot(150, b"img2").unwrap();
    let e3 = rec.capture_snapshot(160, b"img3").unwrap();

    let mut h = fnv1a(b"ring");
    for v in [s1, s2, o1, u64::from(e1), s3, u64::from(e2), u64::from(e3)] {
        h = mix_u64(h, v);
    }
    h = mix_u64(h, rec.len() as u64);
    h = mix_u64(h, rec.first_retained_seq());
    h = mix_u64(h, rec.bytes_used());
    h = mix_u64(h, rec.evicted_records());
    for ev in rec.events() {
        h = mix_u64(h, ev.lamport_seq);
        h = mix_u64(h, ev.fiber_id);
        h = mix_u64(h, u64::from(ev.effect_id));
        h = mix_u64(h, u64::from(ev.op_tag));
        h = mix_u64(h, u64::from(ev.digest));
        h = mix_bytes(h, ev.payload);
    }
    for sn in rec.snapshots() {
        h = mix_u64(h, u64::from(sn.epoch_id));
        h = mix_u64(h, sn.start_lamport_seq);
        h = mix_bytes(h, sn.image);
    }

    let source = format!(
        r#"use @vesper/chrono::ring::{{
    FlightEvent, RecorderConfig, SnapshotImage, bytes_used, capture_snapshot, events,
    first_retained_seq, mint_obligation, new_recorder, record_effect, record_inout_write,
    retained_len, snapshots,
}};
{FNV_PREAMBLE}
fn ok_u64(r: Result[u64, u32]) -> u64 {{
    match r {{ Result::Ok(v) => v, Result::Err(_) => 0 }}
}}

fn ok_u32(r: Result[u32, u32]) -> u32 {{
    match r {{ Result::Ok(v) => v, Result::Err(_) => 0 }}
}}

pub fn main() -> u64 {{
    let cfg = RecorderConfig {{
        ring_bytes: 160,
        max_records: 16,
        max_payload: 32,
        snapshot_slots: 2,
        snapshot_image_bytes: 16,
    }};
    let mut rec = match new_recorder(cfg) {{
        Result::Ok(r) => r,
        Result::Err(_) => return 0,
    }};
    let s1 = ok_u64(record_effect(inout rec, 1, 100, 1, 2, in "alpha".as_bytes()));
    let s2 = ok_u64(record_inout_write(inout rec, 1, 110, 4660, 7));
    let o1 = ok_u64(mint_obligation(inout rec, 2, 120, 8, false, 0));
    let e1 = ok_u32(capture_snapshot(inout rec, 130, in "img1".as_bytes()));
    let s3 = ok_u64(record_effect(inout rec, 2, 140, 3, 4, in "beta-wrap-1234567890".as_bytes()));
    let e2 = ok_u32(capture_snapshot(inout rec, 150, in "img2".as_bytes()));
    let e3 = ok_u32(capture_snapshot(inout rec, 160, in "img3".as_bytes()));

    let mut evs = events(in rec);
    let mut evs_rev: ChunkSeq[FlightEvent] = ChunkSeq::new();
    loop {{
        match evs.pop() {{
            Option::Some(ev) => evs_rev.push(ev),
            Option::None => break,
        }}
    }}
    let mut snaps = snapshots(in rec);
    let mut snaps_rev: ChunkSeq[SnapshotImage] = ChunkSeq::new();
    loop {{
        match snaps.pop() {{
            Option::Some(sn) => snaps_rev.push(sn),
            Option::None => break,
        }}
    }}

    let mut h: u64 = 14695981039346656037;
    let tag = "ring";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, s1);
    h = mix_u64(h, s2);
    h = mix_u64(h, o1);
    h = mix_u64(h, widen_u64(e1));
    h = mix_u64(h, s3);
    h = mix_u64(h, widen_u64(e2));
    h = mix_u64(h, widen_u64(e3));
    h = mix_u64(h, retained_len(in rec));
    h = mix_u64(h, first_retained_seq(in rec));
    h = mix_u64(h, bytes_used(in rec));
    h = mix_u64(h, rec.evicted_records);
    loop {{
        match evs_rev.pop() {{
            Option::Some(ev) => {{
                h = mix_u64(h, ev.lamport_seq);
                h = mix_u64(h, ev.fiber_id);
                h = mix_u64(h, widen_u64(widen_u32(ev.effect_id)));
                h = mix_u64(h, widen_u64(widen_u32(ev.op_tag)));
                h = mix_bool(h, ev.digest);
                h = mix_bytes(h, in ev.payload.as_slice());
            }}
            Option::None => break,
        }}
    }}
    loop {{
        match snaps_rev.pop() {{
            Option::Some(sn) => {{
                h = mix_u64(h, widen_u64(sn.epoch_id));
                h = mix_u64(h, sn.start_lamport_seq);
                h = mix_bytes(h, in sn.image.as_slice());
            }}
            Option::None => break,
        }}
    }}
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}
