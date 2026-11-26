//! The Vesper virtio-mmio register file (`vesper/src/mmio.ves`) against the
//! Rust reference transport (`src/devices/mmio/reference.rs`), both over the
//! same Vesper block device and `tests/common`'s ring driver: a randomized
//! differential of driver sessions (feature negotiation, queue setup, block
//! traffic with plain and `NOTIFICATION_DATA` notifications, interrupt
//! acknowledges) mixed with arbitrary register reads and writes of every
//! width, and an ABBA timing of a driver session that prints the flip gate's
//! `flip-gate pairs` line (crates/vesper-aether/flip-gate.toml).

use std::collections::BTreeMap;
use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::devices::block::{BlockDevice, SECTOR_BYTES, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_MQ};
use crate::devices::block::{VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT};
use crate::devices::mmio::{
    MmioError, MmioTransport, RING_FEATURES, ReferenceMmioTransport, TransportStats,
    VIRTIO_F_RING_PACKED, VIRTIO_F_VERSION_1, reg, status,
};
use crate::devices::net::NetDevice;
use crate::devices::queue::{
    QueueStats, RingFormat, VIRTIO_F_EVENT_IDX, VIRTIO_F_IN_ORDER, VIRTIO_F_NOTIFICATION_DATA,
};
use crate::guest_mem::GuestMemory;
use crate::test_driver::{DriverQueue, ram};

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

trait Transport {
    fn make(dev: BlockDevice, ring_features: u64) -> Self;
    fn rd(&mut self, offset: u64, data: &mut [u8]) -> Result<(), MmioError>;
    fn wr(&mut self, m: &mut GuestMemory, offset: u64, data: &[u8]) -> Result<(), MmioError>;
    fn poll_queue(&mut self, m: &mut GuestMemory, index: u32) -> Result<(), MmioError>;
    fn snapshot(&self) -> (u32, u64, bool, TransportStats, [Option<QueueStats>; 3]);
    fn disk_hash(&self) -> u64;
}

macro_rules! transport_impl {
    ($t:ty) => {
        impl Transport for $t {
            fn make(dev: BlockDevice, ring_features: u64) -> Self {
                <$t>::with_ring_features(dev, 3, ring_features)
            }
            fn rd(&mut self, offset: u64, data: &mut [u8]) -> Result<(), MmioError> {
                self.read(offset, data)
            }
            fn wr(
                &mut self,
                m: &mut GuestMemory,
                offset: u64,
                data: &[u8],
            ) -> Result<(), MmioError> {
                self.write(m, offset, data)
            }
            fn poll_queue(&mut self, m: &mut GuestMemory, index: u32) -> Result<(), MmioError> {
                self.poll(m, index)
            }
            fn snapshot(&self) -> (u32, u64, bool, TransportStats, [Option<QueueStats>; 3]) {
                (
                    self.status(),
                    self.negotiated_features(),
                    self.interrupt_pending(),
                    self.stats(),
                    [
                        self.queue_stats(0),
                        self.queue_stats(1),
                        self.queue_stats(2),
                    ],
                )
            }
            fn disk_hash(&self) -> u64 {
                hash(self.device().disk())
            }
        }
    };
}
transport_impl!(MmioTransport<BlockDevice>);
transport_impl!(ReferenceMmioTransport<BlockDevice>);

fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3)
    })
}

fn hash_str(s: &str) -> u64 {
    hash(s.as_bytes())
}

fn device() -> BlockDevice {
    let disk = (0..64 * SECTOR_BYTES as usize)
        .map(|i| (i * 13 + i / 512) as u8)
        .collect();
    BlockDevice::new(disk, false, "mmio-diff")
        .and_then(|d| d.with_queues(2))
        .expect("valid device")
}

const SLOT: u64 = 0x2000;

/// A driver of one transport: its rings, and whether they still match the
/// device's (an arbitrary QueueReady or Status write desynchronizes them
/// until the next initialization).
struct Rig<T: Transport> {
    t: T,
    m: GuestMemory,
    queues: Vec<DriverQueue>,
    synced: bool,
    notification_data: bool,
    free: Vec<Vec<u64>>,
    posted: Vec<BTreeMap<u16, u64>>,
}

impl<T: Transport> Rig<T> {
    fn new(ring_features: u64) -> Self {
        Rig {
            t: T::make(device(), ring_features),
            m: ram(),
            queues: Vec::new(),
            synced: false,
            notification_data: false,
            free: Vec::new(),
            posted: Vec::new(),
        }
    }

