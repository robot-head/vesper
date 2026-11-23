//! Part 2 differential oracle suite (#1407): pins bit-for-bit behavior of
//! simnet, session (Monitor, LeakAuditor, RawSession, Session/Endpoint/MemoryNet),
//! raft wire codecs/checker/QuorumRule/run_cluster, and replog against exact
//! digests recorded on `cf28be7a` (pure Rust before deleting the Rust logic).

#![allow(clippy::type_complexity, clippy::field_reassign_with_default)]

mod common;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;
use vesper_chorus::chor::RoleId;
use vesper_chorus::parse::parse_choreography;
use vesper_chorus::raft::{
    AppendAck, Checker, Entry, QuorumRule, RaftAppendWireHeader, RaftConfig, Role as RaftRole,
    VoteReq, WireError, config_of, decode_batch, encode_batch, log_matching_violation, run_cluster,
};
use vesper_chorus::replog::{ReplogConfig, run_replicated_log};
use vesper_chorus::session::{
    Act, Branch, Case, ChorusError, End, FaultReason, LeakAuditor, MemoryNet, Message, Monitor,
    NetFail, Offer, Offered, RawSession, Rec, Recv, RoleName, S, Select, Send, Session, TryRecv,
    Var, Z, describe,
};
use vesper_chorus::simnet::{
    RoleStep, SimMode, SimNet, SimNetConfig, SimRole, SplitMix, run_roles,
};

