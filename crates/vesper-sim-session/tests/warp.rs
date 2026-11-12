//! Checked source driven through the scenario version 3 Warp model: the
//! §35.2 `XdpNic` effect over a conserved UMEM and adversarial datagram
//! links. Every test runs real checked source on the reference evaluator
//! under SimPulse; none touches a real socket, NIC, clock, random source or
//! sleep. This is not AF_XDP, DPDK or kernel I/O.
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vesper_sim_session::{
    CheckedProgram, ModelEvent, RecoveredOwner, Report, RunResult, Scenario, Session, SessionError,
    model::{
        link::{Fate, GilbertElliott, LossCause},
        umem::{HostBug, RxDrop},
        warp::{LeaseTimer, PacketKind, WarpEvent},
    },
};

const WARP: &str = include_str!("../fixtures/warp.ves");
const WAN: &str = include_str!("../fixtures/warp_wan.json");

fn program_with(extra: &str) -> CheckedProgram {
    CheckedProgram::new(format!("{WARP}\n{extra}")).unwrap_or_else(|e| panic!("{e}"))
}
fn base() -> Json {
    serde_json::from_str(WAN).unwrap()
}
/// A short run: 15 + 3 bursts, NAT rebinding at 1 ms and 2 ms.
fn small() -> Json {
    let mut json = base();
    let warp = &mut json["model"]["warp"];
    warp["flows"][0]["bursts"] = json!(15);
    warp["flows"][1]["bursts"] = json!(3);
    warp["events"][0]["at_ns"] = json!(1_000_000);
    warp["events"][1]["at_ns"] = json!(2_000_000);
    json
}
fn scenario(json: &Json) -> Scenario {
    Scenario::from_json(&json.to_string()).unwrap()
}
fn complete(result: RunResult<'_>) -> Report {
    match result {
        RunResult::Complete(report) => report,
        other => panic!("expected completion, got {other:?}"),
    }
}
fn interrupted<'w>(result: RunResult<'w>) -> (Session<'w>, SessionError) {
    match result {
        RunResult::Interrupted { session, error } => (session, error),
        other => panic!("expected an interruption, got {other:?}"),
    }
}
fn record(program: &CheckedProgram, json: &Json) -> Report {
    complete(
        Session::record(program, scenario(json))
            .unwrap()
            .run(u64::MAX),
    )
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

/// The full fixture (3,600 datagrams), recorded once per test binary.
fn full() -> &'static Report {
    static FULL: OnceLock<Report> = OnceLock::new();
    FULL.get_or_init(|| record(&program_with(""), &base()))
}

/// Follow every chunk through the model trace. Each harvested frame must
/// leave its task by a recycle or a turn-around that is submitted, parked
/// and retired exactly once.
fn trace_conserves_frames(report: &Report, frames: u32) {
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
            WarpEvent::Retired { lease, chunk, .. } => {
                assert_eq!(lease_chunk.remove(lease), Some(*chunk));
                step(*chunk, &[At::Parked(*lease)], At::Fill)
            }
            _ => {}
        }
    }
    assert!(lease_chunk.is_empty(), "unretired leases {lease_chunk:?}");
    assert!(
        at.values().all(|a| matches!(a, At::Fill | At::Rx)),
        "{at:?}"
    );
}