    fn w(&mut self, offset: u64, value: u32, log: &mut Vec<u64>) -> bool {
        let r = self.t.wr(&mut self.m, offset, &value.to_le_bytes());
        log.push(hash_str(&format!("{r:?}")));
        r.is_ok()
    }

    fn r(&mut self, offset: u64, log: &mut Vec<u64>) -> u32 {
        let mut b = [0u8; 4];
        let r = self.t.rd(offset, &mut b);
        log.push(hash_str(&format!("{r:?}")));
        let v = u32::from_le_bytes(b);
        log.push(u64::from(v));
        v
    }

    /// The §3.1.1 sequence with `features` and `sizes` (one per queue).
    fn init(&mut self, features: u64, sizes: &[u16], log: &mut Vec<u64>) {
        self.synced = false;
        self.w(reg::STATUS, 0, log);
        self.w(reg::STATUS, status::ACKNOWLEDGE, log);
        self.w(reg::STATUS, status::ACKNOWLEDGE | status::DRIVER, log);
        self.w(reg::DEVICE_FEATURES_SEL, 0, log);
        self.r(reg::DEVICE_FEATURES, log);
        self.w(reg::DEVICE_FEATURES_SEL, 1, log);
        self.r(reg::DEVICE_FEATURES, log);
        self.w(reg::DRIVER_FEATURES_SEL, 0, log);
        self.w(reg::DRIVER_FEATURES, features as u32, log);
        self.w(reg::DRIVER_FEATURES_SEL, 1, log);
        self.w(reg::DRIVER_FEATURES, (features >> 32) as u32, log);
        self.w(
            reg::STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
            log,
        );
        let st = self.r(reg::STATUS, log);
        if st & status::FEATURES_OK == 0 {
            return;
        }
        let format = if features & VIRTIO_F_RING_PACKED != 0 {
            RingFormat::Packed
        } else {
            RingFormat::Split
        };
        self.queues = sizes
            .iter()
            .enumerate()
            .map(|(q, &size)| {
                let mut dq =
                    DriverQueue::with_format(format, q as u32, 0x1000 * (q as u64 + 1), size);
                dq.in_order = features & VIRTIO_F_IN_ORDER != 0;
                dq
            })
            .collect();
        let mut ok = true;
        for q in 0..self.queues.len() {
            self.queues[q].clear(&mut self.m);
            let (index, size, desc, driver, dev) = {
                let d = &self.queues[q];
                (d.index, d.size, d.desc, d.driver, d.device)
            };
            self.w(reg::QUEUE_SEL, index, log);
            self.r(reg::QUEUE_NUM_MAX, log);
            ok &= self.w(reg::QUEUE_NUM, u32::from(size), log);
            self.w(reg::QUEUE_DESC_LOW, desc as u32, log);
            self.w(reg::QUEUE_DESC_HIGH, (desc >> 32) as u32, log);
            self.w(reg::QUEUE_DRIVER_LOW, driver as u32, log);
            self.w(reg::QUEUE_DRIVER_HIGH, (driver >> 32) as u32, log);
            self.w(reg::QUEUE_DEVICE_LOW, dev as u32, log);
            self.w(reg::QUEUE_DEVICE_HIGH, (dev >> 32) as u32, log);
            ok &= self.w(reg::QUEUE_READY, 1, log);
            self.r(reg::QUEUE_READY, log);
        }
        ok &= self.w(
            reg::STATUS,
            status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
            log,
        );
        self.synced = ok;
        self.notification_data = features & VIRTIO_F_NOTIFICATION_DATA != 0;
        self.free = (0..self.queues.len() as u64)
            .map(|q| (0..4).map(|i| 0x1_0000 + q * 0x1_0000 + i * SLOT).collect())
            .collect();
        self.posted = self.queues.iter().map(|_| BTreeMap::new()).collect();
    }

    /// Posts one block request on queue `q` (readable header, then `data`
    /// writable bytes and a status byte).
    fn post(&mut self, q: usize, kind: u32, sector: u64, data: u32) -> bool {
        if !self.synced || q >= self.queues.len() {
            return false;
        }
        // At most three descriptors a chain.
        if (self.queues[q].outstanding() + 1) * 3 > usize::from(self.queues[q].size) {
            return false;
        }
        let Some(addr) = self.free[q].pop() else {
            return false;
        };
        let mut h = Vec::with_capacity(16);
        h.extend_from_slice(&kind.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&sector.to_le_bytes());
        let mut bufs = vec![(addr, 16, false)];
        let w = addr + 0x1000;
        if kind == VIRTIO_BLK_T_OUT {
            self.m
                .write(addr + 16, &vec![kind as u8 ^ 0x5a; data as usize])
                .unwrap();
            bufs.push((addr + 16, data, false));
        } else if data > 0 {
            bufs.push((w, data, true));
        }
        self.m.write(addr, &h).unwrap();
        bufs.push((w + 0x800, 1, true));
        let id = self.queues[q].post(&mut self.m, &bufs);
        self.posted[q].insert(id, addr);
        true
    }

