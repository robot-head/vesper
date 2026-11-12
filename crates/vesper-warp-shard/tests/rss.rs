mod common;

use common::{FixedHasher, Rng};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use vesper_warp_shard::rss::{IPPROTO_TCP, IPPROTO_UDP, RssInput, toeplitz_reference};
use vesper_warp_shard::{
    FiveTuple, HashFields, IndirectionTable, MICROSOFT_RSS_KEY, RssError, ShardSet, TableConfig,
    Toeplitz,
};

fn tuple(src: &str, sport: u16, dst: &str, dport: u16) -> FiveTuple {
    FiveTuple {
        src: src.parse().unwrap(),
        dst: dst.parse().unwrap(),
        src_port: sport,
        dst_port: dport,
        protocol: IPPROTO_TCP,
    }
}

/// Microsoft "Verifying the RSS Hash Calculation" vectors:
/// (destination, source, IP-only hash, IP+TCP hash).
const V4: &[(&str, u16, &str, u16, u32, u32)] = &[
    (
        "161.142.100.80",
        1766,
        "66.9.149.187",
        2794,
        0x323e8fc2,
        0x51ccc178,
    ),
    (
        "65.69.140.83",
        4739,
        "199.92.111.2",
        14230,
        0xd718262a,
        0xc626b0ea,
    ),
    (
        "12.22.207.184",
        38024,
        "24.19.198.95",
        12898,
        0xd2d0a5de,
        0x5c2b394a,
    ),
    (
        "209.142.163.6",
        2217,
        "38.27.205.30",
        48228,
        0x82989176,
        0xafc7327f,
    ),
    (
        "202.188.127.2",
        1303,
        "153.39.163.191",
        44251,
        0x5d1809c5,
        0x10e828a2,
    ),
];

const V6: &[(&str, u16, &str, u16, u32, u32)] = &[
    (
        "3ffe:2501:200:3::1",
        1766,
        "3ffe:2501:200:1fff::7",
        2794,
        0x2cc18cd5,
        0x40207d3d,
    ),
    (
        "ff02::1",
        4739,
        "3ffe:501:8::260:97ff:fe40:efab",
        14230,
        0x0f0c461c,
        0xdde51bbf,
    ),
    (
        "fe80::200:f8ff:fe21:67cf",
        38024,
        "3ffe:1900:4545:3:200:f8ff:fe21:67cf",
        44251,
        0x4b61e985,
        0x02d1feef,
    ),
];

#[test]
fn microsoft_verification_vectors() {
    let t = Toeplitz::new(&MICROSOFT_RSS_KEY);
    for &(dst, dport, src, sport, ip_only, with_ports) in V4.iter().chain(V6) {
        let ft = tuple(src, sport, dst, dport);
        for (fields, want) in [
            (HashFields::Addresses, ip_only),
            (HashFields::AddressesAndPorts, with_ports),
        ] {
            let input = RssInput::new(&ft, fields).unwrap();
            assert_eq!(t.hash(input.as_bytes()).unwrap(), want, "{ft:?} {fields:?}");
            assert_eq!(
                toeplitz_reference(&MICROSOFT_RSS_KEY, input.as_bytes()).unwrap(),
                want
            );
        }
    }
}

#[test]
fn table_hash_matches_bitwise_reference() {
    let mut rng = Rng(7);
    let mut key = [0u8; 40];
    for _ in 0..50 {
        key.iter_mut().for_each(|b| *b = rng.next() as u8);
        let t = Toeplitz::new(&key);
        for len in 0..=36 {
            let input: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            assert_eq!(t.hash(&input), toeplitz_reference(&key, &input));
        }
    }
}

#[test]
fn rejects_bad_inputs() {
    let t = Toeplitz::new(&MICROSOFT_RSS_KEY);
    assert_eq!(t.hash(&[0; 37]), Err(RssError::InputTooLong { len: 37 }));
    let mixed = FiveTuple {
        src: IpAddr::V4(Ipv4Addr::LOCALHOST),
        dst: IpAddr::V6(Ipv6Addr::LOCALHOST),
        src_port: 1,
        dst_port: 2,
        protocol: IPPROTO_UDP,
    };
    assert_eq!(
        RssInput::new(&mixed, HashFields::Addresses),
        Err(RssError::MixedFamilies)
    );
    assert_eq!(
        IndirectionTable::round_robin(100, 4),
        Err(RssError::IndirectionLength { len: 100 })
    );
    assert_eq!(
        IndirectionTable::round_robin(0, 4),
        Err(RssError::IndirectionLength { len: 0 })
    );
    assert_eq!(
        IndirectionTable::round_robin(1024, 4),
        Err(RssError::IndirectionLength { len: 1024 })
    );
    assert_eq!(
        IndirectionTable::round_robin(128, 0),
        Err(RssError::ShardCount { shards: 0 })
    );
    let mut rit = IndirectionTable::round_robin(128, 4).unwrap();
    assert_eq!(
        rit.set(3, 4),
        Err(RssError::ShardOutOfRange { entry: 3, shard: 4 })
    );
    assert_eq!(
        rit.set(128, 0),
        Err(RssError::ShardOutOfRange {
            entry: 128,
            shard: 0
        })
    );
    rit.set(3, 0).unwrap();
}

