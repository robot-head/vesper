//! Interpreter semantics that a kernel program can observe: ring-buffer
//! records that wrap the end of the data area, map values deleted while
//! the program still holds a pointer to them, and the existence flags of
//! `bpf_map_update_elem`. Each program is also checked by this crate's
//! verifier, since the interpreter is the oracle for what it accepts.

mod common;

use vesper_ebpf::helpers as h;
use vesper_ebpf::insn::{JmpOp, Reg, Size};
use vesper_ebpf::interp::{RingBufReader, XdpEnv, run_xdp};
use vesper_ebpf::mitigator::{ALERTS_MAP, MitigatorConfig, build};
use vesper_ebpf::program::{Asm, MapDef, MapKind, Program, ProgramType};
use vesper_ebpf::wire::{
    ETH_P_IPV4, EthIpv4TcpWire, IPPROTO_TCP, SynDropAlertWire, TCP_FLAG_SYN, WireFrame, XdpAction,
};
use vesper_ebpf::{VerifierPolicy, verify};

/// Return values of `runs` kernel test runs of `p` on one fresh load, or
/// `None` when the host does not permit `bpf()`.
fn kernel_rets(p: &Program, runs: usize) -> Option<Vec<i64>> {
    #[cfg(target_os = "linux")]
    {
        use vesper_ebpf::kernel;
        if let Err(e) = kernel::probe() {
            eprintln!("kernel BPF evidence unavailable on this host: {e}");
            return None;
        }
        let loaded = kernel::load(p).expect("kernel verifier accepts");
        Some(
            (0..runs)
                .map(|_| {
                    kernel::test_run(&loaded.prog, &[0; 64])
                        .expect("test run")
                        .retval as i32 as i64
                })
                .collect(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (p, runs);
        None
    }
}

fn syn(src: u32, dport: u16) -> Vec<u8> {
    EthIpv4TcpWire {
        ether_type: ETH_P_IPV4,
        ver_ihl: 0x45,
        proto: IPPROTO_TCP,
        src_ipv4: src,
        dst_port: dport,
        data_offset_flags: (5 << 12) | TCP_FLAG_SYN,
        ..Default::default()
    }
    .encode()
}

#[test]
fn ring_records_wrap_when_the_consumer_keeps_up() {
    let p = build(&MitigatorConfig {
        pps_threshold: 0,
        max_sources: 16,
        ring_bytes: 4096,
    })
    .unwrap();
    let mut env = XdpEnv::new(&p);
    let mut seen = Vec::new();
    for i in 0..1000u32 {
        let r = run_xdp(&p, &mut env, &syn(7, i as u16)).unwrap();
        assert_eq!(r.ret, XdpAction::Drop as u64);
        let ring = env.ring(ALERTS_MAP).unwrap();
        let mut rd = RingBufReader::new(&ring.data, ring.consumer, ring.producer).unwrap();
        while let Some(rec) = rd.next_record().unwrap() {
            seen.push(SynDropAlertWire::decode(&rec).unwrap());
        }
        let c = rd.consumer();
        env.ring_mut(ALERTS_MAP).unwrap().consumer = c;
    }
    // 24-byte records do not divide 4096, so many of them straddle the end
    assert_eq!(seen.len(), 1000);
    for (i, a) in seen.iter().enumerate() {
        assert_eq!(a.dst_port, i as u16);
        assert_eq!(a.observed_pps, i as u64 + 1);
    }
}

#[test]
fn a_ring_is_never_filled_to_the_last_byte() {
    // the kernel keeps the producer at most size - 1 bytes ahead: 4096
    // bytes hold 255, not 256, records of 8-byte payload + 8-byte header
    let mut a = Asm::new();
    let rb = a.map(MapDef::ringbuf("rb", 4096)).unwrap();
    let none = a.label();
    a.ld_map(Reg::R1, rb);
    a.mov64(Reg::R2, 8);
    a.mov64(Reg::R3, 0);
    a.call(h::RINGBUF_RESERVE);
    a.jmp(JmpOp::Eq, Reg::R0, 0, none);
    a.mov64(Reg::R1, Reg::R0);
    a.mov64(Reg::R2, 0);
    a.call(h::RINGBUF_SUBMIT);
    a.mov64(Reg::R0, 1);
    a.exit();
    a.bind(none).unwrap();
    a.mov64(Reg::R0, 0);
    a.exit();
    let p = a.finish("fill", ProgramType::Xdp).unwrap();
    verify(&p, &VerifierPolicy::default()).expect("verifies");
    let mut env = XdpEnv::new(&p);
    let ok = (0..300)
        .filter(|_| run_xdp(&p, &mut env, &[0; 64]).unwrap().ret == 1)
        .count();
    assert_eq!(ok, 255);
    if let Some(k) = kernel_rets(&p, 300) {
        assert_eq!(k.iter().filter(|r| **r == 1).count(), 255, "kernel");
    }
}

const HASH: usize = 0;
const ARRAY: usize = 1;

fn maps(a: &mut Asm) {
    a.map(MapDef::new("hm", MapKind::Hash, 4, 8, 4)).unwrap();
    a.map(MapDef::new("am", MapKind::Array, 4, 8, 4)).unwrap();
}

/// `update(map, &(u32)0, &(u64)value, flags)`; result in r0.
fn update(a: &mut Asm, map: usize, value: i32, flags: i32) {
    a.st(Size::W, Reg::R10, -4, 0);
    a.st(Size::DW, Reg::R10, -16, value);
    a.ld_map(Reg::R1, map);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.mov64(Reg::R3, Reg::R10);
    a.add64(Reg::R3, -16);
    a.mov64(Reg::R4, flags);
    a.call(h::MAP_UPDATE_ELEM);
}

fn checked(a: Asm) -> Program {
    let p = a.finish("t", ProgramType::Xdp).unwrap();
    verify(&p, &VerifierPolicy::default()).expect("verifies");
    p
}

#[test]
fn a_deleted_value_stays_readable_until_the_run_ends() {
    let mut a = Asm::new();
    maps(&mut a);
    let none = a.label();
    update(&mut a, HASH, 42, 0);
    a.st(Size::W, Reg::R10, -4, 0);
    a.ld_map(Reg::R1, HASH);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.call(h::MAP_LOOKUP_ELEM);
    a.jmp(JmpOp::Eq, Reg::R0, 0, none);
    a.mov64(Reg::R6, Reg::R0);
    a.ld_map(Reg::R1, HASH);
    a.mov64(Reg::R2, Reg::R10);
    a.add64(Reg::R2, -4);
    a.call(h::MAP_DELETE_ELEM);
    a.ldx(Size::DW, Reg::R0, Reg::R6, 0);
    a.exit();
    a.bind(none).unwrap();
    a.mov64(Reg::R0, 0);
    a.exit();
    let p = checked(a);
    let mut env = XdpEnv::new(&p);
    for _ in 0..3 {
        assert_eq!(run_xdp(&p, &mut env, &[0; 64]).unwrap().ret, 42);
        assert_eq!(env.map_get(HASH, &0u32.to_le_bytes()), None);
    }
    if let Some(k) = kernel_rets(&p, 3) {
        assert_eq!(k, vec![42; 3], "kernel");
    }
}

#[test]
fn update_flags_follow_the_kernel() {
    const EINVAL: i64 = -22;
    const EEXIST: i64 = -17;
    const ENOENT: i64 = -2;
    let cases: [(usize, &[i32], i64); 10] = [
        (HASH, &[0], 0),
        (HASH, &[1], 0),
        (HASH, &[2], ENOENT),
        (HASH, &[0, 1], EEXIST),
        (HASH, &[0, 2], 0),
        (HASH, &[3], EINVAL),
        (HASH, &[4], EINVAL),
        (ARRAY, &[1], EEXIST),
        (ARRAY, &[2], 0),
        (ARRAY, &[0], 0),
    ];
    for (map, flags, want) in cases {
        let mut a = Asm::new();
        maps(&mut a);
        for (i, f) in flags.iter().enumerate() {
            update(&mut a, map, 10 + i as i32, *f);
        }
        a.exit();
        let p = checked(a);
        let mut env = XdpEnv::new(&p);
        let got = run_xdp(&p, &mut env, &[0; 64]).unwrap().ret as i64;
        assert_eq!(got, want, "map {map} flags {flags:?}");
        if let Some(k) = kernel_rets(&p, 1) {
            assert_eq!(k, vec![want], "kernel: map {map} flags {flags:?}");
        }
        // a refused update leaves the map as it was
        let stored = env
            .map_get(map, &0u32.to_le_bytes())
            .map(|v| u64::from_le_bytes(v.try_into().unwrap()));
        // every update but a refused last one took effect; the last applied
        // one wrote 10 + its index
        let n = if want == 0 {
            flags.len()
        } else {
            flags.len() - 1
        };
        let applied = n.checked_sub(1).map(|i| 10 + i as u64);
        let expect = match (map, applied) {
            (ARRAY, None) => Some(0),
            (_, a) => a,
        };
        assert_eq!(stored, expect, "map {map} flags {flags:?}");
    }
}