    fn notify(&mut self, q: usize, log: &mut Vec<u64>) {
        let value = if self.notification_data && q < self.queues.len() {
            self.queues[q].notify_data()
        } else {
            q as u32
        };
        self.w(reg::QUEUE_NOTIFY, value, log);
    }

    fn drain(&mut self, log: &mut Vec<u64>) {
        if !self.synced {
            return;
        }
        for q in 0..self.queues.len() {
            while let Some((id, len)) = self.queues[q].used(&self.m) {
                let addr = self.posted[q].remove(&id).expect("posted id");
                self.free[q].push(addr);
                log.push(u64::from(id) << 32 | u64::from(len));
                let mut b = vec![0u8; 512];
                self.m.read(addr + 0x1000, &mut b).unwrap();
                log.push(hash(&b));
            }
        }
    }

    fn state(&self, log: &mut Vec<u64>) {
        log.push(hash_str(&format!("{:?}", self.t.snapshot())));
    }
}

const REGISTERS: [u64; 30] = [
    reg::MAGIC_VALUE,
    reg::VERSION,
    reg::DEVICE_ID,
    reg::VENDOR_ID,
    reg::DEVICE_FEATURES,
    reg::DEVICE_FEATURES_SEL,
    reg::DRIVER_FEATURES,
    reg::DRIVER_FEATURES_SEL,
    reg::QUEUE_SEL,
    reg::QUEUE_NUM_MAX,
    reg::QUEUE_NUM,
    reg::QUEUE_READY,
    reg::QUEUE_NOTIFY,
    reg::INTERRUPT_STATUS,
    reg::INTERRUPT_ACK,
    reg::STATUS,
    reg::QUEUE_DESC_LOW,
    reg::QUEUE_DESC_HIGH,
    reg::QUEUE_DRIVER_LOW,
    reg::QUEUE_DRIVER_HIGH,
    reg::QUEUE_DEVICE_LOW,
    reg::QUEUE_DEVICE_HIGH,
    reg::SHM_SEL,
    reg::SHM_LEN_LOW,
    reg::SHM_LEN_HIGH,
    reg::SHM_BASE_LOW,
    reg::SHM_BASE_HIGH,
    reg::QUEUE_RESET,
    reg::CONFIG_GENERATION,
    reg::CONFIG,
];

fn offset(r: &mut Rng) -> u64 {
    match r.below(8) {
        0 => r.below(0x220),
        1 => reg::CONFIG + r.below(0x40),
        _ => REGISTERS[r.below(REGISTERS.len() as u64) as usize],
    }
}

fn value(r: &mut Rng) -> u32 {
    match r.below(8) {
        0 => r.next() as u32,
        1 => u32::MAX,
        2 => 0,
        3 => 1 << r.below(8),
        _ => r.below(20) as u32,
    }
}

fn width(r: &mut Rng) -> usize {
    match r.below(10) {
        0 => 1,
        1 => 2,
        2 => 8,
        3 => 3,
        _ => 4,
    }
}

fn features(r: &mut Rng) -> u64 {
    let mut f = 0;
    for bit in [
        VIRTIO_F_RING_PACKED,
        VIRTIO_F_EVENT_IDX,
        VIRTIO_F_IN_ORDER,
        VIRTIO_F_NOTIFICATION_DATA,
        VIRTIO_BLK_F_FLUSH,
        VIRTIO_BLK_F_MQ,
    ] {
        if r.below(2) == 0 {
            f |= bit;
        }
    }
    match r.below(10) {
        0 => f,
        1 => f | VIRTIO_F_VERSION_1 | 1 << (40 + r.below(20)),
        _ => f | VIRTIO_F_VERSION_1,
    }
}

