//! Record a SimPulse session whose schedule depends on external I/O data,
//! export a `.vchrono` bundle, and replay the program from the bundle alone:
//! identical final state, identical flight-record stream, identical SimPulse
//! journal (checked transition by transition by `Simulator::replay`). A
//! tampered bundle is rejected before replay.

use std::collections::BTreeMap;

use vesper_chrono::simpulse::{SimPulseTap, journal_from_bundle, seed_metadata};
use vesper_chrono::{
    Bundle, BundleError, BundleKey, BundleLimits, FlightEvent, FlightRecorder, HopKind,
    ObligationStatus, RecordedEvent, RecorderConfig, RedactionPolicy, ReplayError, ReplaySession,
    Replayer,
};
use vesper_simpulse::{IoResult, Journal, Simulator, WakeReason};

const NET: u16 = 1;
const RECV: u16 = 1;
const NETWORK_FIBER: u64 = 99;
const SCHED_SEED: u64 = 0x005e_edc4_0001;

fn place_of(task: u64) -> u64 {
    0xACC0_0000_0000_0000 | task
}

struct Task {
    requests_left: u32,
    acc: u64,
    obligation: Option<u64>,
}

trait Network {
    fn deliver(&mut self, task: u64) -> Vec<u8>;
}

/// The live environment: data the replay must never consult.
struct LiveNetwork(u64);
impl Network for LiveNetwork {
    fn deliver(&mut self, _task: u64) -> Vec<u8> {
        let mut next = || {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        };
        let len = 1 + (next() % 16) as usize;
        (0..len).map(|_| next() as u8).collect()
    }
}

