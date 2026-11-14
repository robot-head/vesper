//! Acceptance: tape-level shrinking keeps linear protocol sequences valid
//! (WriteTxn begin/write/commit/abort), shrinks a SimPulse interleaving bug
//! to a short schedule, and replays deterministically.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use vesper_crucible::{
    ChoiceTape, Fiber, FiberStep, FuzzConfig, Invalid, PropError, ShrinkConfig, SimConfig, SimRun,
    Source, Verdict, fuzz, replay, replay_fibers, run_fibers, shrink,
};

// ---------------------------------------------------------------------------
// WriteTxn: a linear obligation whose only exits are commit and abort.

#[derive(Default)]
struct Auditor {
    leaked: Cell<u64>,
    opened: Cell<u64>,
    closed: Cell<u64>,
}

/// Non-Clone, non-Copy, must-use owner. Dropping it without commit/abort is
/// counted as a leak by the auditor, never silently released.
#[must_use = "a WriteTxn must be committed or aborted"]
struct WriteTxn {
    writes: Vec<(u8, u8)>,
    live: bool,
    auditor: Rc<Auditor>,
}

impl WriteTxn {
    fn put(&mut self, key: u8, value: u8) {
        self.writes.push((key, value));
    }
    fn close(mut self) -> Vec<(u8, u8)> {
        self.live = false;
        self.auditor.closed.set(self.auditor.closed.get() + 1);
        std::mem::take(&mut self.writes)
    }
}

impl Drop for WriteTxn {
    fn drop(&mut self) {
        if self.live {
            self.auditor.leaked.set(self.auditor.leaked.get() + 1);
        }
    }
}

struct Store {
    data: BTreeMap<u8, u8>,
    auditor: Rc<Auditor>,
}

