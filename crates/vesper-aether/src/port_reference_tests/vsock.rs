//! The Vesper vsock device (`vesper/src/vsock.ves`) against the Rust
//! reference device (`src/devices/vsock/reference.rs`): a randomized
//! differential over guest packets, host socket calls, receive-queue steps
//! and resets, and an ABBA timing of stream traffic that prints the flip
//! gate's `flip-gate pairs` line (crates/vesper-aether/flip-gate.toml).

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::devices::mmio::VirtioDevice;
use crate::devices::vsock::ReferenceVsockDevice;
use crate::devices::vsock::{
    ConnKey, ConnState, HEADER_BYTES, HOST_CID, OP_CREDIT_REQUEST, OP_CREDIT_UPDATE, OP_REQUEST,
    OP_RESPONSE, OP_RST, OP_RW, OP_SHUTDOWN, SEQ_EOM, SeqMessage, TYPE_SEQPACKET, TYPE_STREAM,
    VIRTIO_VSOCK_F_SEQPACKET, VsockDevice, VsockError, VsockHeader, VsockStats,
};

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

/// The device surface both implementations share.
trait Dev: VirtioDevice {
    fn make(cid: u64) -> Self;
    fn listen(&mut self, port: u32) -> Result<(), VsockError>;
    fn listen_seqpacket(&mut self, port: u32) -> Result<(), VsockError>;
    fn connect(&mut self, host: u32, guest: u32) -> Result<ConnKey, VsockError>;
    fn connect_seqpacket(&mut self, host: u32, guest: u32) -> Result<ConnKey, VsockError>;
    fn recv(&mut self, key: ConnKey) -> Result<Vec<u8>, VsockError>;
    fn recv_message(&mut self, key: ConnKey) -> Result<Option<SeqMessage>, VsockError>;
    fn send(&mut self, key: ConnKey, data: &[u8]) -> Result<usize, VsockError>;
    fn send_message(&mut self, key: ConnKey, data: &[u8], eor: bool) -> Result<(), VsockError>;
    fn shutdown(&mut self, key: ConnKey) -> Result<(), VsockError>;
    fn transport_reset(&mut self);
    fn guest(&mut self, bytes: &[u8]);
    fn rx_step(&mut self, room: usize, out: &mut Vec<u8>) -> bool;
    fn stats(&self) -> VsockStats;
    fn connections(&self) -> Vec<ConnKey>;
    fn connection_state(&self, key: ConnKey) -> Option<ConnState>;
    fn outbox_len(&self) -> usize;
}

macro_rules! dev_impl {
    ($t:ty) => {
        impl Dev for $t {
            fn make(cid: u64) -> Self {
                <$t>::new(cid).expect("valid CID")
            }
            fn listen(&mut self, port: u32) -> Result<(), VsockError> {
                <$t>::listen(self, port)
            }
            fn listen_seqpacket(&mut self, port: u32) -> Result<(), VsockError> {
                <$t>::listen_seqpacket(self, port)
            }
            fn connect(&mut self, host: u32, guest: u32) -> Result<ConnKey, VsockError> {
                <$t>::connect(self, host, guest)
            }
            fn connect_seqpacket(&mut self, host: u32, guest: u32) -> Result<ConnKey, VsockError> {
                <$t>::connect_seqpacket(self, host, guest)
            }
            fn recv(&mut self, key: ConnKey) -> Result<Vec<u8>, VsockError> {
                <$t>::recv(self, key)
            }
            fn recv_message(&mut self, key: ConnKey) -> Result<Option<SeqMessage>, VsockError> {
                <$t>::recv_message(self, key)
            }
            fn send(&mut self, key: ConnKey, data: &[u8]) -> Result<usize, VsockError> {
                <$t>::send(self, key, data)
            }
            fn send_message(
                &mut self,
                key: ConnKey,
                data: &[u8],
                eor: bool,
            ) -> Result<(), VsockError> {
                <$t>::send_message(self, key, data, eor)
            }
            fn shutdown(&mut self, key: ConnKey) -> Result<(), VsockError> {
                <$t>::shutdown(self, key)
            }
            fn transport_reset(&mut self) {
                <$t>::transport_reset(self)
            }
            fn guest(&mut self, bytes: &[u8]) {
                <$t>::deliver_tx_for_port_test(self, bytes).expect("never fails");
            }
            fn rx_step(&mut self, room: usize, out: &mut Vec<u8>) -> bool {
                <$t>::rx_step_for_test(self, room, out)
            }
            fn stats(&self) -> VsockStats {
                <$t>::stats(self)
            }
            fn connections(&self) -> Vec<ConnKey> {
                <$t>::connections(self)
            }
            fn connection_state(&self, key: ConnKey) -> Option<ConnState> {
                <$t>::connection_state(self, key)
            }
            fn outbox_len(&self) -> usize {
                <$t>::outbox_len(self)
            }
        }
    };
}
dev_impl!(VsockDevice);
dev_impl!(ReferenceVsockDevice);

const CID: u64 = 42;

