//! The Vesper virtio-blk device (`vesper/src/block.ves`) against the Rust
//! reference device (`src/devices/block/reference.rs`), both behind the
//! real MMIO transport and split/packed rings driven by `tests/common`'s
//! driver: a randomized differential over every request type (valid and
//! malformed), several queues and feature sets, and an ABBA timing of 4 KiB
//! read/write traffic that prints the flip gate's `flip-gate pairs` line
//! (crates/vesper-aether/flip-gate.toml).

use std::collections::BTreeMap;
use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::devices::block::{
    BlockDevice, BlockStats, ReferenceBlockDevice, SECTOR_BYTES, VIRTIO_BLK_F_DISCARD,
    VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_MQ, VIRTIO_BLK_F_WRITE_ZEROES, VIRTIO_BLK_T_DISCARD,
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
    VIRTIO_BLK_T_WRITE_ZEROES,
};
use crate::devices::mmio::{MmioTransport, VIRTIO_F_VERSION_1, VirtioDevice, reg};
use crate::devices::queue::RingFormat;
use crate::guest_mem::GuestMemory;
use crate::test_driver::{DriverQueue, init, ram, w32};

/// xorshift64*, so every run sees the same inputs.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

trait Dev: VirtioDevice + std::fmt::Debug {
    fn make(sectors: usize, read_only: bool, queues: u16) -> Self;
    fn blk_stats(&self) -> BlockStats;
    fn disk_bytes(&self) -> &[u8];
    fn durable_bytes(&self) -> &[u8];
}

macro_rules! dev_impl {
    ($t:ty) => {
        impl Dev for $t {
            fn make(sectors: usize, read_only: bool, queues: u16) -> Self {
                let disk = (0..sectors * SECTOR_BYTES as usize)
                    .map(|i| (i * 7 + i / 512) as u8)
                    .collect();
                <$t>::new(disk, read_only, "aether-disk-0123456789")
                    .and_then(|d| d.with_queues(queues))
                    .expect("valid device")
            }
            fn blk_stats(&self) -> BlockStats {
                self.stats()
            }
            fn disk_bytes(&self) -> &[u8] {
                self.disk()
            }
            fn durable_bytes(&self) -> &[u8] {
                self.durable()
            }
        }
    };
}
dev_impl!(BlockDevice);
dev_impl!(ReferenceBlockDevice);

fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3)
    })
}

struct Rig<D: Dev> {
    t: MmioTransport<D>,
    m: GuestMemory,
    queues: Vec<DriverQueue>,
    free: Vec<Vec<u64>>,
    posted: Vec<BTreeMap<u16, (u64, u32)>>,
}

const SLOT: u64 = 0x2000;

impl<D: Dev> Rig<D> {
    fn new(dev: D, features: u64, format: RingFormat, queues: u16) -> Self {
        let mut m = ram();
        let mut t = MmioTransport::new(dev, 5);
        let qs: Vec<DriverQueue> = (0..u32::from(queues))
            .map(|q| DriverQueue::with_format(format, q, 0x1000 * u64::from(q + 1), 16))
            .collect();
        let refs: Vec<&DriverQueue> = qs.iter().collect();
        init(&mut t, &mut m, features | VIRTIO_F_VERSION_1, &refs);
        let free = (0..u64::from(queues))
            .map(|q| (0..8).map(|i| 0x1_0000 + q * 0x1_0000 + i * SLOT).collect())
            .collect();
        let posted = (0..queues).map(|_| BTreeMap::new()).collect();
        Rig {
            t,
            m,
            queues: qs,
            free,
            posted,
        }
    }

    /// Posts one request (header and payload readable, then `data_in`
    /// writable bytes and `status` writable bytes as separate buffers).
    fn post(&mut self, q: usize, req: &[u8], data_in: u32, status: u32) -> bool {
        if self.queues[q].outstanding() >= 4 {
            return false;
        }
        let Some(addr) = self.free[q].pop() else {
            return false;
        };
        self.m.write(addr, req).unwrap();
        let w = addr + 0x1000;
        let mut bufs = vec![(addr, req.len() as u32, false)];
        if data_in > 0 {
            bufs.push((w, data_in, true));
        }
        if status > 0 {
            bufs.push((w + u64::from(data_in), status, true));
        }
        let id = self.queues[q].post(&mut self.m, &bufs);
        self.posted[q].insert(id, (w, data_in + status));
        true
    }

    fn drain(&mut self, q: usize, log: &mut Vec<u64>) {
        while let Some((id, len)) = self.queues[q].used(&self.m) {
            let (w, room) = self.posted[q].remove(&id).expect("posted id");
            self.free[q].push(w - 0x1000);
            log.push(u64::from(id) << 32 | u64::from(len));
            let mut b = vec![0u8; room.min(len) as usize];
            self.m.read(w, &mut b).unwrap();
            log.push(hash(&b));
        }
    }

