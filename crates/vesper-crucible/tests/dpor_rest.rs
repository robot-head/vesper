//! Acceptance (wave seven): preemption bounding, state caching, fibers that
//! spawn fibers and request cancellation, and I/O completion values chosen
//! by the explorer. Every reduction is checked differentially against
//! exhaustive search: same set of distinct outcomes.

mod common;

use common::{Cells, Op, build, cells_key, random_program, random_program_ext};
use vesper_crucible::{
    Exploration, ExploreConfig, ExploreError, Footprint, IoChoices, IoChoicesError, IoRequest,
    IoResult, Outcome, Proc, PropError, Search, SimFiber, SimStep, StepCx, Wake, explore_cached,
    explore_dpor, explore_exhaustive, format_schedule, replay_schedule,
};

/// The exhaustive baseline's budget: programs above it are skipped. Local
/// stress runs (`VESPER_CRUCIBLE_DPOR_SEEDS`) use a larger one.
fn budget() -> u64 {
    if std::env::var("VESPER_CRUCIBLE_DPOR_SEEDS").is_ok() {
        20_000
    } else {
        4_000
    }
}

fn cfg(bound: Option<u32>) -> ExploreConfig {
    ExploreConfig {
        max_executions: budget(),
        preemption_bound: bound,
        ..ExploreConfig::default()
    }
}

fn observe(w: &Cells) -> Result<Cells, PropError> {
    Ok(w.clone())
}

type Run = Result<Exploration<Cells>, ExploreError>;

fn exhaustive(program: &[Vec<Op>], c: &ExploreConfig) -> Run {
    explore_exhaustive(c, || build(program), observe)
}

fn dpor(program: &[Vec<Op>], c: &ExploreConfig) -> Run {
    explore_dpor(c, || build(program), observe)
}

fn cached(search: Search, program: &[Vec<Op>], c: &ExploreConfig) -> Run {
    explore_cached(search, c, || build(program), observe, cells_key)
}

fn outcomes(e: &Exploration<Cells>) -> Vec<&Outcome<Cells>> {
    e.outcome_set().into_iter().collect()
}

/// Every reported schedule replays to its outcome.
fn schedules_replay(program: &[Vec<Op>], e: &Exploration<Cells>) {
    for (outcome, schedule) in &e.outcomes {
        let (w, f) = build(program);
        match replay_schedule(w, f, schedule, None, &ExploreConfig::default()) {
            Ok(trace) => assert_eq!(Ok(&trace.world), outcome.as_ref()),
            Err(PropError::Fail(f)) => {
                let failures = outcome.as_ref().expect_err("a failing schedule");
                assert!(failures.contains(&f), "{}", format_schedule(schedule));
            }
            Err(e) => panic!("replay: {e:?}"),
        }
    }
}

#[derive(Default)]
struct Totals {
    programs: u64,
    baseline: u64,
    reduced: u64,
}

impl Totals {
    fn add(&mut self, baseline: &Exploration<Cells>, reduced: &Exploration<Cells>) {
        self.programs += 1;
        self.baseline += baseline.executions;
        self.reduced += reduced.executions;
    }
}

fn seeds() -> (u64, usize) {
    match std::env::var("VESPER_CRUCIBLE_DPOR_SEEDS") {
        Ok(n) => (n.parse().expect("seed count"), 4),
        Err(_) => (100, 3),
    }
}

#[test]
fn spawn_cancel_and_chosen_results_dpor_matches_exhaustive() {
    let (seeds, max_ops) = seeds();
    let c = cfg(None);
    let mut t = Totals::default();
    for seed in 0..seeds {
        let program = random_program_ext(seed, 2 + (seed % 2) as usize, max_ops);
        let Ok(ex) = exhaustive(&program, &c) else {
            continue;
        };
        let dp = dpor(&program, &c).unwrap();
        assert_eq!(outcomes(&ex), outcomes(&dp), "seed {seed}: {program:?}");
        assert!(dp.executions <= ex.executions, "seed {seed}");
        if seed < 20 {
            schedules_replay(&program, &dp);
        }
        t.add(&ex, &dp);
    }
    println!(
        "spawn/cancel/choice: {} programs, exhaustive {} executions, dpor {}",
        t.programs, t.baseline, t.reduced
    );
    assert!(t.programs * 100 >= seeds * 60, "{} programs", t.programs);
    assert!(t.reduced * 3 < t.baseline);
}

