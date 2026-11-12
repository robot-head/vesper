//! Typestate sessions over the in-memory and SimPulse transports.

use vesper_chorus::session::{
    Branch, ChorusError, End, Frame, LeakAuditor, MemoryNet, NetFail, Offer, Offered, RawSession,
    Rec, Recv, Select, Send, Session, TryRecv, Var, Z,
};
use vesper_chorus::simnet::{
    RoleStep, SimMode, SimNet, SimNetConfig, SimRole, SimRunError, run_roles,
};
use vesper_chorus::{RoleId, parse_choreography, role, string_message, u64_message, unit_message};

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
type B2Decide =
    Select<RS, Branch<OkMsg, Send<RS, Address, Recv<RS, Date, End>>>, Branch<Quit, End>>;
type B2Proto = Recv<RS, Quote, Recv<RB1, Share, B2Decide>>;
type SOffer =
    Offer<RB2, Branch<OkMsg, Recv<RB2, Address, Send<RB2, Date, End>>>, Branch<Quit, End>>;
type SProto = Recv<RB1, Title, Send<RB1, Quote, Send<RB2, Quote, SOffer>>>;

fn buyer1(s: Session<B1Proto>, title: &str) -> Result<u64, ChorusError> {
    let s = s.send(&Title(title.into())).map_err(|f| f.into_error())?;
    let (Quote(q), s) = s.recv().map_err(|f| f.into_error())?;
    let s = s.send(&Share(q / 2)).map_err(|f| f.into_error())?;
    s.close();
    Ok(q)
}

fn buyer2(s: Session<B2Proto>, budget: u64) -> Result<Option<u64>, ChorusError> {
    let (Quote(q), s) = s.recv().map_err(|f| f.into_error())?;
    let (Share(share), s) = s.recv().map_err(|f| f.into_error())?;
    if q - share <= budget {
        let s = s.select_left(&OkMsg).map_err(|f| f.into_error())?;
        let s = s
            .send(&Address("1 Loop".into()))
            .map_err(|f| f.into_error())?;
        let (Date(d), s) = s.recv().map_err(|f| f.into_error())?;
        s.close();
        Ok(Some(d))
    } else {
        let s = s.select_right(&Quit).map_err(|f| f.into_error())?;
        s.close();
        Ok(None)
    }
}

fn seller(s: Session<SProto>, price: u64) -> Result<bool, ChorusError> {
    let (Title(t), s) = s.recv().map_err(|f| f.into_error())?;
    assert_eq!(t, "Types and Programming Languages");
    let s = s.send(&Quote(price)).map_err(|f| f.into_error())?;
    let s = s.send(&Quote(price)).map_err(|f| f.into_error())?;
    match s.offer().map_err(|f| f.into_error())? {
        Offered::Left(OkMsg, s) => {
            let (Address(a), s) = s.recv().map_err(|f| f.into_error())?;
            assert_eq!(a, "1 Loop");
            let s = s.send(&Date(20261112)).map_err(|f| f.into_error())?;
            s.close();
            Ok(true)
        }
        Offered::Right(Quit, s) => {
            s.close();
            Ok(false)
        }
    }
}

#[test]
fn two_buyer_runs_on_threads_over_memory_net() {
    let chor = parse_choreography(TWO_BUYER).unwrap();
    for (price, budget, expect) in [(100, 60, true), (100, 10, false)] {
        let net = MemoryNet::new();
        let auditor = LeakAuditor::new();
        let b1 = Session::<B1Proto>::bind(&chor, "B1", 7, net.handle(), &auditor).unwrap();
        let b2 = Session::<B2Proto>::bind(&chor, "B2", 7, net.handle(), &auditor).unwrap();
        let s = Session::<SProto>::bind(&chor, "S", 7, net.handle(), &auditor).unwrap();
        let t1 = std::thread::spawn(move || buyer1(b1, "Types and Programming Languages"));
        let t2 = std::thread::spawn(move || buyer2(b2, budget));
        let ts = std::thread::spawn(move || seller(s, price));
        assert_eq!(t1.join().unwrap().unwrap(), price);
        assert_eq!(t2.join().unwrap().unwrap().is_some(), expect);
        assert_eq!(ts.join().unwrap().unwrap(), expect);
        let c = auditor.counts();
        assert_eq!((c.opened, c.closed, c.leaked, c.faulted), (3, 3, 0, 0));
        assert_eq!(net.undelivered(), 0);
    }
}

