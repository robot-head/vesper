//! The Vesper virtio-net device (`vesper/src/net.ves`) against the Rust
//! reference device (`src/devices/net/reference.rs`), both behind the real
//! MMIO transport and split/packed rings driven by `tests/common`'s driver:
//! a randomized differential over host frames, guest receive buffers
//! (single and mergeable), transmits, control commands and link changes,
//! and an ABBA timing of full-size frame traffic that prints the flip
//! gate's `flip-gate pairs` line (crates/vesper-aether/flip-gate.toml).

use std::collections::BTreeMap;
use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::devices::mmio::{MmioTransport, VIRTIO_F_VERSION_1, VirtioDevice, reg};
use crate::devices::net::{
    NET_HDR_BYTES, NetDevice, NetStats, ReferenceNetDevice, VIRTIO_NET_CTRL_MQ,
    VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, VIRTIO_NET_F_CTRL_VQ, VIRTIO_NET_F_MAC, VIRTIO_NET_F_MQ,
    VIRTIO_NET_F_MRG_RXBUF, VIRTIO_NET_F_MTU, VIRTIO_NET_F_STATUS,
};
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

/// The host surface both devices share.
trait Dev: VirtioDevice + std::fmt::Debug {
    fn make(pairs: u16) -> Self;
    fn inject(&mut self, pair: u16, frame: &[u8]) -> bool;
    fn take(&mut self) -> Option<Vec<u8>>;
    fn link(&mut self, up: bool);
    fn net_stats(&self) -> NetStats;
    fn pairs(&self) -> u16;
    fn backlog(&self) -> usize;
}

macro_rules! dev_impl {
    ($t:ty) => {
        impl Dev for $t {
            fn make(pairs: u16) -> Self {
                <$t>::new([2, 0, 0, 0, 0, 7], 1500)
                    .and_then(|d| d.with_queue_pairs(pairs))
                    .expect("valid device")
            }
            fn inject(&mut self, pair: u16, frame: &[u8]) -> bool {
                self.inject_rx_on(pair, frame)
            }
            fn take(&mut self) -> Option<Vec<u8>> {
                self.take_tx()
            }
            fn link(&mut self, up: bool) {
                self.set_link(up)
            }
            fn net_stats(&self) -> NetStats {
                self.stats()
            }
            fn pairs(&self) -> u16 {
                self.active_pairs()
            }
            fn backlog(&self) -> usize {
                self.rx_backlog()
            }
        }
    };
}
dev_impl!(NetDevice);
dev_impl!(ReferenceNetDevice);

const BUF: u64 = 0x1000;

/// A driven device: the transport, guest RAM and the driver queues
/// (`rx0, tx0, rx1, tx1, ..., ctrl`), with each queue's free buffer
/// addresses and the posted ones by id.
struct Rig<D: Dev> {
    t: MmioTransport<D>,
    m: GuestMemory,
    queues: Vec<DriverQueue>,
    free: Vec<Vec<u64>>,
    posted: Vec<BTreeMap<u16, u64>>,
    pairs: u16,
}

impl<D: Dev> Rig<D> {
    fn new(pairs: u16, features: u64, format: RingFormat) -> Self {
        let mut m = ram();
        let mut t = MmioTransport::new(D::make(pairs), 4);
        let n = if pairs > 1 { 2 * pairs + 1 } else { 2 };
        let queues: Vec<DriverQueue> = (0..u32::from(n))
            .map(|q| DriverQueue::with_format(format, q, 0x1000 * u64::from(q + 1), 16))
            .collect();
        let refs: Vec<&DriverQueue> = queues.iter().collect();
        init(&mut t, &mut m, features | VIRTIO_F_VERSION_1, &refs);
        let free = (0..u64::from(n))
            .map(|q| (0..16).map(|i| 0x1_0000 + q * 0x1_0000 + i * BUF).collect())
            .collect();
        let posted = (0..n).map(|_| BTreeMap::new()).collect();
        Rig {
            t,
            m,
            queues,
            free,
            posted,
            pairs,
        }
    }

    /// Publishes every used element of queue `q` into `log`, with the
    /// bytes written into receive buffers.
    fn drain(&mut self, q: usize, log: &mut Vec<u64>) {
        while let Some((id, len)) = self.queues[q].used(&self.m) {
            let addr = self.posted[q].remove(&id).expect("posted id");
            self.free[q].push(addr);
            log.push(u64::from(id) << 32 | u64::from(len));
            if q.is_multiple_of(2) && q < 2 * usize::from(self.pairs) && len > 0 {
                let mut b = vec![0u8; len as usize];
                self.m.read(addr, &mut b).unwrap();
                log.push(
                    b.iter()
                        .fold(0u64, |h, &x| h.wrapping_mul(131).wrapping_add(u64::from(x))),
                );
            }
        }
    }

