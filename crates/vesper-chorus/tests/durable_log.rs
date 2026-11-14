//! Wave seven lane 16 (issue #339): the durable Raft log. Real files and
//! a killed process, a simulated disk that loses unsynced data and tears
//! writes, damaged files as typed errors, and Raft clusters whose servers
//! recover from their disks after crashes, including power failures in
//! the middle of a persist.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use vesper_chorus::durable::{
    Disk, DurableLog, FsDisk, PersistentState, SimDisk, StorageError, crc32c,
};
use vesper_chorus::raft::{Entry, RaftConfig, run_cluster};
use vesper_chorus::simnet::SplitMix;

fn runs(default: u64) -> u64 {
    std::env::var("CHORUS_SIM_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("chorus-durable-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn e(term: u64, cmd: u64) -> Entry {
    Entry { term, cmd }
}

#[test]
fn crc32c_matches_the_standard_check_value() {
    // RFC 3720 / iSCSI check value.
    assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    assert_eq!(crc32c(b""), 0);
}

#[test]
fn real_files_round_trip_appends_truncation_and_snapshots() {
    let dir = scratch("roundtrip");
    let (mut log, rec) = DurableLog::open(FsDisk::new(&dir).unwrap()).unwrap();
    assert!(rec.fresh);
    log.set_term_vote(3, Some(1)).unwrap();
    log.append(&[e(1, 10), e(1, 11), e(2, 12), e(3, 13), e(3, 14)])
        .unwrap();
    log.sync().unwrap();
    drop(log);
    let (mut log, rec) = DurableLog::open(FsDisk::new(&dir).unwrap()).unwrap();
    assert!(!rec.fresh && !rec.rewrote_log && rec.records == 5);
    assert_eq!(log.state().term, 3);
    assert_eq!(log.state().voted_for, Some(1));
    assert_eq!(log.state().log.len(), 5);
    // A conflicting suffix is cut on disk.
    log.truncate_after(3).unwrap();
    log.append(&[e(4, 20)]).unwrap();
    log.sync().unwrap();
    // Compaction: snapshot of 1..=3, log keeps 4.
    let full = log.state().full_log();
    log.install(3, 2, &full[..3], &full[3..]).unwrap();
    log.append(&[e(4, 21)]).unwrap();
    log.sync().unwrap();
    let want = log.state().clone();
    drop(log);
    let (log, rec) = DurableLog::open(FsDisk::new(&dir).unwrap()).unwrap();
    assert_eq!(*log.state(), want);
    assert_eq!(
        rec.records, 2,
        "the rewritten log starts after the snapshot"
    );
    assert_eq!(
        log.state().full_log(),
        vec![e(1, 10), e(1, 11), e(2, 12), e(4, 20), e(4, 21)]
    );
    // Invalid calls are typed errors.
    let (mut log, _) = DurableLog::open(FsDisk::new(&dir).unwrap()).unwrap();
    assert!(matches!(
        log.truncate_after(1),
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        log.set_term_vote(2, None),
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        log.set_term_vote(3, Some(2)),
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        log.install(2, 1, &full[..2], &[]),
        Err(StorageError::Invalid(_))
    ));
    assert!(matches!(
        log.install(4, 9, &full[..4], &[]),
        Err(StorageError::Invalid(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// One random step of a Raft server's persistent state.
fn mutate(rng: &mut SplitMix, s: &PersistentState) -> PersistentState {
    let mut t = s.clone();
    let mut full = t.full_log();
    match rng.range(0, 9) {
        0 => {
            t.term += 1;
            t.voted_for = rng.chance(500).then(|| rng.range(0, 4) as u32);
        }
        1 if t.voted_for.is_none() => t.voted_for = Some(rng.range(0, 4) as u32),
        2 | 3 if full.len() as u64 > t.snapshot_index => {
            // A follower's conflicting suffix, replaced by a new leader's.
            let keep = rng.range(t.snapshot_index, full.len() as u64) as usize;
            full.truncate(keep);
            t.term += rng.range(0, 1);
            for _ in 0..rng.range(0, 4) {
                full.push(e(t.term, rng.next_u64()));
            }
            t.log = full[t.snapshot_index as usize..].to_vec();
        }
        4 if full.len() as u64 > t.snapshot_index + 2 => {
            // Compaction of a committed prefix.
            let at = rng.range(t.snapshot_index + 1, full.len() as u64);
            t.snapshot_index = at;
            t.snapshot_term = full[at as usize - 1].term;
            t.snapshot_image = full[..at as usize].to_vec();
            t.log = full[at as usize..].to_vec();
        }
        5 => {
            // InstallSnapshot from a leader past our snapshot. Log Matching
            // holds: either the image agrees with our log up to its point
            // (we keep the rest), or it ends in a newer leader's term (we
            // keep nothing).
            let snap = t.snapshot_index as usize;
            if full.len() > snap + 1 && rng.chance(500) {
                let at = rng.range(snap as u64 + 1, full.len() as u64) as usize;
                t.snapshot_index = at as u64;
                t.snapshot_term = full[at - 1].term;
                t.snapshot_image = full[..at].to_vec();
                t.log = full[at..].to_vec();
            } else {
                let term = full.iter().map(|e| e.term).max().unwrap_or(0).max(t.term) + 1;
                t.term = term;
                t.voted_for = None;
                let mut image = t.snapshot_image.clone();
                for _ in 0..rng.range(1, 6) {
                    image.push(e(term, rng.next_u64()));
                }
                t.snapshot_index = image.len() as u64;
                t.snapshot_term = term;
                t.snapshot_image = image;
                t.log = Vec::new();
            }
        }
        _ => {
            let term = t.term.max(1);
            for _ in 0..rng.range(1, 5) {
                t.log.push(e(term, rng.next_u64()));
            }
        }
    }
    t
}

/// The recovered state lies between the last synced state and the target
/// (meta and snapshot replacements are synced as they are written).
fn between(old: &PersistentState, new: &PersistentState, got: &PersistentState) -> bool {
    let (o, n, g) = (old.full_log(), new.full_log(), got.full_log());
    let shared = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let log_ok = g == o || (n.starts_with(&g) && g.len() >= shared);
    log_ok
        && (got.term, got.voted_for) == (new.term, new.voted_for)
        && (got.snapshot_index, got.snapshot_term) == (new.snapshot_index, new.snapshot_term)
}

#[test]
fn power_failures_on_the_simulated_disk_keep_synced_state() {
    let seeds = runs(300);
    let (mut crashes, mut lost, mut torn, mut dir_ops, mut rewrites) = (0, 0, 0, 0, 0);
    let mut partial = 0;
    for seed in 0..seeds {
        let mut rng = SplitMix(seed);
        let (mut log, _) = DurableLog::open(SimDisk::new(seed)).unwrap();
        let mut synced = PersistentState::default();
        for _ in 0..120 {
            let target = mutate(&mut rng, &synced);
            if rng.chance(250) {
                // Power fails after the writes, before the log sync.
                log.persist_unsynced(&target).unwrap();
                let mut disk = log.into_disk();
                disk.crash();
                let (l, rec) = DurableLog::open(disk).unwrap();
                crashes += 1;
                if rec.rewrote_log {
                    rewrites += 1;
                }
                let got = l.state().clone();
                assert!(
                    between(&synced, &target, &got),
                    "seed {seed}: recovered {got:?}\nsynced {synced:?}\ntarget {target:?}"
                );
                if got != synced && got != target {
                    partial += 1;
                }
                synced = got;
                log = l;
            } else {
                log.persist(&target).unwrap();
                synced = target;
                if rng.chance(100) {
                    // Power fails after a completed persist: nothing lost.
                    let mut disk = log.into_disk();
                    disk.crash();
                    let (l, _) = DurableLog::open(disk).unwrap();
                    assert_eq!(*l.state(), synced, "seed {seed}");
                    log = l;
                }
            }
            assert_eq!(*log.state(), synced);
        }
        let st = log.disk().stats;
        lost += st.lost_bytes;
        torn += st.torn_bytes;
        dir_ops += st.lost_dir_ops;
    }
    eprintln!(
        "{seeds} seeds: {crashes} torn persists, {partial} recovered a partial append, \
         {rewrites} log rewrites, {lost} unsynced bytes lost, {torn} garbage bytes, \
         {dir_ops} directory operations lost"
    );
    if seeds >= 100 {
        assert!(crashes > 1_000 && partial > 0 && lost > 0 && torn > 0 && rewrites > 0);
    }
}

#[test]
fn power_failures_inside_atomic_replacements_recover_old_or_new() {
    let seeds = runs(300);
    let (mut fails, mut old, mut new, mut mixed) = (0u64, 0u64, 0u64, 0u64);
    for seed in 0..seeds {
        let mut rng = SplitMix(seed ^ 0xa70);
        let (mut log, _) = DurableLog::open(SimDisk::new(seed)).unwrap();
        let mut synced = PersistentState::default();
        for _ in 0..60 {
            let target = mutate(&mut rng, &synced);
            let ops = rng.range(0, 12);
            log.disk_mut().power_fail_after(ops);
            match log.persist(&target) {
                Ok(()) => {
                    synced = target;
                    let mut disk = log.into_disk();
                    disk.crash();
                    let (l, _) = DurableLog::open(disk).unwrap();
                    assert_eq!(*l.state(), synced, "seed {seed}");
                    log = l;
                }
                Err(StorageError::Io(_)) => {
                    fails += 1;
                    let mut disk = log.into_disk();
                    disk.crash();
                    let (l, _) = DurableLog::open(disk).unwrap();
                    let got = l.state().clone();
                    // Every file is the old or the new version: the term,
                    // vote and snapshot point each come from one of the two
                    // states, and the log is the old one or a prefix of the
                    // new one that keeps what they share.
                    let (o, n, g) = (synced.full_log(), target.full_log(), got.full_log());
                    let shared = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
                    assert!(
                        g == o || (n.starts_with(&g) && g.len() >= shared),
                        "seed {seed}: {got:?}\n{synced:?}\n{target:?}"
                    );
                    for (x, a, b) in [
                        (got.term, synced.term, target.term),
                        (
                            got.snapshot_index,
                            synced.snapshot_index,
                            target.snapshot_index,
                        ),
                    ] {
                        assert!(x == a || x == b, "seed {seed}");
                    }
                    assert!(got.voted_for == synced.voted_for || got.voted_for == target.voted_for);
                    if got == synced {
                        old += 1;
                    } else if got == target {
                        new += 1;
                    } else {
                        mixed += 1;
                    }
                    synced = got;
                    log = l;
                }
                Err(e) => panic!("seed {seed}: {e}"),
            }
        }
    }
    eprintln!("{seeds} seeds: {fails} power failures: {old} old, {new} new, {mixed} in between");
    if seeds >= 100 {
        assert!(fails > 1_000 && old > 0 && new > 0 && mixed > 0);
    }
}

#[test]
fn damaged_files_are_typed_errors() {
    let build = || {
        let (mut log, _) = DurableLog::open(SimDisk::new(1)).unwrap();
        log.set_term_vote(2, Some(0)).unwrap();
        log.append(&[e(1, 1), e(1, 2), e(2, 3), e(2, 4)]).unwrap();
        log.sync().unwrap();
        let full = log.state().full_log();
        log.install(2, 1, &full[..2], &full[2..]).unwrap();
        log.append(&[e(2, 5), e(2, 6), e(2, 7)]).unwrap();
        log.sync().unwrap();
        log.into_disk()
    };
    let corrupt = |name: &str, at: usize| {
        let mut d = build();
        d.corrupt(name, at, 0x40).unwrap();
        match DurableLog::open(d) {
            Err(StorageError::Corrupt { .. }) => {}
            other => panic!("{name}@{at}: {other:?}"),
        }
    };
    corrupt("meta", 6);
    corrupt("snapshot", 10);
    corrupt("snapshot", 30);
    corrupt("log", 2); // header
    // A damaged record with valid records after it: not a torn tail.
    corrupt("log", 16 + 32 + 12);
    // A damaged last record is a torn tail: dropped, the rest kept.
    let mut d = build();
    let len = d.read("log").unwrap().unwrap().len();
    d.corrupt("log", len - 3, 1).unwrap();
    let (l, rec) = DurableLog::open(d).unwrap();
    assert_eq!(rec.torn_bytes, 32);
    assert!(rec.rewrote_log);
    assert_eq!(l.state().last_index(), 6);
    // Random bytes in every file never panic.
    let mut rng = SplitMix(9);
    for i in 0..3_000u64 {
        let mut d = build();
        let name = ["meta", "snapshot", "log"][(i % 3) as usize];
        let n = d.read(name).unwrap().unwrap().len();
        for _ in 0..rng.range(1, 4) {
            let at = rng.range(0, n as u64 - 1) as usize;
            d.corrupt(name, at, (rng.next_u64() as u8) | 1).unwrap();
        }
        if rng.chance(300) {
            let cut = rng.range(0, n as u64);
            d.truncate(name, cut).unwrap();
            d.sync(name).unwrap();
        }
        if let Ok((l, _)) = DurableLog::open(d) {
            // Whatever was accepted is internally consistent.
            let s = l.state();
            assert_eq!(s.snapshot_image.len() as u64, s.snapshot_index);
        }
    }
}

// ------------------------------------------------- a real killed process

const CHILD_ENV: &str = "CHORUS_DURABLE_CRASH_CHILD";

fn digest(s: &PersistentState) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    };
    mix(s.term);
    mix(s.voted_for.map_or(u64::MAX, u64::from));
    mix(s.snapshot_index);
    mix(s.snapshot_term);
    for e in s.full_log() {
        mix(e.term);
        mix(e.cmd);
    }
    h
}

/// The child: persists random Raft states into `$CHORUS_DURABLE_CRASH_CHILD`
/// and aborts the process at a random point, possibly between a write and
/// its sync. Run by `a_killed_process_recovers_its_synced_state`.
#[test]
#[ignore = "run as a child process by a_killed_process_recovers_its_synced_state"]
fn durable_crash_child() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let seed: u64 = std::env::var("CHORUS_DURABLE_CRASH_SEED")
        .unwrap()
        .parse()
        .unwrap();
    let mut rng = SplitMix(seed);
    let (mut log, _) = DurableLog::open(FsDisk::new(&dir).unwrap()).unwrap();
    let mut state = log.state().clone();
    let stop = rng.range(5, 80);
    for step in 0.. {
        let target = mutate(&mut rng, &state);
        println!("target {}", digest(&target));
        if step == stop {
            // Die after the writes and before the sync.
            log.persist_unsynced(&target).unwrap();
            std::process::abort();
        }
        log.persist(&target).unwrap();
        println!("synced {}", digest(&target));
        state = target;
    }
}

#[test]
fn a_killed_process_recovers_its_synced_state() {
    let exe = std::env::current_exe().unwrap();
    for seed in 0..runs(12).min(40) {
        let dir = scratch(&format!("kill{seed}"));
        // Several lives per directory: each child continues from what the
        // previous one left.
        for life in 0..3u64 {
            let mut child = Command::new(&exe)
                .args(["--exact", "durable_crash_child", "--ignored", "--nocapture"])
                .args(["--test-threads", "1"])
                .env(CHILD_ENV, &dir)
                .env("CHORUS_DURABLE_CRASH_SEED", (seed * 7 + life).to_string())
                .env("RUST_BACKTRACE", "0")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let (mut last_target, mut last_synced) = (None, None);
            for line in BufReader::new(child.stdout.take().unwrap()).lines() {
                let line = line.unwrap();
                if let Some(d) = line.strip_prefix("target ") {
                    last_target = Some(d.parse::<u64>().unwrap());
                } else if let Some(d) = line.strip_prefix("synced ") {
                    last_synced = Some(d.parse::<u64>().unwrap());
                }
            }
            let status = child.wait().unwrap();
            assert!(!status.success(), "the child aborts");
            let (log, _) = DurableLog::open(FsDisk::new(&dir).unwrap()).unwrap();
            let got = digest(log.state());
            // The process died, not the machine: written bytes are in the
            // page cache, so the state is the last synced one or the one
            // being written when it died.
            assert!(
                Some(got) == last_synced
                    || Some(got) == last_target
                    || (last_synced.is_none() && *log.state() == PersistentState::default()),
                "seed {seed} life {life}: recovered {got}, synced {last_synced:?}, target {last_target:?}"
            );
            assert!(last_target.is_some(), "the child ran");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ------------------------------------------------- Raft on durable disks

#[test]
fn durable_storage_is_transparent_without_power_failures() {
    for seed in 0..runs(40) {
        let mut cfg = RaftConfig::new(5);
        cfg.snapshot_threshold = 6;
        let a = run_cluster(seed, &cfg, None).unwrap();
        cfg.durable = true;
        let b = run_cluster(seed, &cfg, None).unwrap();
        assert!(b.violation.is_none(), "seed {seed}: {:?}", b.violation);
        assert_eq!(a.digest, b.digest, "seed {seed}");
        assert_eq!(b.stats.recoveries, b.stats.crashes.min(b.stats.recoveries));
        assert!(b.stats.persists > 0 && b.stats.disk_syncs > 0);
    }
}

#[test]
fn raft_servers_recover_from_disk_after_crashes_and_torn_persists() {
    let seeds = runs(300);
    let mut tot = vesper_chorus::raft::RaftStats::default();
    let mut committed = 0;
    for seed in 0..seeds {
        let mut cfg = RaftConfig::new(5);
        cfg.snapshot_threshold = 6;
        cfg.durable = true;
        cfg.torn_crash_per_mille = 20;
        let r = run_cluster(seed, &cfg, None).unwrap();
        assert!(r.violation.is_none(), "seed {seed}: {:?}", r.violation);
        assert_eq!(r.open_sessions, 0);
        committed += r.committed;
        let s = &r.stats;
        tot.crashes += s.crashes;
        tot.recoveries += s.recoveries;
        tot.torn_crashes += s.torn_crashes;
        tot.torn_entries_lost += s.torn_entries_lost;
        tot.torn_bytes_dropped += s.torn_bytes_dropped;
        tot.disk_truncations += s.disk_truncations;
        tot.disk_snapshots += s.disk_snapshots;
        tot.disk_syncs += s.disk_syncs;
        tot.snapshots_installed += s.snapshots_installed;
    }
    eprintln!(
        "{seeds} seeds: {} crashes, {} recoveries, {} torn persists losing {} entries \
         ({} torn bytes dropped), {} disk truncations, {} snapshot writes ({} installs), \
         {} log syncs, {committed} committed entries",
        tot.crashes,
        tot.recoveries,
        tot.torn_crashes,
        tot.torn_entries_lost,
        tot.torn_bytes_dropped,
        tot.disk_truncations,
        tot.disk_snapshots,
        tot.snapshots_installed,
        tot.disk_syncs,
    );
    if seeds >= 100 {
        assert!(tot.recoveries > tot.torn_crashes && tot.torn_crashes > 100);
        assert!(tot.torn_entries_lost > 0 && tot.disk_truncations > 0);
        assert!(tot.disk_snapshots > 0 && tot.snapshots_installed > 0);
    }
    // Torn crashes need durable storage.
    let mut cfg = RaftConfig::new(3);
    cfg.torn_crash_per_mille = 5;
    assert!(run_cluster(1, &cfg, None).is_err());
}

/// A disk that loses a synced record (a lying fsync) is not mistaken for a
/// torn write: the recovered state is outside the allowed range, which is
/// what the cluster's `RecoveryLostState` check reports.
#[test]
fn losing_synced_entries_is_detected() {
    let (mut log, _) = DurableLog::open(SimDisk::new(3)).unwrap();
    log.append(&[e(1, 1), e(1, 2), e(1, 3)]).unwrap();
    log.sync().unwrap();
    let synced = log.state().clone();
    let mut d = log.into_disk();
    // The disk forgets a synced record (as a lying fsync would).
    let n = d.read("log").unwrap().unwrap().len() as u64;
    d.truncate("log", n - 32).unwrap();
    d.sync("log").unwrap();
    let (l, _) = DurableLog::open(d).unwrap();
    assert_ne!(*l.state(), synced);
    assert!(!between(&synced, &synced, l.state()));
}

#[test]
fn answering_before_the_sync_breaks_safety() {
    // The same power failures, but the server's answers from the torn step
    // leave anyway: acknowledged entries vanish from its disk, and the
    // checker finds committed entries that diverge or lost leaders' logs.
    let mut found = None;
    for seed in 0..runs(400) {
        let mut cfg = RaftConfig::new(3);
        cfg.durable = true;
        cfg.torn_crash_per_mille = 400;
        cfg.reply_before_sync = true;
        let r = run_cluster(seed, &cfg, None).unwrap();
        if let Some(v) = r.violation {
            found = Some((seed, v));
            break;
        }
    }
    eprintln!("first violation: {found:?}");
    let (last, _) = found.expect("the unsafe variant was not caught");
    // The safe server on the same seeds and failure rate: no violation.
    for seed in 0..=last.max(100) {
        let mut cfg = RaftConfig::new(3);
        cfg.durable = true;
        cfg.torn_crash_per_mille = 400;
        let r = run_cluster(seed, &cfg, None).unwrap();
        assert!(r.violation.is_none(), "seed {seed}: {:?}", r.violation);
        assert!(r.stats.torn_crashes > 0);
    }
}
