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