fn fnv_u64(h: &mut u64, v: u64) {
    for b in v.to_le_bytes() {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

fn fnv_bytes(h: &mut u64, bytes: &[u8]) {
    fnv_u64(h, bytes.len() as u64);
    for &b in bytes {
        *h ^= u64::from(b);
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

fn fnv_str(h: &mut u64, s: &str) {
    fnv_bytes(h, s.as_bytes());
}

// ---------------------------------------------------------------------------
// 1. Wire codecs, QuorumRule, config_of, log_matching_violation, Checker

fn diff_wire_and_checker() -> (u64, usize) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut cases = 0usize;
    let mut rng = SplitMix(0x7769_7265_5f66_757a);

    // Headers + batches (sizes 0..=256 including CONFIG_FLAG)
    for len in [0usize, 1, 2, 7, 64, 255, 256] {
        let hdr = RaftAppendWireHeader {
            term: rng.next_u64(),
            leader_id: rng.next_u64() as u32,
            follower_slot: rng.next_u64() as u32,
            prev_log_index: rng.next_u64(),
            prev_log_term: rng.next_u64(),
            leader_commit: rng.next_u64(),
        };
        let hb = hdr.encode();
        fnv_bytes(&mut h, &hb);
        let hdec = RaftAppendWireHeader::decode(&hb).expect("valid header");
        assert_eq!(hdec, hdr);
        cases += 1;

        let entries: Vec<Entry> = (0..len)
            .map(|k| Entry {
                term: rng.next_u64(),
                cmd: if k % 5 == 0 {
                    rng.next_u64() | (1u64 << 63)
                } else {
                    rng.next_u64() & !(1u64 << 63)
                },
            })
            .collect();
        let bb = encode_batch(&hdr, &entries);
        fnv_bytes(&mut h, &bb);
        let (dh, de) = decode_batch(&bb).expect("valid batch");
        assert_eq!(dh, hdr);
        assert_eq!(de, entries);
        cases += 1;

        // Quorum/config checks on same log
        fnv_u64(&mut h, config_of(&entries, 0b10101));
        cases += 1;
    }

    // Header errors
    for bad_len in [0usize, 1, 39, 41, 64] {
        let buf = vec![0xabu8; bad_len];
        let err = RaftAppendWireHeader::decode(&buf).unwrap_err();
        fnv_str(&mut h, &format!("{err:?}"));
        cases += 1;
    }

    // Batch errors: truncated header, count=257 (TooManyEntries), length mismatch
    for bad_len in [0usize, 43, 45, 61] {
        let mut buf = vec![0u8; bad_len];
        if bad_len == 45 {
            buf[40..44].copy_from_slice(&0u32.to_be_bytes());
        }
        let err = decode_batch(&buf).unwrap_err();
        fnv_str(&mut h, &format!("{err:?}"));
        cases += 1;
    }
    {
        let mut buf = vec![0u8; 44];
        buf[40..44].copy_from_slice(&257u32.to_be_bytes());
        let err = decode_batch(&buf).unwrap_err();
        assert!(matches!(err, WireError::TooManyEntries(257)));
        fnv_str(&mut h, &format!("{err:?}"));
        cases += 1;
    }

    // AppendAck valid + error cases
    for acc in [false, true] {
        let ack = AppendAck {
            follower_id: rng.next_u64() as u32,
            term: rng.next_u64(),
            match_index: rng.next_u64(),
            accepted: acc,
        };
        let b = ack.encode();
        fnv_bytes(&mut h, &b);
        assert_eq!(AppendAck::decode(&b).unwrap(), ack);
        cases += 1;
    }
    for flag in [2u8, 17, 255] {
        let mut b = [0u8; 21];
        b[20] = flag;
        let err = AppendAck::decode(&b).unwrap_err();
        assert_eq!(err, WireError::BadFlag(flag));
        fnv_str(&mut h, &format!("{err:?}"));
        cases += 1;
    }
    for bad_len in [0usize, 20, 22] {
        let err = AppendAck::decode(&vec![0u8; bad_len]).unwrap_err();
        fnv_str(&mut h, &format!("{err:?}"));
        cases += 1;
    }

    // VoteReq valid + error cases
    for _ in 0..8 {
        let vr = VoteReq {
            term: rng.next_u64(),
            candidate: rng.next_u64() as u32,
            last_log_index: rng.next_u64(),
            last_log_term: rng.next_u64(),
        };
        let b = vr.encode();
        fnv_bytes(&mut h, &b);
        assert_eq!(VoteReq::decode(&b).unwrap(), vr);
        cases += 1;
    }
    for bad_len in [0usize, 27, 29] {
        let err = VoteReq::decode(&vec![0u8; bad_len]).unwrap_err();
        fnv_str(&mut h, &format!("{err:?}"));
        cases += 1;
    }

    // QuorumRule sizes 0..=64
    for n in 0..=64u32 {
        fnv_u64(&mut h, u64::from(QuorumRule::Majority.size(n)));
        fnv_u64(&mut h, u64::from(QuorumRule::HalfFloor.size(n)));
        cases += 2;
    }

    // log_matching_violation + Checker invariants
    let a = [
        Entry { term: 1, cmd: 10 },
        Entry { term: 2, cmd: 20 },
        Entry { term: 3, cmd: 30 },
    ];
    let b = [
        Entry { term: 1, cmd: 10 },
        Entry { term: 2, cmd: 99 },
        Entry { term: 3, cmd: 30 },
    ];
    let c = [
        Entry { term: 1, cmd: 10 },
        Entry { term: 3, cmd: 20 },
        Entry { term: 4, cmd: 30 },
    ];
    let v_ab = log_matching_violation(&a, &b);
    let v_ac = log_matching_violation(&a, &c);
    fnv_u64(&mut h, v_ab.map_or(u64::MAX, |x| x as u64));
    fnv_u64(&mut h, v_ac.map_or(u64::MAX, |x| x as u64));
    cases += 2;

    let mut chk = Checker::default();
    chk.leader_elected(10, 1, 0);
    chk.leader_elected(20, 1, 0);
    chk.logs_changed(30, 0, &[&a, &a, &c]);
    chk.committed(40, 0, &a, 2);
    chk.committed(50, 1, &b, 2);
    fnv_u64(&mut h, chk.checks);
    fnv_u64(&mut h, chk.committed_len() as u64);
    fnv_u64(&mut h, chk.leaders().len() as u64);
    fnv_str(&mut h, &format!("{:?}", chk.violation));
    let mut chk2 = Checker::default();
    chk2.leader_elected(15, 2, 1);
    chk2.leader_elected(25, 2, 3);
    chk2.compacted_changed(30, 0, 1);
    fnv_str(&mut h, &format!("{:?}", chk2.violation));
    cases += 2;

    (h, cases)
}

// ---------------------------------------------------------------------------
// 2. SplitMix & SimNet & run_roles

struct SndPing(u64);
impl Message for SndPing {
    const LABEL: &'static str = "Ping";
    const PAYLOAD: &'static str = "U64";
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes());
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let a: [u8; 8] = bytes.try_into().map_err(|_| "bad len".to_string())?;
        Ok(Self(u64::from_be_bytes(a)))
    }
}

struct RecvPing(u64);
impl Message for RecvPing {
    const LABEL: &'static str = "Ping";
    const PAYLOAD: &'static str = "U64";
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes());
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let a: [u8; 8] = bytes.try_into().map_err(|_| "bad len".to_string())?;
        Ok(Self(u64::from_be_bytes(a)))
    }
}

struct SimBob {
    s: Option<Session<Recv<A, SndPing, Send<A, RecvPing, End>>>>,
}
struct A;
impl RoleName for A {
    const NAME: &'static str = "A";
}
struct B;
impl RoleName for B {
    const NAME: &'static str = "B";
}

