//! Malformed and adversarial segments never panic: exhaustive truncation,
//! random bytes and random well-checksummed headers in every state.

mod common;
use common::*;
use vesper_warp_tcp::conn::SendView;
use vesper_warp_tcp::harness::{Harness, LinkConfig, Rng};
use vesper_warp_tcp::wire::{self, HeaderFields, SackBlock, Timestamps};
use vesper_warp_tcp::{
    Connection, Flags, IpPair, NewReno, Segment, Seq, State, TcpError, TcpOptions,
};

fn valid_segment(ips: IpPair) -> Vec<u8> {
    let mut opts = TcpOptions {
        timestamps: Some(Timestamps { val: 5, ecr: 6 }),
        ..TcpOptions::default()
    };
    opts.sack_len = 2;
    opts.sack_blocks[0] = SackBlock {
        left: Seq(10),
        right: Seq(20),
    };
    opts.sack_blocks[1] = SackBlock {
        left: Seq(30),
        right: Seq(40),
    };
    let h = HeaderFields {
        src_port: 40000,
        dst_port: 80,
        seq: Seq(1),
        ack: Seq(2),
        flags: Flags::ACK,
        window: 9,
        urgent: 0,
    };
    let mut b = vec![0u8; 200];
    let hl = wire::write_header(&mut b, &h, &opts).unwrap();
    b.truncate(hl + 33);
    wire::fill_checksum(ips, &mut b).unwrap();
    b
}

#[test]
fn every_truncation_is_a_typed_error_or_valid_parse() {
    let ips = IpPair::V4 {
        src: [1, 2, 3, 4],
        dst: [5, 6, 7, 8],
    };
    let full = valid_segment(ips);
    for n in 0..=full.len() {
        let mut t = full[..n].to_vec();
        let _ = Segment::parse(&t);
        let _ = wire::verify_checksum(ips, &t);
        if n >= 20 {
            wire::fill_checksum(ips, &mut t).unwrap();
            match Segment::parse(&t) {
                Ok(s) => assert!(s.header_len <= n),
                Err(e) => assert!(n < 64, "{e:?} at {n}"),
            }
        } else {
            assert!(Segment::parse(&t).is_err());
        }
        // Option list truncated at every byte.
        let _ = TcpOptions::parse(&full[20..20 + n.min(full.len() - 20).min(40)]);
    }
}

#[test]
fn random_option_bytes_never_panic() {
    let mut rng = Rng::new(77);
    let mut buf = [0u8; 40];
    for _ in 0..200_000 {
        let n = rng.below(41) as usize;
        for b in &mut buf[..n] {
            // Bias towards known kinds and small lengths.
            *b = match rng.below(4) {
                0 => [0, 1, 2, 3, 4, 5, 8][rng.below(7) as usize],
                1 => rng.below(12) as u8,
                _ => rng.next_u64() as u8,
            };
        }
        if let Ok(o) = TcpOptions::parse(&buf[..n]) {
            assert!(o.sack().len() <= 4);
            let mut out = [0u8; 40];
            let _ = o.write(&mut out);
        }
    }
}

