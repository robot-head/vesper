#![allow(dead_code)]
use vesper_warp_tcp::conn::Config;
use vesper_warp_tcp::harness::{Harness, LinkConfig};
use vesper_warp_tcp::rtt::RtoConfig;
use vesper_warp_tcp::{Connection, NewReno};

pub const MS: u64 = 1_000_000;
pub const SEC: u64 = 1_000_000_000;

pub fn cfg(port: u16) -> Config {
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

pub fn harness(a: Config, b: Config, link: LinkConfig) -> Harness<NewReno> {
    Harness::new(
        Connection::new(a).unwrap(),
        Connection::new(b).unwrap(),
        link,
    )
    .unwrap()
}

pub fn pattern(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}
