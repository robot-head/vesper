//! One-million-connection table: build, lookup and a documented memory bound.

mod common;

use common::Rng;
use vesper_warp_shard::{ConnState, ConnTable, ConnectionId, TableConfig};

const N: u32 = 1 << 20;

/// Documented bound: 108 B of tiles + 4 B free stack + 16 B index (2 buckets
/// of 8 B per connection) = 128 B per connection, plus 4 MiB of fixed batch
/// staging and per-tile stage markers. 1 Mi connections must fit in 132 MiB.
const BOUND_BYTES: usize = 132 << 20;

#[test]
fn one_million_connections() {
    let mut t = ConnTable::new(TableConfig::new(N, 8)).unwrap();
    let mem = t.memory_bytes();
    eprintln!(
        "warp-shard 1Mi table: {} bytes ({:.1} B/conn), isa={}",
        mem,
        mem as f64 / f64::from(N),
        t.isa()
    );
    assert!(mem <= BOUND_BYTES, "{mem} > {BOUND_BYTES}");
    let mut rng = Rng(2026);
    let seed = rng.next();
    // A bijective mix so all CIDs are distinct.
    let cid = |i: u64| (i ^ seed).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for i in 0..u64::from(N) {
        t.insert(
            &ConnectionId::from_u64(cid(i)),
            &ConnState::new(ConnectionId::EMPTY, 1, 0),
        )
        .unwrap();
    }
    assert_eq!(t.len(), N);
    eprintln!("warp-shard 1Mi table: max probe {}", t.index_max_probe());
    for _ in 0..200_000 {
        let i = rng.below(u64::from(N));
        let h = t.lookup(&cid(i).to_be_bytes()).unwrap().unwrap();
        assert_eq!(t.local_cid(h).unwrap(), ConnectionId::from_u64(cid(i)));
        assert_eq!(
            t.lookup(&cid(i + u64::from(N)).to_be_bytes()).unwrap(),
            None
        );
    }
    // Spot-check the index against the full SIMD tile scan.
    for _ in 0..20 {
        let i = rng.below(u64::from(N));
        let b = cid(i).to_be_bytes();
        assert_eq!(t.lookup_cid_simd(&b).unwrap(), t.lookup(&b).unwrap());
    }
}
