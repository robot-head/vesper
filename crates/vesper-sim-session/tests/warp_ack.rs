//! The datagram ACK path on the Warp software NIC: peers aggregate ACKs,
//! ACK datagrams cross a lossy link back into the RX ring, and checked source
//! retires leases with `AckPath::on_ack`. Runs real checked source on the
//! reference evaluator under SimPulse; no socket, NIC, clock or randomness.
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use vesper_sim_session::{
    CheckedProgram, ModelEvent, Report, RunResult, Scenario, Session,
    model::{
        link::Fate,
        warp::{LeaseTimer, WarpEvent},
    },
};

const WARP: &str = include_str!("../fixtures/warp.ves");
const ACK: &str = include_str!("../fixtures/warp_ack.ves");
const SCENARIO: &str = include_str!("../fixtures/warp_ack.json");

fn program() -> &'static CheckedProgram {
    static PROGRAM: OnceLock<CheckedProgram> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        CheckedProgram::new(format!("{WARP}\n{ACK}")).unwrap_or_else(|e| panic!("{e}"))
    })
}
fn base() -> Json {
    serde_json::from_str(SCENARIO).unwrap()
}
fn scenario(json: &Json) -> Scenario {
    Scenario::from_json(&json.to_string()).unwrap()
}
fn record(json: &Json) -> Report {
    match Session::record(program(), scenario(json))
        .unwrap()
        .run(u64::MAX)
    {
        RunResult::Complete(report) => report,
        other => panic!("expected completion, got {other:?}"),
    }
}
/// An `AckSent`: packet id, send time and ranges.
type SentAck = (u64, u64, Vec<(u64, u64)>);

fn full() -> &'static Report {
    static FULL: OnceLock<Report> = OnceLock::new();
    FULL.get_or_init(|| record(&base()))
}
fn warp_events(report: &Report) -> impl Iterator<Item = &WarpEvent> {
    report.recording.model_trace.iter().filter_map(|e| match e {
        ModelEvent::Warp(w) => Some(w),
        _ => None,
    })
}
fn server(report: &Report) -> &Json {
    &report
        .outputs
        .iter()
        .find(|(t, _)| t == "server")
        .unwrap()
        .1
}

/// Every chunk follows a legal path, every lease retires exactly once, and
/// returns the retirement cause of each lease.
fn conserved(report: &Report, frames: u32) -> BTreeMap<u64, LeaseTimer> {
    #[derive(Debug, PartialEq)]
    enum At {
        Fill,
        Rx,
        Harvested,
        Staged,
        Leased(u64),
        Parked(u64),
    }
    let mut at: BTreeMap<u32, At> = (0..frames).map(|c| (c, At::Fill)).collect();
    let mut lease_chunk = BTreeMap::new();
    let mut by = BTreeMap::new();
    let mut step = |chunk: u32, from: &[At], to: At| {
        let now = at.get_mut(&chunk).unwrap();
        assert!(from.contains(now), "chunk {chunk}: {now:?} -> {to:?}");
        *now = to;
    };
    for event in warp_events(report) {
        match event {
            WarpEvent::RxArrived { chunk, .. } => step(*chunk, &[At::Fill], At::Rx),
            WarpEvent::Harvested { chunk, .. } => step(*chunk, &[At::Rx], At::Harvested),
            WarpEvent::Recycled { chunk, .. } => step(*chunk, &[At::Harvested], At::Fill),
            WarpEvent::TurnedAround { chunk, .. } => step(*chunk, &[At::Harvested], At::Staged),
            WarpEvent::Submitted { chunk, lease, .. } => {
                lease_chunk.insert(*lease, *chunk);
                step(*chunk, &[At::Staged], At::Leased(*lease))
            }
            WarpEvent::Parked { lease, .. } => step(
                lease_chunk[lease],
                &[At::Leased(*lease)],
                At::Parked(*lease),
            ),
            WarpEvent::Retired {
                lease,
                chunk,
                by: cause,
                ..
            } => {
                assert_eq!(lease_chunk.remove(lease), Some(*chunk));
                assert!(by.insert(*lease, *cause).is_none());
                step(*chunk, &[At::Parked(*lease)], At::Fill)
            }
            _ => {}
        }
    }
    assert!(lease_chunk.is_empty(), "unretired leases {lease_chunk:?}");
    assert!(at.values().all(|a| *a == At::Fill || *a == At::Rx));
    by
}