#[test]
fn bulk_ingress_under_burst_loss_and_rebinding_conserves_every_frame() {
    let report = full();
    let out = server(report);
    assert_eq!(out["error"], json!(false));
    let snapshot = report.model.as_ref().unwrap().warp.as_ref().unwrap();
    let nic = &snapshot.nics["nic0"];
    let a = nic.accounting;
    assert_eq!(a.total, 64);
    assert_eq!((a.harvested, a.staged, a.leased, a.parked), (0, 0, 0, 0));
    assert_eq!(a.fill + a.rx, 64);
    assert_eq!(nic.leases, 0);
    assert_eq!(snapshot.in_flight, 0);
    trace_conserves_frames(report, 64);

    // What the source saw matches what the model delivered to it.
    let mut emitted: BTreeMap<u64, (String, u64, PacketKind)> = BTreeMap::new();
    let mut origin: BTreeMap<u64, u64> = BTreeMap::new();
    let mut harvested = vec![];
    for event in warp_events(report) {
        match event {
            WarpEvent::Emitted {
                flow,
                packet,
                pkt_num,
                kind,
                ..
            } => {
                emitted.insert(*packet, (flow.clone(), *pkt_num, *kind));
            }
            WarpEvent::Transmitted {
                packet,
                duplicate_of: Some(original),
                ..
            } => {
                origin.insert(*packet, *original);
            }
            WarpEvent::Harvested { packet, .. } => harvested.push(*packet),
            _ => {}
        }
    }
    let seen: Vec<&(String, u64, PacketKind)> = harvested
        .iter()
        .map(|p| &emitted[origin.get(p).unwrap_or(p)])
        .collect();
    let of = |kind: PacketKind| seen.iter().filter(|s| s.2 == kind).count() as u64;
    assert_eq!(out["replied"].as_u64().unwrap(), of(PacketKind::Valid));
    assert_eq!(
        out["malformed"].as_u64().unwrap(),
        of(PacketKind::Truncated) + of(PacketKind::LongHeader)
    );
    assert_eq!(
        out["unknown_cid"].as_u64().unwrap(),
        of(PacketKind::UnknownCid)
    );
    assert!(of(PacketKind::Valid) > 2000, "{out}");
    // RFC 9000 PN decoding across the 2^32 boundary of flow a's 32-bit
    // truncated packet numbers: the decoded largest equals the largest valid
    // packet number the source actually harvested.
    let largest = |flow: &str| {
        seen.iter()
            .filter(|s| s.0 == flow && s.2 == PacketKind::Valid)
            .map(|s| s.1)
            .max()
            .unwrap()
    };
    assert!(largest("bulk_a") > 1 << 32);
    assert_eq!(out["largest_a"].as_u64().unwrap(), largest("bulk_a"));
    assert_eq!(out["largest_b"].as_u64().unwrap(), largest("burst_b"));
    assert_eq!(out["rebinds"], json!(2));

    // Every adversarial behaviour was exercised.
    let count = |f: &dyn Fn(&WarpEvent) -> bool| warp_events(report).filter(|e| f(e)).count();
    let lost = |cause: LossCause| {
        move |e: &WarpEvent| {
            matches!(e, WarpEvent::Transmitted { transmission, .. }
                if transmission.fate == Fate::Lost { cause })
        }
    };
    assert!(count(&lost(LossCause::GilbertElliott)) > 50);
    assert!(count(&lost(LossCause::Independent)) > 0);
    assert!(count(&lost(LossCause::QueueOverflow)) > 50);
    assert!(
        count(&|e| matches!(
            e,
            WarpEvent::Transmitted {
                duplicate_of: Some(_),
                ..
            }
        )) > 0
    );
    assert!(
        count(&|e| matches!(e, WarpEvent::Transmitted { transmission, .. }
        if matches!(transmission.fate, Fate::Delivered { reorder_extra_ns, .. } if reorder_extra_ns > 0)))
            > 0
    );
    // Micro-bursts of 40 into a 24-packet buffer: queueing delay builds to
    // nearly the full buffer (24 x 9.6 us, less the head's elapsed service)
    // before tail drops.
    let max_queue = warp_events(report)
        .filter_map(|e| match e {
            WarpEvent::Transmitted {
                link, transmission, ..
            } if link == "down" => match transmission.fate {
                Fate::Delivered { queue_delay_ns, .. } => Some(queue_delay_ns),
                _ => None,
            },
            _ => None,
        })
        .max()
        .unwrap();
    assert!(
        max_queue > 20 * 9600 && max_queue <= 24 * 9600,
        "{max_queue}"
    );
    assert_eq!(count(&|e| matches!(e, WarpEvent::NatRebound { .. })), 2);
    assert!(
        count(&|e| matches!(
            e,
            WarpEvent::Retired {
                by: LeaseTimer::Ack,
                ..
            }
        )) > 2000
    );
    assert!(
        count(&|e| matches!(
            e,
            WarpEvent::Retired {
                by: LeaseTimer::Pto,
                ..
            }
        )) > 0
    );
    // Every ACKed lease's PTO, and duplicate ACKs, fire later as stale.
    assert!(
        count(&|e| matches!(
            e,
            WarpEvent::StaleTimer {
                timer: LeaseTimer::Pto,
                ..
            }
        )) > 2000
    );
    assert!(
        count(&|e| matches!(
            e,
            WarpEvent::StaleTimer {
                timer: LeaseTimer::Ack,
                ..
            }
        )) > 0
    );
}

