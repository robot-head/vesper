//! ABBA benchmarks of the eBPF port against its Rust build, with
//! `vesper-bench`. Both builds are in one process: `vesper_ebpf` is the port
//! (this package under `vesper-port`) and `vesper_ebpf_rust` is the frozen
//! Rust seed package. The workloads:
//!
//! - `decode`: `decode_all` and `encode` over a 4096-slot corpus of every
//!   instruction family (the codec, ported).
//! - `scalar`: the domain's transfer functions and branch refinement on a
//!   fixed pseudo-random stream of abstract values (ported).
//! - `concrete`: `alu_concrete` and `cmp_concrete` over every operation
//!   (ported).
//! - `verify`: `verify` of the SYN-flood mitigator, the largest program the
//!   tests build. The verifier itself is still Rust in this part of the port,
//!   so this measures what the ported domain and codec cost it.
//!
//! The benchmarks are ignored by default; run them with
//!
//! ```text
//! cargo test --release -p vesper-ebpf --features vesper-port \
//!     --test abba -- --ignored --nocapture --test-threads=1
//! ```
#![cfg(feature = "vesper-port")]

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

/// Operations per workload call, for the per-operation times.
fn ops(name: &str) -> u64 {
    match name {
        "decode" => 4096,
        "scalar" => 4096,
        "concrete" => 4096,
        _ => 1,
    }
}

fn words(n: usize) -> Vec<u64> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        })
        .collect()
}