#[test]
fn bounded_dpor_matches_bounded_exhaustive() {
    let (seeds, max_ops) = seeds();
    for bound in [0, 1, 2] {
        let c = cfg(Some(bound));
        let mut t = Totals::default();
        for seed in 0..seeds {
            let program = if seed % 2 == 0 {
                random_program(seed, 2 + (seed % 3) as usize, max_ops)
            } else {
                random_program_ext(seed, 2 + (seed % 3) as usize, max_ops)
            };
            let Ok(ex) = exhaustive(&program, &c) else {
                continue;
            };
            let dp = dpor(&program, &c).unwrap();
            assert_eq!(
                outcomes(&ex),
                outcomes(&dp),
                "bound {bound} seed {seed}: {program:?}"
            );
            t.add(&ex, &dp);
        }
        println!(
            "bound {bound}: {} programs, bounded exhaustive {} executions, bounded dpor {}",
            t.programs, t.baseline, t.reduced
        );
        assert!(t.programs * 100 >= seeds * 60);
        assert!(t.reduced < t.baseline);
    }
}

#[test]
fn the_bound_is_monotone_and_converges() {
    let (seeds, max_ops) = seeds();
    for seed in 0..seeds.min(60) {
        let program = random_program_ext(seed, 2, max_ops);
        let Ok(full) = exhaustive(&program, &cfg(None)) else {
            continue;
        };
        let mut previous = Vec::new();
        for bound in 0..=4 {
            let b = exhaustive(&program, &cfg(Some(bound))).unwrap();
            let now: Vec<_> = outcomes(&b).into_iter().cloned().collect();
            assert!(previous.iter().all(|o| now.contains(o)), "seed {seed}");
            assert!(b.executions <= full.executions);
            previous = now;
        }
        // Two fibers with at most four steps each need at most 4 + 1
        // preemptions... a bound of 8 covers every schedule.
        let wide = exhaustive(&program, &cfg(Some(8))).unwrap();
        assert_eq!(outcomes(&wide), outcomes(&full), "seed {seed}");
        assert_eq!(wide.executions, full.executions);
    }
}

#[test]
fn a_lost_update_needs_one_preemption() {
    // Read then write back in two steps: the lost update needs a switch
    // between them while the reader is still ready.
    let program = vec![
        vec![Op::Read(0), Op::Update(0)],
        vec![Op::Read(0), Op::Update(0)],
    ];
    let full = exhaustive(&program, &cfg(None)).unwrap();
    let none = exhaustive(&program, &cfg(Some(0))).unwrap();
    let one = dpor(&program, &cfg(Some(1))).unwrap();
    // Without preemptions only the two serial orders remain.
    assert_eq!(none.outcomes.len(), 2);
    assert!(none.bound_pruned > 0);
    assert_eq!(outcomes(&one), outcomes(&full));
    assert!(full.outcomes.len() > 2);
    println!(
        "lost update: unbounded {} executions / {} outcomes; bound 0: {}; bounded dpor(1): {}",
        full.executions,
        full.outcomes.len(),
        none.executions,
        one.executions
    );
}

#[test]
fn state_caching_matches_and_saves_work() {
    let (seeds, max_ops) = seeds();
    let mut ex_t = Totals::default();
    let mut dp_t = Totals::default();
    let mut transitions = (0u64, 0u64);
    for seed in 0..seeds {
        let program = if seed % 2 == 0 {
            random_program(seed, 2 + (seed % 2) as usize, max_ops)
        } else {
            random_program_ext(seed, 2 + (seed % 2) as usize, max_ops)
        };
        let bound = (seed % 3 == 0).then_some(1);
        let c = cfg(bound);
        let Ok(ex) = exhaustive(&program, &c) else {
            continue;
        };
        let ex_cached = cached(Search::Exhaustive, &program, &c).unwrap();
        assert_eq!(outcomes(&ex), outcomes(&ex_cached), "seed {seed}");
        assert!(ex_cached.executions <= ex.executions);
        let dp = dpor(&program, &c).unwrap();
        let dp_cached = cached(Search::Dpor, &program, &c).unwrap();
        assert_eq!(
            outcomes(&ex),
            outcomes(&dp_cached),
            "seed {seed} bound {bound:?}: {program:?}"
        );
        if seed < 20 {
            schedules_replay(&program, &dp_cached);
        }
        ex_t.add(&ex, &ex_cached);
        dp_t.add(&dp, &dp_cached);
        transitions.0 += ex.transitions;
        transitions.1 += ex_cached.transitions;
    }
    println!(
        "caching: {} programs; exhaustive {} -> cached {} executions ({} -> {} transitions); \
         dpor {} -> cached dpor {}",
        ex_t.programs,
        ex_t.baseline,
        ex_t.reduced,
        transitions.0,
        transitions.1,
        dp_t.baseline,
        dp_t.reduced
    );
    assert!(ex_t.reduced < ex_t.baseline);
    assert!(transitions.1 < transitions.0);
}