    fn post_rx(&mut self, q: usize, len: u32) -> bool {
        if self.queues[q].outstanding() >= 15 {
            return false;
        }
        let Some(addr) = self.free[q].pop() else {
            return false;
        };
        let id = self.queues[q].post(&mut self.m, &[(addr, len, true)]);
        self.posted[q].insert(id, addr);
        true
    }

    fn notify(&mut self, q: usize, log: &mut Vec<u64>) {
        let r = self
            .t
            .write(&mut self.m, reg::QUEUE_NOTIFY, &(q as u32).to_le_bytes());
        log.push(u64::from(r.is_ok()));
        self.drain(q, log);
    }

    fn transmit(&mut self, q: usize, frame: &[u8], log: &mut Vec<u64>) {
        if self.queues[q].outstanding() >= 15 {
            return;
        }
        let Some(addr) = self.free[q].pop() else {
            return;
        };
        self.m.write(addr, frame).unwrap();
        let id = self.queues[q].post(&mut self.m, &[(addr, frame.len() as u32, false)]);
        self.posted[q].insert(id, addr);
        self.notify(q, log);
    }

    fn control(&mut self, cmd: &[u8], log: &mut Vec<u64>) {
        let q = self.queues.len() - 1;
        if self.queues[q].outstanding() >= 7 {
            return;
        }
        let Some(addr) = self.free[q].pop() else {
            return;
        };
        self.m.write(addr, cmd).unwrap();
        let id = self.queues[q].post(
            &mut self.m,
            &[(addr, cmd.len() as u32, false), (addr + 0x800, 1, true)],
        );
        self.posted[q].insert(id, addr);
        self.notify(q, log);
        let mut ack = [0u8];
        self.m.read(addr + 0x800, &mut ack).unwrap();
        log.push(u64::from(ack[0]));
    }

    fn state(&self, log: &mut Vec<u64>) {
        let d = self.t.device();
        let s = d.net_stats();
        log.extend([
            s.tx_frames,
            s.tx_dropped,
            s.tx_dropped_backlog,
            s.rx_frames,
            s.rx_dropped_backlog,
            s.rx_dropped_small_buffer,
            s.rx_dropped_inactive,
            s.rx_merged,
            s.ctrl_ok,
            s.ctrl_err,
            u64::from(d.pairs()),
            d.backlog() as u64,
            u64::from(self.t.status()),
        ]);
        let mut cfg = [0u8; 12];
        d.read_config(0, &mut cfg);
        log.push(u64::from_le_bytes(cfg[..8].try_into().unwrap()));
    }
}

fn frame(r: &mut Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| r.next() as u8).collect()
}

fn trace<D: Dev>(seed: u64, steps: usize) -> Vec<u64> {
    let mut r = Rng(seed);
    let pairs = [1, 1, 2, 3][r.below(4) as usize];
    let mut features = VIRTIO_NET_F_MAC | VIRTIO_NET_F_MTU | VIRTIO_NET_F_STATUS;
    if r.below(2) == 0 {
        features |= VIRTIO_NET_F_MRG_RXBUF;
    }
    if pairs > 1 && r.below(4) != 0 {
        features |= VIRTIO_NET_F_MQ | VIRTIO_NET_F_CTRL_VQ;
    }
    let format = if r.below(2) == 0 {
        RingFormat::Split
    } else {
        RingFormat::Packed
    };
    let mut rig = Rig::<D>::new(pairs, features, format);
    let ctrl = features & VIRTIO_NET_F_CTRL_VQ != 0;
    let mut log = Vec::new();
    for _ in 0..steps {
        let pair = r.below(u64::from(pairs)) as usize;
        match r.below(100) {
            0..=29 => {
                let len = match r.below(8) {
                    0 => r.below(20) as usize,
                    1 => 1500 + r.below(40) as usize,
                    _ => 14 + r.below(1500) as usize,
                };
                let f = frame(&mut r, len);
                let ok = rig.t.with_device(|d| d.inject(pair as u16, &f));
                log.push(u64::from(ok));
            }
            30..=49 => {
                let q = 2 * pair;
                for _ in 0..1 + r.below(4) {
                    let len = [8, 12, 64, 600, 1526, 2048, 4096][r.below(7) as usize];
                    rig.post_rx(q, len);
                }
                let res = rig.t.poll(&mut rig.m, q as u32);
                log.push(u64::from(res.is_ok()));
                rig.drain(q, &mut log);
            }
            50..=69 => {
                let len = match r.below(8) {
                    0 => r.below(30) as usize,
                    1 => NET_HDR_BYTES + 1514 + r.below(4) as usize,
                    _ => NET_HDR_BYTES + 14 + r.below(1500) as usize,
                };
                let mut f = frame(&mut r, len);
                if f.len() >= 2 && r.below(6) != 0 {
                    f[0] = 0;
                    f[1] = 0;
                }
                rig.transmit(2 * pair + 1, &f, &mut log);
            }
            70..=79 => {
                for _ in 0..r.below(20) {
                    let got = rig.t.with_device(|d| d.take());
                    log.push(got.map_or(u64::MAX, |f| f.len() as u64));
                }
            }
            80..=87 if ctrl => {
                let n = r.below(5) as u8;
                let mut cmd = vec![VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, n, 0];
                if r.below(6) == 0 {
                    cmd[r.below(4) as usize] = r.next() as u8;
                }
                if r.below(8) == 0 {
                    cmd.truncate(r.below(4) as usize);
                }
                rig.control(&cmd, &mut log);
            }
            88..=91 => {
                let up = r.below(3) != 0;
                rig.t.with_device(|d| d.link(up));
                log.push(u64::from(rig.t.with_device(|d| d.take_config_change())));
            }
            _ => {
                let q = 2 * pair;
                let res = rig.t.poll(&mut rig.m, q as u32);
                log.push(u64::from(res.is_ok()));
                rig.drain(q, &mut log);
            }
        }
        rig.state(&mut log);
    }
    log
}

