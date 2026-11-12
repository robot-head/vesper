//! Lane 7's TCP with lane 2's BBRv3 behind the integration adapter.

use vesper_warp::cc::TcpBbr;
use vesper_warp_tcp::cc::{
    AckedPacket, CongestionController, CongestionEvent, LostPacket, SentPacket,
};
use vesper_warp_tcp::conn::Config;
use vesper_warp_tcp::harness::{Harness, LinkConfig};
use vesper_warp_tcp::rtt::RtoConfig;
use vesper_warp_tcp::{Connection, State};

const MS: u64 = 1_000_000;
const SEC: u64 = 1_000_000_000;

fn cfg(port: u16) -> Config {
    Config {
        local_port: port,
        mss: 1000,
        recv_buffer: 64 * 1024,
        rto: RtoConfig {
            initial_ns: 200 * MS,
            min_ns: 50 * MS,
            max_ns: 4 * SEC,
            granularity_ns: MS,
        },
        time_wait_ns: 100 * MS,
        delayed_ack_ns: 10 * MS,
        max_in_flight_segments: 256,
        ..Config::default()
    }
}

fn conn(port: u16) -> Connection<TcpBbr> {
    let cc = TcpBbr::new(TcpBbr::tcp_config(1000), 0, 512).unwrap();
    Connection::with_controller(cfg(port), cc).unwrap()
}

fn pattern(n: usize) -> Vec<u8> {
    let mut x = 0x9e37_79b9_u64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn transfer(link: LinkConfig, len: usize) -> Harness<TcpBbr> {
    let mut h = Harness::new(conn(40000), conn(80), link).unwrap();
    assert!(
        h.run_until(30 * SEC, |h| h.both(State::Established))
            .unwrap()
    );
    let data = pattern(len);
    h.a.write(&data).unwrap();
    h.a.conn.close().unwrap();
    let done = h.run_until(600 * SEC, |h| h.b.peer_finished).unwrap();
    assert!(
        done,
        "got {} of {}: {:?}",
        h.b.received.len(),
        len,
        h.a.conn.stats()
    );
    assert!(h.b.received == data, "byte stream differs");
    assert!(h.a.errors.is_empty() && h.b.errors.is_empty());
    // Let the final acknowledgements reach the sender.
    let until = h.now + 10 * SEC;
    assert!(
        h.run_until(until, |h| h.a.conn.bytes_in_flight() == 0)
            .unwrap()
    );
    h
}

#[test]
fn bulk_transfer_over_tcp_with_bbrv3() {
    let h = transfer(LinkConfig::default(), 400_000);
    let cc = h.a.conn.controller();
    assert_eq!(cc.stats().bbr_errors, 0, "{:?}", cc.last_error());
    assert_eq!(cc.stats().unknown, 0);
    let bbr = cc.controller();
    assert!(
        bbr.delivered_bytes() >= 400_000,
        "{:?} {} {:?}",
        cc.stats(),
        bbr.delivered_bytes(),
        h.a.conn.stats()
    );
    assert!(bbr.round_count() > 3);
    // The link serializes at 12.5 MB/s; BBR's estimate is in that range.
    assert!(bbr.max_bw() > 1_000_000, "{}", bbr.max_bw());
    assert!(bbr.max_bw() < 40_000_000, "{}", bbr.max_bw());
    assert!(bbr.min_rtt_ns().unwrap() >= 10 * MS);
    assert_eq!(h.a.conn.stats().rtos, 0);
}

#[test]
fn lossy_reordering_transfers_over_tcp_with_bbrv3() {
    let mut lost = 0;
    for seed in 1..=4 {
        let link = LinkConfig {
            delay_ns: 10 * MS,
            jitter_ns: 8 * MS,
            loss_ppm: 50_000,
            dup_ppm: 10_000,
            seed,
            ..LinkConfig::default()
        };
        let h = transfer(link, 150_000);
        let cc = h.a.conn.controller();
        assert_eq!(
            cc.stats().bbr_errors,
            0,
            "seed {seed}: {:?}",
            cc.last_error()
        );
        assert_eq!(cc.controller().bytes_in_flight(), 0, "seed {seed}");
        lost += cc.controller().lost_bytes();
    }
    assert!(lost > 0, "losses reached BBR");
}

#[test]
fn partial_acks_and_late_acks_of_lost_segments_balance_the_flight() {
    let mut cc = TcpBbr::new(TcpBbr::tcp_config(1000), 0, 8).unwrap();
    cc.schedule_edt_ns(1000, 0);
    cc.on_sent(SentPacket {
        pn: 0,
        sent_ns: 0,
        bytes: 1000,
        ack_eliciting: true,
        in_flight: true,
    });
    cc.on_sent(SentPacket {
        pn: 1,
        sent_ns: 0,
        bytes: 1000,
        ack_eliciting: true,
        in_flight: true,
    });
    assert_eq!(cc.controller().bytes_in_flight(), 2000);
    let acked = |pn, bytes| CongestionEvent::Acked {
        packet: AckedPacket {
            pn,
            sent_ns: 0,
            bytes,
        },
        bytes_in_flight: 0,
    };
    // pn 0 acknowledged in two parts.
    cc.on_event(acked(0, 400), 10 * MS);
    assert_eq!(cc.controller().bytes_in_flight(), 1600);
    cc.on_event(acked(0, 600), 11 * MS);
    assert_eq!(cc.controller().bytes_in_flight(), 1000);
    // pn 1 lost, then acknowledged anyway: the flight is released once.
    cc.on_event(
        CongestionEvent::Lost {
            packet: LostPacket {
                pn: 1,
                sent_ns: 0,
                bytes: 1000,
            },
            bytes_in_flight: 0,
        },
        12 * MS,
    );
    assert_eq!(cc.controller().bytes_in_flight(), 0);
    cc.on_event(acked(1, 1000), 13 * MS);
    assert_eq!(cc.controller().bytes_in_flight(), 0);
    assert_eq!(cc.controller().delivered_bytes(), 2000);
    // A second loss report or an unknown transmission changes nothing.
    cc.on_event(acked(9, 1000), 14 * MS);
    assert_eq!(cc.stats().unknown, 1);
    assert_eq!(cc.stats().bbr_errors, 0);
}
