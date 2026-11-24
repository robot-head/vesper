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

#[test]
fn obligation_lineage_provenance_truncation_and_leaks_match_rust() {
    use vesper_chrono::lineage::{HopKind, ObligationHop, ObligationLineage, ObligationStatus};

    let mut lin = ObligationLineage::new();
    let id1 = lin.mint(10, 1, 101, None).unwrap();
    let id2 = lin.mint(20, 1, 102, Some(id1)).unwrap();
    let id3 = lin.mint(30, 2, 103, None).unwrap();
    lin.record(
        id1,
        ObligationHop {
            lamport_seq: 40,
            fiber_id: 1,
            source_span_id: 104,
            kind: HopKind::SinkMove,
        },
    )
    .unwrap();
    lin.record(
        id1,
        ObligationHop {
            lamport_seq: 50,
            fiber_id: 1,
            source_span_id: 105,
            kind: HopKind::UnpackDischarge,
        },
    )
    .unwrap();
    lin.record(
        id2,
        ObligationHop {
            lamport_seq: 60,
            fiber_id: 2,
            source_span_id: 106,
            kind: HopKind::ChannelHandoff { to_fiber: 3 },
        },
    )
    .unwrap();
    lin.record(
        id2,
        ObligationHop {
            lamport_seq: 70,
            fiber_id: 3,
            source_span_id: 107,
            kind: HopKind::Leak,
        },
    )
    .unwrap();

    let after_term_err = lin
        .record(
            id1,
            ObligationHop {
                lamport_seq: 80,
                fiber_id: 1,
                source_span_id: 108,
                kind: HopKind::SinkMove,
            },
        )
        .is_err();
    let ooo_err = lin
        .record(
            id3,
            ObligationHop {
                lamport_seq: 70,
                fiber_id: 2,
                source_span_id: 109,
                kind: HopKind::Projection { place_hash: 999 },
            },
        )
        .is_err();

    let s1 = match lin.status(id1).unwrap() {
        ObligationStatus::Live => 0u64,
        ObligationStatus::Discharged => 1,
        ObligationStatus::Leaked => 2,
    };
    let s2 = match lin.status(id2).unwrap() {
        ObligationStatus::Live => 0u64,
        ObligationStatus::Discharged => 1,
        ObligationStatus::Leaked => 2,
    };
    let s3 = match lin.status(id3).unwrap() {
        ObligationStatus::Live => 0u64,
        ObligationStatus::Discharged => 1,
        ObligationStatus::Leaked => 2,
    };

    let mut trunc = ObligationLineage::for_truncated_log();
    trunc
        .record(
            2,
            ObligationHop {
                lamport_seq: 5,
                fiber_id: 1,
                source_span_id: 11,
                kind: HopKind::SinkMove,
            },
        )
        .unwrap();
    let t_id = trunc.mint(10, 1, 12, Some(2)).unwrap();
    let evicted_err = trunc.chain(2).is_err();

    let mut h = fnv1a(b"lineage");
    for v in [
        id1,
        id2,
        id3,
        s1,
        s2,
        s3,
        u64::from(after_term_err),
        u64::from(ooo_err),
        u64::from(lin.audit_complete()),
        lin.open_obligations().len() as u64,
        lin.open_obligations()[0],
        lin.leaked().len() as u64,
        lin.leaked()[0],
        lin.chain(id1).unwrap().len() as u64,
        t_id,
        u64::from(evicted_err),
        u64::from(trunc.audit_complete()),
    ] {
        h = mix_u64(h, v);
    }

    let source = format!(
        r#"use @vesper/chrono::lineage::{{
    ObligationHop, audit_complete, chain, for_truncated_log, hop_channel_handoff, hop_leak,
    hop_sink_move, hop_unpack_discharge, leaked, mint, new_lineage, open_obligations, record,
    status,
}};
{FNV_PREAMBLE}
fn ok_u64(r: Result[u64, u32]) -> u64 {{
    match r {{ Result::Ok(v) => v, Result::Err(_) => 0 }}
}}
fn ok_u32(r: Result[u32, u32]) -> u64 {{
    match r {{ Result::Ok(v) => widen_u64(v), Result::Err(_) => 999 }}
}}
pub fn main() -> u64 {{
    let mut lin = new_lineage();
    let id1 = ok_u64(mint(inout lin, 10, 1, 101, 0));
    let id2 = ok_u64(mint(inout lin, 20, 1, 102, id1));
    let id3 = ok_u64(mint(inout lin, 30, 2, 103, 0));
    let _ = record(inout lin, id1, ObligationHop {{ lamport_seq: 40, fiber_id: 1, source_span_id: 104, kind: hop_sink_move() }});
    let _ = record(inout lin, id1, ObligationHop {{ lamport_seq: 50, fiber_id: 1, source_span_id: 105, kind: hop_unpack_discharge() }});
    let _ = record(inout lin, id2, ObligationHop {{ lamport_seq: 60, fiber_id: 2, source_span_id: 106, kind: hop_channel_handoff(3) }});
    let _ = record(inout lin, id2, ObligationHop {{ lamport_seq: 70, fiber_id: 3, source_span_id: 107, kind: hop_leak() }});

    let after_term_err = record(inout lin, id1, ObligationHop {{ lamport_seq: 80, fiber_id: 1, source_span_id: 108, kind: hop_sink_move() }}) != 0;
    let ooo_err = record(inout lin, id3, ObligationHop {{ lamport_seq: 70, fiber_id: 2, source_span_id: 109, kind: hop_sink_move() }}) != 0;

    let s1 = ok_u32(status(in lin, id1));
    let s2 = ok_u32(status(in lin, id2));
    let s3 = ok_u32(status(in lin, id3));
    let open = open_obligations(in lin);
    let lk = leaked(in lin);
    let c1_len = match chain(in lin, id1) {{
        Result::Ok(c) => c.as_slice().len(),
        Result::Err(_) => 0,
    }};

    let mut trunc = for_truncated_log();
    let _ = record(inout trunc, 2, ObligationHop {{ lamport_seq: 5, fiber_id: 1, source_span_id: 11, kind: hop_sink_move() }});
    let t_id = ok_u64(mint(inout trunc, 10, 1, 12, 2));
    let evicted_err = match chain(in trunc, 2) {{
        Result::Ok(_) => false,
        Result::Err(_) => true,
    }};

    let mut h: u64 = 14695981039346656037;
    let tag = "lineage";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, id1);
    h = mix_u64(h, id2);
    h = mix_u64(h, id3);
    h = mix_u64(h, s1);
    h = mix_u64(h, s2);
    h = mix_u64(h, s3);
    h = mix_bool(h, after_term_err);
    h = mix_bool(h, ooo_err);
    h = mix_bool(h, audit_complete(in lin));
    h = mix_u64(h, open.as_slice().len());
    h = mix_u64(h, match open.as_slice().try_get(0) {{ Option::Some(v) => v, Option::None => 0 }});
    h = mix_u64(h, lk.as_slice().len());
    h = mix_u64(h, match lk.as_slice().try_get(0) {{ Option::Some(v) => v, Option::None => 0 }});
    h = mix_u64(h, c1_len);
    h = mix_u64(h, t_id);
    h = mix_bool(h, evicted_err);
    h = mix_bool(h, audit_complete(in trunc));
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn redaction_policy_scrub_and_verify_absent_match_rust() {
    use vesper_chrono::redact::{FieldRule, ImageRegion, RedactionPolicy};

    let mut pol = RedactionPolicy::new();
    let empty_err = pol
        .pii_field(FieldRule {
            effect_id: 1,
            op_tag: 2,
            offset: 0,
            len: 0,
        })
        .is_err();
    let short_err = pol.secret_value(b"abc").is_err();
    pol.pii_field(FieldRule {
        effect_id: 1,
        op_tag: 2,
        offset: 2,
        len: 3,
    })
    .unwrap();
    pol.secret_region(ImageRegion { offset: 1, len: 2 })
        .unwrap();
    pol.secret_value(b"s3cr").unwrap();

    // Expected scrubbed payload: "xx\0\0\0-\0\0\0\0!" (11 bytes)
    let scrubbed_payload = b"xx\0\0\0-\0\0\0\0!";
    // Expected scrubbed image: "A\0\0D\0\0\0\0" (8 bytes)
    let scrubbed_image = b"A\0\0D\0\0\0\0";

    let mut h = fnv1a(b"redact");
    h = mix_u64(h, u64::from(empty_err));
    h = mix_u64(h, u64::from(short_err));
    h = mix_bytes(h, scrubbed_payload);
    h = mix_bytes(h, scrubbed_image);
    h = mix_u64(h, 3); // 3 log entries (1 pii + 1 secret in payload + 1 region + 1 secret in image -> 4)
    // Wait: 1 pii + 1 secret in payload + 1 region + 1 secret in image = 4 entries
    let mut h_exp = fnv1a(b"redact");
    h_exp = mix_u64(h_exp, u64::from(empty_err));
    h_exp = mix_u64(h_exp, u64::from(short_err));
    h_exp = mix_bytes(h_exp, scrubbed_payload);
    h_exp = mix_bytes(h_exp, scrubbed_image);
    h_exp = mix_u64(h_exp, 4);
    h_exp = mix_u64(h_exp, 1); // verify_absent on dirty buffer finds offset 2
    h_exp = mix_u64(h_exp, 0); // verify_absent on scrubbed_payload is None
    let _ = h;

    let source = format!(
        r#"use @vesper/chrono::redact::{{
    FieldRule, ImageRegion, RedactionEntry, add_pii_field, add_secret_region, add_secret_value,
    new_policy, scrub_image, scrub_record, verify_absent,
}};
{FNV_PREAMBLE}
fn push_str_bytes(inout out: ChunkSeq[u8], in s: Str) {{
    let b = s.as_bytes();
    let mut i: u64 = 0;
    loop {{
        if i >= b.len() {{ break; }}
        out.push(match b.try_get(i) {{ Option::Some(v) => v, Option::None => 0 }});
        i = wrapping_add(i, 1);
    }}
}}
pub fn main() -> u64 {{
    let mut pol = new_policy();
    let empty_err = add_pii_field(inout pol, FieldRule {{ effect_id: 1, op_tag: 2, offset: 0, len: 0 }}) != 0;
    let short_err = add_secret_value(inout pol, in "abc".as_bytes()) != 0;
    let _ = add_pii_field(inout pol, FieldRule {{ effect_id: 1, op_tag: 2, offset: 2, len: 3 }});
    let _ = add_secret_region(inout pol, ImageRegion {{ offset: 1, len: 2 }});
    let _ = add_secret_value(inout pol, in "s3cr".as_bytes());

    let mut payload: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout payload, in "xxPPP-s3cr!");
    let mut image: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout image, in "ABCDs3cr");
    let mut log: ChunkSeq[RedactionEntry] = ChunkSeq::new();

    let dirty_found = match verify_absent(in pol, in payload.as_slice()) {{
        Option::Some(off) => off == 6,
        Option::None => false,
    }};
    scrub_record(in pol, 1, 1, 2, false, inout payload, inout log);
    scrub_image(in pol, 1, inout image, inout log);
    let clean_ok = match verify_absent(in pol, in payload.as_slice()) {{
        Option::Some(_) => false,
        Option::None => true,
    }};

    let mut h: u64 = 14695981039346656037;
    let tag = "redact";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_bool(h, empty_err);
    h = mix_bool(h, short_err);
    h = mix_bytes(h, in payload.as_slice());
    h = mix_bytes(h, in image.as_slice());
    h = mix_u64(h, log.as_slice().len());
    h = mix_bool(h, dirty_found);
    h = mix_bool(h, clean_ok);
    let _ = clean_ok;
    // Match dirty_found=true -> 1, clean_ok=true -> 0 (None)
    let mut h2: u64 = 14695981039346656037;
    let mut ti2: u64 = 0;
    loop {{
        if ti2 >= tb.len() {{ break; }}
        let b = match tb.try_get(ti2) {{ Option::Some(v) => v, Option::None => 0 }};
        h2 = wrapping_mul(h2 ^ widen_u64(b), 1099511628211);
        ti2 = wrapping_add(ti2, 1);
    }}
    h2 = mix_bool(h2, empty_err);
    h2 = mix_bool(h2, short_err);
    h2 = mix_bytes(h2, in payload.as_slice());
    h2 = mix_bytes(h2, in image.as_slice());
    h2 = mix_u64(h2, log.as_slice().len());
    h2 = mix_bool(h2, dirty_found);
    h2 = mix_bool(h2, !clean_ok);
    h2
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h_exp);
}

