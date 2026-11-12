//! The §27.3 SYN-flood mitigator over packet fixtures: in this crate's
//! interpreter against the reference decision, and (when the host allows
//! `bpf()`) in the kernel via `BPF_PROG_TEST_RUN`, with the alerts read out
//! of the kernel ring buffer by the same reader used for the interpreter.

mod common;

use common::Rng;
use vesper_ebpf::interp::{RingBufReader, XdpEnv, run_xdp};
use vesper_ebpf::mitigator::{ALERTS_MAP, COUNTS_MAP, MitigatorConfig, build, reference_decision};
use vesper_ebpf::wire::{
    ETH_P_IPV4, EthIpv4TcpWire, IPPROTO_TCP, SynDropAlertWire, TCP_FLAG_ACK, TCP_FLAG_SYN,
    WireFrame, XdpAction,
};
use vesper_ebpf::{VerifierPolicy, verify};

const THRESHOLD: u64 = 3;

fn cfg() -> MitigatorConfig {
    MitigatorConfig {
        pps_threshold: THRESHOLD,
        max_sources: 1024,
        ring_bytes: 4096,
    }
}

fn tcp(src: u32, dport: u16, flags: u16) -> Vec<u8> {
    let h = EthIpv4TcpWire {
        dst_mac: [2, 0, 0, 0, 0, 1],
        src_mac: [2, 0, 0, 0, 0, 2],
        ether_type: ETH_P_IPV4,
        ver_ihl: 0x45,
        total_len: 40,
        ttl: 64,
        proto: IPPROTO_TCP,
        src_ipv4: src,
        dst_ipv4: 0x0a00_0001,
        src_port: 40000,
        dst_port: dport,
        seq_num: 1,
        data_offset_flags: (5 << 12) | flags,
        window_size: 1024,
        ..Default::default()
    };
    h.encode()
}

fn alerts(env: &XdpEnv) -> Vec<SynDropAlertWire> {
    let ring = env.ring(ALERTS_MAP).expect("ring");
    let mut r = RingBufReader::new(&ring.data, ring.consumer, ring.producer).expect("reader");
    let mut out = Vec::new();
    while let Some(rec) = r.next_record().expect("record") {
        out.push(SynDropAlertWire::decode(&rec).expect("alert"));
    }
    out
}

#[test]
fn wire_layouts_are_exact() {
    assert_eq!(EthIpv4TcpWire::SIZE, 54);
    assert_eq!(SynDropAlertWire::SIZE, 16);
    let p = tcp(0xc0a8_0001, 80, TCP_FLAG_SYN);
    assert_eq!(p.len(), 54);
    assert_eq!(EthIpv4TcpWire::decode(&p).unwrap().src_ipv4, 0xc0a8_0001);
    let a = SynDropAlertWire {
        src_ipv4: 0x0102_0304,
        dst_port: 0x0506,
        tcp_flags: 0x0002,
        observed_pps: 0x1122_3344_5566_7788,
    };
    let b = a.encode();
    assert_eq!(&b[..8], &[1, 2, 3, 4, 5, 6, 0, 2]);
    assert_eq!(&b[8..], &0x1122_3344_5566_7788u64.to_le_bytes());
    assert_eq!(SynDropAlertWire::decode(&b).unwrap(), a);
    assert!(SynDropAlertWire::decode(&b[..15]).is_err());
}

#[test]
fn verified_within_the_stack_budget() {
    let p = build(&cfg()).unwrap();
    let r = verify(&p, &VerifierPolicy::default()).expect("mitigator verifies");
    assert!(r.worst_stack <= 512);
}

