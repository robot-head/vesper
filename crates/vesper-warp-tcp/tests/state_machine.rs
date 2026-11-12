//! RFC 9293 §3.10 transitions driven by a scripted peer, plus RFC 5961
//! challenge-ACK cases. Every reply is parsed back from the wire bytes the
//! endpoint produced.

mod common;
use common::*;
use vesper_warp_tcp::conn::{CloseReason, Config, Event, SendView};
use vesper_warp_tcp::wire::{self, HeaderFields, Timestamps};
use vesper_warp_tcp::{
    Connection, Flags, IpPair, NewReno, Segment, Seq, State, TcpError, TcpOptions,
};

const LOCAL: [u8; 4] = [10, 0, 0, 1];
const PEER: [u8; 4] = [10, 0, 0, 9];
const LPORT: u16 = 80;
const PPORT: u16 = 40000;
const PEER_ISS: u32 = 0xffff_ff00; // wraps during the tests

#[derive(Debug, Clone)]
struct Out {
    seq: u32,
    ack: u32,
    flags: Flags,
    #[allow(dead_code)]
    window: u16,
    payload: Vec<u8>,
    opts: TcpOptions,
    sport: u16,
    dport: u16,
}

impl Out {
    fn has(&self, f: Flags) -> bool {
        self.flags.contains(f)
    }
}

struct T {
    c: Connection<NewReno>,
    ring: Vec<u8>,
    now: u64,
    send: Vec<u8>,
    /// Peer's next sequence number.
    pseq: u32,
    /// Peer's view of our next sequence number (what it acknowledges).
    pack: u32,
    peer_ts: Option<Timestamps>,
}

fn ips_in() -> IpPair {
    IpPair::V4 {
        src: PEER,
        dst: LOCAL,
    }
}

fn build(
    seq: u32,
    ack: u32,
    flags: Flags,
    window: u16,
    opts: &TcpOptions,
    payload: &[u8],
) -> Vec<u8> {
    let mut buf = vec![0u8; 60 + payload.len()];
    let h = HeaderFields {
        src_port: PPORT,
        dst_port: LPORT,
        seq: Seq(seq),
        ack: Seq(ack),
        flags,
        window,
        urgent: 0,
    };
    let hl = wire::write_header(&mut buf, &h, opts).unwrap();
    buf[hl..hl + payload.len()].copy_from_slice(payload);
    buf.truncate(hl + payload.len());
    wire::fill_checksum(ips_in(), &mut buf).unwrap();
    buf
}

fn quiet_cfg() -> Config {
    let mut c = cfg(LPORT);
    c.delayed_ack_ns = 0;
    c.recv_buffer = 4096;
    c
}

impl T {
    fn with(cfg: Config) -> T {
        let ring = vec![0; cfg.recv_buffer as usize];
        T {
            c: Connection::new(cfg).unwrap(),
            ring,
            now: 0,
            send: Vec::new(),
            pseq: PEER_ISS,
            pack: 0,
            peer_ts: None,
        }
    }

    fn rx(
        &mut self,
        seq: u32,
        ack: u32,
        flags: Flags,
        payload: &[u8],
    ) -> Result<(Vec<u8>, bool), TcpError> {
        let mut opts = TcpOptions::default();
        if let Some(ts) = self.peer_ts {
            opts.timestamps = Some(ts);
        }
        self.rx_opts(seq, ack, flags, 1000, &opts, payload)
    }

    fn rx_opts(
        &mut self,
        seq: u32,
        ack: u32,
        flags: Flags,
        window: u16,
        opts: &TcpOptions,
        payload: &[u8],
    ) -> Result<(Vec<u8>, bool), TcpError> {
        let bytes = build(seq, ack, flags, window, opts, payload);
        let r = self
            .c
            .on_segment(self.now, ips_in(), &bytes, &mut self.ring)?;
        let mut v = Vec::new();
        for ch in r.chunks() {
            v.extend_from_slice(ch);
        }
        Ok((v, r.fin))
    }

    fn tx(&mut self) -> Vec<Out> {
        let mut outs = Vec::new();
        let mut buf = vec![0u8; 2048];
        loop {
            let view = SendView {
                base: self.c.acked_offset().min(self.send.len() as u64),
                bytes: &[],
            };
            let base = view.base as usize;
            let view = SendView {
                base: view.base,
                bytes: &self.send[base..],
            };
            let Some(t) = self.c.poll_transmit(self.now, &view, &mut buf).unwrap() else {
                break;
            };
            let pkt = &buf[..t.len];
            wire::verify_checksum(t.ips, pkt).unwrap();
            let s = Segment::parse(pkt).unwrap();
            outs.push(Out {
                seq: s.seq.0,
                ack: s.ack.0,
                flags: s.flags,
                window: s.window,
                payload: s.payload.to_vec(),
                opts: s.options,
                sport: s.src_port,
                dport: s.dst_port,
            });
            assert!(outs.len() < 100, "runaway transmit");
        }
        outs
    }

    fn one(&mut self) -> Out {
        let mut v = self.tx();
        assert_eq!(v.len(), 1, "expected exactly one segment, got {v:?}");
        v.remove(0)
    }

    fn none(&mut self) {
        let v = self.tx();
        assert!(v.is_empty(), "expected silence, got {v:?}");
    }

    fn events(&mut self) -> Vec<Event> {
        let mut v = Vec::new();
        while let Some(e) = self.c.take_event() {
            v.push(e);
        }
        v
    }

