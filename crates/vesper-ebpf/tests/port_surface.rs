//! The small ported functions no other test file pins down: map validation,
//! the helper and license tables, the kind numbers, and every outcome of the
//! stack bound. Runs against both builds (port wave P3a lane 6, #1041).

use vesper_ebpf::callgraph::{CallGraph, FnId, FnNode, StackError, check_stack};
use vesper_ebpf::helpers as h;
use vesper_ebpf::{MapDef, MapKind, ProgramType};

#[test]
fn map_definitions_are_validated_rule_by_rule() {
    let ok = |d: MapDef| assert_eq!(d.validate(), Ok(()), "{d:?}");
    let bad = |d: MapDef, msg: &str| assert_eq!(d.validate(), Err(msg.to_owned()), "{d:?}");
    ok(MapDef::new("counts", MapKind::Hash, 4, 8, 1024));
    ok(MapDef::new("a", MapKind::Array, 4, 1 << 20, 1));
    ok(MapDef::new("fifteen_bytes__", MapKind::LruHash, 512, 8, 1));
    ok(MapDef::ringbuf("events", 4096));
    ok(MapDef::ringbuf("events", 1 << 24));
    ok(MapDef::prog_array("jumps", 8));
    bad(
        MapDef::new("", MapKind::Hash, 4, 8, 1),
        "map name \"\" must be 1..=15 ASCII bytes",
    );
    bad(
        MapDef::new("sixteen_bytes___", MapKind::Hash, 4, 8, 1),
        "map name \"sixteen_bytes___\" must be 1..=15 ASCII bytes",
    );
    bad(
        MapDef::new("caf\u{e9}", MapKind::Hash, 4, 8, 1),
        "map name \"caf\u{e9}\" must be 1..=15 ASCII bytes",
    );
    bad(
        MapDef::new("r", MapKind::RingBuf, 4, 0, 4096),
        "ringbuf key and value size must be 0",
    );
    bad(
        MapDef::ringbuf("r", 2048),
        "ringbuf size must be a power of two >= 4096",
    );
    bad(
        MapDef::ringbuf("r", 4096 + 4096 / 2),
        "ringbuf size must be a power of two >= 4096",
    );
    bad(
        MapDef::new("m", MapKind::Hash, 4, 8, 0),
        "max_entries and value_size must be nonzero",
    );
    bad(
        MapDef::new("m", MapKind::Hash, 4, 0, 1),
        "max_entries and value_size must be nonzero",
    );
    bad(
        MapDef::new("m", MapKind::PerCpuArray, 8, 8, 1),
        "array maps have 4-byte keys",
    );
    bad(
        MapDef::new("m", MapKind::ProgArray, 4, 8, 1),
        "program arrays have 4-byte values",
    );
    bad(
        MapDef::new("m", MapKind::Hash, 0, 8, 1),
        "key size must be 1..=512",
    );
    bad(
        MapDef::new("m", MapKind::PerCpuHash, 513, 8, 1),
        "key size must be 1..=512",
    );
    bad(
        MapDef::new("m", MapKind::Hash, 4, (1 << 20) + 1, 1),
        "value size too large",
    );
}

const KINDS: [MapKind; 7] = [
    MapKind::Hash,
    MapKind::Array,
    MapKind::PerCpuHash,
    MapKind::PerCpuArray,
    MapKind::LruHash,
    MapKind::RingBuf,
    MapKind::ProgArray,
];

#[test]
fn kind_numbers_round_trip() {
    let numbers: Vec<u32> = KINDS.iter().map(|k| k.kernel_type()).collect();
    assert_eq!(numbers, [1, 2, 5, 6, 9, 27, 3]);
    for k in KINDS {
        assert_eq!(MapKind::from_kernel_type(k.kernel_type()), Some(k));
    }
    let arrays: Vec<bool> = KINDS.iter().map(|k| k.is_array()).collect();
    assert_eq!(arrays, [false, true, false, true, false, false, true]);
    let progs: Vec<u32> = ProgramType::ALL.iter().map(|p| p.kernel_type()).collect();
    assert_eq!(progs, [6, 3, 5, 2]);
    for p in ProgramType::ALL {
        assert_eq!(ProgramType::from_kernel_type(p.kernel_type()), Some(p));
        assert_eq!(p.has_packet(), !p.is_tracing());
        assert_eq!(p.test_runnable(), p.has_packet());
    }
    for t in [0, 4, 7, 8, 10, 26, 28, u32::MAX] {
        assert_eq!(MapKind::from_kernel_type(t), None, "map type {t}");
        if t != 2 && t != 3 && t != 5 && t != 6 {
            assert_eq!(ProgramType::from_kernel_type(t), None, "program type {t}");
        }
    }
}

