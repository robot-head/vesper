//! Hardware branch traces mapped onto the effect-boundary timeline, and the
//! reverse watchpoint that names the instruction-level path to a write.

use vesper_chrono::ete::decode_elements;
use vesper_chrono::hwpath::{
    BranchTimeline, EffectAnchor, PathError, StepKind, TraceStep, reverse_watch_path,
};
use vesper_chrono::ipt::decode_packets;
use vesper_chrono::ipt_flow::{BranchKind, CodeImage, FlowLimits, reconstruct};
use vesper_chrono::{
    Bundle, BundleKey, BundleLimits, FlightRecorder, RecorderConfig, RedactionPolicy, ReplaySession,
};

const BASE: u64 = 0x401000;

/// Same program as tests/ipt.rs (see the listing there).
fn program() -> Vec<u8> {
    let mut code = vec![0x90u8; 0x26];
    code[0x00..0x02].copy_from_slice(&[0x31, 0xc0]);
    code[0x02..0x04].copy_from_slice(&[0xff, 0xc0]);
    code[0x04..0x07].copy_from_slice(&[0x83, 0xf8, 0x03]);
    code[0x07..0x09].copy_from_slice(&[0x75, 0xf9]);
    code[0x09..0x0e].copy_from_slice(&[0xe8, 0x0a, 0, 0, 0]);
    code[0x0e..0x10].copy_from_slice(&[0xff, 0xe3]);
    code[0x18..0x1c].copy_from_slice(&[0x48, 0x83, 0xc0, 0x01]);
    code[0x1c] = 0xc3;
    code[0x20..0x22].copy_from_slice(&[0x74, 0x02]);
    code[0x24..0x26].copy_from_slice(&[0xff, 0xe0]);
    code
}

fn tsc(v: u64) -> Vec<u8> {
    let mut p = vec![0x19];
    p.extend_from_slice(&v.to_le_bytes()[..7]);
    p
}

/// PT stream with TSC packets interleaved between branches:
/// cond1 [110,250) cond2 [250,310) cond3+call [310,350) ret [350,390)
/// jmp rbx [390,420) je+disable [420,450).
fn pt_stream() -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..8 {
        v.extend_from_slice(&[0x02, 0x82]);
    }
    v.extend_from_slice(&[0x02, 0x23]);
    v.push((6 << 5) | 0x11);
    v.extend_from_slice(&BASE.to_le_bytes());
    v.extend(tsc(110));
    v.push(0b0000_0110); // TNT: taken
    v.extend(tsc(250));
    v.push(0b0000_0110); // taken
    v.extend(tsc(310));
    v.push(0b0000_0100); // not taken (then the call)
    v.extend(tsc(350));
    v.push(0b0000_0110); // compressed ret
    v.extend(tsc(390));
    v.push((1 << 5) | 0x0d); // TIP 0x1020 (update16)
    v.extend_from_slice(&0x1020u16.to_le_bytes());
    v.extend(tsc(420));
    v.push(0b0000_0100); // je not taken
    v.push(0x01); // TIP.PGD suppressed at jmp rax
    v.extend(tsc(450));
    v
}

