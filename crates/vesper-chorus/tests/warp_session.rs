//! Typed sessions over Warp QUIC streams on the software NIC (wave six lane
//! 17): the same typestate and monitor checks as the in-memory channels,
//! n-ary choices, fan-out/fan-in, lossy links, abandonment and malformed
//! peer bytes.

mod common;

use vesper_chorus::session::{
    Branch, Case, ChorusError, End, Fault, LeakAuditor, NetFail, OfferN, Rec, Recv, SelectN, Send,
    Session, Var, Z,
};
use vesper_chorus::session::{Broadcast, Gather};
use vesper_chorus::warpnet::{STREAM_MAGIC, WarpNet, WarpNetConfig};
use vesper_chorus::{Choreography, unit_message};
use vesper_chorus::{branches, parse_choreography, role, roles, string_message, u64_message};

fn err(f: Fault) -> ChorusError {
    f.into_error()
}

fn lossy(nodes: u32, seed: u64) -> WarpNet {
    WarpNet::new(
        WarpNetConfig {
            nodes,
            seed,
            loss_ppm: 100_000,
            reorder_ppm: 50_000,
            duplicate_ppm: 10_000,
            ..WarpNetConfig::default()
        },
        &common::pki(),
    )
    .unwrap()
}

// ---------------------------------------------------------------- two buyer

const TWO_BUYER: &str = "
roles B1, B2, S;
B1 -> S: Title(String);
S -> B1: Quote(u64);
S -> B2: Quote(u64);
B1 -> B2: Share(u64);
B2 -> S {
    Ok() {
        B2 -> S: Address(String);
        S -> B2: Date(u64);
    }
    Quit() { }
}
";

role!(RB1 = "B1");
role!(RB2 = "B2");
role!(RS = "S");
string_message!(Title = "Title", "String");
u64_message!(Quote = "Quote", "u64");
u64_message!(Share = "Share", "u64");
unit_message!(OkMsg = "Ok");
unit_message!(Quit = "Quit");
string_message!(Address = "Address", "String");
u64_message!(Date = "Date", "u64");

type B1Proto = Send<RS, Title, Recv<RS, Quote, Send<RB2, Share, End>>>;
type B2Proto = Recv<
    RS,
    Quote,
    Recv<
        RB1,
        Share,
        SelectN<
            RS,
            branches![
                Branch<OkMsg, Send<RS, Address, Recv<RS, Date, End>>>,
                Branch<Quit, End>
            ],
        >,
    >,
>;
type SProto = Recv<
    RB1,
    Title,
    Send<
        RB1,
        Quote,
        Send<
            RB2,
            Quote,
            OfferN<
                RB2,
                branches![
                    Branch<Quit, End>,
                    Branch<OkMsg, Recv<RB2, Address, Send<RB2, Date, End>>>
                ],
            >,
        >,
    >,
>;