    /// Passive open through to ESTABLISHED.
    fn established_passive(cfg: Config) -> T {
        let mut t = T::with(cfg);
        t.c.listen(1000).unwrap();
        t.rx(PEER_ISS, 0, Flags::SYN, &[]).unwrap();
        let synack = t.one();
        assert!(synack.has(Flags::SYN | Flags::ACK));
        t.pseq = PEER_ISS.wrapping_add(1);
        t.pack = synack.seq.wrapping_add(1);
        t.rx(t.pseq, t.pack, Flags::ACK, &[]).unwrap();
        assert_eq!(t.c.state(), State::Established);
        t.none();
        t
    }

    fn est() -> T {
        T::established_passive(quiet_cfg())
    }

    fn ack(&mut self) -> Result<(Vec<u8>, bool), TcpError> {
        self.rx(self.pseq, self.pack, Flags::ACK, &[])
    }

    fn fin(&mut self) {
        self.rx(self.pseq, self.pack, Flags::FIN | Flags::ACK, &[])
            .unwrap();
        self.pseq = self.pseq.wrapping_add(1);
    }

    fn rst_exact(&mut self) {
        self.rx(self.pseq, 0, Flags::RST, &[]).unwrap();
    }
}

// ---------------------------------------------------------------- CLOSED

#[test]
fn closed_replies_with_rfc9293_resets() {
    let mut t = T::with(quiet_cfg());
    t.rx(500, 0, Flags::SYN, b"hi").unwrap();
    let r = t.one();
    assert!(r.has(Flags::RST | Flags::ACK));
    assert_eq!((r.seq, r.ack), (0, 503), "<SEQ=0><ACK=SEG.SEQ+SEG.LEN>");
    assert_eq!((r.sport, r.dport), (LPORT, PPORT));
    t.rx(500, 777, Flags::ACK, &[]).unwrap();
    let r = t.one();
    assert_eq!(r.flags, Flags::RST);
    assert_eq!(r.seq, 777, "<SEQ=SEG.ACK><CTL=RST>");
    t.rx(500, 0, Flags::RST, &[]).unwrap();
    t.none();
    assert_eq!(t.c.state(), State::Closed);
}

// ---------------------------------------------------------------- LISTEN

#[test]
fn listen_ignores_rst_resets_ack_and_accepts_syn() {
    let mut t = T::with(quiet_cfg());
    t.c.listen(1000).unwrap();
    t.rx(1, 0, Flags::RST, &[]).unwrap();
    t.none();
    t.rx(1, 4242, Flags::ACK, &[]).unwrap();
    let r = t.one();
    assert_eq!((r.flags, r.seq), (Flags::RST, 4242));
    assert_eq!(t.c.state(), State::Listen);
    t.rx(1, 0, Flags(0), b"data").unwrap();
    t.none();
    t.rx(PEER_ISS, 0, Flags::SYN, &[]).unwrap();
    assert_eq!(t.c.state(), State::SynReceived);
    let r = t.one();
    assert!(r.has(Flags::SYN | Flags::ACK));
    assert_eq!(r.seq, 1000, "ISS from caller base plus 4 µs clock at t=0");
    assert_eq!(r.ack, PEER_ISS.wrapping_add(1));
    assert_eq!(r.opts.mss, Some(1000));
    assert_eq!(r.opts.window_scale, None, "not offered by peer");
    assert!(!r.opts.sack_permitted && r.opts.timestamps.is_none());
}

#[test]
fn listen_close_goes_to_closed() {
    let mut t = T::with(quiet_cfg());
    t.c.listen(0).unwrap();
    t.c.close().unwrap();
    assert_eq!(t.c.state(), State::Closed);
    assert_eq!(t.events(), vec![Event::Closed(None)]);
}

// ---------------------------------------------------------------- SYN-RECEIVED

fn syn_received() -> (T, Out) {
    let mut t = T::with(quiet_cfg());
    t.c.listen(1000).unwrap();
    t.rx(PEER_ISS, 0, Flags::SYN, &[]).unwrap();
    let synack = t.one();
    t.pseq = PEER_ISS.wrapping_add(1);
    t.pack = synack.seq.wrapping_add(1);
    (t, synack)
}

#[test]
fn syn_received_ack_establishes() {
    let (mut t, _) = syn_received();
    t.ack().unwrap();
    assert_eq!(t.c.state(), State::Established);
    assert_eq!(t.events(), vec![Event::Connected]);
}

#[test]
fn syn_received_unacceptable_ack_sends_reset() {
    let (mut t, synack) = syn_received();
    // ACK of our ISS itself (not of the SYN).
    t.rx(t.pseq, synack.seq, Flags::ACK, &[]).unwrap();
    let r = t.one();
    assert_eq!((r.flags, r.seq), (Flags::RST, synack.seq));
    // ACK beyond SND.NXT.
    t.rx(t.pseq, synack.seq.wrapping_add(50), Flags::ACK, &[])
        .unwrap();
    let r = t.one();
    assert_eq!((r.flags, r.seq), (Flags::RST, synack.seq.wrapping_add(50)));
    assert_eq!(t.c.state(), State::SynReceived);
}

#[test]
fn syn_received_rst_returns_passive_open_to_listen() {
    let (mut t, _) = syn_received();
    t.rst_exact();
    assert_eq!(t.c.state(), State::Listen);
    t.none();
    t.rx(77, 0, Flags::SYN, &[]).unwrap();
    assert_eq!(t.c.state(), State::SynReceived);
}