#[test]
fn non_tcp_udp_protocols_hash_addresses_only() {
    let mut ft = tuple("10.0.0.1", 1000, "10.0.0.2", 2000);
    ft.protocol = 1; // ICMP
    let a = RssInput::new(&ft, HashFields::AddressesAndPorts).unwrap();
    assert_eq!(a.as_bytes().len(), 8);
    ft.protocol = IPPROTO_UDP;
    let b = RssInput::new(&ft, HashFields::AddressesAndPorts).unwrap();
    assert_eq!(b.as_bytes().len(), 12);
}

fn random_tuple(rng: &mut Rng) -> FiveTuple {
    let v6 = rng.chance(1, 2);
    let (src, dst) = if v6 {
        (
            IpAddr::V6(Ipv6Addr::from(
                u128::from(rng.next()) << 64 | u128::from(rng.next()),
            )),
            IpAddr::V6(Ipv6Addr::from(
                u128::from(rng.next()) << 64 | u128::from(rng.next()),
            )),
        )
    } else {
        (
            IpAddr::V4(Ipv4Addr::from(rng.next() as u32)),
            IpAddr::V4(Ipv4Addr::from(rng.next() as u32)),
        )
    };
    FiveTuple {
        src,
        dst,
        src_port: rng.next() as u16,
        dst_port: 443,
        protocol: IPPROTO_UDP,
    }
}

/// Steering is deterministic and, on 200k random tuples over 8 shards with a
/// 128-entry round-robin table, every shard receives within 5% of the mean.
#[test]
fn steering_is_deterministic_and_balanced() {
    const SHARDS: usize = 8;
    const FLOWS: usize = 200_000;
    let make = || {
        ShardSet::new(
            &MICROSOFT_RSS_KEY,
            IndirectionTable::round_robin(128, SHARDS).unwrap(),
            HashFields::AddressesAndPorts,
            TableConfig::new(16, 8),
            |_| FixedHasher::default(),
        )
        .unwrap()
    };
    let a = make();
    let b = make();
    let mut rng = Rng(42);
    let mut counts = [0usize; SHARDS];
    for _ in 0..FLOWS {
        let t = random_tuple(&mut rng);
        let s = a.steer(&t).unwrap();
        assert_eq!(s, b.steer(&t).unwrap());
        assert_eq!(s, a.steer(&t).unwrap());
        counts[usize::from(s)] += 1;
    }
    let mean = FLOWS as f64 / SHARDS as f64;
    for (i, &c) in counts.iter().enumerate() {
        let dev = (c as f64 - mean).abs() / mean;
        assert!(
            dev < 0.05,
            "shard {i} got {c}, mean {mean}, deviation {dev}"
        );
    }
}

#[test]
fn indirection_rewrite_moves_flows() {
    let mut set = ShardSet::new(
        &MICROSOFT_RSS_KEY,
        IndirectionTable::round_robin(4, 2).unwrap(),
        HashFields::AddressesAndPorts,
        TableConfig::new(16, 8),
        |_| FixedHasher::default(),
    )
    .unwrap();
    let mut rit = IndirectionTable::round_robin(4, 2).unwrap();
    for e in 0..4 {
        rit.set(e, 1).unwrap();
    }
    set.set_indirection(rit).unwrap();
    let mut rng = Rng(3);
    for _ in 0..100 {
        assert_eq!(set.steer(&random_tuple(&mut rng)).unwrap(), 1);
    }
    assert_eq!(
        set.set_indirection(IndirectionTable::round_robin(4, 3).unwrap()),
        Err(RssError::ShardCount { shards: 3 })
    );
}

/// Each shard is moved to its own thread with no locks: the borrow checker
/// enforces that shards are disjoint, and `ConnTable` is `Send`.
#[test]
fn shards_run_on_separate_threads_without_sharing() {
    use vesper_warp_shard::{ConnState, ConnectionId};
    fn assert_send<T: Send>() {}
    assert_send::<vesper_warp_shard::ConnTable>();
    assert_send::<vesper_warp_shard::Shard<FixedHasher>>();

    let mut set = ShardSet::new(
        &MICROSOFT_RSS_KEY,
        IndirectionTable::round_robin(128, 4).unwrap(),
        HashFields::AddressesAndPorts,
        TableConfig::new(4096, 8),
        |_| FixedHasher::default(),
    )
    .unwrap();
    let mut rng = Rng(9);
    let flows: Vec<FiveTuple> = (0..4000).map(|_| random_tuple(&mut rng)).collect();
    let mut per_shard: Vec<Vec<u64>> = vec![Vec::new(); 4];
    for (i, f) in flows.iter().enumerate() {
        per_shard[usize::from(set.steer(f).unwrap())].push(i as u64);
    }
    std::thread::scope(|s| {
        for (shard, ids) in set.shards_mut().iter_mut().zip(&per_shard) {
            s.spawn(move || {
                for &i in ids {
                    let st = ConnState::new(ConnectionId::EMPTY, 1, 0);
                    shard.table.insert(&ConnectionId::from_u64(i), &st).unwrap();
                }
            });
        }
    });
    for (id, ids) in per_shard.iter().enumerate() {
        let shard = set.shard(id as u16).unwrap();
        assert_eq!(shard.id() as usize, id);
        assert_eq!(shard.table.len() as usize, ids.len());
        for &i in ids {
            assert!(shard.table.lookup(&i.to_be_bytes()).unwrap().is_some());
            for other in (0..4).filter(|&o| o != id) {
                let t = &set.shard(other as u16).unwrap().table;
                assert!(t.lookup(&i.to_be_bytes()).unwrap().is_none());
            }
        }
    }
}