#[test]
fn aggregated_acks_over_a_lossy_path_retire_leases_from_source() {
    let report = full();
    let out = server(report);
    assert_eq!(out["error"], json!(false));
    let by = conserved(report, 64);
    let snapshot = report.model.as_ref().unwrap().warp.as_ref().unwrap();
    let nic = &snapshot.nics["nic0"];
    assert_eq!((nic.leases, snapshot.in_flight), (0, 0));
    assert_eq!(nic.accounting.fill + nic.accounting.rx, 64);
    assert_eq!(nic.softnic.leaks_detected, 0);

    let count = |f: &dyn Fn(&WarpEvent) -> bool| warp_events(report).filter(|e| f(e)).count();
    let received = count(&|e| matches!(e, WarpEvent::PeerReceived { .. }));
    let acks: Vec<SentAck> = warp_events(report)
        .filter_map(|e| match e {
            WarpEvent::AckSent {
                packet,
                at_ns,
                ranges,
                ..
            } => Some((*packet, *at_ns, ranges.clone())),
            _ => None,
        })
        .collect();
    let replies = out["replied"].as_u64().unwrap() as usize;
    assert!(replies > 600, "{replies}");
    // Aggregation: fewer ACKs than replies received, at most 8 ranges each.
    assert!(
        received > 500 && acks.len() < received,
        "{received} {}",
        acks.len()
    );
    assert!(
        acks.len() * 2 + 64 > received,
        "ack_every 2 plus delay ACKs"
    );
    assert!(acks.iter().all(|(_, _, r)| (1..=8).contains(&r.len())));
    // Some ACKs were sent by the max_ack_delay timer (an odd arrival with no
    // second one within 25 µs): their send time is not a PeerReceived time.
    let peer_times: BTreeSet<u64> = warp_events(report)
        .filter_map(|e| match e {
            WarpEvent::PeerReceived { at_ns, .. } => Some(*at_ns),
            _ => None,
        })
        .collect();
    assert!(acks.iter().any(|(_, at, _)| !peer_times.contains(at)));

    // ACK-path loss: some ACK datagrams were lost on the "ack" link.
    let ack_packets: BTreeSet<u64> = acks.iter().map(|(p, _, _)| *p).collect();
    let lost_acks = warp_events(report)
        .filter(|e| {
            matches!(e, WarpEvent::Transmitted { link, packet, transmission, duplicate_of: None, .. }
                if link == "ack" && ack_packets.contains(packet)
                    && matches!(transmission.fate, Fate::Lost { .. }))
        })
        .count();
    assert!(lost_acks > 5, "{lost_acks}");

    // Leases retire from source-observed ACKs or by PTO, never by the timer
    // ACK path; source's own count of retirements matches.
    let source = by.values().filter(|b| **b == LeaseTimer::SourceAck).count();
    let pto = by.values().filter(|b| **b == LeaseTimer::Pto).count();
    assert!(!by.values().any(|b| *b == LeaseTimer::Ack));
    assert_eq!(source + pto, replies);
    assert!(source > replies / 2 && pto > 0, "{source} {pto}");
    assert_eq!(out["retired"].as_u64().unwrap() as usize, source);
    println!(
        "replies {replies}, peer arrivals {received}, ACKs sent {}, ACKs lost {lost_acks}, retired by source ACK {source}, by PTO {pto}",
        acks.len()
    );
    let observed = count(&|e| matches!(e, WarpEvent::AckObserved { result: Ok(_), .. }));
    assert_eq!(out["acks"].as_u64().unwrap() as usize, observed);
    // Every data datagram of a known connection was offered to on_ack and
    // refused as "not an ACK frame" (code 4) before being answered.
    let not_ack = count(&|e| matches!(e, WarpEvent::AckObserved { result: Err(4), .. }));
    assert_eq!(not_ack, replies);
    // The source tracked the largest acknowledged packet number per
    // connection; it equals the largest number any delivered ACK carried.
    assert!(out["largest_acked_a"].as_u64().unwrap() > 0);
    assert!(out["largest_acked_b"].as_u64().unwrap() > 0);
}

