use std::{cell::RefCell, rc::Rc};
use vesper_bench::*;
#[derive(Default)]
struct State {
    events: Vec<&'static str>,
    setups: usize,
    runs: usize,
    teardowns: usize,
    discards: usize,
    after: usize,
}
type Shared = Rc<RefCell<State>>;
struct Case {
    state: Shared,
    fail_run: bool,
    fail_teardown: bool,
}
impl BenchCase for Case {
    type Input = usize;
    type Output = usize;
    type Error = &'static str;
    fn setup(&mut self, _: Invocation) -> Result<usize, Self::Error> {
        let mut s = self.state.borrow_mut();
        s.events.push("setup");
        s.setups += 1;
        Ok(s.setups)
    }
    fn run(&mut self, input: usize) -> Result<usize, Self::Error> {
        let mut s = self.state.borrow_mut();
        s.events.push("run");
        s.runs += 1;
        if self.fail_run && s.runs == 2 {
            Err("body")
        } else {
            Ok(std::hint::black_box(input))
        }
    }
    fn teardown(&mut self, _: usize) -> Result<(), Self::Error> {
        let mut s = self.state.borrow_mut();
        s.events.push("teardown");
        s.teardowns += 1;
        if self.fail_teardown {
            Err("cleanup")
        } else {
            Ok(())
        }
    }
    fn discard(&mut self, _: usize) {
        let mut s = self.state.borrow_mut();
        s.events.push("discard");
        s.discards += 1;
    }
}
struct Observer {
    state: Shared,
    fail_before: bool,
    fail_after: bool,
    completions: Vec<WindowCompletion>,
}
impl WindowObserver for Observer {
    type Token = Invocation;
    type Observation = usize;
    type Error = &'static str;
    fn before_window(&mut self, i: Invocation, n: usize) -> Result<Invocation, Self::Error> {
        assert_eq!(n, 3);
        self.state.borrow_mut().events.push("before");
        if self.fail_before {
            Err("before")
        } else {
            Ok(i)
        }
    }
    fn after_window(&mut self, i: Invocation, c: WindowCompletion) -> Result<usize, Self::Error> {
        assert_eq!(i, c.invocation);
        let mut s = self.state.borrow_mut();
        s.events.push("after");
        s.after += 1;
        self.completions.push(c);
        if self.fail_after {
            Err("after")
        } else {
            Ok(s.after)
        }
    }
}
fn config() -> RunConfig {
    RunConfig {
        epochs: 2,
        warmup_epochs: 1,
        operations_per_window: 3,
        ..RunConfig::default()
    }
}
fn case(s: &Shared) -> Case {
    Case {
        state: s.clone(),
        fail_run: false,
        fail_teardown: false,
    }
}
fn observer(s: &Shared) -> Observer {
    Observer {
        state: s.clone(),
        fail_before: false,
        fail_after: false,
        completions: Vec::new(),
    }
}
#[test]
fn actual_abba_hooks_have_identity_counts_and_untimed_setup_cleanup_order() {
    let s = Shared::default();
    let mut o = observer(&s);
    let run = run_abba_observed(config(), &mut case(&s), &mut case(&s), &mut o).unwrap();
    assert_eq!(run.observations().len(), 12);
    assert_eq!(run.report().pairs().len(), 2);
    let (report, windows) = run.into_parts();
    assert_eq!(report.total_body_calls(), 36);
    for (n, w) in windows.iter().enumerate() {
        assert_eq!(w.invocation.epoch, n / 4);
        assert_eq!(w.invocation.window, n % 4);
        assert_eq!(
            w.invocation.arm,
            [Arm::Baseline, Arm::Candidate, Arm::Candidate, Arm::Baseline][n % 4]
        );
        assert_eq!(w.invocation.warmup, n < 4);
        assert_eq!(w.observation, n + 1);
        assert_eq!(w.completion.invocation, w.invocation);
        assert_eq!(w.completion.planned_operations, 3);
        assert_eq!(w.completion.attempted_operations, 3);
        assert_eq!(w.completion.successful_operations, 3);
        assert_eq!(w.completion.body, BodyStatus::Completed);
        assert!(matches!(w.completion.clock, ClockStatus::Measured(_)));
    }
    let expected = [
        "setup", "setup", "setup", "before", "run", "run", "run", "after", "teardown", "teardown",
        "teardown",
    ];
    for window in s.borrow().events.chunks_exact(expected.len()) {
        assert_eq!(window, expected);
    }
}
#[test]
fn before_failure_discards_all_prepared_without_body_or_after() {
    let s = Shared::default();
    let mut o = observer(&s);
    o.fail_before = true;
    let e = run_abba_observed(config(), &mut case(&s), &mut case(&s), &mut o).unwrap_err();
    assert!(matches!(
        e,
        ObservedRunError::Observer {
            error: ObservationFailure {
                phase: ObservationPhase::Before,
                ..
            },
            cleanup_errors: 0,
            completion: None
        }
    ));
    let s = s.borrow();
    assert_eq!(
        (s.setups, s.runs, s.after, s.discards, s.teardowns),
        (3, 0, 0, 3, 0)
    );
}
#[test]
fn after_failure_retains_completion_and_cleans_all_outputs() {
    let s = Shared::default();
    let mut o = observer(&s);
    o.fail_after = true;
    let e = run_abba_observed(config(), &mut case(&s), &mut case(&s), &mut o).unwrap_err();
    match e {
        ObservedRunError::Observer {
            error,
            cleanup_errors,
            completion: Some(c),
        } => {
            assert_eq!(error.phase, ObservationPhase::After);
            assert_eq!(cleanup_errors, 0);
            assert_eq!(c.successful_operations, 3);
            assert_eq!(c.body, BodyStatus::Completed);
        }
        other => panic!("{other:?}"),
    }
    let s = s.borrow();
    assert_eq!((s.runs, s.after, s.discards, s.teardowns), (3, 1, 0, 3));
}
#[test]
fn body_primary_retains_observer_error_and_cleanup_failures() {
    let s = Shared::default();
    let mut o = observer(&s);
    o.fail_after = true;
    let mut a = case(&s);
    a.fail_run = true;
    a.fail_teardown = true;
    let e = run_abba_observed(config(), &mut a, &mut case(&s), &mut o).unwrap_err();
    match e {
        ObservedRunError::Run {
            error: RunError::Baseline(f),
            observer_error: Some(o),
            completion: Some(c),
        } => {
            assert_eq!(f.phase, CallbackPhase::Run);
            assert_eq!(f.error, "body");
            assert_eq!(f.cleanup_errors, 1);
            assert_eq!(o.phase, ObservationPhase::After);
            assert_eq!(c.body, BodyStatus::Failed);
            assert_eq!(
                (
                    c.planned_operations,
                    c.attempted_operations,
                    c.successful_operations
                ),
                (3, 2, 1)
            );
            assert!(matches!(c.clock, ClockStatus::Measured(_)));
        }
        other => panic!("{other:?}"),
    }
    let s = s.borrow();
    assert_eq!((s.runs, s.after, s.discards, s.teardowns), (2, 1, 1, 1));
}
#[test]
fn teardown_primary_after_observer_failure_still_cleans_every_output() {
    let s = Shared::default();
    let mut o = observer(&s);
    o.fail_after = true;
    let mut a = case(&s);
    a.fail_teardown = true;
    let e = run_abba_observed(config(), &mut a, &mut case(&s), &mut o).unwrap_err();
    match e {
        ObservedRunError::Run {
            error: RunError::Baseline(f),
            observer_error: Some(_),
            completion: Some(c),
        } => {
            assert_eq!(f.phase, CallbackPhase::Teardown);
            assert_eq!(f.cleanup_errors, 2);
            // Completion is body/clock state, not a promise about later teardown.
            assert_eq!(c.body, BodyStatus::Completed);
            assert_eq!(c.successful_operations, 3);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s.borrow().teardowns, 3);
}
#[test]
fn invalid_config_calls_neither_observer_nor_setup() {
    let s = Shared::default();
    let mut o = observer(&s);
    let mut c = config();
    c.max_total_calls = 1;
    assert!(matches!(
        run_abba_observed(c, &mut case(&s), &mut case(&s), &mut o),
        Err(ObservedRunError::Run {
            completion: None,
            ..
        })
    ));
    assert!(s.borrow().events.is_empty());
}