#[test]
fn two_buyer_runs_on_threads_over_lossy_warp_streams() {
    let chor = parse_choreography(TWO_BUYER).unwrap();
    // B1 on host 0, B2 on host 1, S on host 2.
    let placement = [0, 1, 2];
    for (seed, price, budget, expect) in
        [(1, 100, 60, true), (2, 100, 10, false), (3, 80, 40, true)]
    {
        let net = lossy(3, seed);
        let auditor = LeakAuditor::new();
        let t = |node| net.transport(node, &placement).unwrap();
        let b1 = Session::<B1Proto>::bind(&chor, "B1", 9, t(0), &auditor).unwrap();
        let b2 = Session::<B2Proto>::bind(&chor, "B2", 9, t(1), &auditor).unwrap();
        let s = Session::<SProto>::bind(&chor, "S", 9, t(2), &auditor).unwrap();
        let h1 = std::thread::spawn(move || -> Result<u64, ChorusError> {
            let s = b1.send(&Title("TAPL".into())).map_err(err)?;
            let (Quote(q), s) = s.recv().map_err(err)?;
            s.send(&Share(q / 2)).map_err(err)?.close();
            Ok(q)
        });
        let h2 = std::thread::spawn(move || -> Result<Option<u64>, ChorusError> {
            let (Quote(q), s) = b2.recv().map_err(err)?;
            let (Share(share), s) = s.recv().map_err(err)?;
            if q - share <= budget {
                let s = s.select(&OkMsg).map_err(err)?;
                let s = s.send(&Address("1 Loop".into())).map_err(err)?;
                let (Date(d), s) = s.recv().map_err(err)?;
                s.close();
                Ok(Some(d))
            } else {
                s.select(&Quit).map_err(err)?.close();
                Ok(None)
            }
        });
        let hs = std::thread::spawn(move || -> Result<bool, ChorusError> {
            let (Title(t), s) = s.recv().map_err(err)?;
            assert_eq!(t, "TAPL");
            let s = s.send(&Quote(price)).map_err(err)?;
            let s = s.send(&Quote(price)).map_err(err)?;
            match s.offer().map_err(err)?.split().map_err(err)? {
                Case::This(Quit, s) => {
                    s.close();
                    Ok(false)
                }
                Case::Other(c) => match c.split().map_err(err)? {
                    Case::This(OkMsg, s) => {
                        let (Address(a), s) = s.recv().map_err(err)?;
                        assert_eq!(a, "1 Loop");
                        s.send(&Date(20261113)).map_err(err)?.close();
                        Ok(true)
                    }
                    Case::Other(c) => Err(err(c.exhausted())),
                },
            }
        });
        assert_eq!(h1.join().unwrap().unwrap(), price);
        assert_eq!(h2.join().unwrap().unwrap().is_some(), expect);
        assert_eq!(hs.join().unwrap().unwrap(), expect);
        let c = auditor.counts();
        assert_eq!((c.opened, c.closed, c.leaked, c.faulted), (3, 3, 0, 0));
        assert!(net.settle(10_000), "network did not settle");
        assert_eq!(net.undelivered(), 0);
        assert_eq!(net.open_streams(), 0, "every session stream was finished");
        let st = net.stats();
        assert!(st.frames_delivered >= 5, "{st:?}");
        assert_eq!(st.events_dropped, 0);
        assert!(net.link_failure().is_none());
    }
}

// ---------------------------------------------------------------- n-ary loop

const KV: &str = "
roles C, S;
rec Loop {
    C -> S {
        Get(u64) { S -> C: Value(u64); continue Loop; }
        Put(u64) { S -> C: Done(); continue Loop; }
        Del(u64) { S -> C: Done(); continue Loop; }
        Stop() { }
    }
}
";

role!(RC = "C");
role!(RKs = "S");
u64_message!(Get = "Get", "u64");
u64_message!(Put = "Put", "u64");
u64_message!(Del = "Del", "u64");
u64_message!(Value = "Value", "u64");
unit_message!(Done = "Done");
unit_message!(Stop = "Stop");

type ClientLoop = SelectN<
    RKs,
    branches![
        Branch<Get, Recv<RKs, Value, Var<Z>>>,
        Branch<Put, Recv<RKs, Done, Var<Z>>>,
        Branch<Del, Recv<RKs, Done, Var<Z>>>,
        Branch<Stop, End>,
    ],
>;
type ServerLoop = OfferN<
    RC,
    branches![
        Branch<Get, Send<RC, Value, Var<Z>>>,
        Branch<Put, Send<RC, Done, Var<Z>>>,
        Branch<Del, Send<RC, Done, Var<Z>>>,
        Branch<Stop, End>,
    ],
>;

enum Op {
    Get(u64),
    Put(u64),
    Del(u64),
}