/// The run's log, final counters and whether the device ever needed a reset.
fn trace<T: Transport>(seed: u64, steps: usize) -> (Vec<u64>, TransportStats, bool) {
    let mut r = Rng(seed);
    let ring_features = if r.below(3) == 0 {
        r.next() & RING_FEATURES
    } else {
        RING_FEATURES
    };
    let mut rig = Rig::<T>::new(ring_features);
    let mut log = Vec::new();
    let mut saw_reset = false;
    for _ in 0..steps {
        match r.below(100) {
            0..=7 => {
                let f = features(&mut r);
                let sizes = [[16u16, 16], [8, 16], [16, 0], [4, 8]][r.below(4) as usize];
                let sizes: Vec<u16> = sizes.iter().copied().filter(|&s| s > 0).collect();
                rig.init(f, &sizes, &mut log);
            }
            8..=49 => {
                let q = r.below(2) as usize;
                for _ in 0..1 + r.below(3) {
                    let (kind, data) = match r.below(4) {
                        0 => (VIRTIO_BLK_T_OUT, 512 * (1 + r.below(2) as u32)),
                        1 => (VIRTIO_BLK_T_FLUSH, 0),
                        _ => (VIRTIO_BLK_T_IN, 512),
                    };
                    let sector = r.below(70);
                    log.push(u64::from(rig.post(q, kind, sector, data)));
                }
                rig.notify(q, &mut log);
            }
            50 => {
                // A chain whose buffer lies outside RAM: the capture fails
                // and the device waits for a reset.
                let q = r.below(2) as usize;
                if rig.synced && q < rig.queues.len() && rig.queues[q].outstanding() == 0 {
                    rig.queues[q].post(&mut rig.m, &[(0xffff_0000, 16, false)]);
                    rig.synced = false;
                    rig.notify(q, &mut log);
                }
            }
            51..=57 => {
                let q = r.below(3) as u32;
                let res = rig.t.poll_queue(&mut rig.m, q);
                log.push(hash_str(&format!("{res:?}")));
            }
            58..=64 => {
                let isr = rig.r(reg::INTERRUPT_STATUS, &mut log);
                rig.w(reg::INTERRUPT_ACK, isr, &mut log);
            }
            65..=79 => {
                let off = offset(&mut r);
                let mut b = vec![0u8; width(&mut r)];
                let res = rig.t.rd(off, &mut b);
                log.push(hash_str(&format!("{res:?}")));
                log.push(hash(&b));
            }
            _ => {
                let off = offset(&mut r);
                let v = value(&mut r);
                let n = width(&mut r);
                let mut b = v.to_le_bytes().to_vec();
                b.resize(n, 0x77);
                if matches!(off, reg::QUEUE_READY | reg::STATUS | reg::QUEUE_NOTIFY) {
                    rig.synced = false;
                }
                let res = rig.t.wr(&mut rig.m, off, &b);
                log.push(hash_str(&format!("{res:?}")));
            }
        }
        rig.drain(&mut log);
        rig.state(&mut log);
        saw_reset |= rig.t.snapshot().0 & status::DEVICE_NEEDS_RESET != 0;
    }
    log.push(rig.t.disk_hash());
    let mut all = vec![0u8; 1 << 20];
    rig.m.read(0, &mut all).unwrap();
    log.push(hash(&all));
    (log, rig.t.snapshot().3, saw_reset)
}

#[test]
fn mmio_transport_differential() {
    let mut total = TransportStats::default();
    let mut needs_reset = 0u64;
    for seed in 1..=200u64 {
        let (a, s, reset) = trace::<MmioTransport<BlockDevice>>(seed, 300);
        let (b, _, _) = trace::<ReferenceMmioTransport<BlockDevice>>(seed, 300);
        needs_reset += u64::from(reset);
        assert_eq!(a.len(), b.len(), "seed {seed}");
        if let Some(i) = a.iter().zip(&b).position(|(x, y)| x != y) {
            panic!("seed {seed}: first diff at entry {i}: {} vs {}", a[i], b[i]);
        }
        total.notifications += s.notifications;
        total.chains_captured += s.chains_captured;
        total.chains_completed += s.chains_completed;
        total.interrupts += s.interrupts;
        total.rejected_feature_sets += s.rejected_feature_sets;
        total.interrupts_suppressed += s.interrupts_suppressed;
    }
    println!("mmio differential totals {total:?}, {needs_reset} runs reached DEVICE_NEEDS_RESET");
    assert!(needs_reset > 0);
    assert!(
        total.notifications > 0
            && total.chains_completed > 0
            && total.interrupts > 0
            && total.rejected_feature_sets > 0
            && total.interrupts_suppressed > 0,
        "{total:?}"
    );
}

/// One driver session: initialization with EVENT_IDX off, then rounds of
/// four 512-byte reads, a notification, the interrupt acknowledge and the
/// used-ring drain.
fn session<T: Transport>(rig: &mut Rig<T>, rounds: usize) -> u64 {
    let mut log = Vec::new();
    rig.init(VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_FLUSH, &[16, 16], &mut log);
    for i in 0..rounds {
        for k in 0..4 {
            rig.post(0, VIRTIO_BLK_T_IN, ((i * 4 + k) % 60) as u64, 512);
        }
        rig.notify(0, &mut log);
        let isr = rig.r(reg::INTERRUPT_STATUS, &mut log);
        rig.w(reg::INTERRUPT_ACK, isr, &mut log);
        rig.drain(&mut log);
    }
    log.iter()
        .fold(0, |h, &x| h.wrapping_mul(31).wrapping_add(x))
}

