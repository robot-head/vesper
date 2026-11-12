//! Seeded Raft-style replication under SimPulse: 5 and 7 nodes, partitions,
//! loss, reordering, duplication, clock skew, crash/restart.

use vesper_chorus::raft::{
    APPEND_CHOREOGRAPHY, APPEND_HEADER_LEN, AppendAck, Entry, QuorumRule, RaftAppendWireHeader,
    RaftConfig, VOTE_CHOREOGRAPHY, Violation, VoteReq, WireError, decode_batch, encode_batch,
    log_matching_violation, run_cluster,
};
use vesper_chorus::{parse_choreography, project};

/// Seeds per cluster size; `CHORUS_SIM_RUNS` overrides.
fn runs(default: u64) -> u64 {
    std::env::var("CHORUS_SIM_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn sweep(nodes: u32, seeds: std::ops::Range<u64>) {
    let cfg = RaftConfig::new(nodes);
    let (mut leaders, mut committed, mut elections, mut crashes, mut partitioned, mut dups) =
        (0usize, 0usize, 0u64, 0u64, 0u64, 0u64);
    let count = seeds.end - seeds.start;
    for seed in seeds {
        let r = run_cluster(seed, &cfg, None).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        assert_eq!(r.violation, None, "{nodes} nodes, seed {seed}");
        assert_eq!(r.end_ns, cfg.duration_ns, "seed {seed} ran to the end");
        assert_eq!(
            r.audit.leaked, 0,
            "seed {seed}: no session dropped silently"
        );
        assert_eq!(
            r.open_sessions, 0,
            "seed {seed}: every session closed or faulted"
        );
        assert_eq!(
            r.stats.rejected_frames, 0,
            "seed {seed}: honest nodes follow the projection"
        );
        leaders += r.leaders.len();
        committed += r.committed;
        elections += r.stats.elections;
        crashes += r.stats.crashes;
        partitioned += r.stats.partitioned;
        dups += r.stats.duplicated;
    }
    eprintln!(
        "{nodes} nodes, {count} seeds: {leaders} leaders, {elections} elections, {committed} committed entries, {crashes} crashes, {partitioned} partitioned frames, {dups} duplicated frames"
    );
    // The chaos actually happened and the cluster still made progress.
    assert!(crashes > 0 && partitioned > 0 && dups > 0);
    assert!(committed as u64 > 5 * count, "replication made progress");
}

#[test]
fn five_node_cluster_keeps_election_safety_and_log_matching() {
    sweep(5, 0..runs(2000));
}

#[test]
fn seven_node_cluster_keeps_election_safety_and_log_matching() {
    sweep(7, 10_000..10_000 + runs(1000));
}

#[test]
fn non_majority_quorum_is_caught() {
    for nodes in [5, 7] {
        let mut cfg = RaftConfig::new(nodes);
        cfg.quorum = QuorumRule::HalfFloor;
        let mut caught = Vec::new();
        for seed in 0..300 {
            let r = run_cluster(seed, &cfg, None).unwrap();
            if let Some((_, v)) = r.violation {
                caught.push((seed, v));
            }
        }
        eprintln!(
            "{nodes} nodes, half-floor quorum: {} of 300 seeds violated; first {:?}",
            caught.len(),
            caught.first()
        );
        assert!(
            !caught.is_empty(),
            "{nodes}: the checker must catch the buggy quorum"
        );
        assert!(
            caught
                .iter()
                .any(|(_, v)| matches!(v, Violation::ElectionSafety { .. }))
        );
    }
    // The first 5-node seed it catches, pinned.
    let mut cfg = RaftConfig::new(5);
    cfg.quorum = QuorumRule::HalfFloor;
    let r = run_cluster(16, &cfg, None).unwrap();
    assert!(
        matches!(
            r.violation,
            Some((_, Violation::ElectionSafety { term: 1, .. }))
        ),
        "{:?}",
        r.violation
    );
}

#[test]
fn runs_replay_bit_identically() {
    for (nodes, seed) in [(5, 3u64), (5, 77), (7, 10_001), (7, 424_242)] {
        let cfg = RaftConfig::new(nodes);
        let a = run_cluster(seed, &cfg, None).unwrap();
        let b = run_cluster(seed, &cfg, None).unwrap();
        assert_eq!(a.journal, b.journal);
        assert_eq!(a.digest, b.digest);
        assert_eq!(a.stats, b.stats);
        // Replaying the SimPulse journal checks every scheduling event.
        let c = run_cluster(seed, &cfg, Some(a.journal.clone())).unwrap();
        assert_eq!(c.digest, a.digest);
        assert_eq!(c.journal, a.journal);
        // JSON round trip of the journal replays too.
        let json = a.journal.to_json().unwrap();
        let j = vesper_simpulse::Journal::from_json(&json).unwrap();
        assert_eq!(run_cluster(seed, &cfg, Some(j)).unwrap().digest, a.digest);
        // A different seed differs.
        assert_ne!(run_cluster(seed + 1, &cfg, None).unwrap().digest, a.digest);
    }
    // A tampered journal is refused.
    let cfg = RaftConfig::new(5);
    let mut j = run_cluster(9, &cfg, None).unwrap().journal;
    j.seed ^= 1;
    assert!(run_cluster(9, &cfg, Some(j)).is_err());
}

#[test]
fn rpc_choreographies_project_as_documented() {
    let vote = parse_choreography(VOTE_CHOREOGRAPHY).unwrap();
    assert_eq!(
        project(&vote, "Voter").unwrap().render(&vote),
        "Candidate?RequestVote(VoteReq); select Candidate { Deny(VoteResp): end, Grant(VoteResp): end }"
    );
    let append = parse_choreography(APPEND_CHOREOGRAPHY).unwrap();
    assert_eq!(
        project(&append, "Follower").unwrap().render(&append),
        "Leader?AppendEntries(AppendBatch); do persist; select Leader { Accept(AppendAck): end, Reject(AppendAck): end }"
    );
    assert_eq!(
        project(&append, "Leader").unwrap().render(&append),
        "Follower!AppendEntries(AppendBatch); offer Follower { Accept(AppendAck): end, Reject(AppendAck): end }"
    );
}

#[test]
fn wire_formats_have_exact_lengths_and_reject_malformed_input() {
    let h = RaftAppendWireHeader {
        term: 0x0102_0304_0506_0708,
        leader_id: 9,
        follower_slot: 10,
        prev_log_index: 11,
        prev_log_term: 12,
        leader_commit: 13,
    };
    let bytes = h.encode();
    assert_eq!(bytes.len(), APPEND_HEADER_LEN);
    assert_eq!(APPEND_HEADER_LEN, 40);
    assert_eq!(
        &bytes[..8],
        &[1, 2, 3, 4, 5, 6, 7, 8],
        "big-endian term first"
    );
    assert_eq!(RaftAppendWireHeader::decode(&bytes).unwrap(), h);
    assert!(RaftAppendWireHeader::decode(&bytes[..39]).is_err());
    let entries = vec![Entry { term: 1, cmd: 2 }, Entry { term: 3, cmd: 4 }];
    let b = encode_batch(&h, &entries);
    assert_eq!(decode_batch(&b).unwrap(), (h, entries));
    assert!(matches!(
        decode_batch(&b[..b.len() - 1]),
        Err(WireError::Length { .. })
    ));
    let mut huge = b.clone();
    huge[40..44].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        decode_batch(&huge),
        Err(WireError::TooManyEntries(u32::MAX))
    );
    let ack = AppendAck {
        follower_id: 3,
        term: 4,
        match_index: 5,
        accepted: true,
    };
    let mut a = ack.encode();
    assert_eq!(AppendAck::decode(&a).unwrap(), ack);
    a[20] = 2;
    assert_eq!(AppendAck::decode(&a), Err(WireError::BadFlag(2)));
    let v = VoteReq {
        term: 1,
        candidate: 2,
        last_log_index: 3,
        last_log_term: 4,
    };
    assert_eq!(VoteReq::decode(&v.encode()).unwrap(), v);
    // Arbitrary bytes never panic.
    let mut x = 7u64;
    for len in 0..200 {
        let buf: Vec<u8> = (0..len)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                (x >> 56) as u8
            })
            .collect();
        let _ = decode_batch(&buf);
        let _ = AppendAck::decode(&buf);
        let _ = VoteReq::decode(&buf);
    }
}

