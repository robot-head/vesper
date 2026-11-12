use std::convert::Infallible;
use vesper_bench::*;
struct Kernel {
    rounds: usize,
    calls: usize,
}
impl BenchCase for Kernel {
    type Input = u64;
    type Output = u64;
    type Error = Infallible;
    fn setup(&mut self, i: Invocation) -> Result<u64, Infallible> {
        Ok(i.operation as u64 + 1)
    }
    fn run(&mut self, mut value: u64) -> Result<u64, Infallible> {
        self.calls += 1;
        for _ in 0..self.rounds {
            value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        Ok(value)
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = RunConfig {
        epochs: 9,
        warmup_epochs: 1,
        operations_per_window: 256,
        ..RunConfig::default()
    };
    let mut baseline = Kernel {
        rounds: 100,
        calls: 0,
    };
    let mut candidate = Kernel {
        rounds: 110,
        calls: 0,
    };
    let report = run_abba(config, &mut baseline, &mut candidate)?;
    assert_eq!(
        baseline.calls,
        config.operations_per_window * 2 * (config.epochs + config.warmup_epochs)
    );
    assert_eq!(baseline.calls, candidate.calls);
    assert!(
        report
            .epochs()
            .iter()
            .all(|e| e.windows.iter().all(|w| w.elapsed_nanos > 0))
    );
    println!(
        "provider={} unit={} measured_epochs={} body_calls={} (including warmups)",
        report.provider().as_str(),
        report.unit().as_str(),
        report.pairs().len(),
        report.total_body_calls()
    );
    let analysis = analyze_pairs(report.pairs(), 0)?;
    println!(
        "optional 95% population-median gate at 0bp: {:?}; upper sample median bp {:?}",
        analysis.decision,
        analysis.median_upper.regression_bp_bounds()
    );
    let interval = analysis
        .interval
        .expect("nine samples admit a finite interval");
    println!(
        "exact order ranks {}..{}; confidence coverage {}/{} under independent identically distributed epochs",
        interval.lower_rank,
        interval.upper_rank,
        interval.covered_outcomes,
        interval.total_outcomes
    );
    let unavailable = evaluate_evidence_requirement(EvidenceRequirement::RetiredInstructions);
    assert_eq!(unavailable.decision, Decision::Inconclusive);
    println!(
        "hardware evidence: {:?}; layout offset {} is generated metadata only",
        unavailable,
        layout_offset_bytes(19, 3)
    );
    // This example checks execution/report invariants; noisy wall time need not
    // reach any particular performance decision, so it does not assert a speedup.
    Ok(())
}
