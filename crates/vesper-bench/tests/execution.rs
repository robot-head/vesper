use std::{cell::RefCell, rc::Rc};
use vesper_bench::*;
#[derive(Clone, Debug)]
enum Event {
    Setup(Invocation),
    Run(Arm),
    Teardown(Arm),
}
struct Case {
    arm: Arm,
    events: Rc<RefCell<Vec<Event>>>,
}
impl BenchCase for Case {
    type Input = u64;
    type Output = u64;
    type Error = String;
    fn setup(&mut self, i: Invocation) -> Result<u64, String> {
        self.events.borrow_mut().push(Event::Setup(i));
        Ok(7)
    }
    fn run(&mut self, mut n: u64) -> Result<u64, String> {
        self.events.borrow_mut().push(Event::Run(self.arm));
        for _ in 0..100 {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            std::hint::black_box(n);
        }
        Ok(n)
    }
    fn teardown(&mut self, _: u64) -> Result<(), String> {
        self.events.borrow_mut().push(Event::Teardown(self.arm));
        Ok(())
    }
}
#[test]
fn actual_monotonic_provider_executes_counted_setup_run_teardown_in_abba_order() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut a = Case {
        arm: Arm::Baseline,
        events: events.clone(),
    };
    let mut b = Case {
        arm: Arm::Candidate,
        events: events.clone(),
    };
    let config = RunConfig {
        epochs: 6,
        warmup_epochs: 1,
        operations_per_window: 3,
        ..RunConfig::default()
    };
    let report = run_abba(config, &mut a, &mut b).unwrap();
    assert_eq!(report.provider(), MeasurementProvider::StdInstant);
    assert_eq!(report.unit(), MeasurementUnit::Nanoseconds);
    assert_eq!(report.provider().as_str(), "std::time::Instant");
    assert_eq!(report.unit().as_str(), "nanoseconds");
    assert_eq!(report.total_body_calls(), 84);
    assert_eq!(report.epochs().len(), 6);
    assert_eq!(report.pairs().len(), 6);
    assert!(report.elapsed_run_nanos() > 0);
    for epoch in report.epochs() {
        assert_eq!(
            epoch.windows.map(|w| w.arm),
            [Arm::Baseline, Arm::Candidate, Arm::Candidate, Arm::Baseline]
        );
        assert!(
            epoch
                .windows
                .iter()
                .all(|w| w.elapsed_nanos > 0 && w.operations == 3)
        );
        assert_eq!(epoch.pair.operations_per_arm(), 6);
    }
    let events = events.borrow();
    assert_eq!(events.len(), 84 * 3);
    for (window, chunk) in events.as_chunks::<9>().0.iter().enumerate() {
        let arm = [Arm::Baseline, Arm::Candidate, Arm::Candidate, Arm::Baseline][window % 4];
        for (i, event) in chunk[..3].iter().enumerate() {
            let Event::Setup(invocation) = event else {
                panic!("setup ordering")
            };
            assert_eq!(invocation.arm, arm);
            assert_eq!(invocation.operation, i);
            assert_eq!(invocation.warmup, window < 4);
        }
        assert!(
            chunk[3..6]
                .iter()
                .all(|e| matches!(e,Event::Run(a) if *a==arm))
        );
        assert!(
            chunk[6..]
                .iter()
                .all(|e| matches!(e,Event::Teardown(a) if *a==arm))
        );
    }
}
#[test]
fn invalid_configs_execute_no_callbacks() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut a = Case {
        arm: Arm::Baseline,
        events: events.clone(),
    };
    let mut b = Case {
        arm: Arm::Candidate,
        events: events.clone(),
    };
    for config in [
        RunConfig {
            epochs: 0,
            ..RunConfig::default()
        },
        RunConfig {
            epochs: 65,
            ..RunConfig::default()
        },
        RunConfig {
            operations_per_window: 0,
            ..RunConfig::default()
        },
        RunConfig {
            operations_per_window: 65537,
            ..RunConfig::default()
        },
        RunConfig {
            warmup_epochs: 65,
            ..RunConfig::default()
        },
        RunConfig {
            max_total_calls: 1,
            ..RunConfig::default()
        },
        RunConfig {
            max_elapsed_nanos: 0,
            ..RunConfig::default()
        },
    ] {
        assert!(matches!(
            run_abba(config, &mut a, &mut b),
            Err(RunError::Harness(_))
        ));
        assert!(events.borrow().is_empty());
    }
}

#[test]
fn measurement_only_accepts_one_epoch_but_analysis_remains_inconclusive() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut a = Case {
        arm: Arm::Baseline,
        events: events.clone(),
    };
    let mut b = Case {
        arm: Arm::Candidate,
        events,
    };
    let config = RunConfig {
        epochs: 1,
        warmup_epochs: 0,
        operations_per_window: 1,
        ..RunConfig::default()
    };
    let report = run_abba(config, &mut a, &mut b).unwrap();
    assert_eq!(report.total_body_calls(), 4);
    assert_eq!(
        analyze_pairs(report.pairs(), 0).unwrap().decision,
        Decision::Inconclusive
    );
}

#[test]
fn actual_callback_error_reports_correct_arm_phase_and_cleans_unrun_inputs() {
    struct FailureCase {
        runs: usize,
        discards: usize,
        fail: bool,
    }
    impl BenchCase for FailureCase {
        type Input = ();
        type Output = ();
        type Error = String;
        fn setup(&mut self, _: Invocation) -> Result<(), String> {
            Ok(())
        }
        fn run(&mut self, _: ()) -> Result<(), String> {
            self.runs += 1;
            if self.fail {
                Err("target error".into())
            } else {
                Ok(())
            }
        }
        fn discard(&mut self, _: ()) {
            self.discards += 1;
        }
    }
    let mut a = FailureCase {
        runs: 0,
        discards: 0,
        fail: false,
    };
    let mut b = FailureCase {
        runs: 0,
        discards: 0,
        fail: true,
    };
    let error = run_abba(
        RunConfig {
            epochs: 1,
            warmup_epochs: 0,
            operations_per_window: 2,
            ..RunConfig::default()
        },
        &mut a,
        &mut b,
    )
    .unwrap_err();
    assert!(error.to_string().contains("candidate Run"));
    let RunError::Candidate(failure) = error else {
        panic!("wrong callback arm")
    };
    assert_eq!(failure.phase, CallbackPhase::Run);
    assert_eq!(failure.invocation.window, 1);
    assert_eq!(failure.error, "target error");
    assert_eq!((a.runs, b.runs, b.discards), (2, 1, 1));
}