struct SimAlicePoll {
    sent: Option<Session<Recv<B, RecvPing, End>>>,
    init: Option<Session<Send<B, SndPing, Recv<B, RecvPing, End>>>>,
    done_sum: Rc<Cell<u64>>,
}
impl SimRole for SimAlicePoll {
    fn step(&mut self) -> Result<RoleStep, ChorusError> {
        if let Some(s) = self.init.take() {
            let s2 = match s.send(&SndPing(42)) {
                Ok(n) => n,
                Err(f) => return Err(f.into_error()),
            };
            self.sent = Some(s2);
            return Ok(RoleStep::Progress);
        }
        let Some(s) = self.sent.take() else {
            return Ok(RoleStep::Done);
        };
        match s.try_recv() {
            TryRecv::Ready((RecvPing(v), end)) => {
                self.done_sum.set(self.done_sum.get() + v);
                end.close();
                Ok(RoleStep::Done)
            }
            TryRecv::Empty(s) => {
                self.sent = Some(s);
                Ok(RoleStep::Blocked)
            }
            TryRecv::Fault(f) => Err(f.into_error()),
        }
    }
}

impl SimRole for SimBob {
    fn step(&mut self) -> Result<RoleStep, ChorusError> {
        let Some(s) = self.s.take() else {
            return Ok(RoleStep::Done);
        };
        match s.try_recv() {
            TryRecv::Ready((SndPing(v), s)) => {
                let s = match s.send(&RecvPing(v * 2)) {
                    Ok(n) => n,
                    Err(f) => return Err(f.into_error()),
                };
                s.close();
                Ok(RoleStep::Done)
            }
            TryRecv::Empty(s) => {
                self.s = Some(s);
                Ok(RoleStep::Blocked)
            }
            TryRecv::Fault(f) => Err(f.into_error()),
        }
    }
}

fn diff_simnet() -> (u64, usize) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut cases = 0usize;

    let mut sm = SplitMix(123456789);
    for _ in 0..50 {
        fnv_u64(&mut h, sm.next_u64());
        fnv_u64(&mut h, sm.range(10, 10));
        fnv_u64(&mut h, sm.range(100, 50));
        fnv_u64(&mut h, sm.range(1000, 99999));
        fnv_u64(&mut h, u64::from(sm.chance(0)));
        fnv_u64(&mut h, u64::from(sm.chance(333)));
        fnv_u64(&mut h, u64::from(sm.chance(1000)));
        cases += 7;
    }

    let chor =
        parse_choreography("roles A, B; A -> B: Ping(U64); B -> A: Ping(U64);").expect("parses");

    for seed in [1u64, 42, 999] {
        let net = SimNet::new(
            seed,
            SimNetConfig {
                min_latency_ns: 1_000,
                max_latency_ns: 50_000,
                duplicate_per_mille: 300,
            },
        );
        let audit = LeakAuditor::new();
        let sa: Session<Send<B, SndPing, Recv<B, RecvPing, End>>> =
            Session::bind(&chor, "A", 7 + seed, net.handle(), &audit).unwrap();
        let sb: Session<Recv<A, SndPing, Send<A, RecvPing, End>>> =
            Session::bind(&chor, "B", 7 + seed, net.handle(), &audit).unwrap();
        let sum_a = Rc::new(Cell::new(0));
        let a = SimAlicePoll {
            init: Some(sa),
            sent: None,
            done_sum: Rc::clone(&sum_a),
        };
        let b = SimBob { s: Some(sb) };
        let rep = run_roles(
            SimMode::Record(seed),
            &net,
            &mut [Box::new(a), Box::new(b)],
            10_000,
        )
        .expect("run_roles succeeds");
        fnv_u64(&mut h, sum_a.get());
        fnv_u64(&mut h, rep.end_ns);
        fnv_u64(&mut h, rep.stats.sent);
        fnv_u64(&mut h, rep.stats.delivered);
        fnv_u64(&mut h, rep.stats.duplicated);
        fnv_u64(&mut h, rep.undelivered as u64);
        fnv_u64(&mut h, audit.counts().opened);
        fnv_u64(&mut h, audit.counts().closed);
        fnv_u64(&mut h, audit.counts().faulted);
        fnv_u64(&mut h, audit.counts().leaked);
        fnv_u64(&mut h, audit.counts().duplicates_discarded);
        assert_eq!(audit.open(), 0);
        cases += 1;

        // Replay verification
        let net_r = SimNet::new(
            seed,
            SimNetConfig {
                min_latency_ns: 1_000,
                max_latency_ns: 50_000,
                duplicate_per_mille: 300,
            },
        );
        let audit_r = LeakAuditor::new();
        let sar: Session<Send<B, SndPing, Recv<B, RecvPing, End>>> =
            Session::bind(&chor, "A", 7 + seed, net_r.handle(), &audit_r).unwrap();
        let sbr: Session<Recv<A, SndPing, Send<A, RecvPing, End>>> =
            Session::bind(&chor, "B", 7 + seed, net_r.handle(), &audit_r).unwrap();
        let ar = SimAlicePoll {
            init: Some(sar),
            sent: None,
            done_sum: Rc::new(Cell::new(0)),
        };
        let br = SimBob { s: Some(sbr) };
        let rep_r = run_roles(
            SimMode::Replay(rep.journal.clone()),
            &net_r,
            &mut [Box::new(ar), Box::new(br)],
            10_000,
        )
        .expect("replay succeeds");
        assert_eq!(rep_r.end_ns, rep.end_ns);
        assert_eq!(rep_r.stats, rep.stats);
        cases += 1;
    }

    (h, cases)
}

