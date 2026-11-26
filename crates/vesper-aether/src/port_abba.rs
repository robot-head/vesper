//! Randomized differentials and ABBA timings of the aether ports that are
//! not yet Vesper-only (SH6 lane 14, #1409): the vsock header codec,
//! vmgenid, guest memory, the fork wire and seccomp. Each workload runs the
//! Vesper port and the Rust reference on the same inputs.
//!
//! ```text
//! RUSTFLAGS="-C target-cpu=native" cargo test --release -p vesper-aether \
//!     --features vesper-port --lib port_abba -- --ignored --nocapture
//! ```

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

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