#[test]
fn bind_rejects_a_protocol_type_that_is_not_the_projection() {
    let chor = parse_choreography(TWO_BUYER).unwrap();
    let net = MemoryNet::new();
    let auditor = LeakAuditor::new();
    // B2's protocol bound for B1.
    let err = Session::<B2Proto>::bind(&chor, "B1", 1, net.handle(), &auditor)
        .err()
        .unwrap();
    assert!(matches!(err, ChorusError::TypestateMismatch { ref role, .. } if role == "B1"));
    // Swapped message order.
    type Wrong = Recv<RS, Quote, Send<RS, Title, Send<RB2, Share, End>>>;
    let err = Session::<Wrong>::bind(&chor, "B1", 1, net.handle(), &auditor)
        .err()
        .unwrap();
    match err {
        ChorusError::TypestateMismatch {
            projected,
            rust_type,
            ..
        } => {
            assert_eq!(
                projected,
                "S!Title(String); S?Quote(u64); B2!Share(u64); end"
            );
            assert_eq!(
                rust_type,
                "S?Quote(u64); S!Title(String); B2!Share(u64); end"
            );
        }
        other => panic!("{other:?}"),
    }
    // Unknown role marker.
    role!(Nobody = "Z");
    type Ghost = Send<Nobody, Title, End>;
    assert!(matches!(
        Session::<Ghost>::bind(&chor, "B1", 1, net.handle(), &auditor),
        Err(ChorusError::UnknownRole(_))
    ));
    assert_eq!(auditor.counts().opened, 0);
}

fn raw_frame(
    channel: u64,
    from: u16,
    to: u16,
    seq: u64,
    label: &str,
    ty: &str,
    body: Vec<u8>,
) -> Frame {
    Frame {
        channel,
        from: RoleId(from),
        to: RoleId(to),
        seq,
        label: label.into(),
        payload_ty: ty.into(),
        body,
    }
}