#[allow(clippy::too_many_arguments)]
fn packet(
    src_port: u32,
    dst_port: u32,
    kind: u16,
    op: u16,
    flags: u32,
    buf_alloc: u32,
    fwd_cnt: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut v = VsockHeader {
        src_cid: CID,
        dst_cid: HOST_CID,
        src_port,
        dst_port,
        len: payload.len() as u32,
        kind,
        op,
        flags,
        buf_alloc,
        fwd_cnt,
    }
    .encode_reference()
    .to_vec();
    v.extend_from_slice(payload);
    v
}

/// One random event against `d`, logged into `log`.
fn step<D: Dev>(d: &mut D, r: &mut Rng, data: &[u8], log: &mut Vec<String>) {
    let port = |r: &mut Rng| 5000 + r.below(6) as u32;
    let open = d.connections();
    let key = |r: &mut Rng| {
        if !open.is_empty() && r.below(4) != 0 {
            open[r.below(open.len() as u64) as usize]
        } else {
            (1000 + r.below(6) as u32, 5000 + r.below(6) as u32)
        }
    };
    match r.below(100) {
        0..=39 => {
            let (g, h) = key(r);
            let open = d.connection_state((g, h)).is_some();
            let natural = if h < 5003 {
                TYPE_STREAM
            } else {
                TYPE_SEQPACKET
            };
            let kind = match r.below(20) {
                0 => TYPE_STREAM,
                1 => TYPE_SEQPACKET,
                2 => 3,
                _ => natural,
            };
            let op = match (open, r.below(40)) {
                (_, 0..=2) => 1 + r.below(9) as u16,
                (false, _) => OP_REQUEST,
                (true, 3) => OP_SHUTDOWN,
                (true, 4) => OP_RST,
                (true, 5 | 6) => OP_CREDIT_REQUEST,
                (true, 7) => OP_RESPONSE,
                (true, _) => OP_RW,
            };
            let len = match r.below(16) {
                0 | 1 => 0,
                2 => r.below(65_536 + 64) as usize,
                3 | 4 => r.below(8192) as usize,
                _ => r.below(600) as usize,
            };
            let start = r.below((data.len() - len) as u64) as usize;
            let buf_alloc = match r.below(8) {
                0 => 0,
                1 => 4096,
                2 => r.next() as u32,
                _ => 1 << 20,
            };
            let fwd = if r.below(4) == 0 {
                r.below(1 << 12) as u32
            } else {
                0
            };
            let flags = match op {
                OP_SHUTDOWN => 1 + r.below(3) as u32,
                _ => r.below(4) as u32,
            };
            let mut p = packet(
                g,
                h,
                kind,
                op,
                flags,
                buf_alloc,
                fwd,
                &data[start..start + len],
            );
            match r.below(60) {
                0 => p.truncate(r.below(44) as usize),
                1 => p[0] ^= 1,
                2 => p[8] ^= 1,
                3 => p[24] ^= 1,
                _ => {}
            }
            d.guest(&p);
            log.push("guest".into());
        }
        40..=47 => {
            let mut out = Vec::new();
            let room =
                HEADER_BYTES + [0, 1, 100, 4096, r.below(5000) as usize][r.below(5) as usize];
            let mut n = 0;
            while n < 8 && d.rx_step(room, &mut out) {
                log.push(format!("rx {out:?}"));
                n += 1;
            }
        }
        48..=53 => {
            let k = key(r);
            log.push(format!("recv {:?}", d.recv(k)));
        }
        54..=58 => {
            let k = key(r);
            log.push(format!("recv_message {:?}", d.recv_message(k)));
        }
        59..=65 => {
            let k = key(r);
            let len = r.below(12_000) as usize;
            let start = r.below((data.len() - len) as u64) as usize;
            log.push(format!("send {:?}", d.send(k, &data[start..start + len])));
        }
        66..=71 => {
            let k = key(r);
            let len = [0, r.below(9000) as usize][r.below(2) as usize];
            let start = r.below((data.len() - len) as u64) as usize;
            let eor = r.below(2) == 0;
            log.push(format!(
                "send_message {:?}",
                d.send_message(k, &data[start..start + len], eor)
            ));
        }
        72..=77 => {
            let (g, h) = (1000 + r.below(6) as u32, 5000 + r.below(6) as u32);
            let res = if r.below(2) == 0 {
                d.connect(h, g)
            } else {
                d.connect_seqpacket(h, g)
            };
            log.push(format!("connect {res:?}"));
            if res.is_ok() && r.below(4) != 0 {
                let kind = if r.below(2) == 0 {
                    TYPE_STREAM
                } else {
                    TYPE_SEQPACKET
                };
                d.guest(&packet(g, h, kind, OP_RESPONSE, 0, 1 << 20, 0, &[]));
            }
        }
        78..=81 => {
            let k = key(r);
            log.push(format!("shutdown {:?}", d.shutdown(k)));
        }
        82..=85 => {
            let p = port(r);
            let res = if r.below(2) == 0 {
                d.listen(p)
            } else {
                d.listen_seqpacket(p)
            };
            log.push(format!("listen {res:?}"));
        }
        86..=89 => {
            // The guest acknowledges everything the host sent so far.
            for (g, h) in d.connections() {
                let kind = if h < 5003 {
                    TYPE_STREAM
                } else {
                    TYPE_SEQPACKET
                };
                d.guest(&packet(
                    g,
                    h,
                    kind,
                    OP_CREDIT_UPDATE,
                    0,
                    1 << 20,
                    [0, r.below(1 << 16) as u32][r.below(2) as usize],
                    &[],
                ));
            }
            log.push("credit".into());
        }
        90 => {
            d.transport_reset();
            log.push("transport_reset".into());
        }
        91 => {
            d.reset();
            let f = if r.below(2) == 0 {
                VIRTIO_VSOCK_F_SEQPACKET
            } else {
                0
            };
            d.activate(f);
            log.push("reset".into());
        }
        _ => {
            // A seqpacket message in several packets.
            let (g, h) = key(r);
            let parts = 1 + r.below(4);
            for i in 0..parts {
                let len = r.below(300) as usize;
                let start = r.below((data.len() - len) as u64) as usize;
                let flags = if i + 1 == parts {
                    SEQ_EOM | (r.below(2) as u32 * 2)
                } else {
                    0
                };
                d.guest(&packet(
                    g,
                    h,
                    TYPE_SEQPACKET,
                    OP_RW,
                    flags,
                    65536,
                    0,
                    &data[start..start + len],
                ));
            }
            log.push("message".into());
        }
    }
    let keys = d.connections();
    let states: Vec<_> = keys.iter().map(|&k| d.connection_state(k)).collect();
    log.push(format!(
        "{:?} {keys:?} {states:?} {}",
        d.stats(),
        d.outbox_len()
    ));
}