macro_rules! workloads {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::*;
            use ::$krate::insn::{
                AluOp, AtomicOp, CallTarget, Endian, Imm64, Insn, JmpOp, Operand, RawInsn, Reg,
                Size, decode_all, encode,
            };
            use ::$krate::interp::{alu_concrete, cmp_concrete};
            use ::$krate::scalar::Scalar;
            use ::$krate::verifier::{refine_jmp, scalar_alu};

            const ALU: [AluOp; 13] = [
                AluOp::Add,
                AluOp::Sub,
                AluOp::Mul,
                AluOp::Div,
                AluOp::SDiv,
                AluOp::Or,
                AluOp::And,
                AluOp::Lsh,
                AluOp::Rsh,
                AluOp::Mod,
                AluOp::SMod,
                AluOp::Xor,
                AluOp::Arsh,
            ];
            const JMP: [JmpOp; 11] = [
                JmpOp::Eq,
                JmpOp::Gt,
                JmpOp::Ge,
                JmpOp::Set,
                JmpOp::Ne,
                JmpOp::SGt,
                JmpOp::SGe,
                JmpOp::Lt,
                JmpOp::Le,
                JmpOp::SLt,
                JmpOp::SLe,
            ];
            const SIZES: [Size; 4] = [Size::B, Size::H, Size::W, Size::DW];

            fn insn(w: u64) -> Insn {
                let r = |k: u64| Reg::new((k % 10) as u8).unwrap();
                let dst = r(w >> 8);
                let src = r(w >> 12);
                let off = (w >> 16) as i16;
                let imm = (w >> 32) as i32;
                let size = SIZES[(w >> 20) as usize % 4];
                match w % 9 {
                    0 => Insn::Alu {
                        wide: w & 0x80 != 0,
                        op: ALU[(w >> 24) as usize % ALU.len()],
                        dst,
                        src: if w & 0x40 != 0 {
                            Operand::Reg(src)
                        } else {
                            Operand::Imm(imm)
                        },
                    },
                    1 => Insn::Jcond {
                        wide: w & 0x80 != 0,
                        op: JMP[(w >> 24) as usize % JMP.len()],
                        dst,
                        src: Operand::Reg(src),
                        off,
                    },
                    2 => Insn::Ldx {
                        size,
                        dst,
                        src,
                        off,
                        sign_extend: false,
                    },
                    3 => Insn::Stx {
                        size,
                        dst,
                        src,
                        off,
                    },
                    4 => Insn::St {
                        size,
                        dst,
                        off,
                        imm,
                    },
                    5 => Insn::LdImm64 {
                        dst,
                        imm: Imm64::Value(w.rotate_left(17)),
                    },
                    6 => Insn::Atomic {
                        size: Size::DW,
                        op: AtomicOp::Add { fetch: w & 1 != 0 },
                        dst,
                        src,
                        off,
                    },
                    7 => Insn::Endian {
                        kind: Endian::ToBe,
                        bits: 32,
                        dst,
                    },
                    _ => Insn::Call(CallTarget::Helper(1 + (w >> 40) as i32 % 8)),
                }
            }

            pub fn corpus(w: &[u64]) -> Vec<RawInsn> {
                let mut slots = Vec::new();
                for &x in w {
                    slots.extend(encode(&insn(x)));
                    if slots.len() >= 4096 {
                        break;
                    }
                }
                slots.truncate(4096);
                // keep the last LD_IMM64, if any, whole
                if let Some(last) = slots.last()
                    && last.code == 0x18
                {
                    slots.pop();
                }
                slots
            }

            pub fn decode(w: &[u64]) -> u64 {
                let slots = corpus(w);
                let decoded = decode_all(&slots).expect("corpus decodes");
                let mut sum = 0u64;
                for i in decoded.iter().flatten() {
                    for s in encode(i) {
                        sum = sum
                            .wrapping_mul(31)
                            .wrapping_add(u64::from(s.code) ^ s.imm as u64);
                    }
                }
                black_box(sum)
            }

            fn scalar_of(x: u64, y: u64) -> Scalar {
                match x % 4 {
                    0 => Scalar::konst(y >> (x >> 58)),
                    1 => Scalar::urange(y.min(y.rotate_left(9)), y.max(y.rotate_left(9))),
                    2 => Scalar::srange((y as i64).min(-((y >> 3) as i64)), (y >> 40) as i64),
                    _ => Scalar::unknown_bits((y % 65) as u32),
                }
            }

            pub fn scalar(w: &[u64]) -> u64 {
                let mut sum = 0u64;
                for c in w.chunks(2) {
                    let (x, y) = (c[0], c.get(1).copied().unwrap_or(1));
                    let a = scalar_of(x, y);
                    let b = scalar_of(y, x);
                    let r = scalar_alu(ALU[(x >> 32) as usize % ALU.len()], x & 1 != 0, a, b);
                    sum = sum.wrapping_add(r.umin ^ r.umax ^ r.var.mask);
                    if let Some((p, q)) =
                        refine_jmp(JMP[(y >> 32) as usize % JMP.len()], y & 1 != 0, a, b, true)
                    {
                        sum = sum.wrapping_add(p.umax ^ q.smin as u64);
                    }
                    if let Some(m) = Scalar::meet(a, b) {
                        sum ^= m.var.value;
                    }
                }
                black_box(sum)
            }

            pub fn concrete(w: &[u64]) -> u64 {
                let mut sum = 0u64;
                for c in w.chunks(2) {
                    let (x, y) = (c[0], c.get(1).copied().unwrap_or(1));
                    let op = ALU[(x >> 32) as usize % ALU.len()];
                    sum = sum.wrapping_add(alu_concrete(op, x & 1 != 0, x, y));
                    let j = JMP[(y >> 32) as usize % JMP.len()];
                    sum = sum.wrapping_add(u64::from(cmp_concrete(j, y & 1 != 0, x, y)));
                }
                black_box(sum)
            }

            pub fn verify(_: &[u64]) -> u64 {
                use ::$krate::mitigator::{MitigatorConfig, build};
                let p = build(&MitigatorConfig {
                    pps_threshold: 3,
                    max_sources: 1024,
                    ring_bytes: 4096,
                })
                .expect("mitigator builds");
                let r = ::$krate::verify(&p, &::$krate::VerifierPolicy::default())
                    .expect("mitigator verifies");
                black_box(r.worst_stack as u64 ^ r.insns_processed as u64)
            }
        }
    };
}
workloads!(rust_build, vesper_ebpf_rust);
workloads!(port_build, vesper_ebpf);