#[test]
fn misbehaving_peers_are_rejected_at_the_receiving_boundary() {
    let chor = parse_choreography(TWO_BUYER).unwrap();
    let auditor = LeakAuditor::new();
    // S expects Title(String) from B1. A raw peer sends Quote instead.
    let net = MemoryNet::new();
    let s = Session::<SProto>::bind(&chor, "S", 3, net.handle(), &auditor).unwrap();
    net.inject(raw_frame(
        3,
        0,
        2,
        0,
        "Quote",
        "u64",
        5u64.to_be_bytes().to_vec(),
    ));
    let err = s.recv().err().unwrap().into_error();
    assert!(
        matches!(&err, ChorusError::ProtocolViolation { role, expected, found }
            if role == "S" && expected == "receive Title(String) from B1" && found == "receive Quote(u64) from B1"),
        "{err:?}"
    );
    // Right label, malformed payload.
    let net = MemoryNet::new();
    let s = Session::<SProto>::bind(&chor, "S", 3, net.handle(), &auditor).unwrap();
    net.inject(raw_frame(3, 0, 2, 0, "Title", "String", vec![0xff, 0xfe]));
    assert!(matches!(
        s.recv().err().unwrap().into_error(),
        ChorusError::Decode { .. }
    ));
    // B2 offers a label the protocol does not have.
    let net = MemoryNet::new();
    let s = Session::<SProto>::bind(&chor, "S", 3, net.handle(), &auditor).unwrap();
    net.inject(raw_frame(
        3,
        0,
        2,
        0,
        "Title",
        "String",
        b"Types and Programming Languages".to_vec(),
    ));
    let (_, s) = s.recv().unwrap();
    let s = s.send(&Quote(1)).unwrap();
    let s = s.send(&Quote(1)).unwrap();
    net.inject(raw_frame(3, 1, 2, 0, "Maybe", "", vec![]));
    let err = s.offer().err().unwrap().into_error();
    assert!(
        matches!(err, ChorusError::ProtocolViolation { ref expected, .. } if expected == "offer Ok|Quit with B2")
    );
    // A replayed (duplicate) frame is discarded by sequence number.
    let net = MemoryNet::new();
    let s = Session::<SProto>::bind(&chor, "S", 4, net.handle(), &auditor).unwrap();
    let title = raw_frame(
        4,
        0,
        2,
        0,
        "Title",
        "String",
        b"Types and Programming Languages".to_vec(),
    );
    net.inject(title.clone());
    let (_, s) = s.recv().unwrap();
    let s = s.send(&Quote(1)).unwrap();
    let s = s.send(&Quote(1)).unwrap();
    net.inject(title);
    net.inject(raw_frame(4, 1, 2, 0, "Quit", "", vec![]));
    match s.offer().unwrap() {
        Offered::Right(Quit, s) => s.close(),
        Offered::Left(..) => panic!("wrong branch"),
    }
    assert_eq!(
        auditor.counts().duplicates_discarded,
        0,
        "duplicate was from B1, not B2"
    );
    let c = auditor.counts();
    assert_eq!(c.faulted, 3);
    assert_eq!(c.leaked, 0);
}

#[test]
fn dropping_a_session_before_end_is_detected_and_unblocks_the_peer() {
    let chor = parse_choreography(TWO_BUYER).unwrap();
    let net = MemoryNet::new();
    let auditor = LeakAuditor::new();
    let b1 = Session::<B1Proto>::bind(&chor, "B1", 9, net.handle(), &auditor).unwrap();
    let s = Session::<SProto>::bind(&chor, "S", 9, net.handle(), &auditor).unwrap();
    let seller = std::thread::spawn(move || s.recv().err().map(|f| f.into_error()));
    drop(b1);
    assert_eq!(
        seller.join().unwrap(),
        Some(ChorusError::Net(NetFail::PeerGone))
    );
    let leaks = auditor.leaks();
    assert_eq!(leaks.len(), 1);
    assert_eq!(leaks[0].role, "B1");
    assert_eq!(leaks[0].state, "send Title(String) to S");
    // A Fault dropped without handing its error to a handler is a leak too.
    let b1 = Session::<B1Proto>::bind(&chor, "B1", 10, net.handle(), &auditor).unwrap();
    net.shutdown();
    let fault = b1.send(&Title("x".into())).err().unwrap();
    drop(fault);
    assert_eq!(auditor.counts().leaked, 2);
    // Raw sessions: closing early returns the owner; dropping it leaks.
    let raw = RawSession::open(&chor, "B1", 11, &auditor).unwrap();
    let (_, raw) = raw.close().err().unwrap();
    drop(raw);
    assert_eq!(auditor.counts().leaked, 3);
}

const STREAM: &str = "
roles P, C;
rec X {
    P -> C {
        Item(u64) { continue X; }
        Stop() { }
    }
}
";
role!(RP = "P");
role!(RC = "C");
u64_message!(Item = "Item", "u64");
unit_message!(Stop = "Stop");
type PProto = Rec<Select<RC, Branch<Item, Var<Z>>, Branch<Stop, End>>>;
type CProto = Rec<Offer<RP, Branch<Item, Var<Z>>, Branch<Stop, End>>>;