    fn state(&self, log: &mut Vec<u64>) {
        let d = self.t.device();
        let s = d.blk_stats();
        log.extend([
            s.reads,
            s.writes,
            s.flushes,
            s.io_errors,
            s.unsupported,
            s.discards,
            s.write_zeroes,
            u64::from(self.t.status()),
            hash(d.disk_bytes()),
            hash(d.durable_bytes()),
        ]);
        log.extend(s.per_queue);
    }
}

fn header(kind: u32, sector: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(16);
    h.extend_from_slice(&kind.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&sector.to_le_bytes());
    h
}

fn segment(r: &mut Rng, sectors: u64) -> [u8; 16] {
    let mut s = [0u8; 16];
    let sector = match r.below(4) {
        0 => r.below(sectors + 16),
        _ => r.below(sectors / 8) * 8,
    };
    let count: u32 = match r.below(4) {
        0 => r.below(20) as u32,
        _ => 8 * (1 + r.below(3) as u32),
    };
    let flags: u32 = match r.below(8) {
        0 => 1,
        1 => r.next() as u32,
        _ => 0,
    };
    s[0..8].copy_from_slice(&sector.to_le_bytes());
    s[8..12].copy_from_slice(&count.to_le_bytes());
    s[12..16].copy_from_slice(&flags.to_le_bytes());
    s
}

fn trace<D: Dev>(seed: u64, steps: usize) -> Vec<u64> {
    let mut r = Rng(seed);
    let sectors = 64u64;
    let read_only = r.below(5) == 0;
    let queues = [1u16, 1, 2, 3][r.below(4) as usize];
    let mut features = 0;
    for f in [
        VIRTIO_BLK_F_FLUSH,
        VIRTIO_BLK_F_DISCARD,
        VIRTIO_BLK_F_WRITE_ZEROES,
    ] {
        if r.below(4) != 0 {
            features |= f;
        }
    }
    if queues > 1 {
        features |= VIRTIO_BLK_F_MQ;
    }
    if read_only {
        features &= !(VIRTIO_BLK_F_DISCARD | VIRTIO_BLK_F_WRITE_ZEROES);
    }
    let format = if r.below(2) == 0 {
        RingFormat::Split
    } else {
        RingFormat::Packed
    };
    let dev = D::make(sectors as usize, read_only, queues);
    let mut rig = Rig::new(dev, features, format, queues);
    let mut log = Vec::new();
    for _ in 0..steps {
        let q = r.below(u64::from(queues)) as usize;
        let status = if r.below(30) == 0 { 0 } else { 1 };
        let sector = match r.below(6) {
            0 => r.below(sectors + 8),
            1 => u64::MAX / 512 - r.below(2),
            _ => r.below(sectors - 8),
        };
        let len = |r: &mut Rng| -> u32 {
            match r.below(6) {
                0 => r.below(1200) as u32,
                _ => 512 * (1 + r.below(8) as u32),
            }
        };
        let posted = match r.below(100) {
            0..=29 => {
                let mut h = header(VIRTIO_BLK_T_IN, sector);
                if r.below(20) == 0 {
                    h.push(1);
                }
                let n = len(&mut r);
                rig.post(q, &h, n, status)
            }
            30..=59 => {
                let mut req = header(VIRTIO_BLK_T_OUT, sector);
                let n = len(&mut r);
                req.extend((0..n).map(|_| r.next() as u8));
                let data_in = if r.below(20) == 0 { 4 } else { 0 };
                rig.post(q, &req, data_in, status)
            }
            60..=67 => {
                let data_in = if r.below(4) == 0 { 8 } else { 0 };
                rig.post(q, &header(VIRTIO_BLK_T_FLUSH, 0), data_in, status)
            }
            68..=73 => {
                let n = [8u32, 20, 32][r.below(3) as usize];
                rig.post(q, &header(VIRTIO_BLK_T_GET_ID, 0), n, status)
            }
            74..=89 => {
                let kind = if r.below(2) == 0 {
                    VIRTIO_BLK_T_DISCARD
                } else {
                    VIRTIO_BLK_T_WRITE_ZEROES
                };
                let mut req = header(kind, 0);
                for _ in 0..r.below(4) {
                    req.extend_from_slice(&segment(&mut r, sectors));
                }
                if r.below(10) == 0 {
                    req.push(0);
                }
                let data_in = if r.below(15) == 0 { 1 } else { 0 };
                rig.post(q, &req, data_in, status)
            }
            90..=94 => {
                let mut req = header(r.below(16) as u32, sector);
                if r.below(2) == 0 {
                    req.truncate(r.below(16) as usize);
                }
                rig.post(q, &req, len(&mut r) % 64, status)
            }
            _ => false,
        };
        log.push(u64::from(posted));
        let res = rig
            .t
            .write(&mut rig.m, reg::QUEUE_NOTIFY, &(q as u32).to_le_bytes());
        log.push(u64::from(res.is_ok()));
        rig.drain(q, &mut log);
        rig.state(&mut log);
        if rig.t.status() & crate::devices::mmio::status::DEVICE_NEEDS_RESET != 0 {
            break;
        }
    }
    log
}