/// Session-level Gilbert–Elliott check over the full fixture's ingress
/// link. With ~3,700 bursty samples (mean Bad sojourn 4 datagrams) the
/// documented tolerance is 30% relative; the 10^6-sample unit test in
/// `model::link` holds the chain to 5%.
#[test]
fn ingress_loss_matches_the_stationary_gilbert_elliott_rate() {
    let report = full();
    let ge = GilbertElliott {
        good_to_bad_ppm: 20_000,
        bad_to_good_ppm: 250_000,
        loss_good_ppm: 1_000,
        loss_bad_ppm: 500_000,
    };
    let (mut sent, mut lost) = (0u64, 0u64);
    for event in warp_events(report) {
        if let WarpEvent::Transmitted {
            link, transmission, ..
        } = event
            && link == "down"
        {
            sent += 1;
            lost += u64::from(
                transmission.fate
                    == Fate::Lost {
                        cause: LossCause::GilbertElliott,
                    },
            );
        }
    }
    assert!(sent > 3600);
    let measured = lost as f64 * 1e6 / sent as f64;
    let expected = f64::from(ge.stationary_loss_ppm());
    assert!(
        ((measured - expected) / expected).abs() < 0.30,
        "measured {measured} ppm, stationary {expected} ppm over {sent}"
    );
}

#[test]
fn an_exhausted_fill_ring_drops_at_the_nic_and_still_conserves() {
    // 8 frames and a long PTO: lost replies pin frames, so arrivals find no
    // fill frame and are dropped by the NIC (a modelled outcome).
    let program = program_with("");
    let mut json = small();
    let nic = &mut json["model"]["warp"]["nics"][0];
    nic["frames"] = json!(8);
    nic["rx_ring"] = json!(4);
    nic["pto_ns"] = json!(5_000_000);
    json["model"]["warp"]["links"][1]["loss_ppm"] = json!(300_000);
    let report = record(&program, &json);
    trace_conserves_frames(&report, 8);
    let drops = warp_events(&report)
        .filter(|e| {
            matches!(
                e,
                WarpEvent::RxDropped {
                    reason: RxDrop::NoFillFrame,
                    ..
                }
            )
        })
        .count();
    assert!(drops > 0);
    let a = report.model.unwrap().warp.unwrap().nics["nic0"].accounting;
    assert_eq!((a.total, a.fill + a.rx), (8, 8));
}

#[test]
fn the_full_run_replays_byte_identically() {
    let program = program_with("");
    let report = full();
    let replayed = complete(
        Session::replay(&program, scenario(&base()), report.recording.clone())
            .unwrap()
            .run(u64::MAX),
    );
    assert_eq!(replayed.recording.to_json(), report.recording.to_json());
    assert_eq!(&replayed, report);
}

type Mutation = Box<dyn Fn(&mut WarpEvent)>;