#[test]
fn recursive_session_types_loop() {
    let chor = parse_choreography(STREAM).unwrap();
    let net = MemoryNet::new();
    let auditor = LeakAuditor::new();
    let p = Session::<PProto>::bind(&chor, "P", 1, net.handle(), &auditor).unwrap();
    let c = Session::<CProto>::bind(&chor, "C", 1, net.handle(), &auditor).unwrap();
    let producer = std::thread::spawn(move || {
        let mut s = p.enter();
        for i in 0..100u64 {
            s = s.select_left(&Item(i)).unwrap().repeat();
        }
        s.select_right(&Stop).unwrap().close();
    });
    let mut s = c.enter();
    let mut sum = 0;
    loop {
        match s.offer().unwrap() {
            Offered::Left(Item(i), next) => {
                sum += i;
                s = next.repeat();
            }
            Offered::Right(Stop, end) => {
                end.close();
                break;
            }
        }
    }
    producer.join().unwrap();
    assert_eq!(sum, 4950);
    assert_eq!(auditor.counts().closed, 2);
}

// ------------------------------------------------------------ SimPulse

enum B1St {
    Start(Session<B1Proto>),
    Wait(Session<Recv<RS, Quote, Send<RB2, Share, End>>>),
    Done,
}
struct B1Role(Option<B1St>);
impl SimRole for B1Role {
    fn step(&mut self) -> Result<RoleStep, ChorusError> {
        match self.0.take().unwrap() {
            B1St::Start(s) => {
                let s = s
                    .send(&Title("Types and Programming Languages".into()))
                    .map_err(|f| f.into_error())?;
                self.0 = Some(B1St::Wait(s));
                Ok(RoleStep::Progress)
            }
            B1St::Wait(s) => match s.try_recv() {
                TryRecv::Empty(s) => {
                    self.0 = Some(B1St::Wait(s));
                    Ok(RoleStep::Blocked)
                }
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready((Quote(q), s)) => {
                    s.send(&Share(q / 2)).map_err(|f| f.into_error())?.close();
                    self.0 = Some(B1St::Done);
                    Ok(RoleStep::Done)
                }
            },
            B1St::Done => Ok(RoleStep::Done),
        }
    }
}

enum B2St {
    Quote(Session<B2Proto>),
    Share(u64, Session<Recv<RB1, Share, B2Decide>>),
    Date(Session<Recv<RS, Date, End>>),
    Done,
}
struct B2Role(Option<B2St>, u64);
impl SimRole for B2Role {
    fn step(&mut self) -> Result<RoleStep, ChorusError> {
        let blocked = |st: B2St, me: &mut Self| {
            me.0 = Some(st);
            Ok(RoleStep::Blocked)
        };
        match self.0.take().unwrap() {
            B2St::Quote(s) => match s.try_recv() {
                TryRecv::Empty(s) => blocked(B2St::Quote(s), self),
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready((Quote(q), s)) => {
                    self.0 = Some(B2St::Share(q, s));
                    Ok(RoleStep::Progress)
                }
            },
            B2St::Share(q, s) => match s.try_recv() {
                TryRecv::Empty(s) => blocked(B2St::Share(q, s), self),
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready((Share(sh), s)) => {
                    if q - sh <= self.1 {
                        let s = s.select_left(&OkMsg).map_err(|f| f.into_error())?;
                        let s = s
                            .send(&Address("1 Loop".into()))
                            .map_err(|f| f.into_error())?;
                        self.0 = Some(B2St::Date(s));
                        Ok(RoleStep::Progress)
                    } else {
                        s.select_right(&Quit).map_err(|f| f.into_error())?.close();
                        self.0 = Some(B2St::Done);
                        Ok(RoleStep::Done)
                    }
                }
            },
            B2St::Date(s) => match s.try_recv() {
                TryRecv::Empty(s) => blocked(B2St::Date(s), self),
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready((Date(_), s)) => {
                    s.close();
                    self.0 = Some(B2St::Done);
                    Ok(RoleStep::Done)
                }
            },
            B2St::Done => Ok(RoleStep::Done),
        }
    }
}