/// A four-way choice in a loop, driven from one thread: blocking receives
/// step the lossy network until the frame arrives.
#[test]
fn nary_choice_loop_over_lossy_warp() {
    let chor = parse_choreography(KV).unwrap();
    let net = lossy(2, 7);
    let auditor = LeakAuditor::new();
    let c = Session::<Rec<ClientLoop>>::bind(
        &chor,
        "C",
        1,
        net.transport(0, &[0, 1]).unwrap(),
        &auditor,
    )
    .unwrap();
    let s = Session::<Rec<ServerLoop>>::bind(
        &chor,
        "S",
        1,
        net.transport(1, &[0, 1]).unwrap(),
        &auditor,
    )
    .unwrap();
    let mut c = c.enter();
    let mut s = s.enter();
    let mut store = std::collections::BTreeSet::new();
    let ops: Vec<Op> = (0..60u64)
        .map(|i| match i % 3 {
            0 => Op::Put(i % 17),
            1 => Op::Get(i % 13),
            _ => Op::Del(i % 11),
        })
        .collect();
    let mut gets = Vec::new();
    for op in &ops {
        // Client sends its choice; the server matches it.
        let pending = match op {
            Op::Get(k) => (0, c.select(&Get(*k)).map(|s| (Some(s), None, None))),
            Op::Put(k) => (1, c.select(&Put(*k)).map(|s| (None, Some(s), None))),
            Op::Del(k) => (2, c.select(&Del(*k)).map(|s| (None, None, Some(s)))),
        };
        let choice = s.offer().map_err(err).unwrap();
        s = match choice.split().map_err(err).unwrap() {
            Case::This(Get(k), s) => s
                .send(&Value(u64::from(store.contains(&k))))
                .map_err(err)
                .unwrap()
                .repeat(),
            Case::Other(c) => match c.split().map_err(err).unwrap() {
                Case::This(Put(k), s) => {
                    store.insert(k);
                    s.send(&Done).map_err(err).unwrap().repeat()
                }
                Case::Other(c) => match c.split().map_err(err).unwrap() {
                    Case::This(Del(k), s) => {
                        store.remove(&k);
                        s.send(&Done).map_err(err).unwrap().repeat()
                    }
                    Case::Other(_) => panic!("stop is not sent in this loop"),
                },
            },
        };
        let (_, sessions) = pending;
        c = match sessions.map_err(err).unwrap() {
            (Some(g), _, _) => {
                let (Value(v), g) = g.recv().map_err(err).unwrap();
                gets.push(v);
                g.repeat()
            }
            (_, Some(p), _) => p.recv().map_err(err).unwrap().1.repeat(),
            (_, _, Some(d)) => d.recv().map_err(err).unwrap().1.repeat(),
            _ => unreachable!(),
        };
    }
    c.select(&Stop).map_err(err).unwrap().close();
    match s
        .offer()
        .map_err(err)
        .unwrap()
        .split()
        .map_err(err)
        .unwrap()
    {
        Case::Other(c) => match c.split().map_err(err).unwrap() {
            Case::Other(c) => match c.split().map_err(err).unwrap() {
                Case::Other(c) => match c.split().map_err(err).unwrap() {
                    Case::This(Stop, s) => s.close(),
                    Case::Other(c) => panic!("{:?}", c.exhausted()),
                },
                Case::This(..) => panic!("expected Stop"),
            },
            Case::This(..) => panic!("expected Stop"),
        },
        Case::This(..) => panic!("expected Stop"),
    }
    // The server's answers follow the model.
    let mut model = std::collections::BTreeSet::new();
    let mut expect = Vec::new();
    for op in &ops {
        match op {
            Op::Put(k) => {
                model.insert(*k);
            }
            Op::Del(k) => {
                model.remove(k);
            }
            Op::Get(k) => expect.push(u64::from(model.contains(k))),
        }
    }
    assert_eq!(gets, expect);
    let a = auditor.counts();
    assert_eq!((a.opened, a.closed, a.leaked, a.faulted), (2, 2, 0, 0));
    assert!(net.settle(10_000));
    assert_eq!(net.open_streams(), 0);
    let st = net.stats();
    assert!(st.dropped > 0, "the link lost datagrams: {st:?}");
    assert_eq!(st.frames_delivered, st.frames_sent, "{st:?}");
}

// ---------------------------------------------------------------- fan-out