#[test]
fn fixtures_follow_the_specification() {
    let p = build(&cfg()).unwrap();
    let mut env = XdpEnv::new(&p);
    let attacker = 0xc633_6407; // 198.51.100.7
    let peer = 0xcb00_7101; // 203.0.113.1
    // SYNs up to the threshold pass, then drop with one alert each
    for n in 1..=6u64 {
        let r = run_xdp(&p, &mut env, &tcp(attacker, 443, TCP_FLAG_SYN)).unwrap();
        let want = if n <= THRESHOLD {
            XdpAction::Pass
        } else {
            XdpAction::Drop
        };
        assert_eq!(r.ret, want as u64, "SYN {n}");
    }
    let key = attacker.to_be_bytes();
    let count = env.map_get(COUNTS_MAP, &key).unwrap();
    assert_eq!(u64::from_le_bytes(count.try_into().unwrap()), 6);
    let a = alerts(&env);
    assert_eq!(a.len(), 3);
    for (i, al) in a.iter().enumerate() {
        assert_eq!(al.src_ipv4, attacker);
        assert_eq!(al.dst_port, 443);
        assert_eq!(al.tcp_flags, TCP_FLAG_SYN);
        assert_eq!(al.observed_pps, THRESHOLD + 1 + i as u64);
    }
    // established traffic from the same source is never dropped
    for flags in [TCP_FLAG_ACK, TCP_FLAG_SYN | TCP_FLAG_ACK, 0x18, 0x11, 0x3f] {
        let r = run_xdp(&p, &mut env, &tcp(attacker, 443, flags)).unwrap();
        assert_eq!(r.ret, XdpAction::Pass as u64, "flags {flags:#x}");
    }
    // other sources keep their own count
    let r = run_xdp(&p, &mut env, &tcp(peer, 22, TCP_FLAG_SYN)).unwrap();
    assert_eq!(r.ret, XdpAction::Pass as u64);
    // short, non-IPv4 and non-TCP frames pass and are not counted
    let mut udp = tcp(attacker, 53, TCP_FLAG_SYN);
    udp[23] = 17;
    let mut arp = tcp(attacker, 0, TCP_FLAG_SYN);
    arp[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    let short = tcp(attacker, 443, TCP_FLAG_SYN)[..53].to_vec();
    for pkt in [udp, arp, short, Vec::new()] {
        let r = run_xdp(&p, &mut env, &pkt).unwrap();
        assert_eq!(r.ret, XdpAction::Pass as u64);
    }
    assert_eq!(
        u64::from_le_bytes(env.map_get(COUNTS_MAP, &key).unwrap().try_into().unwrap()),
        6
    );
    // flags above the low 6 bits (ECE/CWR/NS) do not change the decision
    let r = run_xdp(&p, &mut env, &tcp(attacker, 443, 0x1c0 | TCP_FLAG_SYN)).unwrap();
    assert_eq!(r.ret, XdpAction::Drop as u64);
}

#[test]
fn a_full_ring_still_drops() {
    let p = build(&cfg()).unwrap();
    let mut env = XdpEnv::new(&p);
    let src = 0x0a0a_0a0a;
    let mut drops = 0;
    for _ in 0..400 {
        let r = run_xdp(&p, &mut env, &tcp(src, 80, TCP_FLAG_SYN)).unwrap();
        if r.ret == XdpAction::Drop as u64 {
            drops += 1;
        }
    }
    assert_eq!(drops, 400 - THRESHOLD);
    // 4096-byte ring, 24 bytes per record: 170 alerts fit
    assert_eq!(alerts(&env).len(), 4096 / 24);
}

#[test]
fn random_packets_match_the_reference_decision() {
    let p = build(&cfg()).unwrap();
    let mut env = XdpEnv::new(&p);
    let mut model: std::collections::HashMap<u32, u64> = Default::default();
    let mut rng = Rng(77);
    for _ in 0..20_000 {
        let src = rng.pick(&[1u32, 2, 3, 0xffff_ffff]);
        let mut pkt = tcp(src, rng.below(65536) as u16, rng.below(64) as u16);
        if rng.chance(10) {
            pkt[12] = rng.next() as u8;
        }
        if rng.chance(10) {
            pkt[23] = rng.next() as u8;
        }
        if rng.chance(10) {
            pkt.truncate(rng.below(60) as usize);
        }
        let extra = rng.below(20) as usize;
        pkt.extend(rng.bytes(extra));
        let (want, count) = reference_decision(&pkt, model.get(&src).copied(), THRESHOLD);
        let got = run_xdp(&p, &mut env, &pkt).unwrap();
        assert_eq!(got.ret, want as u64);
        if let Some(c) = count {
            model.insert(src, c);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kernel_runs_the_mitigator_identically() {
    use vesper_ebpf::kernel;
    if let Err(e) = kernel::probe() {
        eprintln!("kernel BPF evidence unavailable on this host: {e}");
        return;
    }
    kernel::pin_to_cpu(0).expect("pin to CPU 0");
    let p = build(&cfg()).unwrap();
    let loaded = kernel::load(&p).expect("kernel verifier accepts the mitigator");
    let mut ring = kernel::MappedRing::new(&loaded.maps[ALERTS_MAP], 4096).expect("mmap ringbuf");
    let mut env = XdpEnv::new(&p);
    let mut rng = Rng(5);
    let srcs = [0x0a00_0002u32, 0x0a00_0003];
    for _ in 0..200 {
        let src = rng.pick(&srcs);
        let flags = rng.pick(&[TCP_FLAG_SYN, TCP_FLAG_SYN, TCP_FLAG_ACK, 0x12]);
        let pkt = tcp(src, 8080, flags);
        let k = kernel::test_run(&loaded.prog, &pkt).expect("test run");
        let u = run_xdp(&p, &mut env, &pkt).unwrap();
        assert_eq!(k.retval as u64, u.ret);
        assert_eq!(k.packet, u.packet);
    }
    // per-source counts: the kernel's per-CPU values summed over CPUs
    let cpus = kernel::possible_cpus().unwrap();
    for src in srcs {
        let key = src.to_be_bytes();
        let kv = kernel::lookup(&loaded.maps[COUNTS_MAP], &key, 8 * cpus).unwrap();
        let ksum: u64 = kv
            .map(|v| {
                v.chunks(8)
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .sum()
            })
            .unwrap_or(0);
        let mine = env
            .map_get(COUNTS_MAP, &key)
            .map_or(0, |v| u64::from_le_bytes(v.try_into().unwrap()));
        assert_eq!(ksum, mine, "count for {src:#x}");
    }
    // the kernel ring holds exactly the interpreter's alerts
    let (data, cons, prod) = ring.snapshot();
    let mut r = RingBufReader::new(&data, cons, prod).unwrap();
    let mut kernel_alerts = Vec::new();
    while let Some(rec) = r.next_record().unwrap() {
        kernel_alerts.push(SynDropAlertWire::decode(&rec).unwrap());
    }
    ring.set_consumer(r.consumer());
    assert!(!kernel_alerts.is_empty());
    assert_eq!(kernel_alerts, alerts(&env));
}