#[test]
fn reverse_watch_names_the_instruction_path_to_the_write() {
    let mut rec = FlightRecorder::new(RecorderConfig::small()).unwrap();
    // The recorder's time field carries the TSC anchor of each record.
    assert_eq!(rec.record_effect(1, 100, 1, 0, b"a").unwrap(), 1);
    assert_eq!(rec.record_inout_write(1, 200, 7, 10).unwrap(), 2);
    assert_eq!(rec.record_effect(1, 300, 1, 0, b"b").unwrap(), 3);
    assert_eq!(rec.record_inout_write(1, 400, 7, 11).unwrap(), 4);
    assert_eq!(rec.record_effect(1, 500, 1, 0, b"c").unwrap(), 5);
    let key = BundleKey::new([3; 32]);
    let bytes = Bundle::export(&rec, &RedactionPolicy::new(), b"", &key).unwrap();
    let mut session = ReplaySession::open_bundle(&bytes, &key, &BundleLimits::default()).unwrap();

    let code = program();
    let packets = decode_packets(&pt_stream()).unwrap();
    let trace = reconstruct(
        &packets,
        &CodeImage::single(BASE, &code),
        FlowLimits::default(),
    )
    .unwrap();
    let anchors = BranchTimeline::anchors_from_events(session.bundle().events());
    let timeline = BranchTimeline::new(anchors, BranchTimeline::steps_from_pt(&trace)).unwrap();

    let found = reverse_watch_path(&mut session, 7, &timeline).unwrap();
    assert_eq!(found.cursor.lamport_seq, 4);
    assert_eq!(
        found.path.from,
        EffectAnchor {
            lamport_seq: 3,
            tsc: 300
        }
    );
    assert_eq!(
        found.path.to,
        EffectAnchor {
            lamport_seq: 4,
            tsc: 400
        }
    );
    let kinds = |s: &[TraceStep]| s.iter().map(|s| (s.from, s.kind)).collect::<Vec<_>>();
    use BranchKind::*;
    assert_eq!(
        kinds(found.path.definite),
        vec![
            (0x401007, StepKind::Branch(Conditional { taken: false })),
            (0x401009, StepKind::Branch(DirectCall)),
            (0x40101c, StepKind::Branch(Return { compressed: true })),
        ]
    );
    // The branch in [250, 310) may precede record 3; [390, 420) may follow 4.
    assert_eq!(
        kinds(found.path.uncertain_start),
        vec![(0x401007, StepKind::Branch(Conditional { taken: true }))]
    );
    assert_eq!(
        kinds(found.path.uncertain_end),
        vec![(0x40100e, StepKind::Branch(IndirectJump))]
    );

    // Watching again moves to the earlier write (seq 2, between 100 and 200):
    // only the enable at [0, 110) and cond1 at [110, 250) straddle it.
    let earlier = reverse_watch_path(&mut session, 7, &timeline).unwrap();
    assert_eq!(earlier.cursor.lamport_seq, 2);
    assert!(earlier.path.definite.is_empty());
    assert_eq!(earlier.path.uncertain_start.len(), 1);
    assert_eq!(earlier.path.uncertain_end.len(), 1);
    // No earlier write remains.
    assert!(matches!(
        reverse_watch_path(&mut session, 7, &timeline),
        Err(PathError::Replay(_))
    ));
    let _ = session.close();

    // Whole-run and error queries.
    let all = timeline.path_between(1, 5).unwrap();
    assert_eq!(all.definite.len(), trace.branches.len() - 1);
    assert_eq!(
        timeline.path_between(3, 3),
        Err(PathError::Reversed { from: 3, to: 3 })
    );
    assert_eq!(
        timeline.path_between(1, 9),
        Err(PathError::UnknownSeq { lamport_seq: 9 })
    );
    assert_eq!(
        timeline.path_before(1),
        Err(PathError::NoPreviousRecord { lamport_seq: 1 })
    );
}

#[test]
fn unordered_inputs_are_rejected() {
    let a = |s, t| EffectAnchor {
        lamport_seq: s,
        tsc: t,
    };
    let step = |lo, hi| TraceStep {
        from: 0,
        to: None,
        kind: StepKind::Atom { executed: true },
        tsc_lo: lo,
        tsc_hi: hi,
    };
    assert_eq!(
        BranchTimeline::new(vec![a(1, 10), a(1, 20)], vec![]).unwrap_err(),
        PathError::AnchorsOutOfOrder { index: 1 }
    );
    assert_eq!(
        BranchTimeline::new(vec![a(1, 10), a(2, 5)], vec![]).unwrap_err(),
        PathError::AnchorsOutOfOrder { index: 1 }
    );
    assert_eq!(
        BranchTimeline::new(vec![], vec![step(Some(5), Some(5))]).unwrap_err(),
        PathError::BadInterval { index: 0 }
    );
    assert_eq!(
        BranchTimeline::new(vec![], vec![step(Some(5), Some(9)), step(Some(1), Some(9))])
            .unwrap_err(),
        PathError::StepsOutOfOrder { index: 1 }
    );
}

#[test]
fn ete_elements_map_onto_the_timeline() {
    // A-Sync, TS 100, address, E, TS 200, address, N N, TS 300.
    let mut v = vec![0u8; 11];
    v.push(0x80);
    v.extend_from_slice(&[0x02, 100]);
    v.push(0x9d);
    v.extend_from_slice(&[0x00, 0x08, 0, 0, 0, 0, 0, 0]); // 0x1000
    v.push(0xf7);
    v.extend_from_slice(&[0x02, 0x80 | (200u16 & 0x7f) as u8, (200u16 >> 7) as u8]);
    v.push(0x95);
    v.push(0x10); // short: bits [8:2] = 0x10 -> 0x1040
    v.push(0xd8); // f2: N N
    v.extend_from_slice(&[0x02, 0x80 | (300u16 & 0x7f) as u8, (300u16 >> 7) as u8]);
    let elements = decode_elements(&v).unwrap();
    let steps = BranchTimeline::steps_from_ete(&elements);
    let timeline = BranchTimeline::new(
        vec![
            EffectAnchor {
                lamport_seq: 1,
                tsc: 50,
            },
            EffectAnchor {
                lamport_seq: 2,
                tsc: 250,
            },
            EffectAnchor {
                lamport_seq: 3,
                tsc: 300,
            },
        ],
        steps,
    )
    .unwrap();
    let p = timeline.path_between(1, 2).unwrap();
    assert_eq!(p.definite.len(), 2);
    assert_eq!(p.definite[0].to, Some(0x1000));
    assert_eq!(p.definite[1].kind, StepKind::Atom { executed: true });
    let p = timeline.path_between(2, 3).unwrap();
    // [200, 300) straddles the anchor at 250.
    assert!(p.definite.is_empty());
    assert_eq!(p.uncertain_start.len(), 3);
    assert_eq!(p.uncertain_start[0].to, Some(0x1040));
}