#[test]
fn caching_collapses_independent_interleavings() {
    let program = vec![
        vec![Op::Write(0), Op::Update(0), Op::Read(0)],
        vec![Op::Write(1), Op::Update(1), Op::Read(1)],
        vec![Op::Write(2), Op::Update(2), Op::Read(2)],
    ];
    let c = ExploreConfig::default();
    let ex = exhaustive(&program, &c).unwrap();
    let ec = cached(Search::Exhaustive, &program, &c).unwrap();
    assert_eq!(ex.executions, 1680);
    assert_eq!(outcomes(&ex), outcomes(&ec));
    // One execution per final state reached first; the rest hit the cache.
    assert_eq!(ec.executions, 1);
    assert!(ec.cache_hits > 0);
    println!(
        "3x3 independent: exhaustive {} executions / {} transitions, cached {} executions / {} \
         transitions / {} hits",
        ex.executions, ex.transitions, ec.executions, ec.transitions, ec.cache_hits
    );
    assert!(ec.transitions * 10 < ex.transitions);
}

// ---------------------------------------------------------------------------
// Chosen completion values.

/// Parks on a read whose result the explorer chooses; fails on an error.
struct Reader {
    pc: u8,
}

impl SimFiber<Vec<String>> for Reader {
    fn footprint(&self) -> Footprint {
        Footprint::none().write(0)
    }
    fn step(&mut self, w: &mut Vec<String>, cx: &mut StepCx) -> Result<SimStep, PropError> {
        cx.write(0);
        self.pc += 1;
        match cx.reason() {
            Wake::Start => Ok(SimStep::Io(IoRequest::ExternalChoice(
                IoChoices::new(&[
                    IoResult::Success { bytes: 64 },
                    IoResult::Error { code: -5 },
                    IoResult::Cancelled,
                ])
                .unwrap(),
            ))),
            Wake::ExternalIo { result, .. } => {
                w.push(format!("{result:?}"));
                if let IoResult::Error { code } = result {
                    return Err(PropError::fail("io-error", format!("read failed: {code}")));
                }
                Ok(SimStep::Done)
            }
            other => panic!("unexpected wake {other:?}"),
        }
    }
}

#[test]
fn the_explorer_chooses_completion_values() {
    let setup = || (Vec::new(), vec![Reader { pc: 0 }, Reader { pc: 0 }]);
    let ex = explore_exhaustive(&ExploreConfig::default(), setup, |w| Ok(w.clone())).unwrap();
    let dp = explore_dpor(&ExploreConfig::default(), setup, |w| Ok(w.clone())).unwrap();
    assert_eq!(ex.outcome_set(), dp.outcome_set());
    let (failure, schedule) = dp.first_failure().expect("an error result fails");
    assert_eq!(failure.kind, "io-error");
    assert!(
        schedule
            .iter()
            .any(|p| matches!(p, Proc::Io { choice: 1, .. })),
        "{}",
        format_schedule(schedule)
    );
    // Successful outcomes: each reader got success or cancelled, in either
    // delivery order where they differ: {S,C}^2 ordered = 4 outcomes.
    let ok = ex.outcomes.keys().filter(|o| o.is_ok()).count();
    assert_eq!(ok, 4);
    println!(
        "chosen results: exhaustive {} executions, dpor {}, {} outcomes, failing schedule {}",
        ex.executions,
        dp.executions,
        ex.outcomes.len(),
        format_schedule(schedule)
    );
    assert!(dp.executions < ex.executions);
}

#[test]
fn io_choice_sets_are_validated() {
    assert_eq!(IoChoices::new(&[]), Err(IoChoicesError::Empty));
    let r = IoResult::Success { bytes: 1 };
    assert_eq!(IoChoices::new(&[r, r]), Err(IoChoicesError::Duplicate(r)));
    let many: Vec<IoResult> = (0..9).map(|b| IoResult::Success { bytes: b }).collect();
    assert_eq!(
        IoChoices::new(&many),
        Err(IoChoicesError::TooMany { len: 9 })
    );
    assert_eq!(IoChoices::new(&many[..8]).unwrap().len(), 8);
}

// ---------------------------------------------------------------------------
// Spawning and cancellation.

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Pool {
    /// Work units done by workers.
    done: u32,
    /// Workers that observed cancellation.
    cancelled: u32,
}

enum Task {
    /// Spawns `workers` workers, then cancels the first one.
    Supervisor {
        pc: usize,
        workers: usize,
        spawned: Vec<usize>,
        child: Option<Box<Task>>,
    },
    /// Does `units` units of work, one per step, unless cancelled.
    Worker { left: u32 },
}

const DONE: u64 = 0;
const CANCELLED: u64 = 1;

impl SimFiber<Pool> for Task {
    fn footprint(&self) -> Footprint {
        match self {
            Task::Supervisor {
                pc,
                workers,
                spawned,
                ..
            } => {
                if pc < workers {
                    Footprint::none().spawn()
                } else {
                    spawned
                        .first()
                        .map_or(Footprint::none(), |&w| Footprint::none().cancel(w))
                }
            }
            Task::Worker { .. } => Footprint::none().write(DONE).write(CANCELLED),
        }
    }