#[test]
fn helper_table_matches_the_kernel_protos() {
    // Rows: Xdp, SchedCls, Tracepoint, Kprobe.
    let expect = |id: i32| -> [bool; 4] {
        match id {
            h::XDP_ADJUST_HEAD => [true, false, false, false],
            h::SK_LOOKUP_TCP | h::SK_RELEASE => [true, true, false, false],
            h::GET_CURRENT_PID_TGID | h::GET_CURRENT_UID_GID | h::GET_CURRENT_COMM => {
                [false, false, true, true]
            }
            _ => [true; 4],
        }
    };
    for &(id, name) in h::SUPPORTED {
        assert_eq!(h::name(id), Some(name));
        let got: Vec<bool> = ProgramType::ALL
            .iter()
            .map(|&p| h::allowed(p, id))
            .collect();
        assert_eq!(got, expect(id), "{name}");
    }
    for id in [-1, 0, 4, 6, 13, 45, 85, 134, 180, 182, i32::MAX] {
        assert_eq!(h::name(id), None);
        for p in ProgramType::ALL {
            assert!(!h::allowed(p, id), "{p:?} helper {id}");
        }
    }
}

#[test]
fn gpl_compatible_licenses_are_exact() {
    for l in [
        "GPL",
        "GPL v2",
        "GPL and additional rights",
        "Dual BSD/GPL",
        "Dual MIT/GPL",
        "Dual MPL/GPL",
    ] {
        assert!(h::gpl_compatible(l), "{l}");
    }
    for l in ["", "gpl", "GPL ", "MIT", "Dual MIT/GPL\0", "Proprietary"] {
        assert!(!h::gpl_compatible(l), "{l:?}");
    }
}

fn graph(frames: &[u32], calls: &[(u32, u32)]) -> CallGraph {
    CallGraph {
        functions: frames
            .iter()
            .enumerate()
            .map(|(i, &frame_bytes)| FnNode {
                id: FnId(i as u32),
                name: format!("f{i}"),
                frame_bytes,
            })
            .collect(),
        calls: calls.iter().map(|&(a, b)| (FnId(a), FnId(b))).collect(),
        indirect: Vec::new(),
    }
}

fn path(ids: &[u32]) -> Vec<FnId> {
    ids.iter().map(|&i| FnId(i)).collect()
}

#[test]
fn stack_bound_reports_every_outcome() {
    // Worst path 0 -> 2 -> 3 (64 + 256 + 192), duplicate edges counted once.
    let g = graph(
        &[64, 128, 256, 192],
        &[(0, 1), (0, 2), (2, 3), (0, 2), (1, 3)],
    );
    assert_eq!(check_stack(&g, FnId(0), 512, 8), Ok(512));
    assert_eq!(
        check_stack(&g, FnId(0), 511, 8),
        Err(StackError::Exceeded {
            bytes: 512,
            path: path(&[0, 2, 3]),
        })
    );
    assert_eq!(
        check_stack(&g, FnId(0), 512, 2),
        Err(StackError::TooDeep {
            path: path(&[0, 2, 3]),
        })
    );
    let cyc = graph(&[8, 8, 8], &[(0, 1), (1, 2), (2, 1)]);
    assert_eq!(
        check_stack(&cyc, FnId(0), 512, 8),
        Err(StackError::Recursion {
            path: path(&[0, 1, 2, 1]),
        })
    );
    let unknown = graph(&[8], &[(0, 7)]);
    assert_eq!(
        check_stack(&unknown, FnId(0), 512, 8),
        Err(StackError::UnknownFunction(FnId(7)))
    );
    assert_eq!(
        check_stack(&graph(&[8], &[]), FnId(3), 512, 8),
        Err(StackError::UnknownFunction(FnId(3)))
    );
    let mut ind = graph(&[8, 8], &[(0, 1)]);
    ind.indirect.push(FnId(1));
    assert_eq!(
        check_stack(&ind, FnId(0), 512, 8),
        Err(StackError::IndirectCall(FnId(1)))
    );
    // Frame sums saturate instead of wrapping.
    let huge = graph(&[u32::MAX, u32::MAX], &[(0, 1)]);
    assert_eq!(check_stack(&huge, FnId(0), u32::MAX, 8), Ok(u32::MAX));
    // A leaf root alone.
    assert_eq!(check_stack(&graph(&[24], &[]), FnId(0), 512, 1), Ok(24));
}

#[test]
fn implementation_follows_the_feature() {
    let expected = if cfg!(feature = "vesper-port") {
        "vesper"
    } else {
        "rust"
    };
    assert_eq!(vesper_ebpf::IMPLEMENTATION, expected);
}