#[test]
fn changed_choices_are_rejected_on_replay() {
    let program = program_with("");
    let json = small();
    let report = record(&program, &json);
    assert_eq!(server(&report)["rebinds"], json!(2));
    trace_conserves_frames(&report, 64);
    // Re-recording is deterministic, and run slicing changes nothing.
    assert_eq!(record(&program, &json), report);
    let sliced = Session::record(&program, scenario(&json))
        .unwrap()
        .run(1)
        .finish(1);
    assert_eq!(complete(sliced), report);
    let replay = |recording| Session::replay(&program, scenario(&json), recording).unwrap();
    assert_eq!(
        complete(replay(report.recording.clone()).run(u64::MAX)),
        report
    );

    let index_of = |f: &dyn Fn(&WarpEvent) -> bool| {
        report
            .recording
            .model_trace
            .iter()
            .position(|e| matches!(e, ModelEvent::Warp(w) if f(w)))
            .unwrap()
    };
    let mutations: Vec<(usize, Mutation)> = vec![
        (
            index_of(
                &|e| matches!(e, WarpEvent::Transmitted { transmission, .. } if matches!(transmission.fate, Fate::Lost { .. })),
            ),
            Box::new(|e| {
                if let WarpEvent::Transmitted { transmission, .. } = e {
                    transmission.ge_bad = !transmission.ge_bad;
                }
            }),
        ),
        (
            index_of(&|e| {
                matches!(
                    e,
                    WarpEvent::Transmitted {
                        duplicate_of: Some(_),
                        ..
                    }
                )
            }),
            Box::new(|e| {
                if let WarpEvent::Transmitted { duplicate_of, .. } = e {
                    *duplicate_of = None;
                }
            }),
        ),
        (
            index_of(
                &|e| matches!(e, WarpEvent::Transmitted { transmission, .. } if matches!(transmission.fate, Fate::Delivered { reorder_extra_ns, .. } if reorder_extra_ns > 0)),
            ),
            Box::new(|e| {
                if let WarpEvent::Transmitted { transmission, .. } = e
                    && let Fate::Delivered {
                        reorder_extra_ns, ..
                    } = &mut transmission.fate
                {
                    *reorder_extra_ns = 0;
                }
            }),
        ),
        (
            index_of(&|e| matches!(e, WarpEvent::NatRebound { .. })),
            Box::new(|e| {
                if let WarpEvent::NatRebound { to, .. } = e {
                    to.1 += 1;
                }
            }),
        ),
        (
            index_of(&|e| {
                matches!(
                    e,
                    WarpEvent::Emitted {
                        kind: PacketKind::UnknownCid,
                        ..
                    }
                )
            }),
            Box::new(|e| {
                if let WarpEvent::Emitted { kind, .. } = e {
                    *kind = PacketKind::Valid;
                }
            }),
        ),
        (
            index_of(&|e| {
                matches!(
                    e,
                    WarpEvent::Retired {
                        by: LeaseTimer::Ack,
                        ..
                    }
                )
            }),
            Box::new(|e| {
                if let WarpEvent::Retired { by, .. } = e {
                    *by = LeaseTimer::Pto;
                }
            }),
        ),
    ];
    for (index, mutate) in mutations {
        let mut recording = report.recording.clone();
        let ModelEvent::Warp(event) = &mut recording.model_trace[index] else {
            unreachable!()
        };
        mutate(event);
        let (session, error) = interrupted(replay(recording).run(u64::MAX));
        assert!(
            matches!(error, SessionError::ModelTraceMismatch { index: i, .. } if i == index),
            "{error:?}"
        );
        assert!(session.stalled_task().is_some());
    }
    let mut truncated = report.recording.clone();
    truncated.model_trace.pop();
    assert!(matches!(
        interrupted(replay(truncated).run(u64::MAX)).1,
        SessionError::ModelTraceMismatch { .. }
    ));
    // A changed scenario choice (a link probability) is an identity mismatch.
    let mut changed = json.clone();
    changed["model"]["warp"]["links"][0]["duplicate_ppm"] = json!(5001);
    let err = Session::replay(&program, scenario(&changed), report.recording.clone())
        .err()
        .unwrap();
    assert!(matches!(err, SessionError::Identity(m) if m.field == "scenario_sha256"));
}

#[test]
fn a_host_binding_that_drops_a_recycled_frame_is_caught() {
    let program = program_with("");
    let json = small();
    let mut session = Session::record(&program, scenario(&json)).unwrap();
    session.inject_host_bug(HostBug::DropOnRecycle { occurrence: 1 });
    let (mut session, error) = interrupted(session.run(u64::MAX));
    let SessionError::Conservation { task, message } = &error else {
        panic!("{error:?}")
    };
    assert_eq!(task, "server");
    assert!(message.contains("Missing"), "{message}");
    // The recycling task's continuation is retained, not dropped.
    let stalled = session.take_stalled().unwrap();
    assert!(matches!(stalled.owner, RecoveredOwner::Suspended(_)));
    // Nothing was committed for the failing step.
    assert!(
        !session
            .model_trace()
            .iter()
            .any(|e| matches!(e, ModelEvent::Warp(WarpEvent::Recycled { .. })))
    );
}