#[test]
fn syn_received_duplicate_syn_resends_syn_ack_and_other_syn_is_challenged() {
    let (mut t, synack) = syn_received();
    t.rx(PEER_ISS, 0, Flags::SYN, &[]).unwrap();
    let r = t.one();
    assert!(r.has(Flags::SYN | Flags::ACK));
    assert_eq!(r.seq, synack.seq);
    // A SYN with a different, in-window sequence number: challenge ACK.
    t.rx(t.pseq.wrapping_add(10), 0, Flags::SYN, &[]).unwrap();
    let r = t.one();
    assert_eq!(r.flags, Flags::ACK);
    assert_eq!(r.ack, t.pseq);
    assert_eq!(t.c.stats().challenge_acks, 1);
    assert_eq!(t.c.state(), State::SynReceived);
}

#[test]
fn syn_received_close_queues_fin_until_established() {
    let (mut t, _) = syn_received();
    t.c.close().unwrap();
    assert_eq!(t.c.state(), State::SynReceived);
    t.none();
    t.ack().unwrap();
    assert_eq!(t.c.state(), State::FinWait1);
    let fin = t.one();
    assert!(fin.has(Flags::FIN | Flags::ACK));
    assert_eq!(fin.seq, t.pack);
}

#[test]
fn syn_received_retransmits_syn_ack_on_timeout() {
    let (mut t, synack) = syn_received();
    let d = t.c.poll_timeout().unwrap();
    t.now = d;
    t.c.on_timeout(d);
    let r = t.one();
    assert!(r.has(Flags::SYN | Flags::ACK));
    assert_eq!(r.seq, synack.seq);
}

// ---------------------------------------------------------------- SYN-SENT

fn syn_sent(cfg: Config) -> (T, Out) {
    let mut t = T::with(cfg);
    t.c.connect(
        0,
        IpPair::V4 {
            src: LOCAL,
            dst: PEER,
        },
        PPORT,
        5000,
    )
    .unwrap();
    let syn = t.one();
    (t, syn)
}

#[test]
fn syn_sent_offers_options_and_completes_on_syn_ack() {
    let (mut t, syn) = syn_sent(quiet_cfg());
    assert_eq!(syn.flags, Flags::SYN);
    assert_eq!(syn.seq, 5000);
    assert_eq!((syn.sport, syn.dport), (LPORT, PPORT));
    assert_eq!(syn.opts.mss, Some(1000));
    assert_eq!(syn.opts.window_scale, Some(7));
    assert!(syn.opts.sack_permitted);
    assert!(syn.opts.timestamps.is_some());
    let opts = TcpOptions {
        mss: Some(1400),
        window_scale: Some(3),
        sack_permitted: true,
        timestamps: Some(Timestamps {
            val: 9,
            ecr: syn.opts.timestamps.unwrap().val,
        }),
        ..TcpOptions::default()
    };
    t.rx_opts(PEER_ISS, 5001, Flags::SYN | Flags::ACK, 200, &opts, &[])
        .unwrap();
    assert_eq!(t.c.state(), State::Established);
    assert_eq!(t.events(), vec![Event::Connected]);
    assert_eq!(t.c.window_shifts(), (3, 7));
    assert!(t.c.sack_enabled() && t.c.timestamps_enabled());
    assert_eq!(t.c.send_mss(), 1000 - 12);
    assert_eq!(t.c.send_window(), 200, "SYN window is never scaled");
    let a = t.one();
    assert_eq!(a.flags, Flags::ACK);
    assert_eq!((a.seq, a.ack), (5001, PEER_ISS.wrapping_add(1)));
    assert_eq!(a.opts.timestamps.unwrap().ecr, 9);
    // Later windows are scaled by the peer's shift.
    t.peer_ts = Some(Timestamps { val: 10, ecr: 0 });
    t.rx_opts(
        PEER_ISS.wrapping_add(1),
        5001,
        Flags::ACK,
        200,
        &TcpOptions {
            timestamps: t.peer_ts,
            ..TcpOptions::default()
        },
        &[],
    )
    .unwrap();
    assert_eq!(t.c.send_window(), 200 << 3);
}

#[test]
fn syn_sent_bad_ack_resets_and_rst_rules() {
    let (mut t, syn) = syn_sent(quiet_cfg());
    // ACK not acknowledging our SYN: <SEQ=SEG.ACK><CTL=RST>.
    t.rx(PEER_ISS, syn.seq, Flags::SYN | Flags::ACK, &[])
        .unwrap();
    let r = t.one();
    assert_eq!((r.flags, r.seq), (Flags::RST, syn.seq));
    // RST with an unacceptable ACK is dropped without reply.
    t.rx(
        PEER_ISS,
        syn.seq.wrapping_add(9),
        Flags::RST | Flags::ACK,
        &[],
    )
    .unwrap();
    t.none();
    // RST without ACK is dropped.
    t.rx(PEER_ISS, 0, Flags::RST, &[]).unwrap();
    t.none();
    // Segment with neither SYN nor RST: dropped.
    t.rx(PEER_ISS, 0, Flags(0), &[]).unwrap();
    t.none();
    assert_eq!(t.c.state(), State::SynSent);
    // RST with acceptable ACK: connection refused.
    t.rx(
        PEER_ISS,
        syn.seq.wrapping_add(1),
        Flags::RST | Flags::ACK,
        &[],
    )
    .unwrap();
    assert_eq!(t.c.state(), State::Closed);
    assert_eq!(t.c.close_reason(), Some(CloseReason::Refused));
}