// ---------------------------------------------------------------------------
// 3. Session, Monitor, LeakAuditor, Endpoint reorder & errors

struct Yes(u64);
impl Message for Yes {
    const LABEL: &'static str = "Yes";
    const PAYLOAD: &'static str = "Num";
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_be_bytes());
    }
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        Ok(Self(u64::from_be_bytes(
            bytes.try_into().map_err(|_| "bad len".to_string())?,
        )))
    }
}
struct No;
impl Message for No {
    const LABEL: &'static str = "No";
    const PAYLOAD: &'static str = "Unit";
    fn encode(&self, _out: &mut Vec<u8>) {}
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        bytes
            .is_empty()
            .then_some(Self)
            .ok_or_else(|| "non-empty".to_string())
    }
}
struct WorkAct;
impl vesper_chorus::session::ActionName for WorkAct {
    const NAME: &'static str = "tick";
}

type RecProtoA = Rec<Send<B, Yes, Var<Z>>>;
type RecProtoWrong = Rec<Send<B, No, Var<Z>>>;

fn diff_session() -> (u64, usize) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut cases = 0usize;

    let chor = parse_choreography(
        "roles A, B; rec Loop { A -> B: Yes(Num); B: tick; B -> A { Yes(Num) { continue Loop; } No(Unit) { end; } } }",
    )
    .expect("parses");

    // Monitor errors & state inspection
    for role in ["A", "B", "Ghost"] {
        match Monitor::new(&chor, role) {
            Ok(mut m) => {
                fnv_str(&mut h, &format!("{:?}", m.current()));
                fnv_u64(&mut h, u64::from(m.is_end()));
                fnv_u64(&mut h, m.awaiting().map_or(u64::MAX, |r| u64::from(r.0)));
                fnv_u64(&mut h, m.role_id("B").map_or(u64::MAX, |r| u64::from(r.0)));
                let s_err = m.on_send(RoleId(1), "Bogus", "Num");
                fnv_str(
                    &mut h,
                    &s_err.err().map_or(String::new(), |e| e.to_string()),
                );
                if role == "A" {
                    m.on_send(RoleId(1), "Yes", "Num").unwrap();
                    fnv_str(&mut h, &format!("{:?}", m.current()));
                } else {
                    m.on_recv(RoleId(0), "Yes", "Num").unwrap();
                    fnv_str(&mut h, &format!("{:?}", m.current()));
                    let a_err = m.on_send(RoleId(0), "Yes", "Num").unwrap_err();
                    fnv_str(&mut h, &a_err.to_string());
                    m.on_action("tick").unwrap();
                    fnv_str(&mut h, &format!("{:?}", m.current()));
                }
            }
            Err(e) => {
                fnv_str(&mut h, &e.to_string());
            }
        }
        cases += 1;
    }

    // Typestate mismatch on Session::bind vs valid describe
    let loop_chor =
        parse_choreography("roles A, B; rec Loop { A -> B: Yes(Num); continue Loop; }").unwrap();
    let net_m = MemoryNet::new();
    let aud_m = LeakAuditor::new();
    let Err(bind_err) = Session::<RecProtoWrong>::bind(&loop_chor, "A", 1, net_m.handle(), &aud_m)
    else {
        panic!("expected TypestateMismatch");
    };
    fnv_str(&mut h, &bind_err.to_string());
    let ok_desc = describe::<RecProtoA>(&loop_chor);
    assert!(ok_desc.is_ok());
    cases += 2;

    // LeakAuditor + RawSession open / close error / leak
    {
        let aud = LeakAuditor::new();
        let r1 = RawSession::open(&chor, "A", 1, &aud).unwrap();
        let err_r = r1.close().unwrap_err().0;
        fnv_str(&mut h, &err_r.to_string());
        let r2 = RawSession::open(&chor, "A", 2, &aud).unwrap();
        drop(r2); // leaked
        let r3 = RawSession::open(&chor, "A", 3, &aud).unwrap();
        r3.fault(FaultReason::Crash);
        let c = aud.counts();
        fnv_u64(&mut h, c.opened);
        fnv_u64(&mut h, c.closed);
        fnv_u64(&mut h, c.faulted);
        fnv_u64(&mut h, c.leaked);
        let leaks = aud.leaks();
        fnv_u64(&mut h, leaks.len() as u64);
        if let Some(l) = leaks.first() {
            fnv_u64(&mut h, l.channel);
            fnv_str(&mut h, &l.role);
            fnv_str(&mut h, &l.state);
        }
        cases += 1;
    }

    // MemoryNet + duplicate discard + out-of-order delivery + Exhausted error + REORDER_LIMIT error
    {
        let net = MemoryNet::new();
        let aud = LeakAuditor::new();
        let sa: Session<Rec<Send<B, Yes, Offer<B, Branch<Yes, Var<Z>>, Branch<No, End>>>>> =
            Session::bind(&chor, "A", 10, net.handle(), &aud).unwrap();
        let sb: Session<
            Rec<Recv<A, Yes, Act<WorkAct, Select<A, Branch<Yes, Var<Z>>, Branch<No, End>>>>>,
        > = Session::bind(&chor, "B", 10, net.handle(), &aud).unwrap();

        let sa = sa.enter();
        let sb = sb.enter();
        // Send Yes from A -> B, plus duplicate frame injected with seq 0
        let sa = sa.send(&Yes(11)).unwrap();
        net.inject(vesper_chorus::session::Frame {
            channel: 10,
            from: RoleId(0),
            to: RoleId(1),
            seq: 0,
            label: "Yes".into(),
            payload_ty: "Num".into(),
            body: 999u64.to_be_bytes().to_vec(),
        });
        let (Yes(got), sb) = sb.recv().unwrap();
        assert_eq!(got, 11);
        let sb = sb.act();
        let sb = sb.select_left(&Yes(22)).unwrap();
        let Offered::Left(Yes(back), sa) = sa.offer().unwrap() else {
            panic!("expected left");
        };
        assert_eq!(back, 22);
        // Loop second iteration -> No -> End
        let sa = sa.repeat().send(&Yes(33)).unwrap();
        let (Yes(got2), sb) = sb.repeat().recv().unwrap();
        assert_eq!(got2, 33);
        let sb = sb.act();
        let sb = sb.select_right(&No).unwrap();
        sb.close();
        let Offered::Right(No, sa) = sa.offer().unwrap() else {
            panic!("expected right");
        };
        sa.close();
        let counts = aud.counts();
        fnv_u64(&mut h, counts.opened);
        fnv_u64(&mut h, counts.closed);
        fnv_u64(&mut h, counts.duplicates_discarded);
        cases += 1;

        // Reorder far ahead violation + NetFail error string
        let net2 = MemoryNet::new();
        let aud2 = LeakAuditor::new();
        let sc: Session<
            Rec<Recv<A, Yes, Act<WorkAct, Select<A, Branch<Yes, Var<Z>>, Branch<No, End>>>>>,
        > = Session::bind(&chor, "B", 20, net2.handle(), &aud2).unwrap();
        let sc = sc.enter();
        net2.inject(vesper_chorus::session::Frame {
            channel: 20,
            from: RoleId(0),
            to: RoleId(1),
            seq: 5000,
            label: "Yes".into(),
            payload_ty: "Num".into(),
            body: 1u64.to_be_bytes().to_vec(),
        });
        let Err(f) = sc.recv() else {
            panic!("expected reorder violation");
        };
        let f_err = f.into_error();
        fnv_str(&mut h, &f_err.to_string());
        fnv_str(&mut h, &format!("{:?}", NetFail::Timeout));
        cases += 1;
    }

    // Exhausted SelectN/OfferN branch mismatch check
    {
        let chor_c =
            parse_choreography("roles A, B; A -> B { Yes(Num) { end; } No(Unit) { end; } }")
                .unwrap();
        let net = MemoryNet::new();
        let aud = LeakAuditor::new();
        let sa: Session<
            vesper_chorus::session::SelectN<
                B,
                vesper_chorus::session::BCons<
                    Branch<Yes, End>,
                    vesper_chorus::session::BCons<Branch<No, End>, vesper_chorus::session::BNil>,
                >,
            >,
        > = Session::bind(&chor_c, "A", 30, net.handle(), &aud).unwrap();
        let sb: Session<
            vesper_chorus::session::OfferN<
                A,
                vesper_chorus::session::BCons<
                    Branch<No, End>,
                    vesper_chorus::session::BCons<Branch<Yes, End>, vesper_chorus::session::BNil>,
                >,
            >,
        > = Session::bind(&chor_c, "B", 30, net.handle(), &aud).unwrap();
        let sa = sa
            .select::<Yes, vesper_chorus::session::Here>(&Yes(7))
            .unwrap();
        sa.close();
        let c = sb.offer().unwrap();
        fnv_str(&mut h, c.label());
        match c.split() {
            Ok(Case::Other(rest)) => match rest.split() {
                Ok(Case::This(Yes(v), s)) => {
                    fnv_u64(&mut h, v);
                    s.close();
                }
                Ok(Case::Other(empty)) => {
                    fnv_str(&mut h, &empty.exhausted().into_error().to_string());
                }
                Err(f) => panic!("unexpected fault: {}", f.error()),
            },
            _ => panic!("expected Other"),
        };
        cases += 1;
    }

    let _unused: Option<Select<A, End, End>> = None;
    let _unused2: Option<S<Z>> = None;

    (h, cases)
}

