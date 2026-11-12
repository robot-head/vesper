//! Self-checking real-provider probe. Unavailable prints its structured reason
//! and exits2, so denial never masquerades as successful counter measurement.
use std::{convert::Infallible, hint::black_box};
use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba_observed};
use vesper_bench_pmu::PerfGroup;
struct Work;
impl BenchCase for Work {
    type Input = u64;
    type Output = u64;
    type Error = Infallible;
    fn setup(&mut self, invocation: Invocation) -> Result<u64, Infallible> {
        Ok(invocation.operation as u64 + 1)
    }
    fn run(&mut self, mut value: u64) -> Result<u64, Infallible> {
        for i in 0..2048u64 {
            value = black_box(value.wrapping_mul(6364136223846793005).wrapping_add(i));
        }
        Ok(value)
    }
}
fn main() {
    let mut group = match PerfGroup::open() {
        Ok(group) => group,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let config = RunConfig {
        epochs: 2,
        warmup_epochs: 1,
        operations_per_window: 8,
        ..RunConfig::default()
    };
    match run_abba_observed(config, &mut Work, &mut Work, &mut group) {
        Ok(run) => {
            assert_eq!(run.observations().len(), 12);
            for window in run.observations() {
                let counter = &window.observation;
                assert!(counter.cycles() > 0 && counter.instructions() > 0);
                assert_eq!(counter.enabled_nanos(), counter.running_nanos());
                assert_eq!(window.completion.successful_operations, 8);
                println!(
                    "{} {:?} warmup={} cycles={} instructions={} enabled_ns={} running_ns={}",
                    counter.provider(),
                    window.invocation.arm,
                    window.invocation.warmup,
                    counter.cycles(),
                    counter.instructions(),
                    counter.enabled_nanos(),
                    counter.running_nanos()
                );
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