/// Replicate one slot to three followers (§25.2 `raft_replicate_slot`).
pub const REPLICATE_SLOT: &str = "
roles Leader, F1, F2, F3;
Leader -> F1: Append(Slot);
Leader -> F2: Append(Slot);
Leader -> F3: Append(Slot);
F1: persist;
F2: persist;
F3: persist;
F1 -> Leader: Ack(u64);
F2 -> Leader: Ack(u64);
F3 -> Leader: Ack(u64);
";

role!(RLeader = "Leader");
role!(RF1 = "F1");
role!(RF2 = "F2");
role!(RF3 = "F3");
string_message!(Append = "Append", "Slot");
u64_message!(Ack = "Ack", "u64");
struct Persist;
impl vesper_chorus::session::ActionName for Persist {
    const NAME: &'static str = "persist";
}

type LeaderProto =
    Broadcast<roles![RF1, RF2, RF3], Append, Gather<roles![RF1, RF2, RF3], Ack, End>>;
type FollowerProto =
    Recv<RLeader, Append, vesper_chorus::session::Act<Persist, Send<RLeader, Ack, End>>>;

fn replicate(chor: &Choreography, net: &WarpNet, slot: u64, auditor: &LeakAuditor) -> Vec<u64> {
    let placement = [0, 1, 2, 3];
    let l = Session::<LeaderProto>::bind(
        chor,
        "Leader",
        slot,
        net.transport(0, &placement).unwrap(),
        auditor,
    )
    .unwrap();
    let fs: Vec<Session<FollowerProto>> = ["F1", "F2", "F3"]
        .iter()
        .enumerate()
        .map(|(i, r)| {
            Session::bind(
                chor,
                r,
                slot,
                net.transport(i as u32 + 1, &placement).unwrap(),
                auditor,
            )
            .unwrap()
        })
        .collect();
    let l = l
        .broadcast(&Append(format!("slot-{slot}")))
        .map_err(err)
        .unwrap();
    for (i, f) in fs.into_iter().enumerate() {
        let (Append(v), f) = f.recv().map_err(err).unwrap();
        assert_eq!(v, format!("slot-{slot}"));
        f.act()
            .send(&Ack(slot * 10 + i as u64))
            .map_err(err)
            .unwrap()
            .close();
    }
    let (acks, l) = l.gather().map_err(err).unwrap();
    l.close();
    acks.into_iter().map(|Ack(a)| a).collect()
}

#[test]
fn fan_out_and_fan_in_replicate_slots_over_lossy_warp() {
    let chor = parse_choreography(REPLICATE_SLOT).unwrap();
    let net = lossy(4, 11);
    let auditor = LeakAuditor::new();
    for slot in 1..=40 {
        assert_eq!(
            replicate(&chor, &net, slot, &auditor),
            vec![slot * 10, slot * 10 + 1, slot * 10 + 2]
        );
    }
    let a = auditor.counts();
    assert_eq!((a.opened, a.closed, a.leaked, a.faulted), (160, 160, 0, 0));
    assert!(net.settle(20_000));
    assert_eq!(net.open_streams(), 0);
    let st = net.stats();
    // 40 slots × (3 appends + 3 acks), one stream per direction.
    assert_eq!(st.frames_delivered, 240);
    assert_eq!(st.streams_opened, 240);
    assert!(
        st.dropped > 0 && st.stream_bytes_retransmitted > 0,
        "{st:?}"
    );
}

// ---------------------------------------------------------------- rejection