#[test]
fn block_device_differential() {
    // reads, writes, flushes, io_errors, unsupported, discards, write_zeroes
    let mut totals = [0u64; 7];
    for seed in 1..=200u64 {
        let a = trace::<BlockDevice>(seed, 200);
        let b = trace::<ReferenceBlockDevice>(seed, 200);
        assert_eq!(a.len(), b.len(), "seed {seed}");
        if let Some(i) = a.iter().zip(&b).position(|(x, y)| x != y) {
            panic!("seed {seed}: first diff at entry {i}: {} vs {}", a[i], b[i]);
        }
        // The last `state` entry: seven counters, status, two hashes and
        // eight per-queue counts.
        let tail = &a[a.len() - 18..];
        for (t, v) in totals.iter_mut().zip(tail) {
            *t += v;
        }
    }
    println!("block differential totals {totals:?}");
    assert!(
        totals.iter().all(|&t| t > 0),
        "every request kind is exercised: {totals:?}"
    );
}

/// 4 KiB traffic on one queue: four reads and four writes per round at
/// spread-out sectors of a 1 MiB disk.
fn traffic<D: Dev>(rig: &mut Rig<D>, writes: &[Vec<u8>], reads: &[Vec<u8>], rounds: usize) -> u64 {
    let mut h = 0u64;
    for _ in 0..rounds {
        for (w, rd) in writes.iter().zip(reads) {
            rig.post(0, w, 0, 1);
            rig.post(0, rd, 4096, 1);
            w32(&mut rig.t, &mut rig.m, reg::QUEUE_NOTIFY, 0);
            while let Some((id, len)) = rig.queues[0].used(&rig.m) {
                let (w, _) = rig.posted[0].remove(&id).expect("posted id");
                rig.free[0].push(w - 0x1000);
                h = h.wrapping_add(u64::from(len));
            }
        }
    }
    h
}

fn traffic_inputs() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut r = Rng(13);
    let writes = (0..4)
        .map(|i| {
            let mut w = header(VIRTIO_BLK_T_OUT, 8 * (37 * i + 5) % 2040);
            w.extend((0..4096).map(|_| r.next() as u8));
            w
        })
        .collect();
    let reads = (0..4)
        .map(|i| header(VIRTIO_BLK_T_IN, 8 * (53 * i + 11) % 2040))
        .collect();
    (writes, reads)
}

fn traffic_rig<D: Dev>() -> Rig<D> {
    Rig::new(
        D::make(2048, false, 1),
        VIRTIO_BLK_F_FLUSH,
        RingFormat::Split,
        1,
    )
}

#[test]
fn block_traffic_matches() {
    let (w, rd) = traffic_inputs();
    let (mut a, mut b) = (
        traffic_rig::<BlockDevice>(),
        traffic_rig::<ReferenceBlockDevice>(),
    );
    assert_eq!(traffic(&mut a, &w, &rd, 4), traffic(&mut b, &w, &rd, 4));
    assert_eq!(a.t.device().stats(), b.t.device().stats());
    assert_eq!(a.t.device().stats().reads, 16);
    assert_eq!(a.t.device().stats().writes, 16);
    assert_eq!(a.t.device().disk(), b.t.device().disk());
}

struct Arm<F: FnMut() -> u64>(F);
impl<F: FnMut() -> u64> BenchCase for Arm<F> {
    type Input = ();
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<(), ()> {
        Ok(())
    }
    fn run(&mut self, (): ()) -> Result<u64, ()> {
        Ok((self.0)())
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_block_device() {
    let (w, rd) = traffic_inputs();
    let (mut a, mut b) = (
        traffic_rig::<BlockDevice>(),
        traffic_rig::<ReferenceBlockDevice>(),
    );
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(
        run,
        &mut Arm(|| traffic(&mut b, black_box(&w), black_box(&rd), 4)),
        &mut Arm(|| traffic(&mut a, black_box(&w), black_box(&rd), 4)),
    )
    .expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs block_device {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "block_device: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}