// ---------------------------------------------------------------------------
// 4. Raft run_cluster across configs + replay + error strings

fn digest_raft_report(h: &mut u64, rep: &vesper_chorus::raft::RaftRunReport) {
    fnv_u64(h, rep.digest);
    fnv_str(h, &format!("{:?}", rep.violation));
    fnv_u64(h, rep.open_sessions);
    fnv_u64(h, rep.audit.opened);
    fnv_u64(h, rep.audit.closed);
    fnv_u64(h, rep.audit.faulted);
    fnv_u64(h, rep.audit.leaked);
    fnv_u64(h, rep.checks);
    fnv_u64(h, rep.committed as u64);
    fnv_u64(h, rep.end_ns);
    fnv_u64(h, rep.leaders.len() as u64);
    for (&term, &node) in &rep.leaders {
        fnv_u64(h, term);
        fnv_u64(h, u64::from(node));
    }
    for &m in &rep.members {
        fnv_u64(h, m);
    }
    fnv_u64(h, rep.stats.elections);
    fnv_u64(h, rep.stats.leaders_elected);
    fnv_u64(h, rep.stats.max_term);
    fnv_u64(h, rep.stats.sent);
    fnv_u64(h, rep.stats.dropped);
    fnv_u64(h, rep.stats.partitioned);
    fnv_u64(h, rep.stats.duplicated);
    fnv_u64(h, rep.stats.crashes);
    fnv_u64(h, rep.stats.stale_responses);
    fnv_u64(h, rep.stats.duplicate_requests);
    fnv_u64(h, rep.stats.rejected_frames);
    fnv_u64(h, rep.stats.timeouts);
    fnv_u64(h, rep.stats.proposals);
    fnv_u64(h, rep.stats.compactions);
    fnv_u64(h, rep.stats.snapshots_sent);
    fnv_u64(h, rep.stats.snapshots_installed);
    fnv_u64(h, rep.stats.config_changes);
    fnv_u64(h, rep.stats.leader_step_downs);
    fnv_u64(h, rep.stats.sticky_vote_denials);
    fnv_u64(h, rep.stats.persists);
    fnv_u64(h, rep.stats.disk_syncs);
    fnv_u64(h, rep.stats.recoveries);
    fnv_u64(h, rep.stats.torn_crashes);
    fnv_u64(h, rep.stats.torn_entries_lost);
    fnv_u64(h, rep.stats.torn_bytes_dropped);
    fnv_u64(h, rep.stats.disk_truncations);
    fnv_u64(h, rep.stats.disk_snapshots);
}