/// Throw random segments (valid checksums, matching ports/addresses so they
/// reach the state machine) at a connection; interleave transmissions and
/// timers. Nothing may panic, and every error must be typed.
fn bombard(
    c: &mut Connection<NewReno>,
    ips_in: IpPair,
    ports: (u16, u16),
    rng: &mut Rng,
    iters: usize,
) -> u64 {
    let mut ring = vec![0u8; c.config().recv_buffer as usize];
    let mut out = vec![0u8; 4096];
    let send = vec![0u8; 1 << 16];
    let mut now = 1_000_000_000u64;
    let mut deep = 0u64;
    for i in 0..iters {
        let rbase = c.recv_next_seq().map_or(rng.next_u64() as u32, |s| s.0);
        let (una, _) = c.send_seqs();
        let base_seq = una.0;
        let mut seg = vec![0u8; 20 + 40 + rng.below(300) as usize];
        if rng.below(3) == 0 {
            for b in seg.iter_mut() {
                *b = rng.next_u64() as u8;
            }
        } else {
            let plausible = rng.below(3) != 0;
            let mut opts = TcpOptions::default();
            if plausible || rng.below(2) == 0 {
                opts.timestamps = Some(Timestamps {
                    val: 0x7000_0000u32.wrapping_add(i as u32),
                    ecr: 0,
                });
            }
            if !plausible && rng.below(3) == 0 {
                opts.mss = Some(rng.next_u64() as u16);
                opts.window_scale = Some(rng.below(20) as u8);
                opts.sack_permitted = true;
            }
            let nsack = rng.below(4) as u8;
            for k in 0..usize::from(nsack) {
                let l = base_seq.wrapping_add(rng.below(1 << 16) as u32);
                opts.sack_blocks[k] = SackBlock {
                    left: Seq(l),
                    right: Seq(l.wrapping_add(rng.below(5000) as u32)),
                };
            }
            opts.sack_len = if opts.timestamps.is_some() {
                nsack.min(3)
            } else {
                nsack
            };
            let near = |rng: &mut Rng| {
                base_seq
                    .wrapping_add(rng.below(1 << 16) as u32)
                    .wrapping_sub(1 << 15)
            };
            let (una, nxt) = c.send_seqs();
            let span = nxt.0.wrapping_sub(una.0).max(1);
            let h = HeaderFields {
                src_port: ports.0,
                dst_port: ports.1,
                seq: Seq(match rng.below(4) {
                    0 => rbase,
                    1 => rbase.wrapping_add(rng.below(4000) as u32).wrapping_sub(500),
                    2 => near(rng),
                    _ => rng.next_u64() as u32,
                }),
                ack: Seq(if plausible {
                    una.0.wrapping_add(rng.below(u64::from(span) + 1) as u32)
                } else {
                    near(rng)
                }),
                flags: if plausible {
                    Flags(
                        Flags::ACK.0
                            | (rng.next_u64() as u8
                                & (Flags::PSH.0
                                    | Flags::URG.0
                                    | if rng.below(20) == 0 { Flags::FIN.0 } else { 0 })),
                    )
                } else if rng.below(40) == 0 {
                    Flags(rng.next_u64() as u8)
                } else {
                    Flags(rng.next_u64() as u8 & !Flags::RST.0)
                },
                window: if plausible {
                    1000 + rng.below(60000) as u16
                } else {
                    rng.next_u64() as u16
                },
                urgent: rng.next_u64() as u16,
            };
            let hl = match wire::write_header(&mut seg, &h, &opts) {
                Ok(hl) => hl,
                Err(e) => {
                    assert_eq!(e, wire::WireError::OptionsTooLong);
                    continue;
                }
            };
            let plen = rng.below(200) as usize;
            seg.truncate(hl + plen);
        }
        if rng.below(10) != 0 {
            let _ = wire::fill_checksum(ips_in, &mut seg);
        }
        match c.on_segment(now, ips_in, &seg, &mut ring) {
            Ok(r) => {
                deep += r.len() as u64;
                let u = c.unread();
                c.recv_release(u).unwrap();
            }
            Err(TcpError::Wire(_) | TcpError::NotForThisConnection) => {}
            Err(e) => panic!("unexpected error {e:?}"),
        }
        let view = SendView {
            base: c.acked_offset(),
            bytes: &send[..(c.written() - c.acked_offset()) as usize],
        };
        for _ in 0..64 {
            match c.poll_transmit(now, &view, &mut out) {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => panic!("transmit error {e:?}"),
            }
        }
        while c.take_released().is_some() {}
        if i % 7 == 0 {
            let span = if rng.below(50) == 0 {
                2_000_000_000
            } else {
                30_000_000
            };
            now += rng.below(span);
            c.on_timeout(now);
        }
        if i % 50 == 0 && c.state() == State::Established && c.written() - c.acked_offset() < 30_000
        {
            let _ = c.write(1000);
        }
    }
    deep
}

fn state_reached(target: State, seed: u64) -> Option<Harness<NewReno>> {
    let mut h = harness(
        cfg(40000),
        cfg(80),
        LinkConfig {
            seed,
            ..LinkConfig::default()
        },
    );
    let ok = match target {
        State::SynSent => true,
        State::Established => h.run_until(SEC, |h| h.both(State::Established)).unwrap(),
        State::CloseWait | State::FinWait2 => {
            h.run_until(SEC, |h| h.both(State::Established)).unwrap();
            h.a.write(&pattern(5000, 1)).unwrap();
            h.a.conn.close().unwrap();
            h.run_until(10 * SEC, |h| h.a.conn.state() == State::FinWait2)
                .unwrap()
        }
        State::TimeWait | State::LastAck => {
            h.run_until(SEC, |h| h.both(State::Established)).unwrap();
            h.a.conn.close().unwrap();
            h.run_until(10 * SEC, |h| h.b.conn.state() == State::CloseWait)
                .unwrap();
            h.b.conn.close().unwrap();
            h.run_until(10 * SEC, |h| h.a.conn.state() == State::TimeWait)
                .unwrap()
        }
        _ => false,
    };
    ok.then_some(h)
}