type Workload = fn(&[u64]) -> u64;

struct Case(Workload);

impl BenchCase for Case {
    type Input = Vec<u64>;
    type Output = u64;
    type Error = ();
    fn setup(&mut self, _: Invocation) -> Result<Self::Input, ()> {
        Ok(words(8192))
    }
    fn run(&mut self, input: Self::Input) -> Result<Self::Output, ()> {
        Ok((self.0)(&input))
    }
}

fn workload(name: &str) -> (Workload, Workload) {
    match name {
        "decode" => (rust_build::decode, port_build::decode),
        "scalar" => (rust_build::scalar, port_build::scalar),
        "concrete" => (rust_build::concrete, port_build::concrete),
        "verify" => (rust_build::verify, port_build::verify),
        _ => unreachable!(),
    }
}

const WORKLOADS: [&str; 4] = ["decode", "scalar", "concrete", "verify"];

#[test]
fn both_builds_compute_the_same_workloads() {
    assert_eq!(vesper_ebpf::IMPLEMENTATION, "vesper");
    assert_eq!(vesper_ebpf_rust::IMPLEMENTATION, "rust");
    let w = words(8192);
    assert_eq!(rust_build::corpus(&w).len(), port_build::corpus(&w).len());
    for name in WORKLOADS {
        let (rust, port) = workload(name);
        assert_eq!(rust(&w), port(&w), "{name}");
    }
}

fn abba(name: &str) {
    let (rust, port) = workload(name);
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 4,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut Case(rust), &mut Case(port)).expect("ABBA run");
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let n = ops(name) as f64;
    let per_op = |total: u64, p: &vesper_bench::PairedSample| {
        total as f64 / p.operations_per_arm() as f64 / n
    };
    let mut rust_ns: Vec<f64> = pairs
        .iter()
        .map(|p| per_op(p.baseline_total(), p))
        .collect();
    let mut port_ns: Vec<f64> = pairs
        .iter()
        .map(|p| per_op(p.candidate_total(), p))
        .collect();
    rust_ns.sort_by(f64::total_cmp);
    port_ns.sort_by(f64::total_cmp);
    let mid = pairs.len() / 2;
    println!(
        "ABBA {name}: {} epochs x {} calls per arm ({} ops per call); median per op: \
         rust {:.1} ns, vesper {:.1} ns; Vesper/Rust median {:.2} (min {:.2}, max {:.2})",
        pairs.len(),
        pairs[0].operations_per_arm(),
        ops(name),
        rust_ns[mid],
        port_ns[mid],
        ratios[mid],
        ratios[0],
        ratios[ratios.len() - 1]
    );
    let analysis = analyze_pairs(pairs, 10_000).expect("analysis");
    println!(
        "ABBA {name}: 2x gate (max regression 10000 bp): {:?}",
        analysis.decision
    );
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_decode() {
    abba("decode");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_scalar() {
    abba("scalar");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_concrete() {
    abba("concrete");
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_verify() {
    abba("verify");
}

/// One workload of one build, for instruction counts: run it under
/// callgrind with `--toggle-collect='*<build>::<workload>*'`, where `build`
/// is `rust_build` or `port_build` (EBPF_ABBA_BUILD) and `workload` one of
/// the names above (EBPF_ABBA_WORKLOAD).
#[test]
#[ignore = "instruction counts; run under callgrind"]
fn callgrind_one_workload() {
    let name = std::env::var("EBPF_ABBA_WORKLOAD").unwrap_or_else(|_| "decode".into());
    let (rust, port) = workload(&name);
    let run = match std::env::var("EBPF_ABBA_BUILD").as_deref() {
        Ok("rust_build") => rust,
        _ => port,
    };
    let w = words(8192);
    black_box(run(&w));
}