#[test]
fn syn_sent_close_and_syn_retransmission_limit() {
    let (mut t, _) = syn_sent(quiet_cfg());
    t.c.close().unwrap();
    assert_eq!(t.c.state(), State::Closed);

    let mut c = quiet_cfg();
    c.max_retransmits = 3;
    let (mut t, syn) = syn_sent(c);
    let mut last = 0;
    let mut gaps = Vec::new();
    for _ in 0..3 {
        let d = t.c.poll_timeout().unwrap();
        gaps.push(d - last);
        last = d;
        t.now = d;
        t.c.on_timeout(d);
        let r = t.one();
        assert_eq!((r.flags, r.seq), (Flags::SYN, syn.seq));
    }
    assert_eq!(gaps, vec![200 * MS, 400 * MS, 800 * MS], "RFC 6298 backoff");
    let d = t.c.poll_timeout().unwrap();
    t.now = d;
    t.c.on_timeout(d);
    assert_eq!(t.c.state(), State::Closed);
    assert_eq!(t.c.close_reason(), Some(CloseReason::TimedOut));
}

#[test]
fn syn_ack_carrying_data_and_fin() {
    let (mut t, syn) = syn_sent(quiet_cfg());
    let (data, fin) = t
        .rx(
            PEER_ISS,
            syn.seq.wrapping_add(1),
            Flags::SYN | Flags::ACK | Flags::FIN,
            b"early",
        )
        .unwrap();
    assert_eq!(data, b"early");
    assert!(fin);
    assert_eq!(t.c.state(), State::CloseWait);
    let a = t.one();
    assert_eq!(a.ack, PEER_ISS.wrapping_add(1 + 5 + 1));
}

// ---------------------------------------------------------------- simultaneous open/close

fn pump(a: &mut Connection<NewReno>, b: &mut Connection<NewReno>, now: u64) -> usize {
    let ips_ab = IpPair::V4 {
        src: LOCAL,
        dst: PEER,
    };
    let mut ra = vec![0u8; a.config().recv_buffer as usize];
    let mut rb = vec![0u8; b.config().recv_buffer as usize];
    let mut buf = vec![0u8; 2048];
    let mut total = 0;
    for _ in 0..20 {
        // Collect both sides' output first so segments cross on the wire.
        let mut from_a = Vec::new();
        while let Some(t) = a.poll_transmit(now, &SendView::EMPTY, &mut buf).unwrap() {
            from_a.push(buf[..t.len].to_vec());
        }
        let mut from_b = Vec::new();
        while let Some(t) = b.poll_transmit(now, &SendView::EMPTY, &mut buf).unwrap() {
            from_b.push(buf[..t.len].to_vec());
        }
        if from_a.is_empty() && from_b.is_empty() {
            break;
        }
        total += from_a.len() + from_b.len();
        for p in from_a {
            b.on_segment(now, ips_ab, &p, &mut rb).unwrap();
        }
        for p in from_b {
            a.on_segment(now, ips_ab.reversed(), &p, &mut ra).unwrap();
        }
    }
    total
}

fn crossing_pair() -> (Connection<NewReno>, Connection<NewReno>) {
    let mut ca = quiet_cfg();
    ca.local_port = PPORT;
    let mut cb = quiet_cfg();
    cb.local_port = LPORT;
    let mut a = Connection::new(ca).unwrap();
    let mut b = Connection::new(cb).unwrap();
    a.connect(
        0,
        IpPair::V4 {
            src: PEER,
            dst: LOCAL,
        },
        LPORT,
        100,
    )
    .unwrap();
    b.connect(
        0,
        IpPair::V4 {
            src: LOCAL,
            dst: PEER,
        },
        PPORT,
        300,
    )
    .unwrap();
    (a, b)
}

#[test]
fn simultaneous_open_then_simultaneous_close() {
    let (mut a, mut b) = crossing_pair();
    // Fix orientation: a speaks as PEER, b as LOCAL; pump expects a→b as LOCAL→PEER,
    // so swap roles accordingly.
    let n = pump(&mut b, &mut a, 0);
    assert!(n >= 4);
    assert_eq!(
        (a.state(), b.state()),
        (State::Established, State::Established)
    );
    a.close().unwrap();
    b.close().unwrap();
    assert_eq!((a.state(), b.state()), (State::FinWait1, State::FinWait1));
    // Let the FINs cross, then step once more so the ACKs cross.
    let ips_ab = IpPair::V4 {
        src: LOCAL,
        dst: PEER,
    };
    let mut buf = vec![0u8; 2048];
    let mut ra = vec![0u8; 4096];
    let mut rb = vec![0u8; 4096];
    let fa = a
        .poll_transmit(1, &SendView::EMPTY, &mut buf)
        .unwrap()
        .unwrap();
    let pa = buf[..fa.len].to_vec();
    let fb = b
        .poll_transmit(1, &SendView::EMPTY, &mut buf)
        .unwrap()
        .unwrap();
    let pb = buf[..fb.len].to_vec();
    a.on_segment(1, ips_ab, &pb, &mut ra).unwrap();
    b.on_segment(1, ips_ab.reversed(), &pa, &mut rb).unwrap();
    assert_eq!((a.state(), b.state()), (State::Closing, State::Closing));
    pump(&mut b, &mut a, 2);
    assert_eq!((a.state(), b.state()), (State::TimeWait, State::TimeWait));
    let d = a.poll_timeout().unwrap();
    assert_eq!(d, 2 + 100 * MS);
    a.on_timeout(d);
    b.on_timeout(d);
    assert_eq!((a.state(), b.state()), (State::Closed, State::Closed));
}

