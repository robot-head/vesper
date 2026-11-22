//! Timing driver for the flip gate (Rust exit SH6 lane 4, #1399): the same
//! workloads under the Rust build and the Vesper build (`--features
//! vesper-port`), one process per build. Build it both ways (two target
//! directories) and run the two binaries alternately, A B B A, for the
//! flip gate's ratio interval.
//!
//! ```text
//! shard_bench <workload> <rounds>
//! ```
//!
//! prints the CPU nanoseconds per round (median of the rounds) and a
//! checksum, which must agree between the builds. Workloads: `lookup`
//! (index lookup of 4,096 CIDs), `scan` (the 16-lane tile scan of 64 CIDs),
//! `churn` (insert and remove 4,096 connections), `batch` (200 batches of 64
//! events), `idle` (the idle scan of 4,096 connections) and `rss` (Toeplitz
//! hashes of 4,096 inputs).
use std::hash::BuildHasherDefault;
use std::hint::black_box;
use std::time::Instant;

use std::collections::hash_map::DefaultHasher;
use vesper_warp_shard::{
    ConnEvent, ConnEventDesc, ConnState, ConnTable, ConnectionId, MAX_BATCH, MICROSOFT_RSS_KEY,
    SlotHandle, TableConfig, Toeplitz,
};

type Hasher = BuildHasherDefault<DefaultHasher>;
const N: u32 = 4096;

fn next(x: &mut u64) -> u64 {
    *x = x
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *x >> 11
}

fn filled(t: &mut ConnTable<Hasher>, round: u64, x: &mut u64) -> Vec<SlotHandle> {
    (0..N)
        .map(|i| {
            let cid = ConnectionId::from_u64(round << 32 | u64::from(i));
            let st = ConnState::new(ConnectionId::from_u64(next(x)), 14_720, next(x) % 100);
            t.insert(&cid, &st).unwrap()
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let workload = args.get(1).map(String::as_str).unwrap_or("lookup");
    let rounds: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200);
    let mut t = ConnTable::with_hasher(TableConfig::new(N, 8), Hasher::default()).unwrap();
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut sum: u64 = 0;
    let mut times = Vec::with_capacity(rounds);
    let handles = if workload == "churn" {
        Vec::new()
    } else {
        filled(&mut t, 1, &mut x)
    };
    let toeplitz = Toeplitz::new(&MICROSOFT_RSS_KEY);
    let inputs: Vec<[u8; 36]> = (0..N)
        .map(|_| {
            let mut b = [0u8; 36];
            for c in b.chunks_mut(8) {
                let v = next(&mut x).to_le_bytes();
                c.copy_from_slice(&v[..c.len()]);
            }
            b
        })
        .collect();
    let mut batch = Vec::with_capacity(MAX_BATCH);
    let mut expired = vec![SlotHandle::from_raw(0, 0); 256];
    for round in 0..rounds {
        let start = Instant::now();
        match workload {
            "lookup" => {
                for i in 0..N {
                    let cid = (1u64 << 32 | u64::from(i)).to_be_bytes();
                    let h = t.lookup(&cid).unwrap().unwrap();
                    sum = sum.wrapping_add(u64::from(h.index()));
                }
            }
            "scan" => {
                for i in (0..N).step_by(64) {
                    let cid = (1u64 << 32 | u64::from(i)).to_be_bytes();
                    let h = t.lookup_cid_simd(&cid).unwrap().unwrap();
                    sum = sum.wrapping_add(u64::from(h.index()));
                }
            }
            "churn" => {
                let hs = filled(&mut t, round as u64 + 2, &mut x);
                for h in hs {
                    let (_, st) = t.remove(h).unwrap();
                    sum = sum.wrapping_add(st.idle_deadline_ns);
                }
            }
            "batch" => {
                for _ in 0..200 {
                    batch.clear();
                    for _ in 0..MAX_BATCH {
                        let slot = handles[(next(&mut x) % u64::from(N)) as usize];
                        let event = match next(&mut x) % 3 {
                            0 => ConnEvent::PacketSent { bytes: 1200 },
                            1 => ConnEvent::PacketReceived {
                                pn: next(&mut x) % (1 << 40),
                            },
                            _ => ConnEvent::CwndUpdate { cwnd_bytes: 20_000 },
                        };
                        batch.push(ConnEventDesc { slot, event });
                    }
                    t.apply_batch(&batch).unwrap();
                }
                if round + 1 == rounds {
                    for &h in &handles {
                        let st = t.get(h).unwrap();
                        sum = sum
                            .wrapping_add(st.largest_rx_pn.unwrap_or(0))
                            .wrapping_add(u64::from(st.bytes_in_flight))
                            .wrapping_add(u64::from(st.cwnd_bytes));
                    }
                }
            }
            "idle" => {
                let mut at = 0;
                loop {
                    let r = t.scan_idle(50, at, &mut expired);
                    sum = sum.wrapping_add(r.written as u64);
                    match r.resume_at {
                        Some(n) => at = n,
                        None => break,
                    }
                }
            }
            "rss" => {
                for b in &inputs {
                    sum = sum.wrapping_add(u64::from(toeplitz.hash(b).unwrap()));
                }
            }
            other => panic!("unknown workload {other}"),
        }
        times.push(start.elapsed().as_nanos() as u64);
        black_box(&t);
    }
    times.sort_unstable();
    println!("{} {}", times[times.len() / 2], sum);
}