/// The replay environment: only the recorded NET records of the bundle.
struct RecordedNetwork {
    responses: std::vec::IntoIter<(u64, Vec<u8>)>,
}
impl RecordedNetwork {
    fn new(bundle: &Bundle) -> Self {
        let responses: Vec<_> = bundle
            .records
            .iter()
            .filter(|r| r.effect_id == NET && r.op_tag == RECV)
            .map(|r| (r.fiber_id, r.payload[8..].to_vec()))
            .collect();
        Self {
            responses: responses.into_iter(),
        }
    }
}
impl Network for RecordedNetwork {
    fn deliver(&mut self, task: u64) -> Vec<u8> {
        let (fiber, data) = self.responses.next().expect("recorded response");
        assert_eq!(fiber, task, "replayed response belongs to the same fiber");
        data
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    accs: BTreeMap<u64, u64>,
    complete: bool,
    journal: Journal,
    /// `delivered` after each NET record, keyed by its sequence.
    states: Vec<(u64, BTreeMap<u64, u64>)>,
    leaked: Vec<u64>,
}

fn encode_state(state: &BTreeMap<u64, u64>) -> Vec<u8> {
    state
        .iter()
        .flat_map(|(k, v)| k.to_le_bytes().into_iter().chain(v.to_le_bytes()))
        .collect()
}

fn decode_state(bytes: &[u8]) -> Result<BTreeMap<u64, u64>, String> {
    if !bytes.len().is_multiple_of(16) {
        return Err("state image length".into());
    }
    Ok(bytes
        .chunks_exact(16)
        .map(|c| {
            (
                u64::from_le_bytes(c[..8].try_into().unwrap()),
                u64::from_le_bytes(c[8..].try_into().unwrap()),
            )
        })
        .collect())
}

fn sum(data: &[u8]) -> u64 {
    data.iter().map(|b| u64::from(*b)).sum()
}

/// The embedder program. Identical code runs live and in replay; only the
/// network and the simulator's history differ.
fn run(mut sim: Simulator<Task>, net: &mut dyn Network, rec: &mut FlightRecorder) -> Outcome {
    let mut tap = SimPulseTap::new();
    for i in 0..4u32 {
        sim.spawn(Task {
            requests_left: 3 + i,
            acc: 0,
            obligation: None,
        })
        .unwrap();
    }
    tap.drain(&sim, rec).unwrap();
    let mut accs = BTreeMap::new();
    let mut delivered: BTreeMap<u64, u64> = BTreeMap::new();
    let mut states = Vec::new();
    let mut mailbox = BTreeMap::new();
    let mut leaked = Vec::new();
    let mut dispatches = 0u32;
    while let Some(mut token) = sim.next_task().unwrap() {
        tap.drain(&sim, rec).unwrap();
        dispatches += 1;
        if dispatches.is_multiple_of(5) {
            rec.capture_snapshot(sim.now_ns(), &encode_state(&delivered))
                .unwrap();
        }
        let task = token.task().get();
        let now = sim.now_ns();
        if let WakeReason::Io { completion, .. } = token.reason() {
            let data: Vec<u8> = mailbox.remove(&completion).expect("mailbox");
            let payload = token.payload_mut();
            payload.acc += sum(&data);
            accs.insert(task, payload.acc);
            rec.record_inout_write(task, now, place_of(task), 100)
                .unwrap();
            let obligation = payload.obligation.take().expect("pending obligation");
            // Task 3 leaks its final request obligation; the rest discharge.
            let kind = if task == 3 && payload.requests_left == 0 {
                leaked.push(obligation);
                HopKind::Leak
            } else {
                HopKind::UnpackDischarge
            };
            rec.record_obligation_hop(task, now, obligation, kind, 101)
                .unwrap();
            // Data-dependent scheduling: odd sums sleep, even sums yield.
            if payload.acc % 2 == 1 {
                let delay = 10 * (payload.acc % 7 + 1);
                sim.sleep(token, delay).unwrap();
            } else {
                sim.yield_task(token).unwrap();
            }
            tap.drain(&sim, rec).unwrap();
            continue;
        }
        if token.payload().requests_left == 0 {
            sim.finish(token).unwrap();
            tap.drain(&sim, rec).unwrap();
            continue;
        }
        let payload = token.payload_mut();
        payload.requests_left -= 1;
        let obligation = rec.mint_obligation(task, now, 200, None).unwrap();
        rec.record_obligation_hop(
            task,
            now,
            obligation,
            HopKind::ChannelHandoff {
                to_fiber: NETWORK_FIBER,
            },
            201,
        )
        .unwrap();
        payload.obligation = Some(obligation);
        let completion = sim.park_io(token).unwrap();
        tap.drain(&sim, rec).unwrap();
        let data = net.deliver(task);
        let mut record = task.to_le_bytes().to_vec();
        record.extend_from_slice(&data);
        let seq = rec.record_effect(task, now, NET, RECV, &record).unwrap();
        *delivered.entry(task).or_default() += sum(&data);
        states.push((seq, delivered.clone()));
        mailbox.insert(completion, data);
        sim.complete_io(
            completion,
            IoResult::Success {
                bytes: record.len() as u64 - 8,
            },
        )
        .unwrap();
        tap.drain(&sim, rec).unwrap();
    }
    sim.finish_replay().unwrap();
    Outcome {
        accs,
        complete: sim.is_complete(),
        journal: sim.journal().clone(),
        states,
        leaked,
    }
}

/// Step function over the recording: total delivered bytes per task.
struct Delivered;
impl Replayer for Delivered {
    type State = BTreeMap<u64, u64>;
    fn initial(&self) -> Self::State {
        BTreeMap::new()
    }
    fn restore(&self, image: &[u8]) -> Result<Self::State, String> {
        decode_state(image)
    }
    fn apply(&self, state: &mut Self::State, ev: &FlightEvent<'_>) -> Result<(), String> {
        if ev.effect_id == NET && ev.op_tag == RECV {
            let task = u64::from_le_bytes(
                ev.payload
                    .get(..8)
                    .ok_or("short NET record")?
                    .try_into()
                    .unwrap(),
            );
            *state.entry(task).or_default() += sum(&ev.payload[8..]);
        }
        Ok(())
    }
}

fn config() -> RecorderConfig {
    RecorderConfig {
        ring_bytes: 1 << 20,
        max_records: 1 << 14,
        max_payload: 1024,
        snapshot_slots: 64,
        snapshot_image_bytes: 1024,
    }
}

fn key() -> BundleKey {
    BundleKey::new(*blake3::hash(b"chrono replay test key").as_bytes())
}

fn record_live() -> (Outcome, Vec<RecordedEvent>, Vec<u8>) {
    let mut rec = FlightRecorder::new(config()).unwrap();
    let live = run(
        Simulator::new(SCHED_SEED),
        &mut LiveNetwork(0xdead_beef),
        &mut rec,
    );
    let records = rec
        .events()
        .map(|e| RecordedEvent::from_event(&e))
        .collect();
    let bytes = Bundle::export(
        &rec,
        &RedactionPolicy::new(),
        &seed_metadata(SCHED_SEED),
        &key(),
    )
    .unwrap();
    (live, records, bytes)
}

#[test]
fn replay_from_bundle_reproduces_state_and_event_stream() {
    let (live, live_records, bytes) = record_live();
    assert!(live.complete);
    assert_eq!(live.accs.len(), 4);
    assert!(live.journal.events.len() > 40, "nontrivial schedule");
    assert_eq!(live.leaked.len(), 1);

    let session = ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap();
    let journal = journal_from_bundle(session.bundle()).unwrap();
    assert_eq!(journal, live.journal, "journal rebuilt from records");
    let mut rec = FlightRecorder::new(config()).unwrap();
    let mut net = RecordedNetwork::new(session.bundle());
    let replayed = run(Simulator::replay(journal).unwrap(), &mut net, &mut rec);
    assert!(
        net.responses.next().is_none(),
        "every recorded response consumed"
    );
    assert_eq!(replayed, live, "identical final state, journal and states");
    let replay_records: Vec<_> = rec
        .events()
        .map(|e| RecordedEvent::from_event(&e))
        .collect();
    assert_eq!(
        replay_records, live_records,
        "identical flight-record stream"
    );
    assert_eq!(replay_records, session.bundle().records);

    // A different live environment really does produce a different run.
    let mut other = FlightRecorder::new(config()).unwrap();
    let diverged = run(Simulator::new(SCHED_SEED), &mut LiveNetwork(7), &mut other);
    assert_ne!(diverged.accs, live.accs);
    let _ = session.close();
}

#[test]
fn lineage_reverse_watch_and_step_back_over_the_recording() {
    let (live, _, bytes) = record_live();
    let mut session = ReplaySession::open_bundle(&bytes, &key(), &BundleLimits::default()).unwrap();

    // Lineage: every obligation is minted, handed to the network fiber and
    // then discharged, except the one leaked.
    let lineage = session.lineage();
    assert!(lineage.audit_complete());
    assert_eq!(lineage.leaked(), live.leaked);
    assert!(lineage.open_obligations().is_empty());
    let leaked = live.leaked[0];
    assert_eq!(lineage.status(leaked).unwrap(), ObligationStatus::Leaked);
    let chain = session.query_linear_lineage(leaked).unwrap();
    let kinds: Vec<_> = chain.iter().map(|h| h.kind).collect();
    assert_eq!(
        kinds,
        vec![
            HopKind::Mint { parent: None },
            HopKind::ChannelHandoff {
                to_fiber: NETWORK_FIBER
            },
            HopKind::Leak
        ]
    );
    assert!(chain.iter().all(|h| h.fiber_id == 3));
    let mut scratch = [0u8; 3 * 24];
    assert_eq!(
        session
            .query_linear_lineage_into(leaked, &mut scratch)
            .unwrap(),
        3
    );
    assert!(matches!(
        session.query_linear_lineage_into(leaked, &mut [0u8; 47]),
        Err(ReplayError::Bundle(BundleError::LimitExceeded(_)))
    ));

    // Reverse watchpoints walk back through every write to task 2's place.
    let writes: Vec<u64> = session
        .timeline()
        .writes_to(place_of(2))
        .iter()
        .map(|w| w.lamport_seq)
        .collect();
    assert_eq!(writes.len(), 4, "task 2 makes four requests");
    for expected in writes.iter().rev() {
        assert_eq!(
            session
                .reverse_watch_inout(place_of(2))
                .unwrap()
                .lamport_seq,
            *expected
        );
    }
    assert!(matches!(
        session.reverse_watch_inout(place_of(2)),
        Err(ReplayError::NoEarlierWrite { .. })
    ));

    // State at every NET record, from the nearest snapshot and from scratch,
    // equals the live state there.
    assert!(session.bundle().snapshots.len() > 3);
    for (seq, state) in &live.states {
        assert_eq!(&session.state_at(&Delivered, *seq).unwrap(), state);
        assert_eq!(
            &session.state_at_from(&Delivered, *seq, false).unwrap(),
            state
        );
    }
    // Step back from the end, one record at a time, to the start.
    let end = session.bundle().next_seq;
    session.seek(end).unwrap();
    let mut steps = 0;
    loop {
        match session.step_back(&Delivered) {
            Ok((cursor, state)) => {
                let expected = live
                    .states
                    .iter()
                    .rev()
                    .find(|(seq, _)| *seq <= cursor.lamport_seq)
                    .map(|(_, s)| s.clone())
                    .unwrap_or_default();
                assert_eq!(state, expected, "at {cursor:?}");
                let epoch = session.timeline().nearest_snapshot(cursor.lamport_seq);
                assert_eq!(cursor.epoch_id, epoch.map_or(0, |e| e.epoch_id));
                steps += 1;
            }
            Err(ReplayError::AtStart) => break,
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(steps, end - 1);
    let _ = session.close();
}

#[test]
fn tampered_or_wrongly_keyed_bundle_is_rejected() {
    let (_, _, bytes) = record_live();
    let limits = BundleLimits::default();
    for position in (0..bytes.len()).step_by(37).chain([bytes.len() - 1]) {
        let mut tampered = bytes.clone();
        tampered[position] ^= 0x01;
        let err = ReplaySession::open_bundle(&tampered, &key(), &limits).unwrap_err();
        assert!(
            matches!(
                err,
                ReplayError::Bundle(BundleError::Authentication | BundleError::BadMagic)
            ),
            "byte {position}: {err:?}"
        );
    }
    let wrong = BundleKey::new([7; 32]);
    assert!(matches!(
        ReplaySession::open_bundle(&bytes, &wrong, &limits),
        Err(ReplayError::Bundle(BundleError::Authentication))
    ));
    assert!(matches!(
        ReplaySession::open_bundle(&bytes[..bytes.len() - 1], &key(), &limits),
        Err(ReplayError::Bundle(BundleError::Authentication))
    ));
    // A re-sealed bundle with a changed outcome authenticates under the key
    // but no longer matches the recorded schedule: replay detects it.
    let mut bundle = Bundle::open(&bytes, &key(), &limits).unwrap();
    let net = bundle
        .records
        .iter_mut()
        .find(|r| r.effect_id == NET)
        .unwrap();
    net.payload.truncate(8);
    net.payload.extend_from_slice(&[1; 17]); // live lengths are 1..=16
    let resealed = bundle.seal(&key()).unwrap();
    let session = ReplaySession::open_bundle(&resealed, &key(), &limits).unwrap();
    let journal = journal_from_bundle(session.bundle()).unwrap();
    let mut net = RecordedNetwork::new(session.bundle());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut rec = FlightRecorder::new(config()).unwrap();
        run(Simulator::replay(journal).unwrap(), &mut net, &mut rec)
    }));
    assert!(result.is_err(), "replay diverges from the recorded journal");
    let _ = session.close();
}