// ---------------------------------------------------------------- ESTABLISHED and closing states

#[test]
fn passive_close_close_wait_last_ack_closed() {
    let mut t = T::est();
    t.fin();
    assert_eq!(t.c.state(), State::CloseWait);
    assert_eq!(t.events(), vec![Event::Connected, Event::PeerFinished]);
    let a = t.one();
    assert_eq!(a.ack, t.pseq);
    t.c.close().unwrap();
    assert_eq!(t.c.state(), State::LastAck);
    let f = t.one();
    assert!(f.has(Flags::FIN));
    t.pack = t.pack.wrapping_add(1);
    t.ack().unwrap();
    assert_eq!(t.c.state(), State::Closed);
    assert_eq!(t.c.close_reason(), None);
}

#[test]
fn active_close_fin_wait_1_2_time_wait_closed() {
    let mut t = T::est();
    t.c.close().unwrap();
    assert_eq!(t.c.state(), State::FinWait1);
    let f = t.one();
    assert!(f.has(Flags::FIN | Flags::ACK));
    assert!(matches!(t.c.write(1), Err(TcpError::ConnectionClosing)));
    t.pack = t.pack.wrapping_add(1);
    t.ack().unwrap();
    assert_eq!(t.c.state(), State::FinWait2);
    t.fin();
    assert_eq!(t.c.state(), State::TimeWait);
    let a = t.one();
    assert_eq!(a.ack, t.pseq);
    // Retransmitted FIN in TIME-WAIT: ACK it and restart the 2·MSL timer.
    t.now = 50 * MS;
    t.rx(t.pseq.wrapping_sub(1), t.pack, Flags::FIN | Flags::ACK, &[])
        .unwrap();
    let a = t.one();
    assert_eq!(a.ack, t.pseq);
    assert_eq!(t.c.poll_timeout(), Some(150 * MS));
    t.now = 150 * MS;
    t.c.on_timeout(t.now);
    assert_eq!(t.c.state(), State::Closed);
}

#[test]
fn fin_wait_1_fin_without_ack_goes_closing_then_time_wait() {
    let mut t = T::est();
    t.c.close().unwrap();
    t.one();
    t.fin(); // acks only up to before our FIN
    assert_eq!(t.c.state(), State::Closing);
    t.one();
    // A segment in CLOSING that does not ACK our FIN is ignored.
    t.ack().unwrap();
    assert_eq!(t.c.state(), State::Closing);
    t.pack = t.pack.wrapping_add(1);
    t.ack().unwrap();
    assert_eq!(t.c.state(), State::TimeWait);
}

#[test]
fn fin_wait_1_fin_ack_goes_straight_to_time_wait() {
    let mut t = T::est();
    t.c.close().unwrap();
    t.one();
    t.pack = t.pack.wrapping_add(1);
    t.fin();
    assert_eq!(t.c.state(), State::TimeWait);
}

fn in_state(target: State) -> T {
    let mut t = T::est();
    match target {
        State::Established => {}
        State::FinWait1 => {
            t.c.close().unwrap();
            t.one();
        }
        State::FinWait2 => {
            t.c.close().unwrap();
            t.one();
            t.pack = t.pack.wrapping_add(1);
            t.ack().unwrap();
        }
        State::CloseWait => {
            t.fin();
            t.one();
        }
        State::Closing => {
            t.c.close().unwrap();
            t.one();
            t.fin();
            t.one();
        }
        State::LastAck => {
            t.fin();
            t.one();
            t.c.close().unwrap();
            t.one();
        }
        State::TimeWait => {
            t.c.close().unwrap();
            t.one();
            t.pack = t.pack.wrapping_add(1);
            t.fin();
            t.one();
        }
        s => panic!("unsupported {s:?}"),
    }
    assert_eq!(t.c.state(), target);
    t
}

const SYNC_STATES: [State; 7] = [
    State::Established,
    State::FinWait1,
    State::FinWait2,
    State::CloseWait,
    State::Closing,
    State::LastAck,
    State::TimeWait,
];

#[test]
fn rst_with_exact_sequence_closes_every_synchronized_state() {
    for s in SYNC_STATES {
        let mut t = in_state(s);
        t.rst_exact();
        assert_eq!(t.c.state(), State::Closed, "{s:?}");
        let expect = match s {
            State::Established | State::FinWait1 | State::FinWait2 | State::CloseWait => {
                Some(CloseReason::Reset)
            }
            _ => None,
        };
        assert_eq!(t.c.close_reason(), expect, "{s:?}");
        t.none();
    }
}

#[test]
fn rfc5961_in_window_rst_gets_challenge_ack_out_of_window_is_dropped() {
    for s in SYNC_STATES {
        let mut t = in_state(s);
        t.rx(t.pseq.wrapping_add(100), 0, Flags::RST, &[]).unwrap();
        assert_eq!(t.c.state(), s);
        let a = t.one();
        assert_eq!(a.flags, Flags::ACK, "{s:?}");
        assert_eq!(a.ack, t.pseq);
        t.rx(t.pseq.wrapping_add(1 << 20), 0, Flags::RST, &[])
            .unwrap();
        t.none();
        assert_eq!(t.c.state(), s);
    }
}