fn diff_raft() -> (u64, usize) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut cases = 0usize;

    // Default 5-node
    for seed in [1u64, 2, 3, 7, 42] {
        let cfg = RaftConfig::new(5);
        let rep = run_cluster(seed, &cfg, None).expect("5-node");
        assert!(rep.violation.is_none());
        assert_eq!(rep.open_sessions, 0);
        digest_raft_report(&mut h, &rep);
        cases += 1;

        // Replay check on seed 1
        if seed == 1 {
            let rep_r = run_cluster(seed, &cfg, Some(rep.journal)).expect("replay");
            assert_eq!(rep_r.digest, rep.digest);
            cases += 1;
        }
    }

    // 7-node stress
    for seed in [10u64, 11] {
        let mut cfg = RaftConfig::new(7);
        cfg.drop_per_mille = 80;
        cfg.duplicate_per_mille = 150;
        let rep = run_cluster(seed, &cfg, None).expect("7-node");
        assert!(rep.violation.is_none());
        digest_raft_report(&mut h, &rep);
        cases += 1;
    }

    // HalfFloor negative check
    for seed in [1u64, 2, 3] {
        let mut cfg = RaftConfig::new(5);
        cfg.quorum = QuorumRule::HalfFloor;
        cfg.crashes = false;
        cfg.partitions = false;
        let rep = run_cluster(seed, &cfg, None).expect("halffloor");
        digest_raft_report(&mut h, &rep);
        cases += 1;
    }

    // Snapshots
    for seed in [1u64, 4, 9] {
        let mut cfg = RaftConfig::new(5);
        cfg.snapshot_threshold = 4;
        cfg.duration_ns = 3_000_000_000;
        let rep = run_cluster(seed, &cfg, None).expect("snapshots");
        assert!(rep.violation.is_none());
        digest_raft_report(&mut h, &rep);
        cases += 1;
    }

    // Membership changes + combined
    for seed in [1u64, 5] {
        let mut cfg = RaftConfig::new(5);
        cfg.initial_members = 3;
        cfg.membership_per_mille = 400;
        cfg.min_members = 3;
        cfg.snapshot_threshold = 4;
        cfg.duration_ns = 3_000_000_000;
        let rep = run_cluster(seed, &cfg, None).expect("membership+snapshots");
        assert!(rep.violation.is_none());
        digest_raft_report(&mut h, &rep);
        cases += 1;
    }

    // Durable (torn=0 matches non-durable) + torn crashes + unsafe reply_before_sync
    {
        let mut cfg_mem = RaftConfig::new(5);
        cfg_mem.snapshot_threshold = 5;
        let rep_mem = run_cluster(1, &cfg_mem, None).unwrap();
        let mut cfg_dur = cfg_mem;
        cfg_dur.durable = true;
        let rep_dur = run_cluster(1, &cfg_dur, None).unwrap();
        assert_eq!(rep_dur.digest, rep_mem.digest);
        digest_raft_report(&mut h, &rep_dur);
        cases += 1;

        for seed in [1u64, 2] {
            let mut cfg_t = RaftConfig::new(5);
            cfg_t.durable = true;
            cfg_t.torn_crash_per_mille = 200;
            cfg_t.snapshot_threshold = 5;
            let rep_t = run_cluster(seed, &cfg_t, None).unwrap();
            assert!(rep_t.violation.is_none());
            digest_raft_report(&mut h, &rep_t);
            cases += 1;
        }

        let mut cfg_unsafe = RaftConfig::new(5);
        cfg_unsafe.durable = true;
        cfg_unsafe.torn_crash_per_mille = 400;
        cfg_unsafe.reply_before_sync = true;
        cfg_unsafe.propose_per_mille = 900;
        let rep_u = run_cluster(3, &cfg_unsafe, None).unwrap();
        digest_raft_report(&mut h, &rep_u);
        cases += 1;
    }

    // All 7 config validation error cases
    let bad_configs = [
        RaftConfig::new(0),
        {
            let mut c = RaftConfig::new(5);
            c.election_min_ns = 500;
            c.election_max_ns = 100;
            c
        },
        {
            let mut c = RaftConfig::new(5);
            c.max_batch = 257;
            c
        },
        {
            let mut c = RaftConfig::new(5);
            c.snapshot_threshold = 4;
            c.max_log = 300;
            c
        },
        {
            let mut c = RaftConfig::new(5);
            c.initial_members = 0;
            c
        },
        {
            let mut c = RaftConfig::new(64);
            c.membership_per_mille = 100;
            c
        },
        {
            let mut c = RaftConfig::new(5);
            c.torn_crash_per_mille = 50;
            c
        },
    ];
    for c in bad_configs {
        let e = run_cluster(1, &c, None).unwrap_err();
        fnv_str(&mut h, &format!("{e:?}"));
        cases += 1;
    }

    let _ = RaftRole::Follower;
    (h, cases)
}