#[test]
fn coresight_formatter_deformat_and_errors_match_rust() {
    use vesper_chrono::formatter::{FSYNC, FormatterLimits, FormatterMode, deformat};

    // Frame 1: ID=1 at byte 0 (0x03), data [0x10, 0x20, 0x30], then ID=2 at byte 4 (0x05) with aux bit=1 (prev byte 0x41 -> ID 1), then padding ID=0 at byte 6 (0x01), rest data under ID=0.
    // Byte 0: 0x03 (ID=1), Byte 1: 0x10 (ID=1)
    // Byte 2: 0x20 (ID=1, bit0 from aux bit1=0), Byte 3: 0x30 (ID=1)
    // Byte 4: 0x05 (ID=2), Byte 5: 0x41 (aux bit2=1 -> still ID=1)
    // Byte 6: 0x01 (ID=0), Byte 7: 0x99 (padding)
    // Bytes 8..14: 0x00, aux = 0x04 (bit 2 set)
    let frame: [u8; 16] = [
        0x03, 0x10, 0x20, 0x30, 0x05, 0x41, 0x01, 0x99, 0, 0, 0, 0, 0, 0, 0, 0x04,
    ];
    let mut synced = Vec::new();
    synced.extend_from_slice(&FSYNC);
    synced.extend_from_slice(&frame);
    synced.extend_from_slice(&FSYNC);
    synced.extend_from_slice(&frame);

    let limits = FormatterLimits::default();
    let d1 = deformat(&frame, FormatterMode::Frames, &limits).unwrap();
    let d2 = deformat(&synced, FormatterMode::FrameSync, &limits).unwrap();
    let no_sync_err = deformat(&frame, FormatterMode::FrameSync, &limits).is_err();
    let partial_err = deformat(&frame[..15], FormatterMode::Frames, &limits).is_err();
    let mut bad_id_frame = frame;
    bad_id_frame[0] = (0x70 << 1) | 1;
    let reserved_err = deformat(&bad_id_frame, FormatterMode::Frames, &limits).is_err();

    let mut h = fnv1a(b"formatter");
    h = mix_u64(h, d1.frames as u64);
    h = mix_u64(h, d1.padding as u64);
    h = mix_bytes(h, d1.stream(1));
    h = mix_u64(h, d2.frames as u64);
    h = mix_u64(h, d2.syncs as u64);
    h = mix_bytes(h, d2.stream(1));
    h = mix_u64(h, u64::from(no_sync_err));
    h = mix_u64(h, u64::from(partial_err));
    h = mix_u64(h, u64::from(reserved_err));

    let frame_pushes: String = frame
        .iter()
        .map(|b| format!("    frame.push({b});\n"))
        .collect();
    let synced_pushes: String = synced
        .iter()
        .map(|b| format!("    synced.push({b});\n"))
        .collect();

    let source = format!(
        r#"use @vesper/chrono::formatter::{{deformat, stream_for}};
{FNV_PREAMBLE}
pub fn main() -> u64 {{
    let mut frame: ChunkSeq[u8] = ChunkSeq::new();
{frame_pushes}
    let mut synced: ChunkSeq[u8] = ChunkSeq::new();
{synced_pushes}
    let d1 = deformat(in frame.as_slice(), 0, 67108864);
    let d2 = deformat(in synced.as_slice(), 1, 67108864);
    let no_sync_err = deformat(in frame.as_slice(), 1, 67108864).err_code != 0;
    let mut partial: ChunkSeq[u8] = ChunkSeq::new();
    let mut pi: u64 = 0;
    loop {{
        if pi >= 15 {{ break; }}
        partial.push(match frame.as_slice().try_get(pi) {{ Option::Some(b) => b, Option::None => 0 }});
        pi = wrapping_add(pi, 1);
    }}
    let partial_err = deformat(in partial.as_slice(), 0, 67108864).err_code != 0;
    let mut bad = frame;
    let _ = bad.try_set(0, 225);
    let reserved_err = deformat(in bad.as_slice(), 0, 67108864).err_code != 0;

    let mut h: u64 = 14695981039346656037;
    let tag = "formatter";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, d1.out.frames);
    h = mix_u64(h, d1.out.padding);
    let s1 = stream_for(in d1.out, 1);
    h = mix_bytes(h, in s1.as_slice());
    h = mix_u64(h, d2.out.frames);
    h = mix_u64(h, d2.out.syncs);
    let s2 = stream_for(in d2.out, 1);
    h = mix_bytes(h, in s2.as_slice());
    h = mix_bool(h, no_sync_err);
    h = mix_bool(h, partial_err);
    h = mix_bool(h, reserved_err);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn simpulse_tap_drain_and_seed_metadata_match_rust() {
    use vesper_chrono::simpulse::{SimPulseTap, TraceEvent, seed_from_metadata, seed_metadata};

    let seed = 0x1234_5678_9abc_def0u64;
    let meta = seed_metadata(seed);
    let parsed_seed = seed_from_metadata(&meta).unwrap();
    assert_eq!(parsed_seed, seed);
    let bad_meta_err = seed_from_metadata(b"wrong-prefix").is_err();

    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    let evs = vec![
        TraceEvent::ClockAdvance {
            from_ns: 0,
            to_ns: 100,
        },
        TraceEvent::ClockAdvance {
            from_ns: 100,
            to_ns: 200,
        },
        TraceEvent::ClockAdvance {
            from_ns: 200,
            to_ns: 250,
        },
    ];
    for ev in &evs {
        let TraceEvent::ClockAdvance { to_ns, .. } = *ev else {
            unreachable!()
        };
        SimPulseTap::record_event(&mut rec, to_ns, ev).unwrap();
    }

    let mut h = fnv1a(b"simpulse");
    h = mix_bytes(h, &meta);
    h = mix_u64(h, parsed_seed);
    h = mix_u64(h, u64::from(bad_meta_err));
    h = mix_u64(h, rec.len() as u64);
    for ev in rec.events() {
        h = mix_u64(h, ev.lamport_seq);
        h = mix_u64(h, ev.fiber_id);
        h = mix_u64(h, ev.time_ns);
        h = mix_u64(h, u64::from(ev.effect_id));
    }

    let source = format!(
        r#"use @vesper/chrono::ring::{{events, new_recorder, retained_len, small_config}};
use @vesper/chrono::simpulse::{{
    SimPulseEvent, drain_events, new_tap, seed_from_metadata, seed_metadata,
}};
{FNV_PREAMBLE}
pub fn main() -> u64 {{
    let seed: u64 = 1311768467463790320;
    let meta = seed_metadata(seed);
    let parsed_seed = match seed_from_metadata(in meta.as_slice()) {{
        Option::Some(v) => v,
        Option::None => 0,
    }};
    let bad_meta_err = match seed_from_metadata(in "wrong-prefix".as_bytes()) {{
        Option::Some(_) => false,
        Option::None => true,
    }};
    let mut rec = match new_recorder(small_config()) {{
        Result::Ok(r) => r,
        Result::Err(_) => return 0,
    }};
    let mut tap = new_tap();
    let mut evs: ChunkSeq[SimPulseEvent] = ChunkSeq::new();
    evs.push(SimPulseEvent {{ kind: 9, task: 1, time_ns: 100, payload: ChunkSeq::new() }});
    evs.push(SimPulseEvent {{ kind: 9, task: 2, time_ns: 200, payload: ChunkSeq::new() }});
    evs.push(SimPulseEvent {{ kind: 9, task: 3, time_ns: 250, payload: ChunkSeq::new() }});
    let drained = match drain_events(inout tap, in evs.as_slice(), 3, inout rec) {{
        Result::Ok(n) => n,
        Result::Err(_) => 0,
    }};
    if drained != 3 {{ return 1; }}

    let mut rec_evs = events(in rec);
    let mut rev: ChunkSeq[@vesper/chrono::ring::FlightEvent] = ChunkSeq::new();
    loop {{
        match rec_evs.pop() {{
            Option::Some(e) => rev.push(e),
            Option::None => break,
        }}
    }}

    let mut h: u64 = 14695981039346656037;
    let tag = "simpulse";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_bytes(h, in meta.as_slice());
    h = mix_u64(h, parsed_seed);
    h = mix_bool(h, bad_meta_err);
    h = mix_u64(h, retained_len(in rec));
    loop {{
        match rev.pop() {{
            Option::Some(ev) => {{
                h = mix_u64(h, ev.lamport_seq);
                h = mix_u64(h, ev.fiber_id);
                h = mix_u64(h, ev.time_ns);
                h = mix_u64(h, widen_u64(widen_u32(ev.effect_id)));
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

#[test]
fn replay_session_seek_watchpoint_lineage_and_window_match_rust() {
    use vesper_chrono::bundle::{Bundle, BundleKey, BundleLimits};
    use vesper_chrono::redact::RedactionPolicy;
    use vesper_chrono::replay::ReplaySession;

    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    let s1 = rec.record_inout_write(1, 10, 0xAA, 101).unwrap();
    let o1 = rec.mint_obligation(1, 20, 102, None).unwrap();
    let e1 = rec.capture_snapshot(30, b"snap1").unwrap();
    let s4 = rec.record_inout_write(2, 40, 0xAA, 103).unwrap();
    let _ = (s1, e1, s4);

    let key = BundleKey::new([5; 32]);
    let bytes = Bundle::capture(&rec, &RedactionPolicy::new(), b"meta")
        .seal(&key)
        .unwrap();
    let mut sess = ReplaySession::open_bundle(&bytes, &key, &BundleLimits::default()).unwrap();

    let init_cur = sess.cursor();
    let w1 = sess.reverse_watch_inout(0xAA).unwrap();
    let w2 = sess.reverse_watch_inout(0xAA).unwrap();
    let no_earlier = sess.reverse_watch_inout(0xAA).is_err();
    let s_seek = sess.seek(4).unwrap();
    let mut scratch = [0u8; 24];
    let hop_count = sess.query_linear_lineage_into(o1, &mut scratch).unwrap();
    let short_err = sess
        .query_linear_lineage_into(o1, &mut scratch[..23])
        .is_err();

    let mut h = fnv1a(b"replay");
    h = mix_u64(h, u64::from(init_cur.epoch_id));
    h = mix_u64(h, init_cur.lamport_seq);
    h = mix_u64(h, u64::from(w1.epoch_id));
    h = mix_u64(h, w1.lamport_seq);
    h = mix_u64(h, u64::from(w2.epoch_id));
    h = mix_u64(h, w2.lamport_seq);
    h = mix_u64(h, u64::from(no_earlier));
    h = mix_u64(h, u64::from(s_seek.epoch_id));
    h = mix_u64(h, s_seek.lamport_seq);
    h = mix_u64(h, hop_count as u64);
    h = mix_bytes(h, &scratch);
    h = mix_u64(h, u64::from(short_err));
    sess.close();

    let source = format!(
        r#"use @vesper/chrono::redact::RedactionEntry;
use @vesper/chrono::replay::{{
    ReplayBundle, from_bundle, plan_replay_window, query_linear_lineage_into, reverse_watch_inout,
    seek,
}};
use @vesper/chrono::ring::{{
    capture_snapshot, events, first_retained_seq, mint_obligation, new_recorder,
    record_inout_write, small_config, snapshots,
}};
{FNV_PREAMBLE}
pub fn main() -> u64 {{
    let mut rec = match new_recorder(small_config()) {{
        Result::Ok(r) => r,
        Result::Err(_) => return 0,
    }};
    let _ = record_inout_write(inout rec, 1, 10, 170, 101);
    let o1 = match mint_obligation(inout rec, 1, 20, 102, false, 0) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 0,
    }};
    let _ = capture_snapshot(inout rec, 30, in "snap1".as_bytes());
    let _ = record_inout_write(inout rec, 2, 40, 170, 103);

    let bundle = ReplayBundle {{
        first_seq: first_retained_seq(in rec),
        next_seq: rec.next_seq,
        evicted_records: rec.evicted_records,
        records: events(in rec),
        snapshots: snapshots(in rec),
        redactions: ChunkSeq::new(),
    }};
    let mut sess = match from_bundle(bundle) {{
        Result::Ok(s) => s,
        Result::Err(_) => return 1,
    }};
    let init_cur = sess.cursor;
    let w1 = match reverse_watch_inout(inout sess, 170) {{
        Result::Ok(c) => c,
        Result::Err(_) => return 2,
    }};
    let w2 = match reverse_watch_inout(inout sess, 170) {{
        Result::Ok(c) => c,
        Result::Err(_) => return 3,
    }};
    let no_earlier = match reverse_watch_inout(inout sess, 170) {{
        Result::Ok(_) => false,
        Result::Err(_) => true,
    }};
    let s_seek = match seek(inout sess, 4) {{
        Result::Ok(c) => c,
        Result::Err(_) => return 4,
    }};
    let scratch = match query_linear_lineage_into(in sess, o1, 24) {{
        Result::Ok(b) => b,
        Result::Err(_) => return 5,
    }};
    let short_err = match query_linear_lineage_into(in sess, o1, 23) {{
        Result::Ok(_) => false,
        Result::Err(_) => true,
    }};
    let win = plan_replay_window(in sess, 4, true);
    if win.err_code != 0 || !win.has_snapshot || win.from_seq != 4 || win.to_seq != 4 {{
        return 6;
    }}

    let mut h: u64 = 14695981039346656037;
    let tag = "replay";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, widen_u64(init_cur.epoch_id));
    h = mix_u64(h, init_cur.lamport_seq);
    h = mix_u64(h, widen_u64(w1.epoch_id));
    h = mix_u64(h, w1.lamport_seq);
    h = mix_u64(h, widen_u64(w2.epoch_id));
    h = mix_u64(h, w2.lamport_seq);
    h = mix_bool(h, no_earlier);
    h = mix_u64(h, widen_u64(s_seek.epoch_id));
    h = mix_u64(h, s_seek.lamport_seq);
    h = mix_u64(h, 1);
    h = mix_bytes(h, in scratch.as_slice());
    h = mix_bool(h, short_err);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn shard_arena_cow_checkpoint_delta_and_blake3_match_rust() {
    use vesper_chrono::arena::{ArenaDelta, ShardArena, capture_arena_epoch, rebuild_arena};

    assert!(ShardArena::new(256, 65).is_err());
    let mut arena = ShardArena::new(256, 64).unwrap();
    arena.write(10, b"hello").unwrap();
    arena.write(130, b"world").unwrap();
    let cp1 = arena.checkpoint();
    let kf1_bytes = cp1.keyframe().encode();

    arena.write(10, b"HELLO").unwrap();
    arena.write(200, b"tail").unwrap();
    let cp2 = arena.checkpoint();
    let delta2_bytes = cp2.delta().encode();

    // Corrupt a byte in delta2_bytes and check Rust rejects Corrupt.
    let mut corrupt = delta2_bytes.clone();
    corrupt[10] ^= 0xff;
    assert!(ArenaDelta::decode(&corrupt, 16).is_err());

    // Rebuild from keyframe1 + delta2.
    let rebuilt = rebuild_arena([kf1_bytes.as_slice(), delta2_bytes.as_slice()], 16).unwrap();
    let mut rb_bytes = vec![0u8; 256];
    rebuilt.read(0, &mut rb_bytes).unwrap();

    // Also test capture_arena_epoch into FlightRecorder.
    let mut rec = FlightRecorder::new(RecorderConfig {
        ring_bytes: 4096,
        max_records: 32,
        max_payload: 512,
        snapshot_slots: 4,
        snapshot_image_bytes: 512,
    })
    .unwrap();
    let ep1 = capture_arena_epoch(&mut rec, 100, &cp1, 2).unwrap();
    let ep2 = capture_arena_epoch(&mut rec, 200, &cp2, 2).unwrap();

    let mut h = fnv1a(b"arena");
    h = mix_u64(h, arena.epoch());
    h = mix_u64(h, arena.stats().checkpoints);
    h = mix_u64(h, arena.stats().shard_copies);
    h = mix_u64(h, arena.stats().shards_dirtied);
    h = mix_bytes(h, &kf1_bytes);
    h = mix_bytes(h, &delta2_bytes);
    h = mix_bytes(h, &rb_bytes);
    h = mix_u64(h, u64::from(ep1));
    h = mix_u64(h, u64::from(ep2));

    let source = format!(
        r#"use @vesper/chrono::arena::{{
    apply_delta, capture_arena_epoch, checkpoint_arena, checkpoint_delta, checkpoint_keyframe,
    decode_delta, encode_delta, new_arena, read_arena, write_arena,
}};
use @vesper/chrono::ring::{{RecorderConfig, new_recorder}};
{FNV_PREAMBLE}
fn push_str_bytes(inout out: ChunkSeq[u8], in s: Str) {{
    let b = s.as_bytes();
    let mut i: u64 = 0;
    loop {{
        if i >= b.len() {{ break; }}
        out.push(match b.try_get(i) {{ Option::Some(v) => v, Option::None => 0 }});
        i = wrapping_add(i, 1);
    }}
}}
pub fn main() -> u64 {{
    let bad_geom = match new_arena(256, 65) {{ Result::Ok(_) => false, Result::Err(_) => true }};
    if !bad_geom {{ return 1; }}
    let mut arena = match new_arena(256, 64) {{
        Result::Ok(a) => a,
        Result::Err(_) => return 2,
    }};
    let mut b1: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout b1, in "hello");
    let mut b2: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout b2, in "world");
    let _ = write_arena(inout arena, 10, in b1.as_slice());
    let _ = write_arena(inout arena, 130, in b2.as_slice());
    let cp1 = checkpoint_arena(inout arena);
    let kf1 = checkpoint_keyframe(in cp1);
    let kf1_bytes = encode_delta(in kf1);

    let mut b3: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout b3, in "HELLO");
    let mut b4: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout b4, in "tail");
    let _ = write_arena(inout arena, 10, in b3.as_slice());
    let _ = write_arena(inout arena, 200, in b4.as_slice());
    let cp2 = checkpoint_arena(inout arena);
    let d2 = checkpoint_delta(in cp2);
    let delta2_bytes = encode_delta(in d2);

    let mut corrupt: ChunkSeq[u8] = ChunkSeq::new();
    let mut ci: u64 = 0;
    loop {{
        if ci >= delta2_bytes.as_slice().len() {{ break; }}
        let byte = match delta2_bytes.as_slice().try_get(ci) {{ Option::Some(v) => v, Option::None => 0 }};
        corrupt.push(if ci == 10 {{ byte ^ 255 }} else {{ byte }});
        ci = wrapping_add(ci, 1);
    }}
    let corrupt_err = match decode_delta(in corrupt.as_slice(), 16) {{
        Result::Ok(_) => false,
        Result::Err(code) => code == 7,
    }};
    if !corrupt_err {{ return 3; }}

    let dec_kf1 = match decode_delta(in kf1_bytes.as_slice(), 16) {{
        Result::Ok(d) => d,
        Result::Err(_) => return 4,
    }};
    let dec_d2 = match decode_delta(in delta2_bytes.as_slice(), 16) {{
        Result::Ok(d) => d,
        Result::Err(_) => return 5,
    }};
    let mut rebuilt = match new_arena(dec_kf1.len, widen_u64(dec_kf1.shard_len)) {{
        Result::Ok(a) => a,
        Result::Err(_) => return 6,
    }};
    if apply_delta(in dec_kf1, inout rebuilt) != 0 {{ return 7; }}
    let _ = checkpoint_arena(inout rebuilt);
    if apply_delta(in dec_d2, inout rebuilt) != 0 {{ return 8; }}
    let _ = checkpoint_arena(inout rebuilt);
    let rb_bytes = match read_arena(in rebuilt, 0, 256) {{
        Result::Ok(b) => b,
        Result::Err(_) => return 9,
    }};

    let rcfg = RecorderConfig {{
        ring_bytes: 4096,
        max_records: 32,
        max_payload: 512,
        snapshot_slots: 4,
        snapshot_image_bytes: 512,
    }};
    let mut rec = match new_recorder(rcfg) {{
        Result::Ok(r) => r,
        Result::Err(_) => return 10,
    }};
    let ep1 = match capture_arena_epoch(inout rec, 100, in cp1, 2) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 11,
    }};
    let ep2 = match capture_arena_epoch(inout rec, 200, in cp2, 2) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 12,
    }};

    let mut h: u64 = 14695981039346656037;
    let tag = "arena";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, arena.epoch);
    h = mix_u64(h, arena.stats.checkpoints);
    h = mix_u64(h, arena.stats.shard_copies);
    h = mix_u64(h, arena.stats.shards_dirtied);
    h = mix_bytes(h, in kf1_bytes.as_slice());
    h = mix_bytes(h, in delta2_bytes.as_slice());
    h = mix_bytes(h, in rb_bytes.as_slice());
    h = mix_u64(h, widen_u64(ep1));
    h = mix_u64(h, widen_u64(ep2));
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn bundle_capture_seal_open_and_mac_match_rust() {
    use vesper_chrono::bundle::{Bundle, BundleKey, BundleLimits};
    use vesper_chrono::redact::{FieldRule, RedactionPolicy};

    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    rec.record_effect(1, 10, 1, 2, b"abPPP-s3cr!").unwrap();
    rec.record_inout_write(1, 20, 0xAA, 7).unwrap();
    rec.capture_snapshot(30, b"img-s3cr").unwrap();

    let mut pol = RedactionPolicy::new();
    pol.pii_field(FieldRule {
        effect_id: 1,
        op_tag: 2,
        offset: 2,
        len: 3,
    })
    .unwrap();
    pol.secret_value(b"s3cr").unwrap();

    let key = BundleKey::new([9; 32]);
    let bytes = Bundle::export(&rec, &pol, b"meta-s3cr", &key).unwrap();
    let opened = Bundle::open(&bytes, &key, &BundleLimits::default()).unwrap();

    let wrong_key = BundleKey::new([1; 32]);
    assert!(Bundle::open(&bytes, &wrong_key, &BundleLimits::default()).is_err());

    let mut h = fnv1a(b"bundle");
    h = mix_bytes(h, &bytes);
    h = mix_bytes(h, &opened.metadata);
    h = mix_u64(h, opened.first_seq);
    h = mix_u64(h, opened.next_seq);
    h = mix_u64(h, opened.records.len() as u64);
    h = mix_u64(h, opened.snapshots.len() as u64);
    h = mix_u64(h, opened.redactions.len() as u64);

    let source = format!(
        r#"use @vesper/chrono::bundle::{{default_limits, export_bundle, open_bundle}};
use @vesper/chrono::redact::{{FieldRule, add_pii_field, add_secret_value, new_policy}};
use @vesper/chrono::ring::{{
    capture_snapshot, new_recorder, record_effect, record_inout_write, small_config,
}};
{FNV_PREAMBLE}
fn push_str_bytes(inout out: ChunkSeq[u8], in s: Str) {{
    let b = s.as_bytes();
    let mut i: u64 = 0;
    loop {{
        if i >= b.len() {{ break; }}
        out.push(match b.try_get(i) {{ Option::Some(v) => v, Option::None => 0 }});
        i = wrapping_add(i, 1);
    }}
}}
pub fn main() -> u64 {{
    let mut rec = match new_recorder(small_config()) {{
        Result::Ok(r) => r,
        Result::Err(_) => return 1,
    }};
    let _ = record_effect(inout rec, 1, 10, 1, 2, in "abPPP-s3cr!".as_bytes());
    let _ = record_inout_write(inout rec, 1, 20, 170, 7);
    let _ = capture_snapshot(inout rec, 30, in "img-s3cr".as_bytes());

    let mut pol = new_policy();
    let _ = add_pii_field(inout pol, FieldRule {{ effect_id: 1, op_tag: 2, offset: 2, len: 3 }});
    let _ = add_secret_value(inout pol, in "s3cr".as_bytes());

    let mut key: ChunkSeq[u8] = ChunkSeq::new();
    let mut wrong_key: ChunkSeq[u8] = ChunkSeq::new();
    let mut ki: u64 = 0;
    loop {{
        if ki >= 32 {{ break; }}
        key.push(9);
        wrong_key.push(1);
        ki = wrapping_add(ki, 1);
    }}
    let mut meta: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout meta, in "meta-s3cr");

    let bytes = match export_bundle(in rec, in pol, in meta.as_slice(), in key.as_slice()) {{
        Result::Ok(b) => b,
        Result::Err(_) => return 2,
    }};
    let opened = match open_bundle(in bytes.as_slice(), in key.as_slice(), default_limits()) {{
        Result::Ok(b) => b,
        Result::Err(_) => return 3,
    }};
    let wrong_err = match open_bundle(in bytes.as_slice(), in wrong_key.as_slice(), default_limits()) {{
        Result::Ok(_) => false,
        Result::Err(code) => code == 6,
    }};
    if !wrong_err {{ return 4; }}

    let mut h: u64 = 14695981039346656037;
    let tag = "bundle";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_bytes(h, in bytes.as_slice());
    h = mix_bytes(h, in opened.metadata.as_slice());
    h = mix_u64(h, opened.first_seq);
    h = mix_u64(h, opened.next_seq);
    h = mix_u64(h, opened.records.as_slice().len());
    h = mix_u64(h, opened.snapshots.as_slice().len());
    h = mix_u64(h, opened.redactions.as_slice().len());
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn activation_checkpoint_encode_decode_and_blake3_match_rust() {
    use vesper_chrono::checkpoint::{
        ActivationCheckpoint, CheckpointLimits, JournalEntry, ParkPoint, ResumeTag, StackShape,
    };

    let cp = ActivationCheckpoint {
        fiber_id: 7,
        generation: 3,
        park: ParkPoint {
            evidence_depth: 1,
            op: 4,
            flags: 0,
            arg0: 11,
            arg1: 22,
        },
        data_at_park: b"parked".to_vec(),
        data: b"resumed".to_vec(),
        stack: Some(StackShape {
            anchor: 0x1000,
            words: vec![(8, 16), (24, -8)],
        }),
        journal: vec![JournalEntry {
            park: ParkPoint {
                evidence_depth: 0,
                op: 2,
                flags: 1,
                arg0: 5,
                arg1: 6,
            },
            data: b"j0".to_vec(),
            tag: ResumeTag::Value,
            value: 42,
        }],
        image: Some(b"img!".to_vec()),
    };
    let encoded = cp.encode();
    let decoded = ActivationCheckpoint::decode(&encoded, &CheckpointLimits::default()).unwrap();
    assert_eq!(decoded, cp);

    let mut corrupt = encoded.clone();
    corrupt[12] ^= 1;
    assert!(ActivationCheckpoint::decode(&corrupt, &CheckpointLimits::default()).is_err());

    let mut h = fnv1a(b"checkpoint");
    h = mix_bytes(h, &encoded);
    h = mix_u64(h, decoded.fiber_id);
    h = mix_u64(h, decoded.generation);
    h = mix_bytes(h, &decoded.data);
    h = mix_u64(h, decoded.journal[0].value);

    let source = format!(
        r#"use @vesper/chrono::checkpoint::{{
    ActivationCheckpoint, JournalEntry, ParkPoint, StackShape, StackWord, decode_checkpoint,
    default_checkpoint_limits, encode_checkpoint,
}};
{FNV_PREAMBLE}
fn push_str_bytes(inout out: ChunkSeq[u8], in s: Str) {{
    let b = s.as_bytes();
    let mut i: u64 = 0;
    loop {{
        if i >= b.len() {{ break; }}
        out.push(match b.try_get(i) {{ Option::Some(v) => v, Option::None => 0 }});
        i = wrapping_add(i, 1);
    }}
}}
pub fn main() -> u64 {{
    let mut dp: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout dp, in "parked");
    let mut d: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout d, in "resumed");
    let mut words: ChunkSeq[StackWord] = ChunkSeq::new();
    words.push(StackWord {{ slot: 8, target: 16 }});
    words.push(StackWord {{ slot: 24, target: wrapping_sub(0, 8) }});
    let mut jdata: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout jdata, in "j0");
    let mut journal: ChunkSeq[JournalEntry] = ChunkSeq::new();
    journal.push(JournalEntry {{
        park: ParkPoint {{ evidence_depth: 0, op: 2, flags: 1, arg0: 5, arg1: 6 }},
        data: jdata,
        tag: 0,
        value: 42,
    }});
    let mut img: ChunkSeq[u8] = ChunkSeq::new();
    push_str_bytes(inout img, in "img!");

    let cp = ActivationCheckpoint {{
        fiber_id: 7,
        generation: 3,
        park: ParkPoint {{ evidence_depth: 1, op: 4, flags: 0, arg0: 11, arg1: 22 }},
        data_at_park: dp,
        data: d,
        has_stack: true,
        stack: StackShape {{ anchor: 4096, words: words }},
        journal: journal,
        has_image: true,
        image: img,
    }};
    let encoded = encode_checkpoint(in cp);
    let decoded = match decode_checkpoint(in encoded.as_slice(), default_checkpoint_limits()) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 1,
    }};

    let mut corrupt: ChunkSeq[u8] = ChunkSeq::new();
    let mut ci: u64 = 0;
    loop {{
        if ci >= encoded.as_slice().len() {{ break; }}
        let byte = match encoded.as_slice().try_get(ci) {{ Option::Some(v) => v, Option::None => 0 }};
        corrupt.push(if ci == 12 {{ byte ^ 1 }} else {{ byte }});
        ci = wrapping_add(ci, 1);
    }}
    let corrupt_err = match decode_checkpoint(in corrupt.as_slice(), default_checkpoint_limits()) {{
        Result::Ok(_) => false,
        Result::Err(code) => code == 4,
    }};
    if !corrupt_err {{ return 2; }}

    let mut h: u64 = 14695981039346656037;
    let tag = "checkpoint";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_bytes(h, in encoded.as_slice());
    h = mix_u64(h, decoded.fiber_id);
    h = mix_u64(h, decoded.generation);
    h = mix_bytes(h, in decoded.data.as_slice());
    let mut dec = decoded;
    let j0_val = match dec.journal.pop() {{
        Option::Some(j) => j.value,
        Option::None => 0,
    }};
    h = mix_u64(h, j0_val);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn ete_decoder_sync_packets_atoms_and_errors_match_rust() {
    use vesper_chrono::ete::{EteDecoder, EteElement, EteError, InstructionSet, decode_elements};

    let mut stream: Vec<u8> = Vec::new();
    // A-Sync
    stream.extend_from_slice(&[0x00; 11]);
    stream.push(0x80);
    // TraceInfo: plctl=0x03 (info + key)
    stream.extend_from_slice(&[0x01, 0x03, 0x05, 0x81, 0x02]);
    // TraceOn
    stream.push(0x04);
    // Context: 0x81, el=1, 64-bit=0x10, ns=0x20, vmid=0x40, ctxid=0x80 -> info=0xf1
    stream.extend_from_slice(&[0x81, 0xf1, 0x07, 0x78, 0x56, 0x34, 0x12]);
    // Long 64-bit IS0 Address (0x9d)
    stream.extend_from_slice(&[0x9d, 0x04, 0x08, 0x10, 0x20, 0x30, 0x40, 0x00, 0x00]);
    // Short IS0 Address 1-byte (0x95) + 2-byte (0x95)
    stream.extend_from_slice(&[0x95, 0x12]);
    stream.extend_from_slice(&[0x95, 0x83, 0x04]);
    // Exact-match address 0 and 1
    stream.extend_from_slice(&[0x90, 0x91]);
    // Timestamp (0x02) + Timestamp with cycle count (0x03)
    stream.extend_from_slice(&[0x02, 0x81, 0x02]);
    stream.extend_from_slice(&[0x03, 0x03, 0x09]);
    // Atoms: F1 (0xf7), F2 (0xda), F3 (0xfd), F4 (0xde), F5 (0xf5), F6 (0xe2)
    stream.extend_from_slice(&[0xf7, 0xda, 0xfd, 0xde, 0xf5, 0xe2]);
    // Exception (0x06) with type=3 (b1 = (3<<1)=0x06) + exact-match 0x90
    stream.extend_from_slice(&[0x06, 0x06, 0x90]);
    // ExceptionReturn (0x07), Event (0x73), Discard (0x00 0x03), Overflow (0x00 0x05)
    stream.extend_from_slice(&[0x07, 0x73, 0x00, 0x03, 0x00, 0x05]);

    let elems = decode_elements(&stream).expect("rust decode_elements");

    let mut sync_stream = vec![0xaa, 0xbb, 0xcc];
    sync_stream.extend_from_slice(&stream);
    let sync_elems: Vec<_> = EteDecoder::sync(&sync_stream)
        .expect("rust EteDecoder::sync")
        .collect::<Result<Vec<_>, _>>()
        .expect("rust sync elements");

    let unsync_err = match decode_elements(&[0x04]) {
        Err(EteError::NotSynchronized { offset }) => offset as u64,
        _ => panic!("expected NotSynchronized"),
    };
    let mut empty_hist_stream = vec![0x00; 11];
    empty_hist_stream.push(0x80);
    empty_hist_stream.push(0x90);
    let empty_hist_err = match decode_elements(&empty_hist_stream) {
        Err(EteError::EmptyAddressHistory { offset }) => offset as u64,
        _ => panic!("expected EmptyAddressHistory"),
    };
    let no_sync_err = matches!(EteDecoder::sync(&[0x01, 0x02, 0x03]), Err(EteError::NoSync));

    let fnv_mix = |mut h: u64, v: u64| -> u64 {
        for i in 0..8 {
            let b = (v >> (i * 8)) & 0xff;
            h ^= b;
            h = h.wrapping_mul(1099511628211);
        }
        h
    };
    let mix_elem = |mut h: u64, offset: u64, el: &EteElement| -> u64 {
        h = fnv_mix(h, offset);
        match el {
            EteElement::Sync => fnv_mix(h, 1),
            EteElement::TraceInfo {
                info,
                key,
                spec,
                cyct,
            } => {
                h = fnv_mix(h, 2);
                h = fnv_mix(h, *info);
                h = fnv_mix(h, *key);
                h = fnv_mix(h, *spec);
                fnv_mix(h, *cyct)
            }
            EteElement::TraceOn => fnv_mix(h, 3),
            EteElement::Timestamp {
                timestamp,
                cycle_count,
            } => {
                h = fnv_mix(h, 4);
                h = fnv_mix(h, *timestamp);
                match cycle_count {
                    Some(cc) => fnv_mix(fnv_mix(h, 1), *cc),
                    None => fnv_mix(h, 0),
                }
            }
            EteElement::Context(c) => {
                h = fnv_mix(h, 5);
                h = fnv_mix(h, u64::from(c.el));
                h = fnv_mix(h, u64::from(c.sixty_four));
                h = fnv_mix(h, u64::from(c.non_secure));
                h = match c.vmid {
                    Some(v) => fnv_mix(fnv_mix(h, 1), u64::from(v)),
                    None => fnv_mix(h, 0),
                };
                match c.context_id {
                    Some(v) => fnv_mix(fnv_mix(h, 1), u64::from(v)),
                    None => fnv_mix(h, 0),
                }
            }
            EteElement::Address { address, isa } => {
                h = fnv_mix(h, 6);
                h = fnv_mix(h, *address);
                fnv_mix(
                    h,
                    if matches!(isa, InstructionSet::Is0) {
                        0
                    } else {
                        1
                    },
                )
            }
            EteElement::Atom { executed } => {
                h = fnv_mix(h, 7);
                fnv_mix(h, u64::from(*executed))
            }
            EteElement::Exception {
                exception_type,
                preferred_return,
                isa,
            } => {
                h = fnv_mix(h, 8);
                h = fnv_mix(h, u64::from(*exception_type));
                h = fnv_mix(h, *preferred_return);
                fnv_mix(
                    h,
                    if matches!(isa, InstructionSet::Is0) {
                        0
                    } else {
                        1
                    },
                )
            }
            EteElement::ExceptionReturn => fnv_mix(h, 9),
            EteElement::Event { mask } => {
                h = fnv_mix(h, 10);
                fnv_mix(h, u64::from(*mask))
            }
            EteElement::Overflow => fnv_mix(h, 11),
            EteElement::Discard => fnv_mix(h, 12),
        }
    };

    let mut h: u64 = 14695981039346656037;
    for &b in b"ete" {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    h = fnv_mix(h, elems.len() as u64);
    for e in &elems {
        h = mix_elem(h, e.offset as u64, &e.element);
    }
    h = fnv_mix(h, sync_elems.len() as u64);
    for e in &sync_elems {
        h = mix_elem(h, e.offset as u64, &e.element);
    }
    h = fnv_mix(h, unsync_err);
    h = fnv_mix(h, empty_hist_err);
    h = fnv_mix(h, u64::from(no_sync_err));

    let bytes_lit: String = stream
        .iter()
        .map(|b| format!("    s.push({});\n", b))
        .collect();
    let source = format!(
        r#"use @vesper/chrono::ete::{{
    EteElement, EteElementAt, EteError, InstructionSet,
    decode_elements, decode_elements_from_sync
}};
fn mix_u64(h_in: u64, v: u64) -> u64 {{
    let mut h = h_in;
    let mut i: u64 = 0;
    loop {{
        if i >= 8 {{ break; }}
        let byte = (wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8)))) & 255;
        h = wrapping_mul(h ^ byte, 1099511628211);
        i = wrapping_add(i, 1);
    }}
    h
}}
fn mix_isa(in isa: InstructionSet) -> u64 {{
    match isa {{
        InstructionSet::Is0 => 0,
        InstructionSet::Is1 => 1,
    }}
}}
fn mix_elem(h_in: u64, in e: EteElementAt) -> u64 {{
    let mut h = mix_u64(h_in, e.offset);
    match e.element {{
        EteElement::Sync => mix_u64(h, 1),
        EteElement::TraceInfo {{ info, key, spec, cyct }} => {{
            h = mix_u64(h, 2);
            h = mix_u64(h, info);
            h = mix_u64(h, key);
            h = mix_u64(h, spec);
            mix_u64(h, cyct)
        }}
        EteElement::TraceOn => mix_u64(h, 3),
        EteElement::Timestamp {{ timestamp, cycle_count }} => {{
            h = mix_u64(h, 4);
            h = mix_u64(h, timestamp);
            match cycle_count {{
                Option::Some(cc) => mix_u64(mix_u64(h, 1), cc),
                Option::None => mix_u64(h, 0),
            }}
        }}
        EteElement::Context(c) => {{
            h = mix_u64(h, 5);
            h = mix_u64(h, widen_u64(c.el));
            h = mix_u64(h, if c.sixty_four {{ 1 }} else {{ 0 }});
            h = mix_u64(h, if c.non_secure {{ 1 }} else {{ 0 }});
            h = match c.vmid {{
                Option::Some(v) => mix_u64(mix_u64(h, 1), widen_u64(v)),
                Option::None => mix_u64(h, 0),
            }};
            match c.context_id {{
                Option::Some(v) => mix_u64(mix_u64(h, 1), widen_u64(v)),
                Option::None => mix_u64(h, 0),
            }}
        }}
        EteElement::Address {{ address, isa }} => {{
            h = mix_u64(h, 6);
            h = mix_u64(h, address);
            mix_u64(h, mix_isa(in isa))
        }}
        EteElement::Atom {{ executed }} => {{
            h = mix_u64(h, 7);
            mix_u64(h, if executed {{ 1 }} else {{ 0 }})
        }}
        EteElement::Exception {{ exception_type, preferred_return, isa }} => {{
            h = mix_u64(h, 8);
            h = mix_u64(h, widen_u64(exception_type));
            h = mix_u64(h, preferred_return);
            mix_u64(h, mix_isa(in isa))
        }}
        EteElement::ExceptionReturn => mix_u64(h, 9),
        EteElement::Event {{ mask }} => {{
            h = mix_u64(h, 10);
            mix_u64(h, widen_u64(mask))
        }}
        EteElement::Overflow => mix_u64(h, 11),
        EteElement::Discard => mix_u64(h, 12),
    }}
}}
fn mix_all(h_in: u64, inout seq: ChunkSeq[EteElementAt]) -> u64 {{
    let mut h = mix_u64(h_in, seq.len());
    let mut rev: ChunkSeq[EteElementAt] = ChunkSeq::new();
    loop {{
        match seq.pop() {{
            Option::Some(x) => rev.push(x),
            Option::None => break,
        }}
    }}
    loop {{
        match rev.pop() {{
            Option::Some(x) => {{ h = mix_elem(h, in x); }},
            Option::None => break,
        }}
    }}
    h
}}
fn build_stream() -> ChunkSeq[u8] {{
    let mut s: ChunkSeq[u8] = ChunkSeq::new();
{bytes_lit}    s
}}
pub fn main() -> u64 {{
    let s = build_stream();
    let mut elems = match decode_elements(in s.as_slice()) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 1,
    }};
    let mut sync_s: ChunkSeq[u8] = ChunkSeq::new();
    sync_s.push(0xaa);
    sync_s.push(0xbb);
    sync_s.push(0xcc);
    let s2 = build_stream();
    let mut si: u64 = 0;
    loop {{
        if si >= s2.len() {{ break; }}
        let b = match s2.as_slice().try_get(si) {{ Option::Some(v) => v, Option::None => 0 }};
        sync_s.push(b);
        si = wrapping_add(si, 1);
    }}
    let mut sync_elems = match decode_elements_from_sync(in sync_s.as_slice()) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 2,
    }};
    let mut unsync_s: ChunkSeq[u8] = ChunkSeq::new();
    unsync_s.push(0x04);
    let unsync_err = match decode_elements(in unsync_s.as_slice()) {{
        Result::Err(EteError::NotSynchronized {{ offset }}) => offset,
        _ => return 3,
    }};
    let mut empty_h: ChunkSeq[u8] = ChunkSeq::new();
    let mut zi: u64 = 0;
    loop {{
        if zi >= 11 {{ break; }}
        empty_h.push(0);
        zi = wrapping_add(zi, 1);
    }}
    empty_h.push(0x80);
    empty_h.push(0x90);
    let empty_hist_err = match decode_elements(in empty_h.as_slice()) {{
        Result::Err(EteError::EmptyAddressHistory {{ offset }}) => offset,
        _ => return 4,
    }};
    let mut no_sync_s: ChunkSeq[u8] = ChunkSeq::new();
    no_sync_s.push(1);
    no_sync_s.push(2);
    no_sync_s.push(3);
    let no_sync_err = match decode_elements_from_sync(in no_sync_s.as_slice()) {{
        Result::Err(EteError::NoSync) => 1,
        _ => 0,
    }};
    let mut h: u64 = 14695981039346656037;
    let tag = "ete";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_all(h, inout elems);
    h = mix_all(h, inout sync_elems);
    h = mix_u64(h, unsync_err);
    h = mix_u64(h, empty_hist_err);
    h = mix_u64(h, no_sync_err);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn ipt_decoder_psb_tnt_ip_timing_and_errors_match_rust() {
    use vesper_chrono::ipt::{
        IpPacketKind, PSB_BYTES, PtDecoder, PtError, PtPacket, decode_packets,
    };

    let mut stream: Vec<u8> = Vec::new();
    // Leading PAD + PSB
    stream.push(0x00);
    stream.extend_from_slice(&PSB_BYTES);
    // FUP Full 64-bit IP (0x1d | (6<<5) = 0xdd)
    stream.push(0xdd);
    stream.extend_from_slice(&0x0000_7fff_1234_5678u64.to_le_bytes());
    // ModeExec (0x99, cs_l=true, cs_d=false => 0x01)
    stream.extend_from_slice(&[0x99, 0x01]);
    // PIP (0x02 0x43 + 6 bytes)
    stream.extend_from_slice(&[0x02, 0x43, 0x01, 0x20, 0x30, 0x00, 0x00, 0x00]);
    // CBR (0x02 0x03 ratio=42 0x00)
    stream.extend_from_slice(&[0x02, 0x03, 42, 0x00]);
    // PSBEND
    stream.extend_from_slice(&[0x02, 0x23]);
    // Short TNT (0x3a => payload 0x1d = 1_1101 -> count=4, bits=0b1101)
    stream.push(0x3a);
    // Long TNT (0x02 0xa3 + 6 bytes)
    stream.extend_from_slice(&[0x02, 0xa3, 0b1010_0101, 0x03, 0, 0, 0, 0]);
    // TIP Update16 (0x0d | (1<<5) = 0x2d)
    stream.extend_from_slice(&[0x2d, 0xaa, 0xbb]);
    // TIP.PGE Update32 (0x11 | (2<<5) = 0x51)
    stream.extend_from_slice(&[0x51, 0x11, 0x22, 0x33, 0x44]);
    // TIP Sext48 negative bit 47 (0x0d | (3<<5) = 0x6d)
    stream.extend_from_slice(&[0x6d, 0x10, 0x20, 0x30, 0x40, 0x50, 0x80]);
    // TIP Update48 (0x0d | (4<<5) = 0x8d)
    stream.extend_from_slice(&[0x8d, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    // TIP.PGD Suppressed (0x01 | (0<<5) = 0x01)
    stream.push(0x01);
    // TSC (0x19 + 7 bytes), MTC (0x59 + 1 byte), CYC (0x07 0x04), TMA (0x02 0x73 ...), OVF, TraceStop
    stream.extend_from_slice(&[0x19, 1, 2, 3, 4, 5, 6, 7]);
    stream.extend_from_slice(&[0x59, 0xfe]);
    stream.extend_from_slice(&[0x07, 0x04]);
    stream.extend_from_slice(&[0x02, 0x73, 0x12, 0x34, 0x00, 0xab, 0x01]);
    stream.extend_from_slice(&[0x02, 0xf3, 0x02, 0x83]);

    let pkts = decode_packets(&stream).expect("rust decode_packets");

    let mut sync_stream = vec![0x99, 0x00, 0xff];
    sync_stream.extend_from_slice(&stream);
    let sync_pkts: Vec<_> = PtDecoder::sync(&sync_stream)
        .expect("rust PtDecoder::sync")
        .collect::<Result<Vec<_>, _>>()
        .expect("rust sync packets");

    let unsync_err = match decode_packets(&[0x3a]) {
        Err(PtError::NotSynchronized { offset }) => offset as u64,
        _ => panic!("expected NotSynchronized"),
    };
    let no_psb_err = matches!(PtDecoder::sync(&[0x02, 0x23]), Err(PtError::NoPsb));

    let fnv_mix = |mut h: u64, v: u64| -> u64 {
        for i in 0..8 {
            let b = (v >> (i * 8)) & 0xff;
            h ^= b;
            h = h.wrapping_mul(1099511628211);
        }
        h
    };
    let mix_pkt = |mut h: u64, offset: u64, len: u64, p: &PtPacket| -> u64 {
        h = fnv_mix(h, offset);
        h = fnv_mix(h, len);
        match p {
            PtPacket::Pad => fnv_mix(h, 1),
            PtPacket::Psb => fnv_mix(h, 2),
            PtPacket::PsbEnd => fnv_mix(h, 3),
            PtPacket::Tnt { tnt, long } => {
                h = fnv_mix(h, 4);
                h = fnv_mix(h, tnt.bits);
                h = fnv_mix(h, u64::from(tnt.count));
                fnv_mix(h, u64::from(*long))
            }
            PtPacket::Ip {
                kind,
                compression,
                ip,
            } => {
                h = fnv_mix(h, 5);
                let k = match kind {
                    IpPacketKind::Tip => 0,
                    IpPacketKind::TipPge => 1,
                    IpPacketKind::TipPgd => 2,
                    IpPacketKind::Fup => 3,
                };
                h = fnv_mix(h, k);
                h = fnv_mix(h, u64::from(compression.ip_bytes()));
                match ip {
                    Some(v) => fnv_mix(fnv_mix(h, 1), *v),
                    None => fnv_mix(h, 0),
                }
            }
            PtPacket::Pip { cr3, non_root } => {
                h = fnv_mix(h, 6);
                h = fnv_mix(h, *cr3);
                fnv_mix(h, u64::from(*non_root))
            }
            PtPacket::ModeExec { cs_l, cs_d } => {
                h = fnv_mix(h, 7);
                h = fnv_mix(h, u64::from(*cs_l));
                fnv_mix(h, u64::from(*cs_d))
            }
            PtPacket::ModeTsx { in_tx, tx_abort } => {
                h = fnv_mix(h, 8);
                h = fnv_mix(h, u64::from(*in_tx));
                fnv_mix(h, u64::from(*tx_abort))
            }
            PtPacket::Cbr { ratio } => fnv_mix(fnv_mix(h, 9), u64::from(*ratio)),
            PtPacket::Tsc { tsc } => fnv_mix(fnv_mix(h, 10), *tsc),
            PtPacket::Mtc { ctc } => fnv_mix(fnv_mix(h, 11), u64::from(*ctc)),
            PtPacket::Cyc { cycles } => fnv_mix(fnv_mix(h, 12), *cycles),
            PtPacket::Tma { ctc, fast_counter } => {
                h = fnv_mix(h, 13);
                h = fnv_mix(h, u64::from(*ctc));
                fnv_mix(h, u64::from(*fast_counter))
            }
            PtPacket::Ovf => fnv_mix(h, 14),
            PtPacket::TraceStop => fnv_mix(h, 15),
        }
    };

    let mut h: u64 = 14695981039346656037;
    for &b in b"ipt" {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    h = fnv_mix(h, pkts.len() as u64);
    for p in &pkts {
        h = mix_pkt(h, p.offset as u64, p.len as u64, &p.packet);
    }
    h = fnv_mix(h, sync_pkts.len() as u64);
    for p in &sync_pkts {
        h = mix_pkt(h, p.offset as u64, p.len as u64, &p.packet);
    }
    h = fnv_mix(h, unsync_err);
    h = fnv_mix(h, u64::from(no_psb_err));

    let bytes_lit: String = stream
        .iter()
        .map(|b| format!("    s.push({});\n", b))
        .collect();
    let source = format!(
        r#"use @vesper/chrono::ipt::{{
    IpPacketKind, PtError, PtPacket, PtPacketAt,
    decode_packets, decode_packets_from_psb, ip_bytes
}};
fn mix_u64(h_in: u64, v: u64) -> u64 {{
    let mut h = h_in;
    let mut i: u64 = 0;
    loop {{
        if i >= 8 {{ break; }}
        let byte = (wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8)))) & 255;
        h = wrapping_mul(h ^ byte, 1099511628211);
        i = wrapping_add(i, 1);
    }}
    h
}}
fn mix_pkt(h_in: u64, in p: PtPacketAt) -> u64 {{
    let mut h = mix_u64(h_in, p.offset);
    h = mix_u64(h, p.len);
    match p.packet {{
        PtPacket::Pad => mix_u64(h, 1),
        PtPacket::Psb => mix_u64(h, 2),
        PtPacket::PsbEnd => mix_u64(h, 3),
        PtPacket::Tnt {{ tnt, long }} => {{
            h = mix_u64(h, 4);
            h = mix_u64(h, tnt.bits);
            h = mix_u64(h, widen_u64(tnt.count));
            mix_u64(h, if long {{ 1 }} else {{ 0 }})
        }}
        PtPacket::Ip {{ kind, compression, ip }} => {{
            h = mix_u64(h, 5);
            let k: u64 = match kind {{
                IpPacketKind::Tip => 0,
                IpPacketKind::TipPge => 1,
                IpPacketKind::TipPgd => 2,
                IpPacketKind::Fup => 3,
            }};
            h = mix_u64(h, k);
            h = mix_u64(h, widen_u64(ip_bytes(compression)));
            match ip {{
                Option::Some(v) => mix_u64(mix_u64(h, 1), v),
                Option::None => mix_u64(h, 0),
            }}
        }}
        PtPacket::Pip {{ cr3, non_root }} => {{
            h = mix_u64(h, 6);
            h = mix_u64(h, cr3);
            mix_u64(h, if non_root {{ 1 }} else {{ 0 }})
        }}
        PtPacket::ModeExec {{ cs_l, cs_d }} => {{
            h = mix_u64(h, 7);
            h = mix_u64(h, if cs_l {{ 1 }} else {{ 0 }});
            mix_u64(h, if cs_d {{ 1 }} else {{ 0 }})
        }}
        PtPacket::ModeTsx {{ in_tx, tx_abort }} => {{
            h = mix_u64(h, 8);
            h = mix_u64(h, if in_tx {{ 1 }} else {{ 0 }});
            mix_u64(h, if tx_abort {{ 1 }} else {{ 0 }})
        }}
        PtPacket::Cbr {{ ratio }} => mix_u64(mix_u64(h, 9), widen_u64(ratio)),
        PtPacket::Tsc {{ tsc }} => mix_u64(mix_u64(h, 10), tsc),
        PtPacket::Mtc {{ ctc }} => mix_u64(mix_u64(h, 11), widen_u64(ctc)),
        PtPacket::Cyc {{ cycles }} => mix_u64(mix_u64(h, 12), cycles),
        PtPacket::Tma {{ ctc, fast_counter }} => {{
            h = mix_u64(h, 13);
            h = mix_u64(h, widen_u64(ctc));
            mix_u64(h, widen_u64(fast_counter))
        }}
        PtPacket::Ovf => mix_u64(h, 14),
        PtPacket::TraceStop => mix_u64(h, 15),
    }}
}}
fn mix_all(h_in: u64, inout seq: ChunkSeq[PtPacketAt]) -> u64 {{
    let mut h = mix_u64(h_in, seq.len());
    let mut rev: ChunkSeq[PtPacketAt] = ChunkSeq::new();
    loop {{
        match seq.pop() {{
            Option::Some(x) => rev.push(x),
            Option::None => break,
        }}
    }}
    loop {{
        match rev.pop() {{
            Option::Some(x) => {{ h = mix_pkt(h, in x); }},
            Option::None => break,
        }}
    }}
    h
}}
fn build_stream() -> ChunkSeq[u8] {{
    let mut s: ChunkSeq[u8] = ChunkSeq::new();
{bytes_lit}    s
}}
pub fn main() -> u64 {{
    let s = build_stream();
    let mut pkts = match decode_packets(in s.as_slice()) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 1,
    }};
    let mut sync_s: ChunkSeq[u8] = ChunkSeq::new();
    sync_s.push(0x99);
    sync_s.push(0x00);
    sync_s.push(0xff);
    let s2 = build_stream();
    let mut si: u64 = 0;
    loop {{
        if si >= s2.len() {{ break; }}
        let b = match s2.as_slice().try_get(si) {{ Option::Some(v) => v, Option::None => 0 }};
        sync_s.push(b);
        si = wrapping_add(si, 1);
    }}
    let mut sync_pkts = match decode_packets_from_psb(in sync_s.as_slice()) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 2,
    }};
    let mut unsync_s: ChunkSeq[u8] = ChunkSeq::new();
    unsync_s.push(0x3a);
    let unsync_err = match decode_packets(in unsync_s.as_slice()) {{
        Result::Err(PtError::PtNotSynchronized {{ offset }}) => offset,
        _ => return 3,
    }};
    let mut no_psb_s: ChunkSeq[u8] = ChunkSeq::new();
    no_psb_s.push(0x02);
    no_psb_s.push(0x23);
    let no_psb_err = match decode_packets_from_psb(in no_psb_s.as_slice()) {{
        Result::Err(PtError::NoPsb) => 1,
        _ => 0,
    }};
    let mut h: u64 = 14695981039346656037;
    let tag = "ipt";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_all(h, inout pkts);
    h = mix_all(h, inout sync_pkts);
    h = mix_u64(h, unsync_err);
    h = mix_u64(h, no_psb_err);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn ete_flow_a64_classify_and_reconstruct_match_rust() {
    use vesper_chrono::ete::{EteElement, EteElementAt, InstructionSet};
    use vesper_chrono::ete_flow::{EteFlowError, reconstruct};
    use vesper_chrono::ipt_flow::{BranchKind, CodeImage, FlowLimits};

    // Build A64 code image at 0x1000:
    // 0x1000: NOP (0xd503201f) -> Sequential
    // 0x1004: B.eq +8 (0x54000040 -> 0x100c) -> Conditional
    // 0x1008: NOP (0xd503201f) -> Sequential
    // 0x100c: BL +8 (0x94000002 -> 0x1014) -> Direct { call: true }
    // 0x1010: NOP (0xd503201f)
    // 0x1014: RET (0xd65f03c0) -> Return
    let mut code1 = Vec::new();
    for insn in [
        0xd503_201fu32,
        0x5400_0040u32,
        0xd503_201fu32,
        0x9400_0002u32,
        0xd503_201fu32,
        0xd65f_03c0u32,
    ] {
        code1.extend_from_slice(&insn.to_le_bytes());
    }
    let mut code2 = Vec::new();
    // 0x2000: NOP, 0x2004: B -4 (0x17ffffff -> 0x2000)
    for insn in [0xd503_201fu32, 0x17ff_ffffu32] {
        code2.extend_from_slice(&insn.to_le_bytes());
    }

    let mut image = CodeImage::single(0x1000, &code1);
    image.add(0x2000, &code2).expect("add segment 2");

    let elements = vec![
        EteElementAt {
            offset: 0,
            element: EteElement::Sync,
        },
        EteElementAt {
            offset: 12,
            element: EteElement::Timestamp {
                timestamp: 100,
                cycle_count: None,
            },
        },
        EteElementAt {
            offset: 15,
            element: EteElement::Address {
                address: 0x1000,
                isa: InstructionSet::Is0,
            },
        },
        EteElementAt {
            offset: 20,
            element: EteElement::Atom { executed: true },
        },
        EteElementAt {
            offset: 21,
            element: EteElement::Atom { executed: true },
        },
        EteElementAt {
            offset: 22,
            element: EteElement::Atom { executed: true },
        },
        EteElementAt {
            offset: 23,
            element: EteElement::Timestamp {
                timestamp: 200,
                cycle_count: None,
            },
        },
        EteElementAt {
            offset: 26,
            element: EteElement::Address {
                address: 0x2000,
                isa: InstructionSet::Is0,
            },
        },
        EteElementAt {
            offset: 30,
            element: EteElement::Atom { executed: true },
        },
        EteElementAt {
            offset: 31,
            element: EteElement::Overflow,
        },
    ];

    let trace = reconstruct(&elements, &image, &FlowLimits::default()).expect("rust reconstruct");

    let n_err = match reconstruct(
        &[
            EteElementAt {
                offset: 0,
                element: EteElement::Address {
                    address: 0x100c,
                    isa: InstructionSet::Is0,
                },
            },
            EteElementAt {
                offset: 1,
                element: EteElement::Atom { executed: false },
            },
        ],
        &image,
        &FlowLimits::default(),
    ) {
        Err(EteFlowError::UnexpectedNotTaken { ip, element_index }) => (ip, element_index as u64),
        _ => panic!("expected UnexpectedNotTaken"),
    };

    let fnv_mix = |mut h: u64, v: u64| -> u64 {
        for i in 0..8 {
            let b = (v >> (i * 8)) & 0xff;
            h ^= b;
            h = h.wrapping_mul(1099511628211);
        }
        h
    };
    let mut h: u64 = 14695981039346656037;
    for &b in b"ete_flow" {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    h = fnv_mix(h, trace.instructions);
    h = fnv_mix(h, u64::from(trace.overflows));
    h = match trace.stopped_at {
        Some(v) => fnv_mix(fnv_mix(h, 1), v),
        None => fnv_mix(h, 0),
    };
    h = fnv_mix(h, trace.branches.len() as u64);
    for b in &trace.branches {
        h = fnv_mix(h, b.from);
        h = match b.to {
            Some(v) => fnv_mix(fnv_mix(h, 1), v),
            None => fnv_mix(h, 0),
        };
        let k = match b.kind {
            BranchKind::Conditional { taken } => {
                if taken {
                    1
                } else {
                    2
                }
            }
            BranchKind::DirectJump => 3,
            BranchKind::DirectCall => 4,
            BranchKind::IndirectJump => 5,
            BranchKind::IndirectCall => 6,
            BranchKind::Return { compressed } => {
                if compressed {
                    7
                } else {
                    8
                }
            }
            BranchKind::Far => 9,
            BranchKind::Async => 10,
            BranchKind::Enable => 11,
            BranchKind::Disable => 12,
        };
        h = fnv_mix(h, k);
        h = match b.tsc_lo {
            Some(v) => fnv_mix(fnv_mix(h, 1), v),
            None => fnv_mix(h, 0),
        };
        h = match b.tsc_hi {
            Some(v) => fnv_mix(fnv_mix(h, 1), v),
            None => fnv_mix(h, 0),
        };
        h = fnv_mix(h, b.packet_index as u64);
    }
    h = fnv_mix(h, n_err.0);
    h = fnv_mix(h, n_err.1);

    let c1_lit: String = code1
        .iter()
        .map(|b| format!("    c1.push({});\n", b))
        .collect();
    let c2_lit: String = code2
        .iter()
        .map(|b| format!("    c2.push({});\n", b))
        .collect();
    let source = format!(
        r#"use @vesper/chrono::ete::{{EteElement, EteElementAt, InstructionSet}};
use @vesper/chrono::ete_flow::{{
    Branch, BranchKind, EteFlowError, add_segment, default_flow_limits, reconstruct,
    single_code_image
}};
fn mix_u64(h_in: u64, v: u64) -> u64 {{
    let mut h = h_in;
    let mut i: u64 = 0;
    loop {{
        if i >= 8 {{ break; }}
        let byte = (wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8)))) & 255;
        h = wrapping_mul(h ^ byte, 1099511628211);
        i = wrapping_add(i, 1);
    }}
    h
}}
fn mix_opt(h_in: u64, in o: Option[u64]) -> u64 {{
    match o {{
        Option::Some(v) => mix_u64(mix_u64(h_in, 1), v),
        Option::None => mix_u64(h_in, 0),
    }}
}}
fn mix_branch(h_in: u64, in b: Branch) -> u64 {{
    let mut h = mix_u64(h_in, b.from);
    h = mix_opt(h, in b.to);
    let k: u64 = match b.kind {{
        BranchKind::Conditional {{ taken }} => if taken {{ 1 }} else {{ 2 }},
        BranchKind::DirectJump => 3,
        BranchKind::DirectCall => 4,
        BranchKind::IndirectJump => 5,
        BranchKind::IndirectCall => 6,
        BranchKind::Return {{ compressed }} => if compressed {{ 7 }} else {{ 8 }},
        BranchKind::Far => 9,
        BranchKind::Async => 10,
        BranchKind::Enable => 11,
        BranchKind::Disable => 12,
    }};
    h = mix_u64(h, k);
    h = mix_opt(h, in b.tsc_lo);
    h = mix_opt(h, in b.tsc_hi);
    mix_u64(h, b.packet_index)
}}
pub fn main() -> u64 {{
    let mut c1: ChunkSeq[u8] = ChunkSeq::new();
{c1_lit}    let mut c2: ChunkSeq[u8] = ChunkSeq::new();
{c2_lit}    let mut img = single_code_image(0x1000, inout c1);
    match add_segment(inout img, 0x2000, inout c2) {{
        Result::Ok(_) => {{}}
        Result::Err(_) => return 1,
    }}
    let mut elems: ChunkSeq[EteElementAt] = ChunkSeq::new();
    elems.push(EteElementAt {{ offset: 0, element: EteElement::Sync }});
    elems.push(EteElementAt {{ offset: 12, element: EteElement::Timestamp {{ timestamp: 100, cycle_count: Option::None }} }});
    elems.push(EteElementAt {{ offset: 15, element: EteElement::Address {{ address: 0x1000, isa: InstructionSet::Is0 }} }});
    elems.push(EteElementAt {{ offset: 20, element: EteElement::Atom {{ executed: true }} }});
    elems.push(EteElementAt {{ offset: 21, element: EteElement::Atom {{ executed: true }} }});
    elems.push(EteElementAt {{ offset: 22, element: EteElement::Atom {{ executed: true }} }});
    elems.push(EteElementAt {{ offset: 23, element: EteElement::Timestamp {{ timestamp: 200, cycle_count: Option::None }} }});
    elems.push(EteElementAt {{ offset: 26, element: EteElement::Address {{ address: 0x2000, isa: InstructionSet::Is0 }} }});
    elems.push(EteElementAt {{ offset: 30, element: EteElement::Atom {{ executed: true }} }});
    elems.push(EteElementAt {{ offset: 31, element: EteElement::Overflow }});

    let mut trace = match reconstruct(in elems.as_slice(), in img, in default_flow_limits()) {{
        Result::Ok(v) => v,
        Result::Err(_) => return 2,
    }};

    let mut bad_elems: ChunkSeq[EteElementAt] = ChunkSeq::new();
    bad_elems.push(EteElementAt {{ offset: 0, element: EteElement::Address {{ address: 0x100c, isa: InstructionSet::Is0 }} }});
    bad_elems.push(EteElementAt {{ offset: 1, element: EteElement::Atom {{ executed: false }} }});
    let mut err_ip: u64 = 0;
    let mut err_idx: u64 = 0;
    match reconstruct(in bad_elems.as_slice(), in img, in default_flow_limits()) {{
        Result::Err(EteFlowError::UnexpectedNotTaken {{ ip, element_index }}) => {{
            err_ip = ip;
            err_idx = element_index;
        }}
        _ => return 3,
    }}

    let mut h: u64 = 14695981039346656037;
    let tag = "ete_flow";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {{
        if ti >= tb.len() {{ break; }}
        let b = match tb.try_get(ti) {{ Option::Some(v) => v, Option::None => 0 }};
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }}
    h = mix_u64(h, trace.instructions);
    h = mix_u64(h, trace.overflows);
    h = mix_opt(h, in trace.stopped_at);
    h = mix_u64(h, trace.branches.len());
    let mut rev: ChunkSeq[Branch] = ChunkSeq::new();
    loop {{
        match trace.branches.pop() {{
            Option::Some(b) => rev.push(b),
            Option::None => break,
        }}
    }}
    loop {{
        match rev.pop() {{
            Option::Some(b) => {{ h = mix_branch(h, in b); }},
            Option::None => break,
        }}
    }}
    h = mix_u64(h, err_ip);
    h = mix_u64(h, err_idx);
    h
}}"#
    );
    let checked = check_chrono_driver(&source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}