#[test]
fn rfc5961_syn_in_synchronized_state_gets_challenge_ack() {
    for s in SYNC_STATES {
        let mut t = in_state(s);
        for seq in [t.pseq, t.pseq.wrapping_add(7), t.pseq.wrapping_sub(5000)] {
            t.rx(seq, 0, Flags::SYN, &[]).unwrap();
            let a = t.one();
            assert_eq!(a.flags, Flags::ACK, "{s:?}");
            assert_eq!(a.ack, t.pseq);
            assert_eq!(t.c.state(), s);
        }
    }
}

#[test]
fn rfc5961_ack_validation() {
    let mut t = T::est();
    // ACK beyond SND.NXT: send ACK, drop the segment (data not delivered).
    let (d, _) = t
        .rx(t.pseq, t.pack.wrapping_add(10), Flags::ACK, b"zz")
        .unwrap();
    assert!(d.is_empty());
    let a = t.one();
    assert_eq!(a.ack, t.pseq);
    // ACK older than SND.UNA - MAX.SND.WND: challenge ACK, drop.
    let (d, _) = t
        .rx(t.pseq, t.pack.wrapping_sub(100_000), Flags::ACK, b"zz")
        .unwrap();
    assert!(d.is_empty());
    t.one();
    assert_eq!(t.c.stats().challenge_acks, 1);
    // An old but plausible ACK is accepted as a duplicate.
    let (d, _) = t
        .rx(t.pseq, t.pack.wrapping_sub(10), Flags::ACK, b"ok")
        .unwrap();
    assert_eq!(d, b"ok");
}

#[test]
fn rfc5961_challenge_ack_rate_limit() {
    let mut c = quiet_cfg();
    c.challenge_ack_limit = 3;
    let mut t = T::established_passive(c);
    for _ in 0..10 {
        t.rx(t.pseq.wrapping_add(5), 0, Flags::RST, &[]).unwrap();
        t.tx();
    }
    let s = t.c.stats();
    assert_eq!((s.challenge_acks, s.challenge_acks_suppressed), (3, 7));
    t.now = 1_000 * MS;
    t.rx(t.pseq.wrapping_add(5), 0, Flags::RST, &[]).unwrap();
    assert_eq!(t.one().flags, Flags::ACK);
    assert_eq!(t.c.stats().challenge_acks, 4);
}

#[test]
fn unacceptable_segments_get_acks() {
    let mut t = T::est();
    // Entirely old data.
    t.rx(t.pseq.wrapping_sub(10), t.pack, Flags::ACK, b"old")
        .unwrap();
    assert_eq!(t.one().ack, t.pseq);
    // Beyond the window.
    t.rx(t.pseq.wrapping_add(10_000), t.pack, Flags::ACK, b"far")
        .unwrap();
    assert_eq!(t.one().ack, t.pseq);
    // Overlapping old+new: only the new part is delivered.
    let (d, _) = t.rx(t.pseq, t.pack, Flags::ACK, b"abc").unwrap();
    assert_eq!(d, b"abc");
    let (d, _) = t
        .rx(t.pseq.wrapping_add(1), t.pack, Flags::ACK, b"bcnew")
        .unwrap();
    assert_eq!(d, b"new");
}

#[test]
fn in_order_payload_is_borrowed_from_the_segment_and_ooo_from_the_ring() {
    let mut t = T::est();
    let seg = build(
        t.pseq,
        t.pack,
        Flags::ACK,
        1000,
        &TcpOptions::default(),
        b"hello",
    );
    let r = t.c.on_segment(0, ips_in(), &seg, &mut t.ring).unwrap();
    let ch: Vec<&[u8]> = r.chunks().collect();
    assert_eq!(ch, vec![&b"hello"[..]]);
    let seg_range = seg.as_ptr_range();
    assert!(
        seg_range.contains(&ch[0].as_ptr()),
        "zero-copy: slice points into the segment"
    );
    t.pseq = t.pseq.wrapping_add(5);
    t.one();
    // Out of order, then fill the gap.
    let later = build(
        t.pseq.wrapping_add(3),
        t.pack,
        Flags::ACK,
        1000,
        &TcpOptions::default(),
        b"def",
    );
    let r = t.c.on_segment(0, ips_in(), &later, &mut t.ring).unwrap();
    assert!(r.is_empty());
    assert_eq!(t.c.ooo_ranges(), 1);
    let dup = t.one();
    assert_eq!(
        dup.ack, t.pseq,
        "immediate duplicate ACK for out-of-order data"
    );
    let gap = build(
        t.pseq,
        t.pack,
        Flags::ACK,
        1000,
        &TcpOptions::default(),
        b"abc",
    );
    let ring_range = t.ring.as_ptr_range();
    let r = t.c.on_segment(0, ips_in(), &gap, &mut t.ring).unwrap();
    let ch: Vec<&[u8]> = r.chunks().collect();
    assert_eq!(ch, vec![&b"abc"[..], &b"def"[..]]);
    assert!(gap.as_ptr_range().contains(&ch[0].as_ptr()));
    assert!(
        ring_range.contains(&ch[1].as_ptr()),
        "reassembled bytes borrowed from the caller's ring"
    );
    assert_eq!(t.c.ooo_ranges(), 0);
    assert_eq!(t.c.unread(), 11);
    assert!(matches!(
        t.c.recv_release(12),
        Err(TcpError::ReleaseExceedsUnread)
    ));
    t.c.recv_release(11).unwrap();
}

