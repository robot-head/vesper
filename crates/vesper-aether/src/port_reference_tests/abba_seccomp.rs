//! The Vesper seccomp compiler and evaluator against the Rust references:
//! the evaluator on random syscalls under the VMM profile, the compiler on
//! random profiles, and ABBA timings of both that print the flip gate's
//! `flip-gate pairs` lines (crates/vesper-aether/flip-gate.json).

use std::hint::black_box;

use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};

use crate::seccomp::{self, Arch, DefaultAction, SeccompData, SeccompProfile, SockFilter, Syscall};

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

fn seccomp_data(seed: u64, n: usize, arch: Arch) -> Vec<SeccompData> {
    let mut r = Rng(seed);
    let ioctls = SeccompProfile::vmm_ioctls(arch);
    let known = [
        Syscall::Read,
        Syscall::Write,
        Syscall::Ioctl,
        Syscall::Mmap,
        Syscall::Futex,
    ];
    (0..n)
        .map(|_| {
            let nr = match r.below(4) {
                0 => r.below(512) as u32,
                _ => known[r.below(known.len() as u64) as usize]
                    .number(arch)
                    .unwrap_or(0),
            };
            let mut args = [0u64; 6];
            for a in &mut args {
                *a = r.next();
            }
            if r.below(2) == 0 {
                args[1] = u64::from(ioctls[r.below(ioctls.len() as u64) as usize]);
            }
            SeccompData {
                nr,
                arch: if r.below(16) == 0 {
                    r.next() as u32
                } else {
                    arch.audit()
                },
                instruction_pointer: r.next(),
                args,
            }
        })
        .collect()
}

fn seccomp_run(p: &[SockFilter], data: &[SeccompData], port: bool) -> u64 {
    let mut h = 0u64;
    for d in data {
        let r = if port {
            seccomp::evaluate(p, d)
        } else {
            seccomp::evaluate_reference(p, d)
        };
        h = h.wrapping_mul(31).wrapping_add(u64::from(r.unwrap_or(1)));
    }
    h
}

#[test]
fn evaluate_differential() {
    for arch in [Arch::X86_64, Arch::Aarch64] {
        for default in [DefaultAction::KillProcess, DefaultAction::Errno(1)] {
            let p = SeccompProfile::vmm(arch, default).compile().unwrap();
            for d in seccomp_data(67, 20_000, arch) {
                assert_eq!(
                    seccomp::evaluate(&p, &d),
                    seccomp::evaluate_reference(&p, &d)
                );
            }
        }
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_seccomp_evaluate() {
    let arch = Arch::host().unwrap_or(Arch::X86_64);
    let p = SeccompProfile::vmm(arch, DefaultAction::KillProcess)
        .compile()
        .unwrap();
    let data = seccomp_data(71, 512, arch);
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
        &mut Arm(|| seccomp_run(black_box(&p), black_box(&data), false)),
        &mut Arm(|| seccomp_run(black_box(&p), black_box(&data), true)),
    )
    .expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs seccomp_evaluate {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "seccomp_evaluate: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}

/// Random profiles: syscall subsets with repeats (and sometimes `ioctl`,
/// which is refused), ioctl request lists from empty to past the 255-insn
/// jump reach, both architectures and both default actions.
#[test]
fn seccomp_compile_random_differential() {
    use crate::seccomp::Syscall::*;
    let calls = [
        Read,
        Write,
        Readv,
        Writev,
        Pread64,
        Pwrite64,
        Close,
        Ioctl,
        Ppoll,
        EpollPwait,
        Eventfd2,
        Futex,
        Mmap,
        Munmap,
        Mprotect,
        Madvise,
        Brk,
        RtSigreturn,
        RtSigprocmask,
        Sigaltstack,
        Exit,
        ExitGroup,
        ClockGettime,
        SchedYield,
        Getrandom,
        Gettid,
        Tgkill,
        Openat,
        Socket,
        Execve,
        Getpid,
        Ptrace,
    ];
    let mut r = Rng(97);
    let mut ok = 0;
    for _ in 0..4_000 {
        let arch = if r.below(2) == 0 {
            Arch::X86_64
        } else {
            Arch::Aarch64
        };
        let default = if r.below(2) == 0 {
            DefaultAction::KillProcess
        } else {
            DefaultAction::Errno(r.below(200) as u16)
        };
        let n = r.below(40) as usize;
        let mut allowed = Vec::with_capacity(n);
        for _ in 0..n {
            let c = calls[r.below(calls.len() as u64) as usize];
            if c != Ioctl || r.below(20) == 0 {
                allowed.push(c);
            }
        }
        let m = match r.below(10) {
            0 => 0,
            1 => 200 + r.below(120) as usize,
            _ => r.below(30) as usize,
        };
        let ioctl_requests = (0..m)
            .map(|_| match r.below(3) {
                0 => r.below(8) as u32,
                _ => r.next() as u32,
            })
            .collect();
        let p = SeccompProfile {
            arch,
            allowed,
            ioctl_requests,
            default,
        };
        let a = p.compile();
        assert_eq!(a, p.compile_reference(), "{p:?}");
        ok += usize::from(a.is_ok());
    }
    assert!(ok > 1_000, "{ok} profiles compiled");
}

/// Prints the gate's pairs line and the median interval of an ABBA run.
fn report(name: &str, mut rust: impl FnMut() -> u64, mut vesper: impl FnMut() -> u64) {
    let run = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let report = run_abba(run, &mut Arm(&mut rust), &mut Arm(&mut vesper)).expect("ABBA run");
    let pairs = report.pairs();
    let line: Vec<String> = pairs
        .iter()
        .map(|p| format!("{}:{}", p.baseline_total(), p.candidate_total()))
        .collect();
    println!("flip-gate pairs {name} {}", line.join(" "));
    let analysis = analyze_pairs(pairs, 5_000).expect("analysis");
    if let Some(i) = analysis.interval {
        println!(
            "{name}: 95% median interval of Vesper/Rust [{:.3}, {:.3}], {:?}",
            i.lower.candidate_total() as f64 / i.lower.baseline_total() as f64,
            i.upper.candidate_total() as f64 / i.upper.baseline_total() as f64,
            analysis.decision
        );
    }
}

#[test]
#[ignore = "benchmark; run in release with --ignored --nocapture"]
fn abba_seccomp_compile() {
    let arch = Arch::host().unwrap_or(Arch::X86_64);
    let profile = SeccompProfile::vmm(arch, DefaultAction::KillProcess);
    report(
        "seccomp_compile",
        || profile.compile_reference().unwrap().len() as u64,
        || profile.compile().unwrap().len() as u64,
    );
}
