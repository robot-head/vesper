//! Randomized differentials and ABBA timings of the aether ports that are
//! not yet Vesper-only (SH6 lane 14, #1409): virtio-mmio status, blk, net,
//! vsock, vmgenid, guest memory, the fork wire and seccomp. Each workload
//! runs the Vesper port and the Rust reference on the same inputs.
//!
//! ```text
//! RUSTFLAGS="-C target-cpu=native" cargo test --release -p vesper-aether \
//!     --features vesper-port --lib port_abba -- --ignored --nocapture
//! ```

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::devices::block::{BlockDevice, SECTOR_BYTES};
use crate::devices::mmio::{MmioTransport, VIRTIO_F_VERSION_1, status};
use crate::devices::net::NetDevice;
use crate::devices::vmgenid::VmGenIdDevice;
use crate::devices::vsock::VsockHeader;
use crate::fork::{VmGenId, VmGenIdWire};
use crate::guest_mem::GuestMemory;
use crate::seccomp::{self, Arch, DefaultAction, SeccompProfile};

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

/// Times `vesper` against `rust` and prints the gate line.
fn gate(name: &str, rust: impl FnMut() -> u64, vesper: impl FnMut() -> u64) {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut Arm(rust), &mut Arm(vesper)).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    let (lo, hi) = analysis.interval.map_or((f64::NAN, f64::NAN), |i| {
        (
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
        )
    });
    println!(
        "ABBA vesper/rust {name} {:.2}x [{lo:.2}x, {hi:.2}x] n={} gate {:?}",
        ratios[pairs.len() / 2],
        pairs.len(),
        analysis.decision
    );
}

// ---- virtio-mmio status --------------------------------------------------

fn transport() -> MmioTransport<BlockDevice> {
    let dev = BlockDevice::new(vec![0u8; 16 * SECTOR_BYTES as usize], false, "abba").unwrap();
    MmioTransport::new(dev, 0)
}

/// Status writes: a random walk over status values and driver features.
fn mmio_steps(seed: u64, n: usize) -> Vec<(u64, u32)> {
    let mut r = Rng(seed);
    let bits = [
        status::ACKNOWLEDGE,
        status::DRIVER,
        status::FEATURES_OK,
        status::DRIVER_OK,
        status::FAILED,
        status::DEVICE_NEEDS_RESET,
    ];
    (0..n)
        .map(|_| {
            let features = match r.below(4) {
                0 => r.next(),
                1 => 0,
                _ => VIRTIO_F_VERSION_1 | (r.next() & 0xff),
            };
            let mut s = 0;
            for b in bits {
                if r.below(2) == 0 {
                    s |= b;
                }
            }
            if r.below(8) == 0 {
                s = 0;
            }
            (features, s)
        })
        .collect()
}

fn mmio_run(t: &mut MmioTransport<BlockDevice>, steps: &[(u64, u32)], port: bool) -> u64 {
    let mut h = 0u64;
    for &(features, s) in steps {
        t.set_driver_features_for_test(features);
        let r = if port {
            t.write_status(s)
        } else {
            t.write_status_reference(s)
        };
        h = h
            .wrapping_mul(31)
            .wrapping_add(u64::from(r.is_ok()))
            .wrapping_add(u64::from(t.status()))
            .wrapping_add(t.negotiated_features());
        if r.is_err() {
            t.write_status_reference(0).unwrap();
        }
    }
    h ^ t.stats().rejected_feature_sets
}

#[test]
fn mmio_status_differential() {
    let steps = mmio_steps(7, 200_000);
    let (mut a, mut b) = (transport(), transport());
    for chunk in steps.chunks(64) {
        assert_eq!(
            mmio_run(&mut a, chunk, true),
            mmio_run(&mut b, chunk, false)
        );
        assert_eq!(a.stats(), b.stats());
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_mmio_status() {
    let steps = mmio_steps(11, 4_096);
    let (mut a, mut b) = (transport(), transport());
    gate(
        "mmio_status",
        || mmio_run(&mut b, black_box(&steps), false),
        || mmio_run(&mut a, black_box(&steps), true),
    );
}

// ---- virtio-blk ----------------------------------------------------------

const DISK_SECTORS: u64 = 4096;

fn disk() -> BlockDevice {
    BlockDevice::new(
        vec![0u8; (DISK_SECTORS * SECTOR_BYTES) as usize],
        false,
        "abba",
    )
    .unwrap()
}

/// Random (sector, len) requests, mostly in range.
fn spans(seed: u64, n: usize) -> Vec<(u64, u64)> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let sector = match r.below(16) {
                0 => r.next(),
                _ => r.below(DISK_SECTORS + 8),
            };
            let len = match r.below(16) {
                0 => r.next(),
                1 => r.below(4096),
                _ => SECTOR_BYTES * (1 + r.below(16)),
            };
            (sector, len)
        })
        .collect()
}