// ---------------------------------------------------------------------------
// 5. Replog run_replicated_log

fn digest_replog(h: &mut u64, rep: &vesper_chorus::replog::ReplogReport) {
    fnv_u64(h, rep.digest);
    fnv_str(h, &format!("{:?}", rep.violation));
    fnv_u64(h, rep.checks);
    fnv_u64(h, rep.committed as u64);
    fnv_u64(h, rep.open_sessions);
    fnv_u64(h, rep.open_streams as u64);
    fnv_u64(h, u64::from(rep.settled));
    for (&t, &n) in &rep.leaders {
        fnv_u64(h, t);
        fnv_u64(h, u64::from(n));
    }
    fnv_u64(h, rep.stats.elections);
    fnv_u64(h, rep.stats.elections_won);
    fnv_u64(h, rep.stats.votes_granted);
    fnv_u64(h, rep.stats.votes_denied);
    fnv_u64(h, rep.stats.appends);
    fnv_u64(h, rep.stats.accepted);
    fnv_u64(h, rep.stats.rejected);
    fnv_u64(h, rep.stats.rpc_faults);
    fnv_u64(h, rep.stats.proposals);
    fnv_u64(h, rep.stats.partitioned_links);
    fnv_u64(h, rep.net.datagrams);
    fnv_u64(h, rep.net.dropped);
    fnv_u64(h, rep.audit.opened);
    fnv_u64(h, rep.audit.closed);
    fnv_u64(h, rep.audit.faulted);
    fnv_u64(h, rep.audit.leaked);
}

