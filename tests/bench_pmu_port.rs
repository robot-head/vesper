//! Differential verification of `@vesper/bench-pmu` (`crates/vesper-bench-pmu/vesper/`):
//! event encodings, sysfs format bit-scattering, `sign_extend_pmc`,
//! `checked_delta`, Linux `perf_event` group-read decoding, and
//! `perf_event_mmap_page` seqlock reads.
mod std_io_support;

use std_io_support::{MODES, Temp, agree};
use vesper::{ffi_lowering, packages};

fn check_pmu_driver(source: &str) -> ffi_lowering::ForeignChecked {
    let dir = Temp::new("bench-pmu-port");
    let pmu_pkg = std::fs::canonicalize("crates/vesper-bench-pmu/vesper").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Loom.toml"),
        format!(
            "[package]\nname = \"@test/bench-pmu-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\n[dependencies]\n\"@vesper/bench-pmu\" = {{ version = \"^0.1\", path = {:?} }}\n",
            pmu_pkg
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.ves"), source).unwrap();
    let assembled = packages::load(dir.path()).expect("load @test/bench-pmu-driver");
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &ffi_lowering::Imports::new())
        .unwrap_or_else(|e| panic!("{e}: {:?}", e.diagnostics))
}

#[test]
fn generic_encodings_and_sysfs_format_scattering_match() {
    let src = r#"
use @vesper/bench-pmu::{generic_encoding, parse_format_spec, place_bits};

pub fn check_generic() -> u64 {
    let (ok0, t0, c0) = generic_encoding(0);
    let (ok1, t1, c1) = generic_encoding(1);
    let (ok2, t2, c2) = generic_encoding(2);
    let (ok3, t3, c3) = generic_encoding(3);
    let (ok4, _, _) = generic_encoding(99);
    if !ok0 || !ok1 || !ok2 || !ok3 || ok4 {
        return 0;
    }
    if t0 != 0 || c0 != 9 || t1 != 3 || c1 != 65536 || t2 != 3 || c2 != 65538 || t3 != 0 || c3 != 5 {
        return 0;
    }
    1
}

pub fn check_scatter() -> u64 {
    let spec = match parse_format_spec(in "config:0-3,8-11\n".as_bytes()) {
        Option::Some(s) => s,
        Option::None => return 0,
    };
    if spec.word != 0 {
        return 0;
    }
    let placed = match place_bits(in spec, 165) {
        Option::Some(v) => v,
        Option::None => return 0,
    };
    // 0xA5 = 0b1010_0101 -> low nibble 5 at bits 0..=3, high nibble 10 at bits 8..=11 -> 0xA05 = 2565
    if placed != 2565 {
        return 0;
    }
    // Overflow past 8 bits must fail
    match place_bits(in spec, 256) {
        Option::Some(_) => 0,
        Option::None => 1,
    }
}
"#;
    let checked = check_pmu_driver(src);
    assert_eq!(agree(&checked, "check_generic", &MODES), 1);
    assert_eq!(agree(&checked, "check_scatter", &MODES), 1);
}

#[test]
fn sign_extend_checked_delta_and_group_read_match() {
    let src = r#"
use @vesper/bench-pmu::{sign_extend_pmc, checked_delta, decode_group_read2};

fn push_u64_le(inout out: ChunkSeq[u8], v: u64) {
    for i in 0..8 {
        out.push(wrapping_u8(wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8)))));
    }
}

pub fn check_sign_and_delta() -> u64 {
    let neg = sign_extend_pmc(140737488355327, 47);
    if neg != -1 {
        return 0;
    }
    let pos = sign_extend_pmc(1234, 48);
    if pos != 1234 {
        return 0;
    }
    let ok = checked_delta(100, 350, 10, 60, 10, 60);
    if !ok.ok || ok.delta != 250 {
        return 0;
    }
    let mux = checked_delta(100, 350, 10, 60, 10, 59);
    let reg = checked_delta(400, 350, 10, 60, 10, 60);
    if mux.ok || reg.ok {
        return 0;
    }
    1
}