    fn step(&mut self, w: &mut Pool, cx: &mut StepCx) -> Result<SimStep, PropError> {
        match self {
            Task::Supervisor {
                pc,
                workers,
                spawned,
                child,
            } => {
                if *pc < *workers {
                    *pc += 1;
                    spawned.push(cx.spawn());
                    *child = Some(Box::new(Task::Worker { left: 2 }));
                    return Ok(SimStep::Yield);
                }
                if let Some(&first) = spawned.first() {
                    cx.request_cancel(first);
                }
                Ok(SimStep::Done)
            }
            Task::Worker { left } => {
                if cx.cancel_requested() {
                    cx.write(CANCELLED);
                    w.cancelled += 1;
                    return Ok(SimStep::Done);
                }
                cx.write(DONE);
                w.done += 1;
                *left -= 1;
                Ok(if *left == 0 {
                    SimStep::Done
                } else {
                    SimStep::Yield
                })
            }
        }
    }

    fn take_spawned(&mut self) -> Option<Self> {
        match self {
            Task::Supervisor { child, .. } => child.take().map(|b| *b),
            Task::Worker { .. } => None,
        }
    }
}

fn pool() -> (Pool, Vec<Task>) {
    (
        Pool::default(),
        vec![Task::Supervisor {
            pc: 0,
            workers: 2,
            spawned: Vec::new(),
            child: None,
        }],
    )
}

#[test]
fn spawned_workers_and_cancellation_are_explored() {
    let c = ExploreConfig::default();
    let ex = explore_exhaustive(&c, pool, |w| Ok(w.clone())).unwrap();
    let dp = explore_dpor(&c, pool, |w| Ok(w.clone())).unwrap();
    assert_eq!(ex.outcome_set(), dp.outcome_set());
    let got: Vec<Pool> = ex.outcomes.keys().map(|o| o.clone().unwrap()).collect();
    // Worker 1 was cancelled before any work, after one unit, or finished
    // first (the cancellation is then ignored); worker 2 always does 2.
    assert_eq!(
        got,
        vec![
            Pool {
                done: 2,
                cancelled: 1
            },
            Pool {
                done: 3,
                cancelled: 1
            },
            Pool {
                done: 4,
                cancelled: 0
            },
        ]
    );
    println!(
        "spawn/cancel pool: exhaustive {} executions, dpor {}",
        ex.executions, dp.executions
    );
    assert!(dp.executions < ex.executions);
    for (outcome, schedule) in &dp.outcomes {
        let (w, f) = pool();
        let trace = replay_schedule(w, f, schedule, None, &c).unwrap();
        assert_eq!(Ok(&trace.world), outcome.as_ref());
        assert!(schedule.contains(&Proc::Task(2)));
    }
}

/// A misbehaving fiber for the typed-error tests.
enum Bad {
    CancelsGhost,
    SpawnsWithoutChild,
    SpawnsUndeclared,
    SpawnsForever,
}

impl SimFiber<()> for Bad {
    fn footprint(&self) -> Footprint {
        match self {
            Bad::CancelsGhost => Footprint::none().cancel(9),
            Bad::SpawnsUndeclared => Footprint::none(),
            _ => Footprint::none().spawn(),
        }
    }
    fn step(&mut self, _: &mut (), cx: &mut StepCx) -> Result<SimStep, PropError> {
        match self {
            Bad::CancelsGhost => cx.request_cancel(9),
            _ => {
                cx.spawn();
            }
        }
        Ok(SimStep::Done)
    }
    fn take_spawned(&mut self) -> Option<Self> {
        match self {
            Bad::SpawnsForever => Some(Bad::SpawnsForever),
            _ => None,
        }
    }
}

#[test]
fn spawn_and_cancel_misuse_are_typed_errors() {
    let c = ExploreConfig {
        max_fibers: 4,
        ..ExploreConfig::default()
    };
    let run = |bad: fn() -> Bad| explore_dpor(&c, || ((), vec![bad()]), |_| Ok(())).unwrap_err();
    assert_eq!(
        run(|| Bad::CancelsGhost),
        ExploreError::UnknownFiber { task: 0, target: 9 }
    );
    assert_eq!(
        run(|| Bad::SpawnsWithoutChild),
        ExploreError::SpawnMismatch {
            task: 0,
            expected: 1,
            got: 0
        }
    );
    assert!(matches!(
        run(|| Bad::SpawnsUndeclared),
        ExploreError::Footprint { task: 0, .. }
    ));
    assert_eq!(
        run(|| Bad::SpawnsForever),
        ExploreError::FiberLimit { limit: 4 }
    );
}