#[test]
fn bind_rejects_wrong_nary_and_fan_out_types() {
    let net = WarpNet::new(
        WarpNetConfig {
            nodes: 4,
            ..WarpNetConfig::default()
        },
        &common::pki(),
    )
    .unwrap();
    let a = LeakAuditor::new();
    let kv = parse_choreography(KV).unwrap();
    let t = || net.transport(0, &[0, 1]).unwrap();
    let mismatch = |r: Result<(), ChorusError>| {
        assert!(
            matches!(r, Err(ChorusError::TypestateMismatch { .. })),
            "{r:?}"
        );
    };
    // A branch missing.
    type Missing = Rec<
        SelectN<
            RKs,
            branches![
                Branch<Get, Recv<RKs, Value, Var<Z>>>,
                Branch<Put, Recv<RKs, Done, Var<Z>>>,
                Branch<Stop, End>,
            ],
        >,
    >;
    mismatch(Session::<Missing>::bind(&kv, "C", 1, t(), &a).map(drop));
    // A branch continuation wrong (Del answered with Value).
    type WrongNext = Rec<
        SelectN<
            RKs,
            branches![
                Branch<Get, Recv<RKs, Value, Var<Z>>>,
                Branch<Put, Recv<RKs, Done, Var<Z>>>,
                Branch<Del, Recv<RKs, Value, Var<Z>>>,
                Branch<Stop, End>,
            ],
        >,
    >;
    mismatch(Session::<WrongNext>::bind(&kv, "C", 1, t(), &a).map(drop));
    // Selecting where the projection offers.
    type Flipped = Rec<
        OfferN<
            RKs,
            branches![
                Branch<Get, Send<RKs, Value, Var<Z>>>,
                Branch<Put, Send<RKs, Done, Var<Z>>>,
                Branch<Del, Send<RKs, Done, Var<Z>>>,
                Branch<Stop, End>,
            ],
        >,
    >;
    mismatch(Session::<Flipped>::bind(&kv, "C", 1, t(), &a).map(drop));
    // An empty choice is never a projection.
    type Empty = Rec<SelectN<RKs, branches![]>>;
    mismatch(Session::<Empty>::bind(&kv, "C", 1, t(), &a).map(drop));
    // Branch order does not matter: projection sorts labels.
    type Reordered = Rec<
        SelectN<
            RKs,
            branches![
                Branch<Stop, End>,
                Branch<Del, Recv<RKs, Done, Var<Z>>>,
                Branch<Put, Recv<RKs, Done, Var<Z>>>,
                Branch<Get, Recv<RKs, Value, Var<Z>>>,
            ],
        >,
    >;
    let ok = Session::<Reordered>::bind(&kv, "C", 1, t(), &a).unwrap();
    drop(ok); // counted as a leak below
    // Fan-out in the wrong order, or to too few followers.
    let rs = parse_choreography(REPLICATE_SLOT).unwrap();
    let t4 = || net.transport(0, &[0, 1, 2, 3]).unwrap();
    type Order = Broadcast<roles![RF2, RF1, RF3], Append, Gather<roles![RF1, RF2, RF3], Ack, End>>;
    mismatch(Session::<Order>::bind(&rs, "Leader", 1, t4(), &a).map(drop));
    type Short = Broadcast<roles![RF1, RF2], Append, Gather<roles![RF1, RF2, RF3], Ack, End>>;
    mismatch(Session::<Short>::bind(&rs, "Leader", 1, t4(), &a).map(drop));
    type NoPersist = Recv<RLeader, Append, Send<RLeader, Ack, End>>;
    mismatch(Session::<NoPersist>::bind(&rs, "F1", 1, t4(), &a).map(drop));
    let c = a.counts();
    assert_eq!((c.opened, c.leaked), (1, 1));
}

// ---------------------------------------------------------------- failures

#[test]
fn abandoned_peer_is_observed_over_warp() {
    let chor = parse_choreography(KV).unwrap();
    let net = lossy(2, 5);
    let a = LeakAuditor::new();
    let c = Session::<Rec<ClientLoop>>::bind(&chor, "C", 3, net.transport(0, &[0, 1]).unwrap(), &a)
        .unwrap();
    let s = Session::<Rec<ServerLoop>>::bind(&chor, "S", 3, net.transport(1, &[0, 1]).unwrap(), &a)
        .unwrap();
    // The client crashes mid-protocol (drops its end).
    let c = c.enter().select(&Get(1)).map_err(err).unwrap();
    drop(c);
    // The server still gets the Get (sent before the drop), answers, and
    // then sees the client gone.
    let s = match s
        .enter()
        .offer()
        .map_err(err)
        .unwrap()
        .split()
        .map_err(err)
        .unwrap()
    {
        Case::This(Get(1), s) => s,
        _ => panic!("expected Get"),
    };
    let s = s.send(&Value(0));
    // The send may already fail with PeerGone if the abandon arrived.
    let fault = match s {
        Ok(s) => s.repeat().offer().err().expect("client is gone"),
        Err(f) => f,
    };
    assert_eq!(fault.into_error(), ChorusError::Net(NetFail::PeerGone));
    let c = a.counts();
    assert_eq!((c.opened, c.leaked, c.faulted), (2, 1, 1));
    assert!(net.settle(10_000));
    assert_eq!(net.open_streams(), 0);
}