#[test]
fn session_matches() {
    let mut a = Rig::<MmioTransport<BlockDevice>>::new(RING_FEATURES);
    let mut b = Rig::<ReferenceMmioTransport<BlockDevice>>::new(RING_FEATURES);
    for _ in 0..3 {
        assert_eq!(session(&mut a, 32), session(&mut b, 32));
    }
    assert_eq!(a.t.snapshot().3, b.t.snapshot().3);
    assert_eq!(a.t.snapshot().3.chains_completed, 3 * 32 * 4);
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
fn abba_mmio_transport() {
    let mut a = Rig::<MmioTransport<BlockDevice>>::new(RING_FEATURES);
    let mut b = Rig::<ReferenceMmioTransport<BlockDevice>>::new(RING_FEATURES);
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
        &mut Arm(|| session(&mut b, black_box(32))),
        &mut Arm(|| session(&mut a, black_box(32))),
    )
    .expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs mmio_transport {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "mmio_transport: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}

/// Host-side configuration changes (the net device's link state through
/// `with_device`) and configuration-space accesses of every width.
fn config_trace<T: NetTransport>(seed: u64) -> Vec<String> {
    let mut r = Rng(seed);
    let mut m = ram();
    let dev = NetDevice::new([0x52, 0x54, 0, 1, 2, 3], 1500).unwrap();
    let mut t = T::make_net(dev);
    let mut log = Vec::new();
    for _ in 0..200 {
        match r.below(4) {
            0 => {
                let up = r.below(2) == 0;
                t.link(up);
            }
            1 => {
                let off = reg::CONFIG + r.below(16);
                let mut b = vec![0u8; [1, 2, 4, 8, 3][r.below(5) as usize]];
                let res = t.rd(off, &mut b);
                log.push(format!("{res:?} {b:?}"));
            }
            2 => {
                let off = reg::CONFIG + r.below(16);
                let res = t.wr(&mut m, off, &[1, 2][..1 + r.below(2) as usize]);
                log.push(format!("{res:?}"));
            }
            _ => {
                for off in [reg::CONFIG_GENERATION, reg::INTERRUPT_STATUS] {
                    let mut b = [0u8; 4];
                    let res = t.rd(off, &mut b);
                    log.push(format!("{res:?} {b:?}"));
                }
                let ack = r.below(4) as u32;
                let res = t.wr(&mut m, reg::INTERRUPT_ACK, &ack.to_le_bytes());
                log.push(format!("{res:?}"));
            }
        }
        log.push(format!("{:?}", t.counters()));
    }
    log
}

trait NetTransport {
    fn make_net(dev: NetDevice) -> Self;
    fn link(&mut self, up: bool);
    fn rd(&mut self, offset: u64, data: &mut [u8]) -> Result<(), MmioError>;
    fn wr(&mut self, m: &mut GuestMemory, offset: u64, data: &[u8]) -> Result<(), MmioError>;
    fn counters(&self) -> (TransportStats, bool);
}

macro_rules! net_transport_impl {
    ($t:ty) => {
        impl NetTransport for $t {
            fn make_net(dev: NetDevice) -> Self {
                <$t>::new(dev, 1)
            }
            fn link(&mut self, up: bool) {
                self.with_device(|d| d.set_link(up));
            }
            fn rd(&mut self, offset: u64, data: &mut [u8]) -> Result<(), MmioError> {
                self.read(offset, data)
            }
            fn wr(
                &mut self,
                m: &mut GuestMemory,
                offset: u64,
                data: &[u8],
            ) -> Result<(), MmioError> {
                self.write(m, offset, data)
            }
            fn counters(&self) -> (TransportStats, bool) {
                (self.stats(), self.interrupt_pending())
            }
        }
    };
}
net_transport_impl!(MmioTransport<NetDevice>);
net_transport_impl!(ReferenceMmioTransport<NetDevice>);

#[test]
fn config_changes_match() {
    for seed in 1..=50u64 {
        assert_eq!(
            config_trace::<MmioTransport<NetDevice>>(seed),
            config_trace::<ReferenceMmioTransport<NetDevice>>(seed),
            "seed {seed}"
        );
    }
    let log = config_trace::<MmioTransport<NetDevice>>(1).join("\n");
    assert!(
        log.contains("interrupts: 1"),
        "link changes raise config interrupts"
    );
}
