# vesper-crucible

Vesper Crucible (design §29, excluding §29.3 `vesper migrate`): choice-tape
property testing, coverage-guided fuzzing with a comparison-operand solver,
SimPulse schedule exploration, `refute`, and a termination-checked shrinker.

```rust
use vesper_crucible::{fuzz, FuzzConfig, PropError, Source};

let report = fuzz(&FuzzConfig::default(), |src: &mut Source| {
    let data = src.draw_bytes(64)?;
    if data.len() >= 4 {
        let word = u32::from_le_bytes(data[..4].try_into().unwrap());
        if src.probe().eq(1, u64::from(word), 0xfeed_f00d) {
            return Err(PropError::fail("magic", "reached"));
        }
    }
    Ok(())
});
let cx = report.counterexample.expect("solver pierces the guard");
println!("{}", cx.tape.to_text()); // save as a regression tape
```

- Every drawn value and every SimPulse run/defer decision is one `u64` on a
  `ChoiceTape`. `Source::replay(&tape)` reproduces a run exactly; the text
  format (`vesper-crucible-tape/1`) is stable.
- `shrink` reduces the tape (span deletion, zeroing, value lowering) and keeps
  only candidates that still fail the same way, are generator-valid, and are
  strictly smaller in the `(len, sum)` order. `lean/CrucibleShrink.lean`
  proves that order well-founded and the modelled loop terminating.
- `fuzz` keeps coverage-novel runs (edges, comparison outcomes, operand
  closeness) and solves comparisons by bounded input-to-state replacement.
  It is not an SMT solver.
- `run_fibers` explores schedules of step-machine fibers on the real
  `vesper-simpulse` simulator; `replay_fibers` re-validates a SimPulse
  journal.
- `SimFiber` step machines may also sleep on SimPulse timers, submit I/O
  SimPulse completes after a virtual delay, or park on external I/O whose
  delivery the explorer schedules. `explore_random` draws each scheduling
  decision (step, deliver, fire timer) from the choice tape, so a planted
  reply-after-timeout race is fuzzed and shrunk like any other bug;
  `replay_schedule` re-validates the SimPulse journal.
- `explore_exhaustive` enumerates every schedule; `explore_dpor` explores at
  least one per Mazurkiewicz trace (persistent-set DPOR with sleep sets over
  declared step footprints). Tests check both find the same outcomes.
- `VirtualArena`: a 64 KiB `vesper-alloc` region reset after every
  execution, with `ArbitraryIn` generators that draw straight into it and a
  reusable source; warmed-up executions do not allocate (counted).

DPOR's reduction is only as sound as the declared footprints (an access
outside one is a typed error) and is validated differentially, not proved.

Not here: Cranelift instrumentation of Vesper source, the `vesper fuzz` CLI,
MPK differential shadowing (lane 11 / integration), and the source-level
`FuzzSource` effect. Rust ownership in this crate is not an implementation of
Vesper's source typing rules.