enum SSt {
    Title(Session<SProto>),
    Offer(Session<SOffer>),
    Address(Session<Recv<RB2, Address, Send<RB2, Date, End>>>),
    Done,
}
struct SRole(Option<SSt>, u64);
impl SimRole for SRole {
    fn step(&mut self) -> Result<RoleStep, ChorusError> {
        let blocked = |st: SSt, me: &mut Self| {
            me.0 = Some(st);
            Ok(RoleStep::Blocked)
        };
        match self.0.take().unwrap() {
            SSt::Title(s) => match s.try_recv() {
                TryRecv::Empty(s) => blocked(SSt::Title(s), self),
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready((_, s)) => {
                    let s = s.send(&Quote(self.1)).map_err(|f| f.into_error())?;
                    let s = s.send(&Quote(self.1)).map_err(|f| f.into_error())?;
                    self.0 = Some(SSt::Offer(s));
                    Ok(RoleStep::Progress)
                }
            },
            SSt::Offer(s) => match s.try_offer() {
                TryRecv::Empty(s) => blocked(SSt::Offer(s), self),
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready(Offered::Left(OkMsg, s)) => {
                    self.0 = Some(SSt::Address(s));
                    Ok(RoleStep::Progress)
                }
                TryRecv::Ready(Offered::Right(Quit, s)) => {
                    s.close();
                    self.0 = Some(SSt::Done);
                    Ok(RoleStep::Done)
                }
            },
            SSt::Address(s) => match s.try_recv() {
                TryRecv::Empty(s) => blocked(SSt::Address(s), self),
                TryRecv::Fault(f) => Err(f.into_error()),
                TryRecv::Ready((_, s)) => {
                    s.send(&Date(1)).map_err(|f| f.into_error())?.close();
                    self.0 = Some(SSt::Done);
                    Ok(RoleStep::Done)
                }
            },
            SSt::Done => Ok(RoleStep::Done),
        }
    }
}

fn sim_two_buyer(
    mode: SimMode,
    net_seed: u64,
    budget: u64,
) -> (
    Result<vesper_chorus::simnet::SimRunReport, SimRunError>,
    LeakAuditor,
) {
    let chor = parse_choreography(TWO_BUYER).unwrap();
    let net = SimNet::new(
        net_seed,
        SimNetConfig {
            duplicate_per_mille: 300,
            ..SimNetConfig::default()
        },
    );
    let auditor = LeakAuditor::new();
    let b1 = Session::<B1Proto>::bind(&chor, "B1", 1, net.handle(), &auditor).unwrap();
    let b2 = Session::<B2Proto>::bind(&chor, "B2", 1, net.handle(), &auditor).unwrap();
    let s = Session::<SProto>::bind(&chor, "S", 1, net.handle(), &auditor).unwrap();
    let mut roles: Vec<Box<dyn SimRole>> = vec![
        Box::new(B1Role(Some(B1St::Start(b1)))),
        Box::new(B2Role(Some(B2St::Quote(b2)), budget)),
        Box::new(SRole(Some(SSt::Title(s)), 100)),
    ];
    let r = run_roles(mode, &net, &mut roles, 100_000);
    (r, auditor)
}

#[test]
fn two_buyer_runs_under_simpulse_with_reordering_and_duplication() {
    let mut duplicates = 0;
    for seed in 0..300u64 {
        let budget = if seed % 2 == 0 { 60 } else { 10 };
        let (r, auditor) = sim_two_buyer(SimMode::Record(seed), seed, budget);
        let r = r.unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        let c = auditor.counts();
        assert_eq!(
            (c.opened, c.closed, c.leaked, c.faulted),
            (3, 3, 0, 0),
            "seed {seed}"
        );
        duplicates += c.duplicates_discarded;
        // Late duplicates may still be in flight or unread when every role
        // has closed its session.
        assert!(
            r.stats.delivered + r.undelivered as u64 >= r.stats.sent,
            "seed {seed}"
        );
        // Replay with the recorded journal: identical scheduling, checked
        // event by event.
        let (again, _) = sim_two_buyer(SimMode::Replay(r.journal.clone()), seed, budget);
        let again = again.unwrap_or_else(|e| panic!("replay seed {seed}: {e:?}"));
        assert_eq!(again.journal, r.journal);
        assert_eq!(again.end_ns, r.end_ns);
    }
    assert!(duplicates > 0, "duplication was exercised");
}