fn span_run(d: &BlockDevice, s: &[(u64, u64)], port: bool) -> u64 {
    let mut h = 0u64;
    for &(sector, len) in s {
        let r = if port {
            d.span(sector, len)
        } else {
            d.span_reference(sector, len)
        };
        h = h.wrapping_mul(31).wrapping_add(match r {
            Some(r) => r.start as u64 ^ (r.end as u64) << 1,
            None => 7,
        });
    }
    h
}

/// Discard / write-zeroes payloads of 16-byte segments.
fn segments(seed: u64, n: usize) -> Vec<(Vec<u8>, bool)> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let count = match r.below(8) {
                0 => r.below(4) as usize,
                _ => 1,
            };
            let mut p = Vec::new();
            for _ in 0..count {
                let sector = r.below(DISK_SECTORS + 4);
                let num = match r.below(8) {
                    0 => r.next() as u32,
                    _ => 1 + r.below(8) as u32,
                };
                let flags = if r.below(8) == 0 { r.next() as u32 } else { 0 };
                p.extend_from_slice(&sector.to_le_bytes());
                p.extend_from_slice(&num.to_le_bytes());
                p.extend_from_slice(&flags.to_le_bytes());
            }
            if r.below(16) == 0 {
                p.push(0);
            }
            (p, r.below(2) == 0)
        })
        .collect()
}

fn discard_run(d: &mut BlockDevice, s: &[(Vec<u8>, bool)], port: bool) -> u64 {
    let mut h = 0u64;
    for (p, discard) in s {
        let st = if port {
            d.discard_or_zero(p, 1, *discard)
        } else {
            d.discard_or_zero_reference(p, 1, *discard)
        };
        h = h.wrapping_mul(31).wrapping_add(u64::from(st));
    }
    h
}

#[test]
fn blk_differential() {
    let (mut a, mut b) = (disk(), disk());
    let s = spans(3, 200_000);
    assert_eq!(span_run(&a, &s, true), span_run(&b, &s, false));
    let seg = segments(5, 50_000);
    assert_eq!(
        discard_run(&mut a, &seg, true),
        discard_run(&mut b, &seg, false)
    );
    assert_eq!(a.stats(), b.stats());
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_blk() {
    let s = spans(13, 4_096);
    let seg = segments(17, 1_024);
    let (mut a, mut b) = (disk(), disk());
    gate(
        "blk",
        || span_run(&b, black_box(&s), false) ^ discard_run(&mut b, black_box(&seg), false),
        || span_run(&a, black_box(&s), true) ^ discard_run(&mut a, black_box(&seg), true),
    );
}

// ---- virtio-net ----------------------------------------------------------

fn net() -> NetDevice {
    NetDevice::new([2, 0, 0, 0, 0, 1], 1500).unwrap()
}

/// TX frames (header + ethernet frame) and control commands.
fn net_inputs(seed: u64, n: usize) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut r = Rng(seed);
    let tx = (0..n)
        .map(|_| {
            let len = match r.below(8) {
                0 => r.below(40) as usize,
                1 => 1500 + r.below(64) as usize,
                _ => 12 + 14 + r.below(1500) as usize,
            };
            let mut f = vec![0u8; len];
            if len > 1 && r.below(8) == 0 {
                f[r.below(2) as usize] = r.next() as u8;
            }
            f
        })
        .collect();
    let ctrl = (0..n)
        .map(|_| {
            let len = if r.below(8) == 0 {
                r.below(8) as usize
            } else {
                4
            };
            let mut c: Vec<u8> = (0..len).map(|_| r.next() as u8).collect();
            if len == 4 && r.below(2) == 0 {
                c[0] = 4; // VIRTIO_NET_CTRL_MQ
                c[1] = 0; // VQ_PAIRS_SET
                c[2] = r.below(10) as u8;
                c[3] = if r.below(8) == 0 { 1 } else { 0 };
            }
            c
        })
        .collect();
    (tx, ctrl)
}