/// A lost ACK is covered by a later one: leases whose first ACK was lost
/// still retire from source, because the peer keeps acknowledging its most
/// recent ranges.
#[test]
fn a_later_ack_covers_a_lost_one() {
    let report = full();
    // Per flow: the ranges of ACKs that were lost outright (no copy delivered).
    let mut fates: BTreeMap<u64, bool> = BTreeMap::new();
    let mut copies: BTreeMap<u64, u64> = BTreeMap::new();
    let mut sent = vec![];
    for e in warp_events(report) {
        match e {
            WarpEvent::AckSent {
                flow,
                packet,
                ranges,
                ..
            } => sent.push((*packet, flow.clone(), ranges.clone())),
            WarpEvent::Transmitted {
                link,
                packet,
                duplicate_of,
                transmission,
                ..
            } if link == "ack" => {
                let original = duplicate_of.unwrap_or(*packet);
                copies.insert(*packet, original);
                let delivered = matches!(transmission.fate, Fate::Delivered { .. });
                *fates.entry(original).or_default() |= delivered;
            }
            _ => {}
        }
    }
    let lost: Vec<_> = sent.iter().filter(|(p, _, _)| !fates[p]).collect();
    assert!(!lost.is_empty());
    // For a lost ACK, a later delivered ACK of the same flow still carries
    // its largest packet number.
    let covered = lost
        .iter()
        .filter(|(p, flow, ranges)| {
            let largest = ranges[0].1;
            sent.iter().any(|(q, f, r)| {
                q > p
                    && f == flow
                    && fates[q]
                    && r.iter().any(|(lo, hi)| (*lo..=*hi).contains(&largest))
            })
        })
        .count();
    assert!(
        covered > 0 && covered * 2 >= lost.len(),
        "{covered} of {}",
        lost.len()
    );
}

#[test]
fn with_every_ack_lost_leases_retire_by_pto() {
    let mut json = base();
    let warp = &mut json["model"]["warp"];
    warp["flows"][0]["bursts"] = json!(6);
    warp["flows"][1]["bursts"] = json!(2);
    let ack = warp["links"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|l| l["name"] == "ack")
        .unwrap();
    ack["loss_ppm"] = json!(1_000_000);
    let report = record(&json);
    let by = conserved(&report, 64);
    let out = server(&report);
    assert_eq!(out["acks"], json!(0));
    assert!(!by.is_empty());
    assert!(by.values().all(|b| *b == LeaseTimer::Pto));
    assert!(
        warp_events(&report).any(|e| matches!(e, WarpEvent::AckSent { .. })),
        "peers still send ACKs"
    );
}

#[test]
fn the_ack_path_run_replays_byte_identically() {
    let report = full();
    let replayed = match Session::replay(program(), scenario(&base()), report.recording.clone())
        .unwrap()
        .run(u64::MAX)
    {
        RunResult::Complete(report) => report,
        other => panic!("expected completion, got {other:?}"),
    };
    assert_eq!(replayed.recording.to_json(), report.recording.to_json());
    assert_eq!(&replayed, report);
}

#[test]
fn ack_paths_are_validated_at_load() {
    let load = |json: &Json| match Session::record(program(), scenario(json)) {
        Ok(_) => String::from("ok"),
        Err(e) => format!("{e:?}"),
    };
    assert_eq!(load(&base()), "ok");
    let cases: [(&str, Json, &str); 5] = [
        ("link", json!("nope"), "unknown ACK link"),
        ("ack_every", json!(0), "ack_every"),
        ("max_ack_delay_ns", json!(0), "max_ack_delay_ns"),
        ("max_ranges", json!(65), "max_ranges"),
        ("max_ranges", json!(64), "ok"),
    ];
    for (field, value, needle) in cases {
        let mut j = base();
        j["model"]["warp"]["nics"][0]["ack_path"][field] = value;
        let message = load(&j);
        assert!(message.contains(needle), "{field}: {message}");
    }
    let mut j = base();
    j["model"]["warp"]["flows"][1]["cid"] = j["model"]["warp"]["flows"][0]["cid"].clone();
    assert!(load(&j).contains("distinct connection IDs"));
    // The software NIC's geometry rules apply: power-of-two frames of at
    // least 2 KiB and a power-of-two RX ring.
    let mut j = base();
    j["model"]["warp"]["nics"][0]["frame_bytes"] = json!(3000);
    assert!(load(&j).contains("software NIC"), "{}", load(&j));
    let mut j = base();
    j["model"]["warp"]["nics"][0]["rx_ring"] = json!(24);
    assert!(load(&j).contains("power of two"), "{}", load(&j));
    // on_ack's result shape is checked against the source declaration.
    let wrong = CheckedProgram::new(format!(
        "{WARP}\n{ACK}\ncopy struct Wrong {{ x: u64 }}\nlinear effect AckPath2 {{ fn on_ack2(bytes: ByteSlice) -> Result[Wrong, u32]; }}"
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    let mut j = base();
    j["bindings"].as_array_mut().unwrap().push(json!({
        "effect": "AckPath2", "operation": "on_ack2", "params": ["ByteSlice"],
        "result": "Result[Wrong, u32]", "action": { "xdp_on_ack": { "nic": "nic0" } }
    }));
    let message = match Session::record(&wrong, scenario(&j)) {
        Ok(_) => "ok".into(),
        Err(e) => format!("{e:?}"),
    };
    assert!(message.contains("not a copy ACK summary"), "{message}");
}
