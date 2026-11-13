//! Raft depth in the SimPulse cluster (wave six lane 17): snapshots with
//! InstallSnapshot, and single-server membership change, under the same
//! chaos (partitions, loss, duplication, skew, crash/restart) and checks.

use vesper_chorus::raft::{CONFIG_FLAG, RaftConfig, SNAPSHOT_CHOREOGRAPHY, config_of, run_cluster};
use vesper_chorus::{parse_choreography, project};

fn runs(default: u64) -> u64 {
    std::env::var("CHORUS_SIM_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
fn snapshot_choreography_projects() {
    let c = parse_choreography(SNAPSHOT_CHOREOGRAPHY).unwrap();
    let l = project(&c, "Leader").unwrap().render(&c);
    let f = project(&c, "Follower").unwrap().render(&c);
    assert_eq!(
        l,
        "Follower!InstallSnapshot(SnapshotImage); Follower?Installed(AppendAck); end"
    );
    assert_eq!(
        f,
        "Leader?InstallSnapshot(SnapshotImage); do persist; Leader!Installed(AppendAck); end"
    );
}

#[test]
fn snapshots_keep_safety_and_catch_up_lagging_followers() {
    let mut cfg = RaftConfig::new(5);
    cfg.snapshot_threshold = 6;
    let (mut sent, mut installed, mut compactions, mut committed) = (0, 0, 0, 0);
    let count = runs(400);
    for seed in 0..count {
        let r = run_cluster(seed, &cfg, None).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        assert_eq!(r.violation, None, "seed {seed}");
        assert_eq!(r.audit.leaked, 0, "seed {seed}");
        assert_eq!(r.open_sessions, 0, "seed {seed}");
        assert_eq!(r.stats.rejected_frames, 0, "seed {seed}");
        sent += r.stats.snapshots_sent;
        installed += r.stats.snapshots_installed;
        compactions += r.stats.compactions;
        committed += r.committed;
    }
    eprintln!(
        "{count} seeds: {compactions} compactions, {sent} snapshots sent, {installed} installed, {committed} committed"
    );
    assert!(compactions > 0 && sent > 0 && installed > 0);
    assert!(committed as u64 > 5 * count);
}

#[test]
fn single_server_membership_changes_keep_safety() {
    let mut cfg = RaftConfig::new(7);
    cfg.initial_members = 5;
    cfg.membership_per_mille = 200;
    cfg.snapshot_threshold = 8;
    let (mut changes, mut step_downs, mut committed, mut grew, mut shrank) = (0, 0, 0, 0, 0);
    let count = runs(400);
    for seed in 0..count {
        let r = run_cluster(seed, &cfg, None).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        assert_eq!(r.violation, None, "seed {seed}");
        assert_eq!(r.audit.leaked, 0, "seed {seed}");
        assert_eq!(r.open_sessions, 0, "seed {seed}");
        assert_eq!(r.stats.rejected_frames, 0, "seed {seed}");
        changes += r.stats.config_changes;
        step_downs += r.stats.leader_step_downs;
        committed += r.committed;
        for m in &r.members {
            let k = m.count_ones();
            assert!(k >= cfg.min_members, "seed {seed}: {m:#b}");
            grew += u64::from(k > 5);
            shrank += u64::from(k < 5);
        }
    }
    eprintln!(
        "{count} seeds: {changes} config changes, {step_downs} leader step-downs, {committed} committed, {grew}/{shrank} node views grown/shrunk"
    );
    assert!(changes > 0 && grew > 0 && shrank > 0);
    assert!(committed as u64 > 3 * count);
}

#[test]
fn membership_configuration_is_read_from_the_newest_entry() {
    use vesper_chorus::raft::Entry;
    let e = |term, cmd| Entry { term, cmd };
    assert_eq!(config_of(&[], 0b111), 0b111);
    assert_eq!(
        config_of(&[e(1, 5), e(1, CONFIG_FLAG | 0b1011)], 0b111),
        0b1011
    );
    assert_eq!(
        config_of(
            &[
                e(1, CONFIG_FLAG | 0b1011),
                e(2, 9),
                e(2, CONFIG_FLAG | 0b11)
            ],
            0b111
        ),
        0b11
    );
}

#[test]
fn bad_depth_configuration_is_rejected() {
    let mut cfg = RaftConfig::new(5);
    cfg.initial_members = 6;
    assert!(run_cluster(1, &cfg, None).is_err());
    cfg.initial_members = 0;
    assert!(run_cluster(1, &cfg, None).is_err());
    let mut cfg = RaftConfig::new(5);
    cfg.snapshot_threshold = 4;
    cfg.max_log = 1000;
    assert!(run_cluster(1, &cfg, None).is_err());
}
