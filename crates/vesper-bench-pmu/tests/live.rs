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