#[test]
fn simpulse_reports_a_deadlock_when_a_role_waits_forever() {
    // B1 never sends its Title: the seller and B2 block, nothing is in flight.
    struct Idle;
    impl SimRole for Idle {
        fn step(&mut self) -> Result<RoleStep, ChorusError> {
            Ok(RoleStep::Done)
        }
    }
    let chor = parse_choreography(TWO_BUYER).unwrap();
    let net = SimNet::new(1, SimNetConfig::default());
    let auditor = LeakAuditor::new();
    let b2 = Session::<B2Proto>::bind(&chor, "B2", 1, net.handle(), &auditor).unwrap();
    let s = Session::<SProto>::bind(&chor, "S", 1, net.handle(), &auditor).unwrap();
    let mut roles: Vec<Box<dyn SimRole>> = vec![
        Box::new(Idle),
        Box::new(B2Role(Some(B2St::Quote(b2)), 0)),
        Box::new(SRole(Some(SSt::Title(s)), 1)),
    ];
    match run_roles(SimMode::Record(5), &net, &mut roles, 10_000) {
        Err(SimRunError::Deadlock { blocked }) => assert_eq!(blocked, vec![1, 2]),
        other => panic!("{other:?}"),
    }
    drop(roles);
    // The blocked sessions were dropped with the roles: two counted leaks.
    assert_eq!(auditor.counts().leaked, 2);
}

#[test]
fn replay_rejects_a_journal_with_unconsumed_events() {
    let (r, _) = sim_two_buyer(SimMode::Record(3), 3, 60);
    let mut j = r.unwrap().journal;
    let last = j.events.last().unwrap().clone();
    j.events.push(last);
    match sim_two_buyer(SimMode::Replay(j), 3, 60).0 {
        Err(SimRunError::Sim(vesper_simpulse::Error::ReplayUnconsumed { remaining: 1 })) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_abandoned_peer_wakes_a_blocked_receiver_under_simpulse() {
    // B1 makes progress once, then drops its session without sending. The
    // seller, possibly already parked waiting for Title, must observe
    // PeerGone rather than the run ending in a deadlock.
    struct Quitter(Option<Session<B1Proto>>, bool);
    impl SimRole for Quitter {
        fn step(&mut self) -> Result<RoleStep, ChorusError> {
            if !self.1 {
                self.1 = true;
                return Ok(RoleStep::Progress);
            }
            drop(self.0.take());
            Ok(RoleStep::Done)
        }
    }
    let chor = parse_choreography(TWO_BUYER).unwrap();
    for seed in 0..50 {
        let net = SimNet::new(seed, SimNetConfig::default());
        let auditor = LeakAuditor::new();
        let b1 = Session::<B1Proto>::bind(&chor, "B1", 1, net.handle(), &auditor).unwrap();
        let s = Session::<SProto>::bind(&chor, "S", 1, net.handle(), &auditor).unwrap();
        let mut roles: Vec<Box<dyn SimRole>> = vec![
            Box::new(Quitter(Some(b1), false)),
            Box::new(SRole(Some(SSt::Title(s)), 1)),
        ];
        match run_roles(SimMode::Record(seed), &net, &mut roles, 10_000) {
            Err(SimRunError::Role {
                role: 1,
                error: ChorusError::Net(NetFail::PeerGone),
            }) => {}
            other => panic!("seed {seed}: {other:?}"),
        }
    }
}