#[test]
fn random_segments_in_every_state_never_panic() {
    let mut rng = Rng::new(2024);
    let mut deep = 0;
    // CLOSED and LISTEN.
    for listen in [false, true] {
        let mut c = Connection::new(cfg(80)).unwrap();
        if listen {
            c.listen(1).unwrap();
        }
        deep += bombard(
            &mut c,
            IpPair::V4 {
                src: [10, 0, 0, 1],
                dst: [10, 0, 0, 2],
            },
            (40000, 80),
            &mut rng,
            3000,
        );
    }
    // SYN-SENT and SYN-RECEIVED.
    for _ in 0..3 {
        let mut c = Connection::new(cfg(40000)).unwrap();
        let ips = IpPair::V4 {
            src: [10, 0, 0, 1],
            dst: [10, 0, 0, 2],
        };
        c.connect(0, ips, 80, 3).unwrap();
        deep += bombard(&mut c, ips.reversed(), (80, 40000), &mut rng, 3000);
    }
    // Synchronized states reached through the harness; bombard both ends.
    for (k, target) in [State::Established, State::CloseWait, State::TimeWait]
        .into_iter()
        .enumerate()
    {
        let mut h = state_reached(target, k as u64 + 1).expect("reach state");
        let ips_a = h.a.ips;
        deep += bombard(&mut h.b.conn, ips_a, (40000, 80), &mut rng, 6000);
        deep += bombard(&mut h.a.conn, ips_a.reversed(), (80, 40000), &mut rng, 6000);
    }
    // A passive endpoint in SYN-RECEIVED.
    let mut c = Connection::new(cfg(80)).unwrap();
    c.listen(5).unwrap();
    let ips_in = IpPair::V4 {
        src: [10, 0, 0, 1],
        dst: [10, 0, 0, 2],
    };
    let h = HeaderFields {
        src_port: 40000,
        dst_port: 80,
        seq: Seq(9),
        ack: Seq(0),
        flags: Flags::SYN,
        window: 100,
        urgent: 0,
    };
    let mut syn = vec![0u8; 60];
    let n = wire::write_header(&mut syn, &h, &TcpOptions::default()).unwrap();
    syn.truncate(n);
    wire::fill_checksum(ips_in, &mut syn).unwrap();
    let mut ring = vec![0u8; c.config().recv_buffer as usize];
    c.on_segment(0, ips_in, &syn, &mut ring).unwrap();
    assert_eq!(c.state(), State::SynReceived);
    deep += bombard(&mut c, ips_in, (40000, 80), &mut rng, 6000);
    assert!(
        deep > 10_000,
        "random segments reached data delivery: {deep}"
    );
}

#[test]
fn hostile_mss_and_window_scale_are_clamped() {
    let ips = IpPair::V4 {
        src: [10, 0, 0, 1],
        dst: [10, 0, 0, 2],
    };
    let mut c = Connection::new(cfg(40000)).unwrap();
    c.connect(0, ips, 80, 3).unwrap();
    let mut out = vec![0u8; 2048];
    let t = c
        .poll_transmit(0, &SendView::EMPTY, &mut out)
        .unwrap()
        .unwrap();
    let syn = Segment::parse(&out[..t.len]).unwrap();
    let opts = TcpOptions {
        mss: Some(0),
        window_scale: Some(200),
        ..TcpOptions::default()
    };
    let h = HeaderFields {
        src_port: 80,
        dst_port: 40000,
        seq: Seq(1),
        ack: syn.seq.add(1),
        flags: Flags::SYN | Flags::ACK,
        window: 65535,
        urgent: 0,
    };
    let mut b = vec![0u8; 80];
    let n = wire::write_header(&mut b, &h, &opts).unwrap();
    b.truncate(n);
    wire::fill_checksum(ips.reversed(), &mut b).unwrap();
    let mut ring = vec![0u8; c.config().recv_buffer as usize];
    c.on_segment(0, ips.reversed(), &b, &mut ring).unwrap();
    assert_eq!(c.state(), State::Established);
    assert_eq!(c.send_mss(), u32::from(vesper_warp_tcp::conn::MIN_SEND_MSS));
    assert_eq!(c.window_shifts().0, 14);
}