pub fn check_group_decode() -> u64 {
    let mut buf: ChunkSeq[u8] = ChunkSeq::new[u8]();
    push_u64_le(inout buf, 2);
    push_u64_le(inout buf, 500);
    push_u64_le(inout buf, 500);
    // Kernel returns id1 first, then id0
    push_u64_le(inout buf, 999);
    push_u64_le(inout buf, 22);
    push_u64_le(inout buf, 777);
    push_u64_le(inout buf, 11);
    let r = decode_group_read2(in buf.as_slice(), 11, 22);
    if !r.ok || r.count0 != 777 || r.count1 != 999 || r.time_enabled != 500 || r.time_running != 500 {
        return 0;
    }
    1
}
"#;
    let checked = check_pmu_driver(src);
    assert_eq!(agree(&checked, "check_sign_and_delta", &MODES), 1);
    assert_eq!(agree(&checked, "check_group_decode", &MODES), 1);
}

#[test]
fn perf_mmap_page_seqlock_reads_match() {
    let src = r#"
use @vesper/bench-pmu::{read_perf_page_once};

fn set_u32_le(inout b: ChunkSeq[u8], off: u64, v: u32) {
    for i in 0..4 {
        let byte = wrapping_u8(wrapping_shr_u64(widen_u64(v), wrapping_u32(wrapping_mul(i, 8))));
        let _ = b.try_set(wrapping_add(off, i), byte);
    }
}

fn set_u64_le(inout b: ChunkSeq[u8], off: u64, v: u64) {
    for i in 0..8 {
        let byte = wrapping_u8(wrapping_shr_u64(v, wrapping_u32(wrapping_mul(i, 8))));
        let _ = b.try_set(wrapping_add(off, i), byte);
    }
}

pub fn check_seqlock() -> u64 {
    let mut page: ChunkSeq[u8] = ChunkSeq::new[u8]();
    page.resize(64, 0);
    set_u32_le(inout page, 8, 2); // lock = 2
    set_u32_le(inout page, 12, 1); // index = 1
    set_u64_le(inout page, 16, 1000); // offset = 1000
    set_u64_le(inout page, 24, 500); // time_enabled
    set_u64_le(inout page, 32, 500); // time_running
    set_u64_le(inout page, 40, 4); // CAP_USER_RDPMC
    set_u32_le(inout page, 48, 48); // pmc_width = 48 (bytes 48..50)
    let r = read_perf_page_once(in page.as_slice(), 250, false);
    if r.status != 0 || r.count != 1250 || r.time_enabled != 500 || r.time_running != 500 {
        return 0;
    }
    set_u64_le(inout page, 40, 0); // clear CAP_USER_RDPMC
    let denied = read_perf_page_once(in page.as_slice(), 250, false);
    if denied.status != 1 {
        return 0;
    }
    1
}
"#;
    let checked = check_pmu_driver(src);
    assert_eq!(agree(&checked, "check_seqlock", &MODES), 1);
}

#[test]
fn sysfs_event_encoder_matches() {
    let src = r#"
use @vesper/bench-pmu::{format_table_new, format_table_add, encode_sysfs_event};

pub fn check_encode() -> u64 {
    let mut t = format_table_new();
    if !format_table_add(inout t, in "event".as_bytes(), in "config:0-7".as_bytes()) {
        return 0;
    }
    if !format_table_add(inout t, in "umask".as_bytes(), in "config:8-15".as_bytes()) {
        return 0;
    }
    if !format_table_add(inout t, in "edge".as_bytes(), in "config:18".as_bytes()) {
        return 0;
    }
    if !format_table_add(inout t, in "ldlat".as_bytes(), in "config1:0-15".as_bytes()) {
        return 0;
    }

    let (c0, c1) = match encode_sysfs_event(
        in "event=0x3c,umask=0x01,edge,ldlat=12".as_bytes(),
        in t
    ) {
        Option::Some(v) => v,
        Option::None => return 0,
    };
    // 0x3c | (0x01 << 8) | (1 << 18) = 60 + 256 + 262144 = 262460
    if c0 != 262460 || c1 != 12 {
        return 0;
    }
    // Unknown term must fail
    match encode_sysfs_event(in "bogus=1".as_bytes(), in t) {
        Option::Some(_) => 0,
        Option::None => 1,
    }
}
"#;
    let checked = check_pmu_driver(src);
    assert_eq!(agree(&checked, "check_encode", &MODES), 1);
}