#[test]
fn unanswered_receive_times_out() {
    let chor = parse_choreography(KV).unwrap();
    let net = WarpNet::new(
        WarpNetConfig {
            max_wait_steps: 50,
            ..WarpNetConfig::default()
        },
        &common::pki(),
    )
    .unwrap();
    let a = LeakAuditor::new();
    let s = Session::<Rec<ServerLoop>>::bind(&chor, "S", 1, net.transport(1, &[0, 1]).unwrap(), &a)
        .unwrap();
    let e = s.enter().offer().err().unwrap().into_error();
    assert_eq!(e, ChorusError::Net(NetFail::Timeout));
    assert_eq!(a.counts().faulted, 1);
}

/// A frame is only accepted from the host that the receiver's placement
/// assigns to the sending role: a third host that writes a well-formed
/// stream claiming to be role C is ignored, and the same bytes from C's
/// real host are accepted.
#[test]
fn frames_are_bound_to_the_sending_host() {
    let chor = parse_choreography(KV).unwrap();
    let get = [
        header(5, 0, 1),
        vec![1],
        vec![0; 8],
        vec![3],
        b"Get".to_vec(),
        vec![3],
        b"u64".to_vec(),
        vec![0, 0, 0, 8],
        vec![0; 8],
    ]
    .concat();
    for (forger, accepted) in [(2, false), (0, true)] {
        let net = WarpNet::new(
            WarpNetConfig {
                nodes: 3,
                seed: 9,
                loss_ppm: 50_000,
                max_wait_steps: 400,
                ..WarpNetConfig::default()
            },
            &common::pki(),
        )
        .unwrap();
        let a = LeakAuditor::new();
        // C lives on host 0, S on host 1; host 2 is a bystander.
        let s =
            Session::<Rec<ServerLoop>>::bind(&chor, "S", 5, net.transport(1, &[0, 1]).unwrap(), &a)
                .unwrap();
        net.inject_stream(forger, 1, &get).unwrap();
        let got = s.enter().offer().map(|c| c.split().is_ok());
        match got {
            Ok(true) => assert!(accepted, "frame forged by host {forger} was accepted"),
            Ok(false) => panic!("unexpected branch"),
            Err(f) => {
                assert!(!accepted, "genuine frame was rejected");
                assert_eq!(f.into_error(), ChorusError::Net(NetFail::Timeout));
            }
        }
    }
}

fn header(channel: u64, from: u16, to: u16) -> Vec<u8> {
    let mut v = STREAM_MAGIC.to_vec();
    v.extend(channel.to_be_bytes());
    v.extend(from.to_be_bytes());
    v.extend(to.to_be_bytes());
    v
}

