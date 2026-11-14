//! Regressions for review findings on PR #139.

use vesper_chrono::simpulse::SimPulseTap;
use vesper_chrono::{
    Bundle, BundleKey, BundleLimits, FlightEvent, FlightRecorder, RecorderConfig, RedactionPolicy,
    ReplayError, ReplaySession, Replayer,
};
use vesper_simpulse::Simulator;

/// Sums the first payload byte of effect 1.
struct Sum;
impl Replayer for Sum {
    type State = u64;
    fn initial(&self) -> u64 {
        0
    }
    fn restore(&self, image: &[u8]) -> Result<u64, String> {
        Ok(u64::from_le_bytes(image.try_into().map_err(|_| "image")?))
    }
    fn models(&self, effect_id: u16) -> bool {
        effect_id == 1
    }
    fn apply(&self, state: &mut u64, ev: &FlightEvent<'_>) -> Result<(), String> {
        if ev.effect_id == 1 {
            *state += u64::from(ev.payload[0]);
        }
        Ok(())
    }
}

fn key() -> BundleKey {
    BundleKey::new([1; 32])
}

fn session(rec: &FlightRecorder) -> ReplaySession {
    let bytes = Bundle::export(rec, &RedactionPolicy::new(), b"", &key()).unwrap();
    ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap()
}

fn config(max_records: usize) -> RecorderConfig {
    RecorderConfig {
        ring_bytes: 4096,
        max_records,
        max_payload: 64,
        snapshot_slots: 2,
        snapshot_image_bytes: 8,
    }
}

#[test]
fn digest_records_of_modelled_effects_are_not_replayed() {
    let mut rec = FlightRecorder::new(config(16)).unwrap();
    rec.record_effect(0, 0, 1, 0, &[5]).unwrap();
    rec.record_effect_digest(0, 0, 2, 0, b"unmodelled outcome")
        .unwrap();
    rec.record_effect(0, 0, 1, 0, &[7]).unwrap();
    rec.record_effect_digest(0, 0, 1, 0, &[9]).unwrap();
    let s = session(&rec);
    assert_eq!(
        s.state_at(&Sum, 3),
        Ok(12),
        "digest of an unmodelled effect is skipped"
    );
    assert_eq!(
        s.state_at(&Sum, 4),
        Err(ReplayError::NotReplayable { lamport_seq: 4 })
    );
    let _ = s.close();
}

#[test]
fn snapshot_stays_usable_when_only_its_marker_was_evicted() {
    let mut rec = FlightRecorder::new(config(2)).unwrap();
    rec.capture_snapshot(0, &40u64.to_le_bytes()).unwrap(); // seq 1
    rec.record_effect(0, 0, 1, 0, &[2]).unwrap(); // seq 2
    rec.record_effect(0, 0, 1, 0, &[3]).unwrap(); // seq 3, evicts the marker
    let s = session(&rec);
    assert_eq!(s.bundle().first_seq, 2);
    assert_eq!(s.state_at(&Sum, 2), Ok(42));
    assert_eq!(s.state_at(&Sum, 3), Ok(45));
    let _ = s.close();
}

#[test]
fn a_failed_tap_drain_never_duplicates_records_on_retry() {
    let mut rec = FlightRecorder::new(RecorderConfig {
        ring_bytes: 4096,
        max_records: 64,
        max_payload: 32,
        snapshot_slots: 0,
        snapshot_image_bytes: 0,
    })
    .unwrap();
    let mut sim = Simulator::new(1);
    sim.spawn(()).unwrap();
    let token = sim.next_task().unwrap().unwrap(); // Dispatch JSON exceeds 32 bytes
    let mut tap = SimPulseTap::new();
    assert!(tap.drain(&sim, &mut rec).is_err());
    assert!(tap.drain(&sim, &mut rec).is_err());
    assert_eq!(rec.len(), 1, "Spawn recorded once");
    sim.finish(token).unwrap();
}