fn trace<D: Dev>(seed: u64, steps: usize, data: &[u8]) -> Vec<String> {
    let mut r = Rng(seed);
    let mut d = D::make(CID);
    d.activate(if r.below(4) == 0 {
        0
    } else {
        VIRTIO_VSOCK_F_SEQPACKET
    });
    for p in 5000..5003 {
        d.listen(p).expect("free port");
    }
    for p in 5003..5006 {
        d.listen_seqpacket(p).expect("free port");
    }
    let mut log = Vec::new();
    for _ in 0..steps {
        step(&mut d, &mut r, data, &mut log);
    }
    log
}

fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng(seed);
    (0..n).map(|_| r.next() as u8).collect()
}

#[test]
fn vsock_device_differential() {
    let data = bytes(5, 1 << 17);
    for seed in 1..=300u64 {
        let a = trace::<VsockDevice>(seed, 400, &data);
        let b = trace::<ReferenceVsockDevice>(seed, 400, &data);
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert_eq!(x, y, "seed {seed} entry {i}");
        }
        assert_eq!(a.len(), b.len());
    }
}

/// Stream traffic: 64 guest connections, each sending 512-byte packets that
/// the host reads back (with a credit update), host replies of 1 KiB, and
/// the receive queue drained into 4 KiB buffers.
fn traffic<D: Dev>(data: &[u8], rounds: usize) -> u64 {
    let mut d = D::make(CID);
    d.activate(VIRTIO_VSOCK_F_SEQPACKET);
    d.listen(5000).expect("free port");
    let mut out = Vec::with_capacity(HEADER_BYTES + 4096);
    let mut h = 0u64;
    let mut drain = |d: &mut D, h: &mut u64| {
        while d.rx_step(HEADER_BYTES + 4096, &mut out) {
            *h = h.wrapping_mul(31).wrapping_add(out.len() as u64);
        }
    };
    let conns = 64u32;
    for g in 0..conns {
        d.guest(&packet(
            2000 + g,
            5000,
            TYPE_STREAM,
            OP_REQUEST,
            0,
            65536,
            0,
            &[],
        ));
    }
    drain(&mut d, &mut h);
    let packets: Vec<Vec<u8>> = (0..conns)
        .map(|g| {
            packet(
                2000 + g,
                5000,
                TYPE_STREAM,
                OP_RW,
                0,
                1 << 30,
                0,
                &data[g as usize..g as usize + 512],
            )
        })
        .collect();
    for round in 0..rounds {
        for g in 0..conns {
            d.guest(&packets[g as usize]);
            if (round + g as usize).is_multiple_of(4) {
                let key = (2000 + g, 5000);
                let got = d.recv(key).map_or(0, |v| v.len());
                let sent = d.send(key, &data[..1024]).unwrap_or(0);
                h = h.wrapping_add((got + sent) as u64);
            }
        }
        drain(&mut d, &mut h);
    }
    h
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
fn vsock_traffic_matches() {
    let data = bytes(9, 4096);
    assert_eq!(
        traffic::<VsockDevice>(&data, 20),
        traffic::<ReferenceVsockDevice>(&data, 20)
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_vsock_device() {
    let data = bytes(9, 4096);
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
        &mut Arm(|| traffic::<ReferenceVsockDevice>(black_box(&data), 16)),
        &mut Arm(|| traffic::<VsockDevice>(black_box(&data), 16)),
    )
    .expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs vsock_device {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "vsock_device: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}