#[test]
fn log_matching_checker_detects_divergence() {
    let e = |term, cmd| Entry { term, cmd };
    assert_eq!(
        log_matching_violation(&[e(1, 1), e(2, 2)], &[e(1, 1), e(2, 2), e(2, 3)]),
        None
    );
    // Same index and term, different command.
    assert_eq!(
        log_matching_violation(&[e(1, 1), e(2, 2)], &[e(1, 1), e(2, 9)]),
        Some(1)
    );
    // Same term at index 1 but different history before it.
    assert_eq!(
        log_matching_violation(&[e(1, 1), e(3, 5)], &[e(2, 7), e(3, 5)]),
        Some(1)
    );
    // Diverging terms are allowed.
    assert_eq!(
        log_matching_violation(&[e(1, 1), e(2, 2)], &[e(1, 1), e(3, 2)]),
        None
    );
}

#[test]
fn quorum_sizes() {
    assert_eq!(QuorumRule::Majority.size(5), 3);
    assert_eq!(QuorumRule::Majority.size(7), 4);
    assert_eq!(QuorumRule::HalfFloor.size(5), 2);
    assert_eq!(QuorumRule::HalfFloor.size(7), 3);
    // Majority quorums of every size up to 64 intersect; half-floor ones do
    // not (the property the Lean file proves for majorities).
    for n in 1..=64u32 {
        let q = QuorumRule::Majority.size(n);
        assert!(2 * q > n);
        if n >= 2 {
            assert!(2 * QuorumRule::HalfFloor.size(n) <= n);
        }
    }
}