#[test]
fn net_device_differential() {
    for seed in 1..=200u64 {
        let a = trace::<NetDevice>(seed, 300);
        let b = trace::<ReferenceNetDevice>(seed, 300);
        assert_eq!(a.len(), b.len(), "seed {seed}");
        if let Some(i) = a.iter().zip(&b).position(|(x, y)| x != y) {
            panic!("seed {seed}: first diff at entry {i}: {} vs {}", a[i], b[i]);
        }
    }
}

/// Full-size frame traffic on one pair with mergeable buffers: the guest
/// posts 12 receive buffers of 2 KiB and the host injects 12 1514-byte
/// frames into them, then the guest transmits 12 frames the host takes.
fn traffic<D: Dev>(rig: &mut Rig<D>, rx: &[Vec<u8>], tx: &[Vec<u8>], rounds: usize) -> u64 {
    let mut h = 0u64;
    for _ in 0..rounds {
        for f in rx {
            rig.post_rx(0, 2048);
            rig.t.with_device(|d| d.inject(0, f));
        }
        rig.t.poll(&mut rig.m, 0).unwrap();
        while let Some((id, len)) = rig.queues[0].used(&rig.m) {
            let addr = rig.posted[0].remove(&id).expect("posted id");
            rig.free[0].push(addr);
            h = h.wrapping_add(u64::from(len));
        }
        for f in tx {
            let addr = rig.free[1].pop().expect("free tx buffer");
            rig.m.write(addr, f).unwrap();
            let id = rig.queues[1].post(&mut rig.m, &[(addr, f.len() as u32, false)]);
            rig.posted[1].insert(id, addr);
        }
        w32(&mut rig.t, &mut rig.m, reg::QUEUE_NOTIFY, 1);
        while let Some((id, _)) = rig.queues[1].used(&rig.m) {
            let addr = rig.posted[1].remove(&id).expect("posted id");
            rig.free[1].push(addr);
        }
        while let Some(f) = rig.t.with_device(|d| d.take()) {
            h = h.wrapping_add(f.len() as u64);
        }
    }
    h
}

fn traffic_rig<D: Dev>() -> Rig<D> {
    Rig::new(
        1,
        VIRTIO_NET_F_MAC | VIRTIO_NET_F_MTU | VIRTIO_NET_F_STATUS | VIRTIO_NET_F_MRG_RXBUF,
        RingFormat::Split,
    )
}

fn traffic_frames() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut r = Rng(11);
    let rx = (0..12).map(|_| frame(&mut r, 1514)).collect();
    let tx = (0..12)
        .map(|_| {
            let mut f = frame(&mut r, NET_HDR_BYTES + 1514);
            f[0] = 0;
            f[1] = 0;
            f
        })
        .collect();
    (rx, tx)
}

#[test]
fn net_traffic_matches() {
    let (rx, tx) = traffic_frames();
    let (mut a, mut b) = (
        traffic_rig::<NetDevice>(),
        traffic_rig::<ReferenceNetDevice>(),
    );
    assert_eq!(traffic(&mut a, &rx, &tx, 4), traffic(&mut b, &rx, &tx, 4));
    assert_eq!(a.t.device().stats(), b.t.device().stats());
    assert_eq!(a.t.device().stats().rx_frames, 48);
    assert_eq!(a.t.device().stats().tx_frames, 48);
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
fn abba_net_device() {
    let (rx, tx) = traffic_frames();
    let (mut a, mut b) = (
        traffic_rig::<NetDevice>(),
        traffic_rig::<ReferenceNetDevice>(),
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
        &mut Arm(|| traffic(&mut b, black_box(&rx), black_box(&tx), 4)),
        &mut Arm(|| traffic(&mut a, black_box(&rx), black_box(&tx), 4)),
    )
    .expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs net_device {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "net_device: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}
