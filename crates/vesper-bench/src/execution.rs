use crate::{Error, MAX_EPOCHS, PairedSample, layout_offset_bytes};
use std::{fmt, time::Instant};

pub const MAX_OPERATIONS_PER_WINDOW: usize = 65_536;
pub const MAX_TOTAL_CALLS: usize = 10_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arm {
    Baseline,
    Candidate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasurementProvider {
    StdInstant,
}
impl MeasurementProvider {
    pub const fn as_str(self) -> &'static str {
        "std::time::Instant"
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasurementUnit {
    Nanoseconds,
}
impl MeasurementUnit {
    pub const fn as_str(self) -> &'static str {
        "nanoseconds"
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RunConfig {
    pub epochs: usize,
    pub warmup_epochs: usize,
    pub operations_per_window: usize,
    pub max_total_calls: usize,
    /// Checked between windows, not a timeout/preemption mechanism. A callback
    /// must terminate itself; setup/body/cleanup can exceed this before detection.
    pub max_elapsed_nanos: u64,
    pub layout_seed: u64,
}
impl Default for RunConfig {
    fn default() -> Self {
        Self {
            epochs: 12,
            warmup_epochs: 1,
            operations_per_window: 128,
            max_total_calls: 1_000_000,
            max_elapsed_nanos: 10_000_000_000,
            layout_seed: 0,
        }
    }
}
impl RunConfig {
    fn validate(self) -> Result<usize, Error> {
        if self.epochs == 0 || self.epochs > MAX_EPOCHS {
            return Err(Error::InvalidConfig("epochs must be 1..=64"));
        }
        if self.warmup_epochs > MAX_EPOCHS {
            return Err(Error::InvalidConfig("warmup epochs exceed 64"));
        }
        if self.operations_per_window == 0 || self.operations_per_window > MAX_OPERATIONS_PER_WINDOW
        {
            return Err(Error::InvalidConfig("operations must be 1..=65536"));
        }
        if self.max_total_calls == 0 || self.max_total_calls > MAX_TOTAL_CALLS {
            return Err(Error::InvalidConfig(
                "total call budget must be 1..=10000000",
            ));
        }
        if self.max_elapsed_nanos == 0 || self.max_elapsed_nanos > 600_000_000_000 {
            return Err(Error::InvalidConfig("elapsed budget must be 1ns..=600s"));
        }
        let calls = self
            .epochs
            .checked_add(self.warmup_epochs)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| n.checked_mul(self.operations_per_window))
            .ok_or(Error::ArithmeticOverflow)?;
        if calls > self.max_total_calls {
            return Err(Error::Limit("body calls"));
        }
        Ok(calls)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub arm: Arm,
    pub epoch: usize,
    /// A/B/B/A window position 0,1,2,3.
    pub window: usize,
    pub operation: usize,
    pub warmup: bool,
    /// Generated metadata only; the harness changes no code layout.
    pub layout_offset_bytes: u32,
}
/// Owned inputs are prepared before each timed window. run consumes one input;
/// teardown consumes one output after timing; discard cleans unrun prepared
/// inputs after setup/body failure. First callback error is returned after all
/// available cleanup; later cleanup errors are counted. Panic/OOM/abort and
/// Vesper linear obligation exhaustion are outside this host trait's guarantee.
pub trait BenchCase {
    type Input;
    type Output;
    type Error;
    fn setup(&mut self, invocation: Invocation) -> Result<Self::Input, Self::Error>;
    fn run(&mut self, input: Self::Input) -> Result<Self::Output, Self::Error>;
    fn teardown(&mut self, output: Self::Output) -> Result<(), Self::Error> {
        drop(output);
        Ok(())
    }
    fn discard(&mut self, input: Self::Input) {
        drop(input);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackPhase {
    Setup,
    Run,
    Teardown,
}
#[derive(Debug)]
pub struct CallbackFailure<E> {
    pub phase: CallbackPhase,
    pub invocation: Invocation,
    pub error: E,
    pub cleanup_errors: usize,
}
#[derive(Debug)]
pub enum RunError<A, B> {
    Harness(Error),
    Baseline(CallbackFailure<A>),
    Candidate(CallbackFailure<B>),
}
impl<A: fmt::Display, B: fmt::Display> fmt::Display for RunError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Harness(e) => e.fmt(f),
            Self::Baseline(e) => write!(
                f,
                "baseline {:?} failed at {:?}: {} ({} cleanup errors)",
                e.phase, e.invocation, e.error, e.cleanup_errors
            ),
            Self::Candidate(e) => write!(
                f,
                "candidate {:?} failed at {:?}: {} ({} cleanup errors)",
                e.phase, e.invocation, e.error, e.cleanup_errors
            ),
        }
    }
}
impl<A: fmt::Debug + fmt::Display, B: fmt::Debug + fmt::Display> std::error::Error
    for RunError<A, B>
{
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSample {
    pub arm: Arm,
    pub elapsed_nanos: u64,
    pub operations: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochSample {
    pub windows: [WindowSample; 4],
    pub pair: PairedSample,
    pub layout_offset_bytes: u32,
}
#[derive(Debug)]
pub struct RunReport {
    epochs: Vec<EpochSample>,
    pairs: Vec<PairedSample>,
    calls: usize,
    elapsed: u64,
}
impl RunReport {
    pub fn epochs(&self) -> &[EpochSample] {
        &self.epochs
    }
    pub fn pairs(&self) -> &[PairedSample] {
        &self.pairs
    }
    pub fn provider(&self) -> MeasurementProvider {
        MeasurementProvider::StdInstant
    }
    pub fn unit(&self) -> MeasurementUnit {
        MeasurementUnit::Nanoseconds
    }
    pub fn total_body_calls(&self) -> usize {
        self.calls
    }
    /// Includes warmups, setup, execution, teardown and harness overhead.
    pub fn elapsed_run_nanos(&self) -> u64 {
        self.elapsed
    }
}
fn reserve<T>(count: usize) -> Result<Vec<T>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| Error::AllocationFailed)?;
    Ok(values)
}
trait WindowClock {
    type Mark;
    fn begin(&mut self) -> Self::Mark;
    fn end(&mut self, mark: Self::Mark) -> Result<u64, Error>;
}
struct InstantClock;
impl WindowClock for InstantClock {
    type Mark = Instant;
    fn begin(&mut self) -> Instant {
        Instant::now()
    }
    fn end(&mut self, mark: Instant) -> Result<u64, Error> {
        u64::try_from(mark.elapsed().as_nanos()).map_err(|_| Error::ClockOverflow)
    }
}
/// Hooks are outside Instant timing and outside setup/teardown. Once before
/// succeeds, after is called exactly once, even after body or clock failure.
/// Observation therefore includes the Instant readings and safe boundary work;
/// the observer determines its own provider/units and must release its token.
/// Result errors are handled; panics/aborts in user callbacks are not caught.
pub trait WindowObserver {
    type Token;
    type Observation;
    type Error;
    fn before_window(
        &mut self,
        invocation: Invocation,
        operations: usize,
    ) -> Result<Self::Token, Self::Error>;
    fn after_window(
        &mut self,
        token: Self::Token,
        completion: WindowCompletion,
    ) -> Result<Self::Observation, Self::Error>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyStatus {
    Completed,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClockStatus {
    Measured(u64),
    Failed(Error),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowCompletion {
    pub invocation: Invocation,
    pub planned_operations: usize,
    /// Includes the callback that returned an error, if any.
    pub attempted_operations: usize,
    pub successful_operations: usize,
    pub body: BodyStatus,
    pub clock: ClockStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationPhase {
    Before,
    After,
}
#[derive(Debug)]
pub struct ObservationFailure<E> {
    pub phase: ObservationPhase,
    pub invocation: Invocation,
    pub error: E,
}
#[derive(Debug)]
pub enum ObservedRunError<A, B, O> {
    Run {
        error: RunError<A, B>,
        observer_error: Option<ObservationFailure<O>>,
        completion: Option<WindowCompletion>,
    },
    Observer {
        error: ObservationFailure<O>,
        cleanup_errors: usize,
        completion: Option<WindowCompletion>,
    },
}
impl<A: fmt::Display, B: fmt::Display, O: fmt::Display> fmt::Display for ObservedRunError<A, B, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run {
                error,
                observer_error,
                ..
            } => {
                write!(f, "{error}")?;
                if let Some(e) = observer_error {
                    write!(f, "; observer {:?}: {}", e.phase, e.error)?;
                }
                Ok(())
            }
            Self::Observer {
                error,
                cleanup_errors,
                ..
            } => write!(
                f,
                "observer {:?} at {:?}: {} ({} cleanup errors)",
                error.phase, error.invocation, error.error, cleanup_errors
            ),
        }
    }
}
impl<A: fmt::Debug + fmt::Display, B: fmt::Debug + fmt::Display, O: fmt::Debug + fmt::Display>
    std::error::Error for ObservedRunError<A, B, O>
{
}
#[derive(Debug)]
pub struct ObservedWindow<T> {
    pub invocation: Invocation,
    pub completion: WindowCompletion,
    pub observation: T,
}
#[derive(Debug)]
pub struct ObservedRun<T> {
    report: RunReport,
    observations: Vec<ObservedWindow<T>>,
}
impl<T> ObservedRun<T> {
    pub fn report(&self) -> &RunReport {
        &self.report
    }
    /// Ordered ABBA windows including report-excluded warmups.
    pub fn observations(&self) -> &[ObservedWindow<T>] {
        &self.observations
    }
    pub fn into_parts(self) -> (RunReport, Vec<ObservedWindow<T>>) {
        (self.report, self.observations)
    }
}
struct NoObservation;
impl WindowObserver for NoObservation {
    type Token = ();
    type Observation = ();
    type Error = std::convert::Infallible;
    fn before_window(&mut self, _: Invocation, _: usize) -> Result<(), Self::Error> {
        Ok(())
    }
    fn after_window(&mut self, _: (), _: WindowCompletion) -> Result<(), Self::Error> {
        Ok(())
    }
}
enum WindowFailure<E> {
    Harness(Error),
    Callback(CallbackFailure<E>),
}
enum WindowObservedFailure<C, O> {
    Run {
        error: WindowFailure<C>,
        observer_error: Option<ObservationFailure<O>>,
        completion: Option<WindowCompletion>,
    },
    Observer {
        error: ObservationFailure<O>,
        completion: Option<WindowCompletion>,
    },
}
// Keep primary/secondary errors and completion inline: error reporting must not
// require another fallible allocation. Associated callback types are independent.
#[allow(clippy::result_large_err, clippy::type_complexity)]
fn execute_window_observed<C: BenchCase, T: WindowClock, O: WindowObserver>(
    case: &mut C,
    base: Invocation,
    count: usize,
    clock: &mut T,
    observer: &mut O,
) -> Result<(u64, ObservedWindow<O::Observation>), WindowObservedFailure<C::Error, O::Error>> {
    // Pre-setup allocation failures have no observer token and no owned inputs.
    let fail_alloc = |error| WindowObservedFailure::Run {
        error: WindowFailure::Harness(error),
        observer_error: None,
        completion: None,
    };
    let mut inputs = reserve(count).map_err(fail_alloc)?;
    let mut outputs = reserve(count).map_err(fail_alloc)?;
    for operation in 0..count {
        let invocation = Invocation { operation, ..base };
        match case.setup(invocation) {
            Ok(input) => inputs.push((invocation, Some(input))),
            Err(error) => {
                for (_, input) in inputs {
                    if let Some(input) = input {
                        case.discard(input);
                    }
                }
                return Err(WindowObservedFailure::Run {
                    error: WindowFailure::Callback(CallbackFailure {
                        phase: CallbackPhase::Setup,
                        invocation,
                        error,
                        cleanup_errors: 0,
                    }),
                    observer_error: None,
                    completion: None,
                });
            }
        }
    }
    let token = match observer.before_window(base, count) {
        Ok(token) => token,
        Err(error) => {
            for (_, input) in inputs {
                if let Some(input) = input {
                    case.discard(input);
                }
            }
            return Err(WindowObservedFailure::Observer {
                error: ObservationFailure {
                    phase: ObservationPhase::Before,
                    invocation: base,
                    error,
                },
                completion: None,
            });
        }
    };
    let mut failure = None;
    let mut attempted = 0usize;
    let mark = clock.begin();
    for (invocation, input) in &mut inputs {
        let input = std::hint::black_box(input.take().expect("one prepared input per operation"));
        attempted += 1;
        match case.run(input) {
            Ok(output) => outputs.push((*invocation, std::hint::black_box(output))),
            Err(error) => {
                failure = Some(CallbackFailure {
                    phase: CallbackPhase::Run,
                    invocation: *invocation,
                    error,
                    cleanup_errors: 0,
                });
                break;
            }
        }
    }
    let elapsed = clock.end(mark);
    let completion = WindowCompletion {
        invocation: base,
        planned_operations: count,
        attempted_operations: attempted,
        successful_operations: outputs.len(),
        body: if failure.is_some() {
            BodyStatus::Failed
        } else {
            BodyStatus::Completed
        },
        clock: match &elapsed {
            Ok(n) => ClockStatus::Measured(*n),
            Err(e) => ClockStatus::Failed(e.clone()),
        },
    };
    let observed = observer.after_window(token, completion.clone());
    for (_, input) in inputs {
        if let Some(input) = input {
            case.discard(input);
        }
    }
    for (invocation, output) in outputs {
        if let Err(error) = case.teardown(output) {
            if let Some(failure) = &mut failure {
                failure.cleanup_errors += 1;
            } else {
                failure = Some(CallbackFailure {
                    phase: CallbackPhase::Teardown,
                    invocation,
                    error,
                    cleanup_errors: 0,
                });
            }
        }
    }
    let (observation, observer_error) = match observed {
        Ok(value) => (Some(value), None),
        Err(error) => (
            None,
            Some(ObservationFailure {
                phase: ObservationPhase::After,
                invocation: base,
                error,
            }),
        ),
    };
    // Preserve legacy callback/teardown-over-clock priority, without losing an
    // additional observer error or the body's/clock's completion metadata.
    if let Some(failure) = failure {
        return Err(WindowObservedFailure::Run {
            error: WindowFailure::Callback(failure),
            observer_error,
            completion: Some(completion),
        });
    }
    let elapsed = match elapsed {
        Err(error) => {
            return Err(WindowObservedFailure::Run {
                error: WindowFailure::Harness(error),
                observer_error,
                completion: Some(completion),
            });
        }
        Ok(elapsed) => elapsed,
    };
    if let Some(error) = observer_error {
        return Err(WindowObservedFailure::Observer {
            error,
            completion: Some(completion),
        });
    }
    Ok((
        elapsed,
        ObservedWindow {
            invocation: base,
            completion,
            observation: observation.expect("successful observer value"),
        },
    ))
}
#[cfg(test)]
fn execute_window<C: BenchCase, T: WindowClock>(
    case: &mut C,
    base: Invocation,
    count: usize,
    clock: &mut T,
) -> Result<u64, WindowFailure<C::Error>> {
    match execute_window_observed(case, base, count, clock, &mut NoObservation) {
        Ok((elapsed, _)) => Ok(elapsed),
        Err(WindowObservedFailure::Run { error, .. }) => Err(error),
        Err(WindowObservedFailure::Observer { error, .. }) => match error.error {},
    }
}
fn elapsed_run(start: Instant) -> Result<u64, Error> {
    u64::try_from(start.elapsed().as_nanos()).map_err(|_| Error::ClockOverflow)
}
/// Runs actual callbacks, always with the fixed A/B/B/A order per epoch.
/// Setup, output capacity allocation and teardown are outside each measured
/// window. Loop, input moves, result storage, std black_box and callback/JIT
/// adapter overhead remain inside. No calibration subtraction is performed.
/// This function measures only; apply analyze_pairs explicitly if a gate is wanted.
pub fn run_abba<A: BenchCase, B: BenchCase>(
    config: RunConfig,
    baseline: &mut A,
    candidate: &mut B,
) -> Result<RunReport, RunError<A::Error, B::Error>> {
    match run_observed_inner(config, baseline, candidate, &mut NoObservation, false) {
        Ok(observed) => Ok(observed.report),
        Err(ObservedRunError::Run {
            error,
            observer_error,
            ..
        }) => {
            if let Some(e) = observer_error {
                match e.error {}
            }
            Err(error)
        }
        Err(ObservedRunError::Observer { error, .. }) => match error.error {},
    }
}
/// Observe every actual window, including warmups. Reserves at most 512 records
/// before any setup; no observer call or observation allocation is inside the
/// Instant interval. Observer errors never bypass available callback cleanup.
/// Callback/teardown errors take priority over clock errors, then observer errors;
/// secondary observer failures and body/clock completion remain in the error.
/// Completion describes the body and clock, before the subsequent cleanup.
// Keep primary/secondary errors and completion inline: error reporting must not
// require another fallible allocation. Associated callback types are independent.
#[allow(clippy::result_large_err, clippy::type_complexity)]
pub fn run_abba_observed<A: BenchCase, B: BenchCase, O: WindowObserver>(
    config: RunConfig,
    baseline: &mut A,
    candidate: &mut B,
    observer: &mut O,
) -> Result<ObservedRun<O::Observation>, ObservedRunError<A::Error, B::Error, O::Error>> {
    run_observed_inner(config, baseline, candidate, observer, true)
}
fn observed_harness<A, B, O>(error: Error) -> ObservedRunError<A, B, O> {
    ObservedRunError::Run {
        error: RunError::Harness(error),
        observer_error: None,
        completion: None,
    }
}
fn map_window_failure<A, B, C, O>(
    failure: WindowObservedFailure<C, O>,
    callback: impl FnOnce(CallbackFailure<C>) -> RunError<A, B>,
) -> ObservedRunError<A, B, O> {
    match failure {
        WindowObservedFailure::Run {
            error,
            observer_error,
            completion,
        } => ObservedRunError::Run {
            error: match error {
                WindowFailure::Harness(e) => RunError::Harness(e),
                WindowFailure::Callback(e) => callback(e),
            },
            observer_error,
            completion,
        },
        WindowObservedFailure::Observer { error, completion } => ObservedRunError::Observer {
            error,
            cleanup_errors: 0,
            completion,
        },
    }
}
// Keep primary/secondary errors and completion inline: error reporting must not
// require another fallible allocation. Associated callback types are independent.
#[allow(clippy::result_large_err, clippy::type_complexity)]
fn run_observed_inner<A: BenchCase, B: BenchCase, O: WindowObserver>(
    config: RunConfig,
    baseline: &mut A,
    candidate: &mut B,
    observer: &mut O,
    collect: bool,
) -> Result<ObservedRun<O::Observation>, ObservedRunError<A::Error, B::Error, O::Error>> {
    let calls = config.validate().map_err(observed_harness)?;
    let mut epochs = reserve(config.epochs).map_err(observed_harness)?;
    let mut pairs = reserve(config.epochs).map_err(observed_harness)?;
    let start = Instant::now();
    let mut clock = InstantClock;
    let total_epochs = config
        .warmup_epochs
        .checked_add(config.epochs)
        .ok_or(observed_harness(Error::ArithmeticOverflow))?;
    let mut observations = if collect {
        reserve(
            total_epochs
                .checked_mul(4)
                .ok_or_else(|| observed_harness(Error::ArithmeticOverflow))?,
        )
        .map_err(observed_harness)?
    } else {
        Vec::new()
    };
    let operations = u64::try_from(config.operations_per_window)
        .map_err(|_| observed_harness(Error::ArithmeticOverflow))?;
    for epoch in 0..total_epochs {
        let warmup = epoch < config.warmup_epochs;
        let offset = layout_offset_bytes(
            config.layout_seed,
            u64::try_from(epoch).map_err(|_| observed_harness(Error::ArithmeticOverflow))?,
        );
        let mut windows = [WindowSample {
            arm: Arm::Baseline,
            elapsed_nanos: 0,
            operations,
        }; 4];
        for (position, arm) in [Arm::Baseline, Arm::Candidate, Arm::Candidate, Arm::Baseline]
            .into_iter()
            .enumerate()
        {
            if elapsed_run(start).map_err(observed_harness)? > config.max_elapsed_nanos {
                return Err(observed_harness(Error::Limit("elapsed run budget")));
            }
            let invocation = Invocation {
                arm,
                epoch,
                window: position,
                operation: 0,
                warmup,
                layout_offset_bytes: offset,
            };
            let (elapsed, observation) = match arm {
                Arm::Baseline => execute_window_observed(
                    baseline,
                    invocation,
                    config.operations_per_window,
                    &mut clock,
                    observer,
                )
                .map_err(|e| map_window_failure(e, RunError::Baseline))?,
                Arm::Candidate => execute_window_observed(
                    candidate,
                    invocation,
                    config.operations_per_window,
                    &mut clock,
                    observer,
                )
                .map_err(|e| map_window_failure(e, RunError::Candidate))?,
            };
            if elapsed_run(start).map_err(observed_harness)? > config.max_elapsed_nanos {
                return Err(observed_harness(Error::Limit("elapsed run budget")));
            }
            if !warmup && elapsed == 0 {
                return Err(observed_harness(Error::ZeroDuration));
            }
            if collect {
                observations.push(observation);
            }
            windows[position] = WindowSample {
                arm,
                elapsed_nanos: elapsed,
                operations,
            };
        }
        if !warmup {
            let a = windows[0]
                .elapsed_nanos
                .checked_add(windows[3].elapsed_nanos)
                .ok_or(observed_harness(Error::ArithmeticOverflow))?;
            let b = windows[1]
                .elapsed_nanos
                .checked_add(windows[2].elapsed_nanos)
                .ok_or(observed_harness(Error::ArithmeticOverflow))?;
            let pair = PairedSample::new(
                a,
                b,
                operations
                    .checked_mul(2)
                    .ok_or(observed_harness(Error::ArithmeticOverflow))?,
            )
            .map_err(observed_harness)?;
            pairs.push(pair);
            epochs.push(EpochSample {
                windows,
                pair,
                layout_offset_bytes: offset,
            });
        }
    }
    let elapsed = elapsed_run(start).map_err(observed_harness)?;
    if elapsed > config.max_elapsed_nanos {
        return Err(observed_harness(Error::Limit("elapsed run budget")));
    }
    Ok(ObservedRun {
        report: RunReport {
            epochs,
            pairs,
            calls,
            elapsed,
        },
        observations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct ObservedClock(Rc<Cell<bool>>);
    impl WindowClock for ObservedClock {
        type Mark = ();
        fn begin(&mut self) {
            assert!(!self.0.replace(true));
        }
        fn end(&mut self, _: ()) -> Result<u64, Error> {
            assert!(self.0.replace(false));
            Ok(100)
        }
    }
    struct ObservedCase {
        timed: Rc<Cell<bool>>,
        setups: usize,
        runs: usize,
        teardowns: usize,
        discards: usize,
        fail_setup: Option<usize>,
        fail_run: Option<usize>,
        fail_teardown: bool,
    }
    impl BenchCase for ObservedCase {
        type Input = usize;
        type Output = usize;
        type Error = &'static str;
        fn setup(&mut self, _: Invocation) -> Result<usize, Self::Error> {
            assert!(!self.timed.get());
            self.setups += 1;
            if self.fail_setup == Some(self.setups) {
                Err("setup")
            } else {
                Ok(self.setups)
            }
        }
        fn run(&mut self, input: usize) -> Result<usize, Self::Error> {
            assert!(self.timed.get());
            self.runs += 1;
            if self.fail_run == Some(self.runs) {
                Err("run")
            } else {
                Ok(input)
            }
        }
        fn teardown(&mut self, _: usize) -> Result<(), Self::Error> {
            assert!(!self.timed.get());
            self.teardowns += 1;
            if self.fail_teardown {
                Err("teardown")
            } else {
                Ok(())
            }
        }
        fn discard(&mut self, _: usize) {
            assert!(!self.timed.get());
            self.discards += 1;
        }
    }
    fn case() -> ObservedCase {
        ObservedCase {
            timed: Rc::new(Cell::new(false)),
            setups: 0,
            runs: 0,
            teardowns: 0,
            discards: 0,
            fail_setup: None,
            fail_run: None,
            fail_teardown: false,
        }
    }
    fn invocation() -> Invocation {
        Invocation {
            arm: Arm::Baseline,
            epoch: 0,
            window: 0,
            operation: 0,
            warmup: false,
            layout_offset_bytes: 0,
        }
    }
    #[test]
    fn setup_and_all_cleanup_are_outside_clock_window() {
        let mut c = case();
        let mut clock = ObservedClock(c.timed.clone());
        assert_eq!(
            execute_window(&mut c, invocation(), 4, &mut clock)
                .unwrap_or_else(|_| panic!("window failed")),
            100
        );
        assert_eq!((c.setups, c.runs, c.teardowns, c.discards), (4, 4, 4, 0));
    }
    #[test]
    fn body_error_stops_clock_then_discards_and_tears_down_all_outputs() {
        let mut c = case();
        c.fail_run = Some(3);
        c.fail_teardown = true;
        let mut clock = ObservedClock(c.timed.clone());
        let Err(WindowFailure::Callback(f)) = execute_window(&mut c, invocation(), 5, &mut clock)
        else {
            panic!("expected body failure")
        };
        assert_eq!(f.phase, CallbackPhase::Run);
        assert_eq!(f.cleanup_errors, 2);
        assert_eq!((c.setups, c.runs, c.teardowns, c.discards), (5, 3, 2, 2));
    }
    #[test]
    fn failed_setup_disposes_prior_inputs_without_starting_clock() {
        let mut c = case();
        c.fail_setup = Some(3);
        let mut clock = ObservedClock(c.timed.clone());
        assert!(execute_window(&mut c, invocation(), 5, &mut clock).is_err());
        assert!(!c.timed.get());
        assert_eq!((c.setups, c.runs, c.teardowns, c.discards), (3, 0, 0, 2));
    }
    #[test]
    fn teardown_failure_still_cleans_every_output() {
        let mut c = case();
        c.fail_teardown = true;
        let mut clock = ObservedClock(c.timed.clone());
        let Err(WindowFailure::Callback(f)) = execute_window(&mut c, invocation(), 4, &mut clock)
        else {
            panic!("expected teardown failure")
        };
        assert_eq!(f.phase, CallbackPhase::Teardown);
        assert_eq!(f.cleanup_errors, 3);
        assert_eq!(c.teardowns, 4);
    }
    #[test]
    fn clock_error_still_tears_down_outputs_after_the_end_reading() {
        struct FailingClock(Rc<Cell<bool>>);
        impl WindowClock for FailingClock {
            type Mark = ();
            fn begin(&mut self) {
                assert!(!self.0.replace(true));
            }
            fn end(&mut self, _: ()) -> Result<u64, Error> {
                assert!(self.0.replace(false));
                Err(Error::ClockOverflow)
            }
        }
        let mut c = case();
        let mut clock = FailingClock(c.timed.clone());
        assert!(matches!(
            execute_window(&mut c, invocation(), 3, &mut clock),
            Err(WindowFailure::Harness(Error::ClockOverflow))
        ));
        assert_eq!((c.runs, c.teardowns, c.discards), (3, 3, 0));
    }
}

#[cfg(test)]
mod observation_boundary_tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    type Events = Rc<RefCell<Vec<&'static str>>>;
    struct Clock(Events);
    impl WindowClock for Clock {
        type Mark = ();
        fn begin(&mut self) {
            self.0.borrow_mut().push("begin");
        }
        fn end(&mut self, _: ()) -> Result<u64, Error> {
            self.0.borrow_mut().push("end");
            Err(Error::ClockOverflow)
        }
    }
    struct Case {
        events: Events,
        fail_body: bool,
        teardowns: usize,
    }
    impl BenchCase for Case {
        type Input = ();
        type Output = ();
        type Error = &'static str;
        fn setup(&mut self, _: Invocation) -> Result<(), Self::Error> {
            self.events.borrow_mut().push("setup");
            Ok(())
        }
        fn run(&mut self, _: ()) -> Result<(), Self::Error> {
            self.events.borrow_mut().push("body");
            if self.fail_body { Err("body") } else { Ok(()) }
        }
        fn teardown(&mut self, _: ()) -> Result<(), Self::Error> {
            self.events.borrow_mut().push("teardown");
            self.teardowns += 1;
            Ok(())
        }
    }
    struct Observer(Events);
    impl WindowObserver for Observer {
        type Token = ();
        type Observation = ();
        type Error = &'static str;
        fn before_window(&mut self, _: Invocation, _: usize) -> Result<(), Self::Error> {
            self.0.borrow_mut().push("before");
            Ok(())
        }
        fn after_window(&mut self, _: (), c: WindowCompletion) -> Result<(), Self::Error> {
            self.0.borrow_mut().push("after");
            assert_eq!(c.clock, ClockStatus::Failed(Error::ClockOverflow));
            Err("observer")
        }
    }
    fn invocation() -> Invocation {
        Invocation {
            arm: Arm::Baseline,
            epoch: 0,
            window: 0,
            operation: 0,
            warmup: false,
            layout_offset_bytes: 0,
        }
    }
    #[test]
    fn clock_failure_still_finishes_observer_then_teardown_and_retains_both_errors() {
        let events = Events::default();
        let mut case = Case {
            events: events.clone(),
            fail_body: false,
            teardowns: 0,
        };
        let result = execute_window_observed(
            &mut case,
            invocation(),
            1,
            &mut Clock(events.clone()),
            &mut Observer(events.clone()),
        );
        assert!(matches!(
            result,
            Err(WindowObservedFailure::Run {
                error: WindowFailure::Harness(Error::ClockOverflow),
                observer_error: Some(_),
                completion: Some(_)
            })
        ));
        assert_eq!(
            events.borrow().as_slice(),
            [
                "setup", "before", "begin", "body", "end", "after", "teardown"
            ]
        );
        assert_eq!(case.teardowns, 1);
    }
    #[test]
    fn body_and_clock_and_observer_failure_preserve_primary_and_completion() {
        let events = Events::default();
        let mut case = Case {
            events: events.clone(),
            fail_body: true,
            teardowns: 0,
        };
        let result = execute_window_observed(
            &mut case,
            invocation(),
            1,
            &mut Clock(events.clone()),
            &mut Observer(events.clone()),
        );
        match result {
            Err(WindowObservedFailure::Run {
                error: WindowFailure::Callback(f),
                observer_error: Some(o),
                completion: Some(c),
            }) => {
                assert_eq!(f.error, "body");
                assert_eq!(o.error, "observer");
                assert_eq!(c.attempted_operations, 1);
                assert_eq!(c.successful_operations, 0);
                assert_eq!(c.body, BodyStatus::Failed);
                assert_eq!(c.clock, ClockStatus::Failed(Error::ClockOverflow));
            }
            _ => panic!("wrong failure priority"),
        }
        assert_eq!(
            events.borrow().as_slice(),
            ["setup", "before", "begin", "body", "end", "after"]
        );
    }
}