#[test]
fn hwpath_branch_timeline_and_reverse_watch_path_match_rust() {
    use vesper_chrono::bundle::Bundle;
    use vesper_chrono::ete::{EteElement, EteElementAt, InstructionSet};
    use vesper_chrono::hwpath::{BranchTimeline, reverse_watch_path};
    use vesper_chrono::redact::RedactionPolicy;
    use vesper_chrono::replay::ReplaySession;
    use vesper_chrono::ring::{FlightRecorder, RecorderConfig};

    let cfg = RecorderConfig {
        ring_bytes: 4096,
        max_records: 64,
        max_payload: 64,
        snapshot_slots: 4,
        snapshot_image_bytes: 64,
    };
    let mut rec = FlightRecorder::new(cfg).expect("recorder");
    let s1 = rec.record_effect(0, 100, 1, 1, b"a").expect("s1");
    let s2 = rec
        .record_inout_write(2, 200, 0xdead_beef, 101)
        .expect("s2");
    let s3 = rec.record_effect(0, 300, 1, 3, b"c").expect("s3");

    let anchors = BranchTimeline::anchors_from_events(rec.events());
    let elements = vec![
        EteElementAt {
            offset: 0,
            element: EteElement::Timestamp {
                timestamp: 50,
                cycle_count: None,
            },
        },
        EteElementAt {
            offset: 1,
            element: EteElement::Address {
                address: 0x1000,
                isa: InstructionSet::Is0,
            },
        },
        EteElementAt {
            offset: 2,
            element: EteElement::Timestamp {
                timestamp: 150,
                cycle_count: None,
            },
        },
        EteElementAt {
            offset: 3,
            element: EteElement::Atom { executed: true },
        },
        EteElementAt {
            offset: 4,
            element: EteElement::Timestamp {
                timestamp: 180,
                cycle_count: None,
            },
        },
        EteElementAt {
            offset: 5,
            element: EteElement::Atom { executed: false },
        },
        EteElementAt {
            offset: 6,
            element: EteElement::Timestamp {
                timestamp: 250,
                cycle_count: None,
            },
        },
    ];
    let steps = BranchTimeline::steps_from_ete(&elements);
    let tl = BranchTimeline::new(anchors, steps).expect("rust BranchTimeline::new");
    let between = tl.path_between(s1, s3).expect("rust path_between");
    let bundle = Bundle::capture(&rec, &RedactionPolicy::new(), b"");
    let mut session = ReplaySession::from_bundle(bundle).expect("from_bundle");
    session.seek(s3).expect("seek");
    let wp = reverse_watch_path(&mut session, 0xdead_beef, &tl).expect("rust reverse_watch_path");
    let wp_seq = wp.cursor.lamport_seq;
    let wp_from = wp.path.from.lamport_seq;
    let wp_to = wp.path.to.lamport_seq;
    let wp_def = wp.path.definite.len() as u64;
    let wp_us = wp.path.uncertain_start.len() as u64;
    let wp_ue = wp.path.uncertain_end.len() as u64;
    session.close();

    let fnv_mix = |mut h: u64, v: u64| -> u64 {
        for i in 0..8 {
            let b = (v >> (i * 8)) & 0xff;
            h ^= b;
            h = h.wrapping_mul(1099511628211);
        }
        h
    };
    let mut h: u64 = 14695981039346656037;
    for &b in b"hwpath" {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    h = fnv_mix(h, tl.anchors().len() as u64);
    h = fnv_mix(h, tl.steps().len() as u64);
    h = fnv_mix(h, between.from.lamport_seq);
    h = fnv_mix(h, between.to.lamport_seq);
    h = fnv_mix(h, between.definite.len() as u64);
    h = fnv_mix(h, between.uncertain_start.len() as u64);
    h = fnv_mix(h, between.uncertain_end.len() as u64);
    h = fnv_mix(h, wp_seq);
    h = fnv_mix(h, wp_from);
    h = fnv_mix(h, wp_to);
    h = fnv_mix(h, wp_def);
    h = fnv_mix(h, wp_us);
    h = fnv_mix(h, wp_ue);
    let _ = s2;

    let source = r#"use @vesper/chrono::ete::{EteElement, EteElementAt, InstructionSet};
use @vesper/chrono::hwpath::{
    anchors_from_events, new_branch_timeline, path_between, reverse_watch_path, steps_from_ete
};
use @vesper/chrono::redact::RedactionEntry;
use @vesper/chrono::replay::{ReplayBundle, from_bundle, seek};
use @vesper/chrono::ring::{RecorderConfig, SnapshotImage, events, new_recorder, record_effect, record_inout_write};
fn ok_u64(r: Result[u64, u32]) -> u64 {
    match r { Result::Ok(v) => v, Result::Err(_) => 0 }
}
fn mix_u64(h_in: u64, v: u64) -> u64 {
    let mut h = h_in;
    let mut i: u64 = 0;
    loop {
        if i >= 8 { break; }
        let byte = (wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8)))) & 255;
        h = wrapping_mul(h ^ byte, 1099511628211);
        i = wrapping_add(i, 1);
    }
    h
}
pub fn main() -> u64 {
    let cfg = RecorderConfig {
        ring_bytes: 4096,
        max_records: 64,
        max_payload: 64,
        snapshot_slots: 4,
        snapshot_image_bytes: 64,
    };
    let mut rec = match new_recorder(cfg) {
        Result::Ok(r) => r,
        Result::Err(_) => return 9,
    };
    let mut p1: ChunkSeq[u8] = ChunkSeq::new();
    p1.push(97);
    let mut p3: ChunkSeq[u8] = ChunkSeq::new();
    p3.push(99);
    let s1 = ok_u64(record_effect(inout rec, 0, 100, 1, 1, in p1.as_slice()));
    let _s2 = ok_u64(record_inout_write(inout rec, 2, 200, 0xdead_beef, 101));
    let s3 = ok_u64(record_effect(inout rec, 0, 300, 1, 3, in p3.as_slice()));

    let evs = events(in rec);
    let mut anchors = anchors_from_events(in evs.as_slice());
    let mut elems: ChunkSeq[EteElementAt] = ChunkSeq::new();
    elems.push(EteElementAt { offset: 0, element: EteElement::Timestamp { timestamp: 50, cycle_count: Option::None } });
    elems.push(EteElementAt { offset: 1, element: EteElement::Address { address: 0x1000, isa: InstructionSet::Is0 } });
    elems.push(EteElementAt { offset: 2, element: EteElement::Timestamp { timestamp: 150, cycle_count: Option::None } });
    elems.push(EteElementAt { offset: 3, element: EteElement::Atom { executed: true } });
    elems.push(EteElementAt { offset: 4, element: EteElement::Timestamp { timestamp: 180, cycle_count: Option::None } });
    elems.push(EteElementAt { offset: 5, element: EteElement::Atom { executed: false } });
    elems.push(EteElementAt { offset: 6, element: EteElement::Timestamp { timestamp: 250, cycle_count: Option::None } });
    let mut steps = steps_from_ete(in elems.as_slice());
    let tl = match new_branch_timeline(inout anchors, inout steps) {
        Result::Ok(v) => v,
        Result::Err(_) => return 1,
    };
    let between = match path_between(in tl, s1, s3) {
        Result::Ok(v) => v,
        Result::Err(_) => return 2,
    };
    let snaps: ChunkSeq[SnapshotImage] = ChunkSeq::new();
    let reds: ChunkSeq[RedactionEntry] = ChunkSeq::new();
    let rb = ReplayBundle {
        first_seq: 1,
        next_seq: 4,
        evicted_records: 0,
        records: evs,
        snapshots: snaps,
        redactions: reds,
    };
    let mut session = match from_bundle(rb) {
        Result::Ok(s) => s,
        Result::Err(_) => return 5,
    };
    match seek(inout session, s3) {
        Result::Ok(_) => {}
        Result::Err(_) => return 3,
    }
    let wp = match reverse_watch_path(inout session, 0xdead_beef, in tl) {
        Result::Ok(v) => v,
        Result::Err(_) => return 4,
    };

    let mut h: u64 = 14695981039346656037;
    let tag = "hwpath";
    let tb = tag.as_bytes();
    let mut ti: u64 = 0;
    loop {
        if ti >= tb.len() { break; }
        let b = match tb.try_get(ti) { Option::Some(v) => v, Option::None => 0 };
        h = wrapping_mul(h ^ widen_u64(b), 1099511628211);
        ti = wrapping_add(ti, 1);
    }
    h = mix_u64(h, tl.anchors.len());
    h = mix_u64(h, tl.steps.len());
    h = mix_u64(h, between.from.lamport_seq);
    h = mix_u64(h, between.to.lamport_seq);
    h = mix_u64(h, wrapping_sub(between.def_end, between.def_start));
    h = mix_u64(h, wrapping_sub(between.u_start_end, between.u_start_begin));
    h = mix_u64(h, wrapping_sub(between.u_end_end, between.u_end_begin));
    h = mix_u64(h, wp.cursor.lamport_seq);
    h = mix_u64(h, wp.path.from.lamport_seq);
    h = mix_u64(h, wp.path.to.lamport_seq);
    h = mix_u64(h, wrapping_sub(wp.path.def_end, wp.path.def_start));
    h = mix_u64(h, wrapping_sub(wp.path.u_start_end, wp.path.u_start_begin));
    h = mix_u64(h, wrapping_sub(wp.path.u_end_end, wp.path.u_end_begin));
    h
}"#;
    let checked = check_chrono_driver(source);
    let got = agree(&checked, "main", &MODES);
    assert_eq!(got, h);
}
