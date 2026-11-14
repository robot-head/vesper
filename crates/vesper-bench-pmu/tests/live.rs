//! Conditional live exercise, separate from ABI fixtures. An unavailable result
//! is explicitly logged and is NOT positive hardware-counter coverage.
use std::convert::Infallible;
use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba_observed};
use vesper_bench_pmu::PerfGroup;
struct Work;
impl BenchCase for Work {
    type Input = u64;
    type Output = u64;
    type Error = Infallible;
    fn setup(&mut self, _: Invocation) -> Result<u64, Infallible> {
        Ok(1)
    }
    fn run(&mut self, mut n: u64) -> Result<u64, Infallible> {
        for _ in 0..4096 {
            n = std::hint::black_box(n.wrapping_mul(13).wrapping_add(11));
        }
        Ok(n)
    }
}
#[test]
fn live_provider_result_is_either_actual_counters_or_explicit_unavailable() {
    let mut group = match PerfGroup::open() {
        Ok(group) => group,
        Err(error) => {
            eprintln!("LIVE_PMU_UNAVAILABLE: {error}");
            return;
        }
    };
    let config = RunConfig {
        epochs: 1,
        warmup_epochs: 0,
        operations_per_window: 8,
        ..RunConfig::default()
    };
    match run_abba_observed(config, &mut Work, &mut Work, &mut group) {
        Ok(run) => {
            assert_eq!(run.observations().len(), 4);
            for w in run.observations() {
                assert!(w.observation.cycles() > 0);
                assert!(w.observation.instructions() > 0);
                assert_eq!(w.observation.enabled_nanos(), w.observation.running_nanos());
            }
            eprintln!("LIVE_PMU_ACTUAL_COUNTERS: four real windows");
        }
        Err(error) => eprintln!("LIVE_PMU_UNAVAILABLE_DURING_OBSERVATION: {error}"),
    }
}

#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[test]
fn unsupported_target_reports_structured_unavailable() {
    match PerfGroup::open() {
        Err(error) => assert_eq!(
            error.reason,
            vesper_bench_pmu::UnavailableReason::UnsupportedPlatform
        ),
        Ok(_) => panic!("unsupported target cannot produce a hardware provider"),
    }
}

fn exercise<O>(name: &str, mut counters: O)
where
    O: vesper_bench::WindowObserver<
            Observation = vesper_bench_pmu::CounterDelta,
            Error = vesper_bench_pmu::Unavailable,
        >,
{
    let config = RunConfig {
        epochs: 1,
        warmup_epochs: 0,
        operations_per_window: 8,
        ..RunConfig::default()
    };
    match run_abba_observed(config, &mut Work, &mut Work, &mut counters) {
        Ok(run) => {
            assert_eq!(run.observations().len(), 4);
            for w in run.observations() {
                assert!(w.observation.cycles() > 0);
                assert!(w.observation.instructions() > 0);
            }
            eprintln!("LIVE_{name}_ACTUAL_COUNTERS: four real windows");
        }
        Err(error) => eprintln!("LIVE_{name}_UNAVAILABLE_DURING_OBSERVATION: {error}"),
    }
}

#[test]
fn rdpmc_is_actual_counters_or_explicit_unavailable_with_a_host_reason() {
    use vesper_bench_pmu::{RdpmcCounters, UnavailableReason, host_pmu_status};
    let status = host_pmu_status();
    match RdpmcCounters::open() {
        Ok(counters) => exercise("RDPMC", counters),
        Err(error) => {
            eprintln!("LIVE_RDPMC_UNAVAILABLE: {error}; host {status:?}");
            if cfg!(not(all(target_os = "linux", target_arch = "x86_64"))) {
                assert_eq!(error.reason, UnavailableReason::UnsupportedPlatform);
            } else if status.cpu_pmu_device == Some(false) {
                // No hardware PMU device: the kernel reports it missing, or a
                // sandbox (seccomp in a container executor) refuses the
                // syscall first. Either way no count may be produced.
                assert!(
                    matches!(
                        error.reason,
                        UnavailableReason::NoHardwarePmu
                            | UnavailableReason::PermissionDenied
                            | UnavailableReason::OsError
                    ),
                    "{error}"
                );
            }
        }
    }
}

#[test]
fn kpc_is_actual_counters_or_explicit_unavailable() {
    use vesper_bench_pmu::{KpcCounters, UnavailableReason};
    match KpcCounters::open() {
        Ok(counters) => exercise("KPC", counters),
        Err(error) => {
            eprintln!("LIVE_KPC_UNAVAILABLE: {error}");
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                // The framework and symbols resolve on every macOS AArch64
                // host; only privilege or counter state may be missing.
                assert_ne!(error.reason, UnavailableReason::LibraryMissing);
                assert_ne!(error.reason, UnavailableReason::UnsupportedPlatform);
            } else {
                assert_eq!(error.reason, UnavailableReason::UnsupportedPlatform);
            }
        }
    }
}