fn net_run(d: &mut NetDevice, tx: &[Vec<u8>], ctrl: &[Vec<u8>], port: bool) -> u64 {
    let mut h = 0u64;
    let max_frame = d.max_frame() as u64;
    let link = true;
    for f in tx {
        let ok = if port {
            crate::binding::net_validate_tx(f, max_frame, link)
        } else {
            d.transmit_valid_reference(f)
        };
        h = h.wrapping_mul(31).wrapping_add(u64::from(ok));
    }
    for c in ctrl {
        let ack = if port {
            d.control(c)
        } else {
            d.control_reference(c)
        };
        h = h.wrapping_mul(31).wrapping_add(u64::from(ack));
    }
    h
}

#[test]
fn net_differential() {
    let (tx, ctrl) = net_inputs(19, 50_000);
    for features in [0, u64::MAX] {
        let (mut a, mut b) = (net(), net());
        crate::devices::mmio::VirtioDevice::activate(&mut a, features);
        crate::devices::mmio::VirtioDevice::activate(&mut b, features);
        assert_eq!(
            net_run(&mut a, &tx, &ctrl, true),
            net_run(&mut b, &tx, &ctrl, false)
        );
        assert_eq!(a.stats(), b.stats());
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_net() {
    let (tx, ctrl) = net_inputs(23, 2_048);
    let (mut a, mut b) = (net(), net());
    crate::devices::mmio::VirtioDevice::activate(&mut a, u64::MAX);
    crate::devices::mmio::VirtioDevice::activate(&mut b, u64::MAX);
    gate(
        "net",
        || net_run(&mut b, black_box(&tx), black_box(&ctrl), false),
        || net_run(&mut a, black_box(&tx), black_box(&ctrl), true),
    );
}

// ---- vsock header codec --------------------------------------------------

fn vsock_inputs(seed: u64, n: usize) -> Vec<Vec<u8>> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let len = if r.below(8) == 0 {
                r.below(64) as usize
            } else {
                44 + r.below(16) as usize
            };
            (0..len).map(|_| r.next() as u8).collect()
        })
        .collect()
}

fn vsock_run(inputs: &[Vec<u8>], port: bool) -> u64 {
    let mut h = 0u64;
    for b in inputs {
        let d = if port {
            VsockHeader::decode(b)
        } else {
            VsockHeader::decode_reference(b)
        };
        if let Some(d) = d {
            let e = if port {
                d.encode()
            } else {
                d.encode_reference()
            };
            h = h
                .wrapping_mul(31)
                .wrapping_add(u64::from_le_bytes(e[24..32].try_into().unwrap()));
            h ^= u64::from(e[43]);
        } else {
            h = h.wrapping_mul(31).wrapping_add(1);
        }
    }
    h
}

#[test]
fn vsock_differential() {
    let inputs = vsock_inputs(29, 100_000);
    assert_eq!(vsock_run(&inputs, true), vsock_run(&inputs, false));
    for b in &inputs {
        assert_eq!(VsockHeader::decode(b), VsockHeader::decode_reference(b));
    }
    let mut r = Rng(31);
    for _ in 0..100_000 {
        let (a, t, f) = (r.next() as u32, r.next() as u32, r.next() as u32);
        let (t, f) = if r.below(2) == 0 {
            (t, t.wrapping_sub(r.below(1 << 20) as u32))
        } else {
            (t, f)
        };
        assert_eq!(
            crate::binding::vsock_peer_credit(a, t, f),
            a.saturating_sub(t.wrapping_sub(f))
        );
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_vsock() {
    let inputs = vsock_inputs(37, 4_096);
    gate(
        "vsock",
        || vsock_run(black_box(&inputs), false),
        || vsock_run(black_box(&inputs), true),
    );
}

// ---- vmgenid and the fork wire -------------------------------------------

fn wire_inputs(seed: u64, n: usize) -> Vec<Vec<u8>> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let len = if r.below(8) == 0 {
                r.below(40) as usize
            } else {
                24
            };
            (0..len).map(|_| r.next() as u8).collect()
        })
        .collect()
}

fn genid_run(inputs: &[Vec<u8>], port: bool) -> u64 {
    let mut h = 0u64;
    for b in inputs {
        let w = if port {
            VmGenIdWire::decode(b)
        } else {
            VmGenIdWire::decode_reference(b)
        };
        match w {
            Ok(w) => {
                let e = if port {
                    w.encode()
                } else {
                    w.encode_reference()
                };
                let id = VmGenId {
                    lo: w.vm_gen_lo,
                    hi: w.vm_gen_hi,
                };
                let g = if port {
                    VmGenIdDevice::encode(id)
                } else {
                    VmGenIdDevice::encode_reference(id)
                };
                let valid = if port {
                    crate::binding::vmgenid_valid_addr(w.vm_gen_lo & 0xffff_ffff)
                } else {
                    VmGenIdDevice::valid_addr_reference(w.vm_gen_lo & 0xffff_ffff)
                };
                h = h
                    .wrapping_mul(31)
                    .wrapping_add(u64::from(e[17]) ^ u64::from(g[9]) ^ u64::from(valid));
            }
            Err(_) => h = h.wrapping_mul(31).wrapping_add(3),
        }
    }
    h
}