impl Store {
    fn begin(&mut self) -> WriteTxn {
        self.auditor.opened.set(self.auditor.opened.get() + 1);
        WriteTxn {
            writes: Vec::new(),
            live: true,
            auditor: self.auditor.clone(),
        }
    }
    /// Planted bug: a transaction with three or more writes loses its last.
    fn commit(&mut self, txn: WriteTxn) {
        let mut writes = txn.close();
        if writes.len() >= 3 {
            writes.pop();
        }
        self.data.extend(writes);
    }
    fn abort(&mut self, txn: WriteTxn) {
        let _ = txn.close();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Read(u8),
    Begin,
    Write(u8, u8),
    Commit,
    Abort,
}

/// Generates only legal sequences: the alternatives offered depend on
/// whether a transaction is open, and an open one is aborted at the end.
/// Nothing here knows about shrinking.
fn gen_ops(src: &mut Source) -> Result<Vec<Op>, Invalid> {
    let mut open = false;
    // One continuation flag per op, immediately followed by the op's own
    // choices, so each op is a contiguous span of the tape.
    let mut ops = src.draw_vec(24, |src| {
        let op = if open {
            match src.draw_index(3)? {
                0 => Op::Write(src.draw_u64_bounded(3)? as u8, src.draw_u8()?),
                1 => Op::Commit,
                _ => Op::Abort,
            }
        } else {
            match src.draw_index(2)? {
                0 => Op::Read(src.draw_u64_bounded(3)? as u8),
                _ => Op::Begin,
            }
        };
        open = match op {
            Op::Begin => true,
            Op::Commit | Op::Abort => false,
            _ => open,
        };
        Ok(op)
    })?;
    if open {
        ops.push(Op::Abort);
    }
    Ok(ops)
}

/// Independent protocol checker (not used by the generator).
fn protocol_valid(ops: &[Op]) -> bool {
    let mut open = false;
    for op in ops {
        match (open, op) {
            (false, Op::Begin) => open = true,
            (false, Op::Read(_)) => {}
            (true, Op::Write(..)) => {}
            (true, Op::Commit | Op::Abort) => open = false,
            _ => return false,
        }
    }
    !open
}

fn execute_ops(ops: &[Op], auditor: &Rc<Auditor>) -> Result<(), PropError> {
    let mut store = Store {
        data: BTreeMap::new(),
        auditor: auditor.clone(),
    };
    let mut model = BTreeMap::new();
    let mut txn: Option<WriteTxn> = None;
    for op in ops {
        match *op {
            Op::Begin => txn = Some(store.begin()),
            Op::Write(k, v) => txn.as_mut().expect("valid").put(k, v),
            Op::Commit => {
                let t = txn.take().expect("valid");
                model.extend(t.writes.iter().copied());
                store.commit(t);
            }
            Op::Abort => store.abort(txn.take().expect("valid")),
            Op::Read(k) => {
                if store.data.get(&k) != model.get(&k) {
                    return Err(PropError::fail(
                        "lost-write",
                        format!(
                            "key {k}: store {:?}, model {:?}",
                            store.data.get(&k),
                            model.get(&k)
                        ),
                    ));
                }
            }
        }
    }
    assert!(txn.is_none());
    Ok(())
}

#[test]
fn write_txn_shrinking_preserves_protocol_validity() {
    let auditor = Rc::new(Auditor::default());
    let invalid_sequences = Rc::new(Cell::new(0u64));
    let executed = Rc::new(Cell::new(0u64));
    let (a, bad, n) = (auditor.clone(), invalid_sequences.clone(), executed.clone());
    let prop = move |src: &mut Source| {
        let ops = gen_ops(src)?;
        n.set(n.get() + 1);
        if !protocol_valid(&ops) {
            bad.set(bad.get() + 1);
            return Err(PropError::fail("invalid-protocol", format!("{ops:?}")));
        }
        execute_ops(&ops, &a)
    };
    let mut prop2 = prop.clone();
    let report = fuzz(&FuzzConfig::default(), prop);
    let cx = report.counterexample.expect("planted lost-write bug found");
    assert_eq!(cx.failure.kind, "lost-write");
    let original = gen_ops(&mut Source::replay(&cx.original)).unwrap();
    let shrunk = gen_ops(&mut Source::replay(&cx.tape)).unwrap();
    eprintln!(
        "WriteTxn: original {} ops, shrunk {} ops {:?}; {} sequences executed \
         ({} during shrinking), {} invalid, {} leaked",
        original.len(),
        shrunk.len(),
        shrunk,
        executed.get(),
        cx.shrink.runs,
        invalid_sequences.get(),
        auditor.leaked.get()
    );
    // Every sequence the fuzzer and the shrinker ran was protocol-valid,
    // and every transaction opened was closed (no leaked obligations).
    assert_eq!(invalid_sequences.get(), 0);
    assert_eq!(auditor.leaked.get(), 0);
    assert_eq!(auditor.opened.get(), auditor.closed.get());
    assert!(protocol_valid(&shrunk));
    // Minimal shape: Begin, three writes, Commit, Read of the lost key.
    assert_eq!(shrunk.len(), 6, "{shrunk:?}");
    assert_eq!(shrunk[0], Op::Begin);
    assert_eq!(shrunk[4], Op::Commit);
    assert!(matches!(shrunk[5], Op::Read(_)));
    assert!(shrunk.len() <= original.len());
    assert!(matches!(replay(&mut prop2, &cx.tape), Verdict::Fail(f) if f.kind == "lost-write"));
}

#[test]
fn type_blind_list_shrinking_would_break_the_protocol() {
    // Contrast: shrinking the decoded op list by deleting elements (what a
    // type-directed Vec shrinker does) produces illegal sequences.
    let auditor = Rc::new(Auditor::default());
    let report = fuzz(&FuzzConfig::default(), |src: &mut Source| {
        execute_ops(&gen_ops(src)?, &auditor)
    });
    let original = gen_ops(&mut Source::replay(
        &report.counterexample.unwrap().original,
    ))
    .unwrap();
    let illegal = (0..original.len())
        .filter(|&i| {
            let mut c = original.clone();
            c.remove(i);
            !protocol_valid(&c)
        })
        .count();
    eprintln!(
        "type-blind deletion: {illegal} of {} single-op deletions are illegal",
        original.len()
    );
    assert!(illegal > 0);
}

// ---------------------------------------------------------------------------
// SimPulse: a lost-update race between two read-then-write incrementers.

struct World {
    counter: u64,
    noise: u64,
}

enum Task {
    /// `pre` padding steps, then read, then write read+1.
    Inc {
        pre: u32,
        read: Option<u64>,
    },
    Noise {
        left: u32,
    },
}

impl Fiber<World> for Task {
    fn step(&mut self, world: &mut World) -> Result<FiberStep, PropError> {
        match self {
            Task::Inc { pre, read } => {
                if *pre > 0 {
                    *pre -= 1;
                    return Ok(FiberStep::Yield);
                }
                match read.take() {
                    None => {
                        *read = Some(world.counter);
                        Ok(FiberStep::Yield)
                    }
                    Some(seen) => {
                        world.counter = seen + 1;
                        Ok(FiberStep::Done)
                    }
                }
            }
            Task::Noise { left } => {
                world.noise += 1;
                if *left == 0 {
                    Ok(FiberStep::Done)
                } else {
                    *left -= 1;
                    Ok(FiberStep::Yield)
                }
            }
        }
    }
}

fn race_prop(
    last: Rc<RefCell<Option<SimRun>>>,
) -> impl FnMut(&mut Source) -> Result<(), PropError> + Clone {
    move |src: &mut Source| {
        let noise = src.draw_u64_bounded(4)?;
        let mut fibers = vec![
            Task::Inc { pre: 0, read: None },
            Task::Inc { pre: 2, read: None },
        ];
        for _ in 0..noise {
            fibers.push(Task::Noise {
                left: src.draw_u64_bounded(6)? as u32,
            });
        }
        let mut world = World {
            counter: 0,
            noise: 0,
        };
        let run = run_fibers(src, &mut world, fibers, &SimConfig::default())?;
        *last.borrow_mut() = Some(run);
        if world.counter != 2 {
            return Err(PropError::fail(
                "lost-update",
                format!("counter {}", world.counter),
            ));
        }
        Ok(())
    }
}

fn schedule_of<P: FnMut(&mut Source) -> Result<(), PropError>>(
    tape: &ChoiceTape,
    prop: &mut P,
    last: &Rc<RefCell<Option<SimRun>>>,
) -> SimRun {
    let verdict = replay(prop, tape);
    assert!(
        matches!(verdict, Verdict::Fail(ref f) if f.kind == "lost-update"),
        "{verdict:?}"
    );
    last.borrow().clone().expect("run recorded")
}

#[test]
fn simpulse_interleaving_bug_shrinks_to_short_schedule_and_replays() {
    let last = Rc::new(RefCell::new(None));
    let mut prop = race_prop(last.clone());
    let report = fuzz(&FuzzConfig::default(), prop.clone());
    let cx = report.counterexample.expect("race found");
    assert_eq!(cx.failure.kind, "lost-update");

    let schedule = |tape: &ChoiceTape, prop: &mut _| schedule_of(tape, prop, &last);
    let before = schedule(&cx.original, &mut prop);
    let after = schedule(&cx.tape, &mut prop);
    eprintln!(
        "SimPulse race: found after {} execs; schedule {} dispatches ({} deferred) -> {} dispatches \
         ({} deferred), tape {} -> {} choices, seed {} -> {}",
        cx.found_after_execs,
        before.dispatches,
        before.deferrals,
        after.dispatches,
        after.deferrals,
        cx.original.len(),
        cx.tape.len(),
        before.seed,
        after.seed,
    );
    // Noise fibers removed: just the two incrementers (six steps). Without a
    // deferral the windows cannot overlap; with one, the first incrementer
    // reads in round two and writes in round three, after the second reads
    // there (round order from seed 0). Seven dispatches is the shortest
    // losing schedule.
    assert_eq!(cx.tape.values[0], 0, "noise fibers shrunk away");
    assert_eq!(after.seed, 0);
    assert_eq!(after.deferrals, 1);
    assert_eq!(after.dispatches, 7);
    assert!(after.dispatches <= before.dispatches);

    // Deterministic replay: same tape, same SimPulse journal, every time.
    for _ in 0..3 {
        assert_eq!(schedule(&cx.tape, &mut prop).journal, after.journal);
    }
    // And SimPulse's own journal replay validates every transition.
    let mut src = Source::replay(&cx.tape);
    let noise = src.draw_u64_bounded(4).unwrap();
    assert_eq!(noise, 0);
    let mut world = World {
        counter: 0,
        noise: 0,
    };
    let fibers = vec![
        Task::Inc { pre: 0, read: None },
        Task::Inc { pre: 2, read: None },
    ];
    let replayed = replay_fibers(
        &mut src,
        &mut world,
        fibers,
        after.journal.clone(),
        &SimConfig::default(),
    )
    .expect("journal replay validates");
    assert_eq!(replayed.journal, after.journal);
    assert_eq!(world.counter, 1);
    // A mismatching journal is rejected, not silently accepted.
    let mut src = Source::replay(&cx.tape);
    src.draw_u64_bounded(4).unwrap();
    let mut wrong = after.journal.clone();
    wrong.events.pop();
    let fibers = vec![
        Task::Inc { pre: 0, read: None },
        Task::Inc { pre: 2, read: None },
    ];
    let err = replay_fibers(&mut src, &mut world, fibers, wrong, &SimConfig::default());
    assert!(matches!(err, Err(PropError::Fail(f)) if f.kind == "simpulse"));
}

// ---------------------------------------------------------------------------
// Shrinker contract.

#[test]
fn shrink_rejects_a_passing_tape() {
    let mut prop = |src: &mut Source| {
        src.draw_u8()?;
        Ok(())
    };
    let err = shrink(
        &mut prop,
        &ChoiceTape::new(vec![3]),
        &ShrinkConfig::default(),
    )
    .unwrap_err();
    assert_eq!(err.0, Verdict::Pass);
}

#[test]
fn shrink_never_grows_and_respects_budget() {
    let mut rng = vesper_crucible::SplitMix64(9);
    for round in 0..200u64 {
        let tape = ChoiceTape::new(
            (0..(rng.index(40) + 1))
                .map(|_| rng.next_u64() >> rng.index(64))
                .collect(),
        );
        // Fails when the sum of the first few draws is odd: many local minima.
        let mut prop = |src: &mut Source| {
            let n = src.draw_u64_bounded(5)?;
            let mut sum = 0u64;
            for _ in 0..n {
                sum = sum.wrapping_add(src.draw_u64()?);
            }
            if sum % 2 == 1 {
                return Err(PropError::fail("odd", sum.to_string()));
            }
            Ok(())
        };
        let budget = if round % 2 == 0 { 15 } else { 5_000 };
        if let Ok(s) = shrink(
            &mut prop,
            &tape,
            &ShrinkConfig {
                max_runs: budget,
                ..ShrinkConfig::default()
            },
        ) {
            assert!(s.stats.runs <= budget);
            assert!(
                s.stats.shrunk == s.stats.initial || s.stats.shrunk.smaller_than(s.stats.initial)
            );
            assert!(matches!(replay(&mut prop, &s.tape), Verdict::Fail(_)));
            if budget == 5_000 {
                assert!(!s.stats.budget_exhausted);
                // Locally minimal: one odd draw of value 1 after n = 1.
                assert_eq!(s.tape.values, vec![1, 1]);
            }
        }
    }
}

#[test]
fn strict_replay_overrun_is_invalid_not_success() {
    let mut prop = |src: &mut Source| {
        src.draw_u64()?;
        src.draw_u64()?;
        Ok(())
    };
    assert_eq!(
        replay(&mut prop, &ChoiceTape::new(vec![1])),
        Verdict::Invalid(Invalid::Overrun)
    );
}
