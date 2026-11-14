//! A Raft-style replicated log over typed sessions on lossy Warp links
//! (wave six lane 17 acceptance).

mod common;

use vesper_chorus::raft::log_matching_violation;
use vesper_chorus::replog::{ReplogConfig, run_replicated_log};
use vesper_chorus::warpnet::WarpNetConfig;

fn cfg(seed: u64, nodes: u32) -> ReplogConfig {
    let d = ReplogConfig::default();
    ReplogConfig {
        net: WarpNetConfig {
            seed,
            nodes,
            ..d.net
        },
        ..d
    }
}

#[test]
fn replicated_log_converges_over_lossy_partitioned_warp_links() {
    let mut faults = 0;
    let mut rejected = 0;
    let mut dropped = 0;
    let mut retransmitted = 0;
    for (seed, nodes) in [(1, 5), (2, 5), (3, 3), (4, 7), (5, 5), (6, 3)] {
        let r = run_replicated_log(&cfg(seed, nodes), &common::pki()).unwrap();
        assert_eq!(r.violation, None, "seed {seed}");
        // Every log equals the final leader's, and all of it is committed.
        let first = &r.logs[0];
        assert!(!first.is_empty());
        for (i, l) in r.logs.iter().enumerate() {
            assert_eq!(l, first, "seed {seed}: node {i}");
            assert_eq!(r.commit[i], first.len() as u64);
        }
        assert_eq!(r.committed, first.len());
        for a in &r.logs {
            for b in &r.logs {
                assert_eq!(log_matching_violation(a, b), None);
            }
        }
        // At most one leader per term (the checker would have reported it),
        // and at least one election was won.
        assert!(!r.leaders.is_empty());
        assert!(r.checks > 0);
        // Every session left through a documented exit.
        assert_eq!(r.open_sessions, 0, "seed {seed}: {:?}", r.audit);
        assert_eq!(r.audit.leaked, 0, "seed {seed}");
        assert!(r.settled, "seed {seed}");
        assert_eq!(
            r.open_streams, 0,
            "seed {seed}: streams left open: {:?}",
            r.open_stream_details
        );
        assert_eq!(r.net.events_dropped, 0);
        faults += r.stats.rpc_faults;
        rejected += r.stats.rejected;
        dropped += r.net.dropped;
        retransmitted += r.net.stream_bytes_retransmitted;
    }
    // The scenarios exercised partitions (RPC timeouts), log repair
    // (rejections and back-off), datagram loss and QUIC retransmission.
    assert!(
        faults > 0 && rejected > 0,
        "faults {faults}, rejected {rejected}"
    );
    assert!(
        dropped > 0 && retransmitted > 0,
        "dropped {dropped}, retransmitted {retransmitted}"
    );
}

#[test]
fn replicated_log_is_deterministic_per_seed() {
    let a = run_replicated_log(&cfg(42, 5), &common::pki()).unwrap();
    let b = run_replicated_log(&cfg(42, 5), &common::pki()).unwrap();
    assert_eq!(a.digest, b.digest);
    assert_eq!(a.stats, b.stats);
    assert_eq!(a.net, b.net);
    let c = run_replicated_log(&cfg(43, 5), &common::pki()).unwrap();
    assert_ne!(a.digest, c.digest);
}

#[test]
fn replicated_log_rejects_bad_configuration() {
    assert!(run_replicated_log(&cfg(1, 2), &common::pki()).is_err());
    assert!(run_replicated_log(&cfg(1, 8), &common::pki()).is_err());
    let mut c = cfg(1, 3);
    c.max_batch = 0;
    assert!(run_replicated_log(&c, &common::pki()).is_err());
}