/// Bytes from a peer that does not use the session API: malformed records
/// fault the session with `Malformed`; a frame that is well formed but off
/// the projection is a protocol violation. Nothing panics.
#[test]
fn malformed_and_off_protocol_stream_bytes_are_typed_errors() {
    let chor = parse_choreography(KV).unwrap();
    type Expect = fn(&ChorusError) -> bool;
    let cases: Vec<(Vec<u8>, Expect)> = vec![
        // Unknown record tag.
        ([header(5, 0, 1), vec![9, 9, 9]].concat(), |e| {
            *e == ChorusError::Net(NetFail::Malformed)
        }),
        // Oversized label length.
        (
            [header(5, 0, 1), vec![1], vec![0; 8], vec![200]].concat(),
            |e| *e == ChorusError::Net(NetFail::Malformed),
        ),
        // Invalid UTF-8 label.
        (
            [header(5, 0, 1), vec![1], vec![0; 8], vec![2, 0xff, 0xfe]].concat(),
            |e| *e == ChorusError::Net(NetFail::Malformed),
        ),
        // Body longer than max_body.
        (
            [
                header(5, 0, 1),
                vec![1],
                vec![0; 8],
                vec![3],
                b"Get".to_vec(),
                vec![3],
                b"u64".to_vec(),
                vec![0xff; 4],
            ]
            .concat(),
            |e| *e == ChorusError::Net(NetFail::Malformed),
        ),
        // Truncated record, then FIN.
        ([header(5, 0, 1), vec![1, 0, 0]].concat(), |e| {
            *e == ChorusError::Net(NetFail::Malformed)
        }),
        // Well formed, but `Value` is not something the server is offered.
        (
            [
                header(5, 0, 1),
                vec![1],
                vec![0; 8],
                vec![5],
                b"Value".to_vec(),
                vec![3],
                b"u64".to_vec(),
                vec![0, 0, 0, 8],
                vec![0; 8],
            ]
            .concat(),
            |e| matches!(e, ChorusError::ProtocolViolation { .. }),
        ),
        // Well formed and on the projection, but a bad payload encoding.
        (
            [
                header(5, 0, 1),
                vec![1],
                vec![0; 8],
                vec![3],
                b"Get".to_vec(),
                vec![3],
                b"u64".to_vec(),
                vec![0, 0, 0, 2],
                vec![0; 2],
            ]
            .concat(),
            |e| matches!(e, ChorusError::Decode { .. }),
        ),
    ];
    for (i, (bytes, ok)) in cases.into_iter().enumerate() {
        let net = lossy(2, 100 + i as u64);
        let a = LeakAuditor::new();
        let s =
            Session::<Rec<ServerLoop>>::bind(&chor, "S", 5, net.transport(1, &[0, 1]).unwrap(), &a)
                .unwrap();
        net.inject_stream(0, 1, &bytes).unwrap();
        let e = match s.enter().offer() {
            Err(f) => f.into_error(),
            Ok(c) => match c.split() {
                Err(f) => f.into_error(),
                Ok(_) => panic!("case {i}: accepted"),
            },
        };
        assert!(ok(&e), "case {i}: {e:?}");
        assert!(
            net.link_failure().is_none(),
            "case {i}: the connection survives"
        );
    }
    // A bad magic number cannot be attributed to any session: it is counted
    // and stopped, and the connection keeps working.
    let net = lossy(2, 1);
    net.inject_stream(0, 1, b"XXXX and then some").unwrap();
    assert!(net.settle(10_000));
    assert_eq!(net.stats().malformed_streams, 1);
    assert!(net.link_failure().is_none());
}

#[test]
fn config_is_validated() {
    for cfg in [
        WarpNetConfig {
            nodes: 1,
            ..WarpNetConfig::default()
        },
        WarpNetConfig {
            nodes: 17,
            ..WarpNetConfig::default()
        },
        WarpNetConfig {
            loss_ppm: 1_000_000,
            ..WarpNetConfig::default()
        },
        WarpNetConfig {
            step_ns: 0,
            ..WarpNetConfig::default()
        },
        WarpNetConfig {
            max_streams_uni: 0,
            ..WarpNetConfig::default()
        },
        WarpNetConfig {
            max_body: 0,
            ..WarpNetConfig::default()
        },
    ] {
        assert!(WarpNet::new(cfg, &common::pki()).is_err(), "{cfg:?}");
    }
    let net = WarpNet::new(WarpNetConfig::default(), &common::pki()).unwrap();
    assert!(net.transport(2, &[0, 1]).is_err());
    assert!(net.transport(0, &[0, 5]).is_err());
}