fn diff_replog() -> (u64, usize) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut cases = 0usize;
    let tls = common::replog_pki();

    for (seed, nodes, loss, part) in [
        (1u64, 3u32, 0u32, 0u32),
        (1, 5, 50_000, 150),
        (7, 5, 50_000, 150),
        (3, 7, 50_000, 150),
    ] {
        let mut cfg = ReplogConfig::default();
        cfg.net.seed = seed;
        cfg.net.nodes = nodes;
        cfg.net.loss_ppm = loss;
        if loss == 0 {
            cfg.net.reorder_ppm = 0;
            cfg.net.duplicate_ppm = 0;
        }
        cfg.partition_per_mille = part;
        let rep = run_replicated_log(&cfg, &tls).expect("replog");
        assert!(rep.violation.is_none());
        assert_eq!(rep.open_sessions, 0);
        assert!(rep.settled);
        digest_replog(&mut h, &rep);
        cases += 1;
    }

    for mut bad in [
        {
            let mut c = ReplogConfig::default();
            c.net.nodes = 2;
            c
        },
        {
            let mut c = ReplogConfig::default();
            c.max_batch = 0;
            c
        },
        {
            let mut c = ReplogConfig::default();
            c.max_batch = 257;
            c
        },
    ] {
        bad.net.seed = 1;
        let e = run_replicated_log(&bad, &tls).unwrap_err();
        fnv_str(&mut h, &format!("{e:?}"));
        cases += 1;
    }

    (h, cases)
}

// Reference FNV-1a digests recorded on `cf28be7a` before deleting Rust code.
const PIN_WIRE: (u64, usize) = (0x4b1ab48d7939fda6, 184);
const PIN_SIMNET: (u64, usize) = (0x1e878dfdaf9fd030, 356);
const PIN_SESSION: (u64, usize) = (0x8894357d773e0dbb, 9);
const PIN_RAFT: (u64, usize) = (0x4149c4cb64fa4a56, 27);
const PIN_REPLOG: (u64, usize) = (0x9ce9a53c48241d55, 7);

static RESULTS: OnceLock<[(u64, usize); 5]> = OnceLock::new();

fn compute_all() -> [(u64, usize); 5] {
    *RESULTS.get_or_init(|| {
        [
            diff_wire_and_checker(),
            diff_simnet(),
            diff_session(),
            diff_raft(),
            diff_replog(),
        ]
    })
}

#[test]
fn part2_differential_against_cf28be7a_pin() {
    let all = compute_all();
    let names = ["WIRE", "SIMNET", "SESSION", "RAFT", "REPLOG"];
    let pins = [PIN_WIRE, PIN_SIMNET, PIN_SESSION, PIN_RAFT, PIN_REPLOG];
    if std::env::var("VESPER_CHORUS_PRINT_PIN").is_ok() {
        for (n, (d, c)) in names.iter().zip(all.iter()) {
            println!("const PIN_{n}: (u64, usize) = ({d:#018x}, {c});");
        }
        return;
    }
    let mut diffs = 0usize;
    let mut total_cases = 0usize;
    for ((n, got), pin) in names.iter().zip(all.iter()).zip(pins.iter()) {
        total_cases += got.1;
        if *got != *pin {
            eprintln!(
                "DIFF {n}: got=({:#018x}, {}) pin=({:#018x}, {})",
                got.0, got.1, pin.0, pin.1
            );
            diffs += 1;
        }
    }
    println!("flip-gate differential differences={diffs} cases={total_cases}");
    assert_eq!(
        diffs, 0,
        "Part 2 behavior diverged from cf28be7a Rust baseline"
    );
    assert_eq!(vesper_chorus::port_live_allocations(), 0);
}