#[test]
fn out_of_order_fin_waits_for_the_gap() {
    let mut t = T::est();
    let (d, fin) = t
        .rx(
            t.pseq.wrapping_add(2),
            t.pack,
            Flags::ACK | Flags::FIN,
            b"cd",
        )
        .unwrap();
    assert!(d.is_empty() && !fin);
    assert_eq!(t.c.state(), State::Established);
    let (d, fin) = t.rx(t.pseq, t.pack, Flags::ACK, b"ab").unwrap();
    assert_eq!(d, b"abcd");
    assert!(fin, "buffered FIN consumed once contiguous");
    assert_eq!(t.c.state(), State::CloseWait);
}

#[test]
fn bounded_out_of_order_table() {
    let mut c = quiet_cfg();
    c.max_ooo_ranges = 2;
    let mut t = T::established_passive(c);
    for k in 1..=4u32 {
        t.rx(t.pseq.wrapping_add(k * 10), t.pack, Flags::ACK, b"x")
            .unwrap();
    }
    assert_eq!(t.c.ooo_ranges(), 2);
    assert_eq!(t.c.stats().ooo_dropped, 2);
    // Adjacent data merges into an existing range even when full.
    t.rx(t.pseq.wrapping_add(11), t.pack, Flags::ACK, b"y")
        .unwrap();
    assert_eq!(t.c.ooo_ranges(), 2);
    assert_eq!(t.c.stats().ooo_dropped, 2);
}

#[test]
fn receiver_advertises_sack_blocks_most_recent_first() {
    let mut t = T::with(quiet_cfg());
    t.c.listen(1000).unwrap();
    let syn_opts = TcpOptions {
        sack_permitted: true,
        ..TcpOptions::default()
    };
    t.rx_opts(PEER_ISS, 0, Flags::SYN, 1000, &syn_opts, &[])
        .unwrap();
    let synack = t.one();
    assert!(synack.opts.sack_permitted);
    t.pseq = PEER_ISS.wrapping_add(1);
    t.pack = synack.seq.wrapping_add(1);
    t.ack().unwrap();
    for k in [10u32, 30, 20] {
        t.rx(t.pseq.wrapping_add(k), t.pack, Flags::ACK, b"zz")
            .unwrap();
    }
    let mut outs = t.tx();
    let last = outs.pop().unwrap();
    let blocks = last.opts.sack();
    assert_eq!(blocks.len(), 3);
    let lefts: Vec<u32> = blocks
        .iter()
        .map(|b| b.left.0.wrapping_sub(t.pseq))
        .collect();
    assert_eq!(lefts, vec![20, 30, 10]);
    assert_eq!(blocks[0].right.0.wrapping_sub(blocks[0].left.0), 2);
}

#[test]
fn paws_drops_old_timestamps_and_segments_without_them() {
    let mut t = T::with(quiet_cfg());
    t.c.listen(1000).unwrap();
    let opts = TcpOptions {
        timestamps: Some(Timestamps { val: 1000, ecr: 0 }),
        ..TcpOptions::default()
    };
    t.rx_opts(PEER_ISS, 0, Flags::SYN, 1000, &opts, &[])
        .unwrap();
    let synack = t.one();
    assert_eq!(synack.opts.timestamps.unwrap().ecr, 1000);
    t.pseq = PEER_ISS.wrapping_add(1);
    t.pack = synack.seq.wrapping_add(1);
    t.peer_ts = Some(Timestamps { val: 1001, ecr: 0 });
    t.ack().unwrap();
    assert!(t.c.timestamps_enabled());
    t.peer_ts = Some(Timestamps { val: 999, ecr: 0 });
    let (d, _) = t.rx(t.pseq, t.pack, Flags::ACK, b"stale").unwrap();
    assert!(d.is_empty());
    assert_eq!(t.one().opts.timestamps.unwrap().ecr, 1001);
    t.peer_ts = None;
    let (d, _) = t.rx(t.pseq, t.pack, Flags::ACK, b"nots").unwrap();
    assert!(d.is_empty());
    t.none();
    assert_eq!(t.c.stats().paws_dropped, 2);
    t.peer_ts = Some(Timestamps { val: 1002, ecr: 0 });
    let (d, _) = t.rx(t.pseq, t.pack, Flags::ACK, b"fresh").unwrap();
    assert_eq!(d, b"fresh");
}

#[test]
fn urgent_pointer_is_recorded_and_data_delivered_inline() {
    let mut t = T::est();
    let mut buf = vec![0u8; 80];
    let h = HeaderFields {
        src_port: PPORT,
        dst_port: LPORT,
        seq: Seq(t.pseq),
        ack: Seq(t.pack),
        flags: Flags::ACK | Flags::URG,
        window: 1000,
        urgent: 3,
    };
    let hl = wire::write_header(&mut buf, &h, &TcpOptions::default()).unwrap();
    buf[hl..hl + 4].copy_from_slice(b"urg!");
    buf.truncate(hl + 4);
    wire::fill_checksum(ips_in(), &mut buf).unwrap();
    let r = t.c.on_segment(0, ips_in(), &buf, &mut t.ring).unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(
        t.c.urgent_offset(),
        Some(3),
        "points one past the urgent data"
    );
}