#[test]
fn vmgenid_and_wire_differential() {
    let inputs = wire_inputs(41, 100_000);
    assert_eq!(genid_run(&inputs, true), genid_run(&inputs, false));
    let mut r = Rng(43);
    for _ in 0..100_000 {
        let gpa = match r.below(3) {
            0 => r.next(),
            1 => r.below(1 << 20) * 8,
            _ => r.next() & !7,
        };
        assert_eq!(
            crate::binding::vmgenid_valid_addr(gpa),
            VmGenIdDevice::valid_addr_reference(gpa),
            "gpa {gpa:#x}"
        );
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_vmgenid_wire() {
    let inputs = wire_inputs(47, 4_096);
    gate(
        "vmgenid_wire",
        || genid_run(black_box(&inputs), false),
        || genid_run(black_box(&inputs), true),
    );
}

// ---- guest memory --------------------------------------------------------

fn layout() -> Vec<(u64, u64)> {
    vec![(0, 1 << 20), (1 << 24, 1 << 20), (1 << 32, 1 << 20)]
}

fn locates(seed: u64, n: usize) -> Vec<(u64, u64)> {
    let mut r = Rng(seed);
    let bases = [0u64, 1 << 24, 1 << 32];
    (0..n)
        .map(|_| {
            let gpa = match r.below(16) {
                0 => r.next(),
                _ => bases[r.below(3) as usize] + r.below((1 << 20) + 64),
            };
            let len = match r.below(16) {
                0 => r.next(),
                _ => r.below(4096),
            };
            (gpa, len)
        })
        .collect()
}

fn locate_run(m: &GuestMemory, q: &[(u64, u64)], port: bool) -> u64 {
    let mut h = 0u64;
    for &(gpa, len) in q {
        let r = if port {
            m.locate(gpa, len)
        } else {
            m.locate_reference(gpa, len)
        };
        h = h.wrapping_mul(31).wrapping_add(r.map_or(5, |o| o as u64));
    }
    h
}

#[test]
fn guest_mem_differential() {
    let m = GuestMemory::new(&layout()).unwrap();
    let q = locates(53, 200_000);
    assert_eq!(locate_run(&m, &q, true), locate_run(&m, &q, false));
    for &(gpa, len) in &q[..5_000] {
        assert_eq!(m.locate(gpa, len), m.locate_reference(gpa, len));
    }
    let mut r = Rng(59);
    for _ in 0..2_000 {
        let n = r.below(5) as usize;
        let l: Vec<(u64, u64)> = (0..n)
            .map(|_| (r.below(1 << 16) * 4096, r.below(4) * 4096 + r.below(2)))
            .collect();
        let (a, b) = (GuestMemory::new(&l), GuestMemory::new_reference(&l));
        assert_eq!(a.as_ref().err(), b.as_ref().err(), "{l:?}");
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_guest_mem_locate() {
    let m = GuestMemory::new(&layout()).unwrap();
    let q = locates(61, 8_192);
    gate(
        "guest_mem_locate",
        || locate_run(&m, black_box(&q), false),
        || locate_run(&m, black_box(&q), true),
    );
}

// ---- seccomp compile (the evaluator is Vesper-only:
// port_reference_tests::abba_seccomp) --------------------------------------

#[test]
fn seccomp_compile_differential() {
    for arch in [Arch::X86_64, Arch::Aarch64] {
        for default in [DefaultAction::KillProcess, DefaultAction::Errno(1)] {
            let profile = SeccompProfile::vmm(arch, default);
            let p = profile.compile().unwrap();
            assert_eq!(p, profile.compile_reference().unwrap());
            assert_eq!(seccomp::validate(&p), seccomp::validate_reference(&p));
        }
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn port_abba_seccomp_compile() {
    let arch = Arch::host().unwrap_or(Arch::X86_64);
    let profile = SeccompProfile::vmm(arch, DefaultAction::KillProcess);
    gate(
        "seccomp_compile",
        || profile.compile_reference().unwrap().len() as u64,
        || profile.compile().unwrap().len() as u64,
    );
}