fn run_entry(program: &CheckedProgram, entry: &str, arguments: Json) -> (String, SessionError) {
    let mut json = small();
    json["tasks"][0]["entry"] = json!(entry);
    json["tasks"][0]["arguments"] = arguments;
    let (mut session, error) = interrupted(
        Session::record(program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(session.take_stalled().is_some());
    (session.stalled_task().unwrap_or_default(), error)
}

#[test]
fn forged_stale_and_leaked_frames_are_model_errors() {
    let program = program_with(
        r#"
fn forge() -> u64 eff [XdpNic] {
    XdpNic::recycle_to_fill_ring(UmemFrame { chunk_idx: 3, data_offset: 256, pkt_len: 1200 });
    0
}
fn stale_view() -> u64 eff [XdpNic, Wire, RxWait] {
    let ready = RxWait::wait_rx(10000000);
    match XdpNic::harvest_rx_single() {
        None => 0,
        Some(frame) => {
            let view = XdpNic::view_frame_bytes(frame);
            XdpNic::recycle_to_fill_ring(frame);
            match Wire::split_short_header(view) { Ok(h) => 1, Err(code) => 2 }
        },
    }
}
fn leak() -> Option[UmemFrame[RxHarvested]] eff [XdpNic, RxWait] {
    let ready = RxWait::wait_rx(10000000);
    XdpNic::harvest_rx_single()
}
fn bad_len() -> u64 eff [XdpNic, RxWait] {
    let ready = RxWait::wait_rx(10000000);
    match XdpNic::harvest_rx_single() {
        None => 0,
        Some(frame) => {
            let tx = XdpNic::turn_around_tx_in_place(frame, 4000);
            let lease = XdpNic::submit_paced_tx(tx, 1, 0);
            XdpNic::park_for_ack_or_pto(lease);
            1
        },
    }
}
"#,
    );
    let (_, error) = run_entry(&program, "forge", json!([]));
    assert!(
        matches!(&error, SessionError::Model { message, .. } if message.contains("NotHeld")),
        "{error:?}"
    );
    let (_, error) = run_entry(&program, "stale_view", json!([]));
    assert!(
        matches!(&error, SessionError::Model { message, .. } if message.contains("StaleView")),
        "{error:?}"
    );
    let (_, error) = run_entry(&program, "leak", json!([]));
    assert!(
        matches!(&error, SessionError::Conservation { message, .. } if message.contains("holding")),
        "{error:?}"
    );
    let (_, error) = run_entry(&program, "bad_len", json!([]));
    assert!(
        matches!(&error, SessionError::Model { message, .. } if message.contains("TxLenOutOfRange")),
        "{error:?}"
    );
}

#[test]
fn the_lease_budget_is_enforced_before_a_lease_is_retained() {
    let program = program_with("");
    let mut json = small();
    json["model"]["warp"]["limits"]["max_leases"] = json!(1);
    let (mut session, error) = interrupted(
        Session::record(&program, scenario(&json))
            .unwrap()
            .run(u64::MAX),
    );
    assert!(
        matches!(&error, SessionError::ModelBudget { message, .. } if message.contains("LeaseLimit")),
        "{error:?}"
    );
    let snapshot = session.model_snapshot().unwrap().warp.unwrap();
    // The refused submit left its staged frame with the task and no lease.
    let a = snapshot.nics["nic0"].accounting;
    assert_eq!((a.staged, a.leased + a.parked, a.total), (1, 1, 64));
    assert!(matches!(
        session.take_stalled().unwrap().owner,
        RecoveredOwner::Suspended(_)
    ));
}

/// The §35.3 obligations the existing checker must enforce.
#[test]
fn the_checker_rejects_unsound_frame_handling() {
    let cases = [
        (
            "E0302",
            "fn dropped() -> bool eff [XdpNic] { match XdpNic::harvest_rx_single() { None => false, Some(frame) => true } }",
        ),
        (
            "E0402",
            "fn early(r: Result[u64, QuicError]) -> Result[bool, QuicError] eff [XdpNic, Fail[QuicError]] { match XdpNic::harvest_rx_single() { None => Ok(false), Some(frame) => { let x = r?; XdpNic::recycle_to_fill_ring(frame); Ok(true) } } }",
        ),
        (
            "E0303",
            "fn twice() -> bool eff [XdpNic] { match XdpNic::harvest_rx_single() { None => false, Some(frame) => { XdpNic::recycle_to_fill_ring(frame); XdpNic::recycle_to_fill_ring(frame); true } } }",
        ),
        (
            "E0309",
            "fn some_paths(c: bool) -> bool eff [XdpNic] { match XdpNic::harvest_rx_single() { None => false, Some(frame) => { if c { XdpNic::recycle_to_fill_ring(frame); true } else { false } } } }",
        ),
        // Checker gap recorded for the coordinator: the §35.3 signature with
        // `inout conns` and `Fail` cannot use `?` at all, even before any
        // frame is live, because Fail crosses the inout parameter.
        (
            "E0402",
            "fn inout_conns(inout conns: Conns) -> Result[bool, QuicError] eff [Fail[QuicError]] { let left = pn_space_left(conns)?; Ok(true) }",
        ),
    ];
    for (code, function) in cases {
        let error = match CheckedProgram::new(format!("{WARP}\n{function}")) {
            Ok(_) => panic!("accepted: {function}"),
            Err(e) => e,
        };
        assert!(
            error.diagnostics.iter().any(|d| d.starts_with(code)),
            "{code}: {:?}",
            error.diagnostics
        );
    }
}

#[test]
fn warp_scenarios_are_validated_at_load() {
    let program = program_with("");
    let load = |json: &Json| match Session::record(&program, scenario(json)) {
        Ok(_) => String::from("ok"),
        Err(e) => format!("{e:?}"),
    };
    let reject = |json: Json, needle: &str| {
        let message = load(&json);
        assert!(message.contains(needle), "{needle}: {message}");
    };
    assert_eq!(load(&small()), "ok");
    let mut j = small();
    j["version"] = json!(2);
    reject(j, "version 3");
    let mut j = small();
    j["model"]["warp"] = Json::Null;
    reject(j, "requires a model with a warp section");
    let mut j = small();
    j["bindings"][2]["action"] = json!({ "xdp_view": { "nic": "nic0" } });
    reject(j, "wrong parameter passing");
    let mut j = small();
    j["bindings"][0]["action"] = json!({ "xdp_harvest": { "nic": "nic9" } });
    reject(j, "unknown NIC nic9");
    let mut j = small();
    j["model"]["warp"]["flows"][0]["payload_bytes"] = json!(1780);
    reject(j, "must fit a frame");
    let mut j = small();
    j["model"]["processes"] = json!([{ "name": "server" }, { "name": "other" }]);
    j["tasks"][0]["process"] = json!("other");
    reject(j, "does not own");
    let mut j = small();
    j["model"]["limits"]["max_epochs"] = json!(1);
    j["model"]["faults"] = json!([{ "at_ns": 5, "fault": { "crash": { "process": "server" } } }]);
    reject(j, "does not model a crash");
    let mut j = small();
    j["model"]["warp"]["links"][0]["loss_ppm"] = json!(1_000_001);
    reject(j, "ppm");
    let mut j = small();
    j["model"]["warp"]["flows"][0]["first_pn"] = json!(4611686018427387903u64);
    reject(j, "below 2^62");
    let mut j = small();
    j["model"]["warp"]["events"][0]["event"]["nat_rebind"]["flow"] = json!("nope");
    reject(j, "unknown flow");

    // Structural signature checks against the checked declarations.
    let shapes = [
        (
            "copy struct RxHarvested {} copy struct TxStaged {} linear struct UmemFrame[State] { chunk_idx: u32, data_offset: u32 }",
            "not a linear frame struct",
        ),
        (
            "copy struct RxHarvested {} copy struct TxStaged {} copy struct UmemFrame[State] { chunk_idx: u32, data_offset: u32, pkt_len: u32 }",
            "not a linear frame struct",
        ),
    ];
    for (types, needle) in shapes {
        let source = WARP
            .replace(
                "copy struct RxHarvested {}\ncopy struct TxStaged {}\nlinear struct UmemFrame[State] { chunk_idx: u32, data_offset: u32, pkt_len: u32 }",
                types,
            );
        let program = CheckedProgram::new(source).unwrap_or_else(|e| panic!("{e}"));
        let message = match Session::record(&program, scenario(&small())) {
            Ok(_) => "ok".into(),
            Err(e) => format!("{e:?}"),
        };
        assert!(message.contains(needle), "{needle}: {message}");
    }
}