#[test]
fn abort_sends_reset_and_api_misuse_is_rejected() {
    let mut t = T::est();
    t.c.abort();
    let r = t.one();
    assert_eq!((r.flags, r.seq), (Flags::RST, t.pack));
    assert_eq!(t.c.state(), State::Closed);
    assert_eq!(t.c.close_reason(), Some(CloseReason::Aborted));
    assert!(matches!(
        t.c.write(1),
        Err(TcpError::InvalidState(State::Closed))
    ));
    assert!(matches!(
        t.c.close(),
        Err(TcpError::InvalidState(State::Closed))
    ));
    assert!(matches!(t.c.listen(0), Ok(())));
    assert!(matches!(
        t.c.listen(0),
        Err(TcpError::InvalidState(State::Listen))
    ));

    let mut t = T::est();
    let seg = build(
        t.pseq,
        t.pack,
        Flags::ACK,
        1000,
        &TcpOptions::default(),
        b"x",
    );
    let mut small = vec![0u8; 10];
    assert_eq!(
        t.c.on_segment(0, ips_in(), &seg, &mut small).unwrap_err(),
        TcpError::RingSizeMismatch
    );
    let other = IpPair::V4 {
        src: [1, 2, 3, 4],
        dst: LOCAL,
    };
    let mut seg2 = seg.clone();
    wire::fill_checksum(other, &mut seg2).unwrap();
    assert_eq!(
        t.c.on_segment(0, other, &seg2, &mut t.ring).unwrap_err(),
        TcpError::NotForThisConnection
    );
    let mut bad = seg.clone();
    bad[20] ^= 1;
    assert!(matches!(
        t.c.on_segment(0, ips_in(), &bad, &mut t.ring),
        Err(TcpError::Wire(_))
    ));
    let mut out = vec![0u8; 59];
    assert_eq!(
        t.c.poll_transmit(0, &SendView::EMPTY, &mut out)
            .unwrap_err(),
        TcpError::BufferTooSmall
    );
    t.c.write(10).unwrap();
    let mut out = vec![0u8; 2048];
    assert!(matches!(
        t.c.poll_transmit(0, &SendView::EMPTY, &mut out),
        Err(TcpError::SendViewMissing { start: 0, end: 10 })
    ));
    assert!(
        Connection::new(Config {
            mss: 10,
            ..quiet_cfg()
        })
        .is_err()
    );
    assert!(
        Connection::new(Config {
            window_scale: Some(15),
            ..quiet_cfg()
        })
        .is_err()
    );
    assert!(
        Connection::new(Config {
            recv_buffer: 0,
            ..quiet_cfg()
        })
        .is_err()
    );
}

#[test]
fn send_ranges_are_released_only_when_acknowledged() {
    let mut t = T::est();
    t.send = b"0123456789".to_vec();
    t.c.write(10).unwrap();
    let d = t.one();
    assert_eq!(d.payload, b"0123456789");
    assert!(d.has(Flags::PSH));
    assert!(t.c.take_released().is_none());
    t.rx(t.pseq, t.pack.wrapping_add(4), Flags::ACK, &[])
        .unwrap();
    assert_eq!(
        t.c.take_released(),
        Some(vesper_warp_tcp::Released { start: 0, end: 4 })
    );
    // Retransmission after RTO re-reads the caller's buffer for 4..10.
    let dl = t.c.poll_timeout().unwrap();
    t.now = dl;
    t.c.on_timeout(dl);
    let r = t.one();
    assert_eq!(r.payload, b"456789");
    t.rx(t.pseq, t.pack.wrapping_add(10), Flags::ACK, &[])
        .unwrap();
    assert_eq!(
        t.c.take_released(),
        Some(vesper_warp_tcp::Released { start: 4, end: 10 })
    );
    assert!(t.c.take_released().is_none());
}

#[test]
fn karn_rule_skips_retransmitted_samples() {
    let mut t = T::est();
    assert!(t.c.rtt().has_sample(), "handshake SYN-ACK sampled");
    let before = t.c.rtt().srtt_ns();
    t.send = vec![7u8; 100];
    t.c.write(100).unwrap();
    t.one();
    let dl = t.c.poll_timeout().unwrap();
    t.now = dl;
    t.c.on_timeout(dl);
    t.one();
    t.now += 5 * MS;
    t.rx(t.pseq, t.pack.wrapping_add(100), Flags::ACK, &[])
        .unwrap();
    assert_eq!(t.c.rtt().srtt_ns(), before, "ambiguous sample discarded");
}

#[test]
fn fin_respects_a_zero_send_window() {
    let mut t = T::est();
    // Peer closes its window.
    t.rx_opts(t.pseq, t.pack, Flags::ACK, 0, &TcpOptions::default(), &[])
        .unwrap();
    assert_eq!(t.c.send_window(), 0);
    t.c.close().unwrap();
    t.none();
    // The persist timer sends the FIN as the probe.
    let d = t.c.poll_timeout().expect("persist timer armed");
    t.now = d;
    t.c.on_timeout(d);
    let f = t.one();
    assert!(f.has(Flags::FIN));
    assert_eq!(t.c.stats().window_probes, 1);
}

#[test]
fn fin_is_not_attached_beyond_the_window() {
    let mut t = T::est();
    t.rx_opts(t.pseq, t.pack, Flags::ACK, 10, &TcpOptions::default(), &[])
        .unwrap();
    t.send = b"0123456789".to_vec();
    t.c.write(10).unwrap();
    t.c.close().unwrap();
    let d = t.one();
    assert_eq!(d.payload.len(), 10);
    assert!(!d.has(Flags::FIN), "FIN would be one past the window");
    t.none();
    t.rx_opts(
        t.pseq,
        t.pack.wrapping_add(10),
        Flags::ACK,
        10,
        &TcpOptions::default(),
        &[],
    )
    .unwrap();
    let f = t.one();
    assert!(f.has(Flags::FIN));
    assert_eq!(f.seq, t.pack.wrapping_add(10));
}
