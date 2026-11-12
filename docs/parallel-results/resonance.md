# Wave five lane 8: Vesper Resonance (§32)

Design coverage: §32.1 items 1–5, §32.2 (`RtParamEventWire`, `RtOutputLease`,
`RtRetirementSink`, the `RtBus` operations), §32.3 (SVF voice bank, realtime
quantum, 1 kHz torque loop), §32.4 (SPSC and PDC proofs), §32.5 (verifier and
PDC compiler). Claim issue #128.

- Base: `af03122ea74d403f203170926945f9d35786b289` (origin/main).
- Branch: `claude/project-thread-i3wayw`.
- New crate `crates/vesper-resonance`, new `lean/ResonanceSpsc.lean`, this
  report, and a separate root registration commit.

## What was built

### Realtime confinement verifier (`confine`)

- `CallGraph { functions: Vec<FnNode>, calls: Vec<(FnId, FnId)>,
  indirect_calls: Vec<IndirectCall> }` with `FnId(u32)` and per-function
  `frame_bytes` (wave-five shared vocabulary). `FnNode` carries the
  function's *own* declared effects (`Effect::{Realtime, ConstDenorm, RtBus,
  Isa, Mmio, WaitFreeAtomic, Alloc, Dealloc, BlockingLock, BlockingSyscall,
  Async, Net, Storage, Unwind, Ffi, Other(String)}`), a `known` flag and a
  `LoopBound`.
- `RealtimePolicy { roots: Vec<RealtimeRoot { function, stack_budget_bytes }>,
  allowed_effects, indirect_targets, reject_unbounded_loops,
  max_loop_iterations }` carries what the source cannot express yet.
- `verify_realtime(&CallGraph, &RealtimePolicy) -> Result<Vec<VerifierFinding>,
  GraphError>`. For each root it computes the reachable set (direct edges
  plus policy-resolved indirect edges, BFS, shortest call path kept) and
  reports, with the call path in the message:
  `rt-forbidden-effect`, `rt-unknown-callee`, `rt-unresolved-indirect-call`,
  `rt-unbounded-loop`, `rt-loop-bound`, `rt-recursion` and
  `rt-stack-budget` (worst-case stack depth = deepest frame-byte path,
  iterative DFS, checked sums). Malformed graphs (dangling ids, targets for
  sites that do not exist) are `GraphError`, never success.
- This is transitive reachability over effects. The design's sketched
  `verify_realtime_effect_row` checks one effect row against a name list;
  `clean_own_effect_row_does_not_hide_transitive_lock` is a counterexample
  that passes such a check (clean root row, harmless names) and is rejected
  here (expansion-37 review correction).

### Cranelift input (`clif`)

`call_graph_from_clif(&[(String, ir::Function)], &SymbolEffects)` builds the
same graph from real Cranelift 0.125 IR: `call`, `return_call`, `try_call`
become direct edges (callee resolved through `UserExternalName` → name table,
`TestCase` names, or `LibCall` names); `call_indirect`,
`return_call_indirect`, `try_call_indirect` become indirect sites keyed by
layout instruction index; loops come from Cranelift's own `LoopAnalysis`
(unbounded unless `SymbolEffects::loop_bounds` attests a bound); frame bytes
are the sum of sized stack slots. Undeclared external symbols, including
every libcall, are unknown callees. Findings carry the root's call
instruction index that starts the offending path.

### Delay-compensated DAG compiler (`pdc`)

- `NodeSpec { id, latency, inputs: Vec<Input> }` with
  `Input::Forward(NodeId)` and `Input::Feedback { from, delay }`.
- `compile_schedule` → `Schedule { ops, lines, buffer_count }`: deterministic
  Kahn order, `arrival = max` forward input latency, compensation
  `arrival - out(src)` on every forward input, `out = arrival + intrinsic`.
  Rejected with typed errors: forward cycles (`CycleWithoutDelay` lists the
  stuck nodes), zero-delay feedback, duplicate/unknown nodes, latency
  overflow (u64 sums, u32 delay lines).
- Buffer liveness: linear scan over the schedule; outputs are freed after
  their last forward reader, compensation/feedback scratch after their op,
  sink outputs live to the end. A 10-node chain uses 2 buffers.
- `CompiledGraph` preallocates buffers and `DelayLine`s (capacity
  `delay + block`, so a block read and write never overlap in either order)
  and runs blocks without allocating. Block execution requires every
  feedback delay ≥ block length (`FeedbackShorterThanBlock`); delay lines
  longer than `MAX_DELAY_SAMPLES` (2^24) are `DelayTooLong`.

### Wait-free SPSC handoff (`spsc`) and leases (`lease`)

- The ring is `rtrb` 0.4.1 (pinned `=0.4.1`), a widely used wait-free SPSC
  ring: no locks, syscalls or allocation after construction. The lane's
  unsafe allowance covers only the FTZ/DAZ register, so the ring is not
  hand-rolled.
- `retirement_channel(cap) -> (RtRetirementSink<T>, RtJanitor<T>)`:
  `RtRetirementSink::retire(T) -> Result<(), T>` moves the value in O(1) and
  hands it back when full; the janitor drains (drops) on its own thread.
- `mailbox(cap) -> (MailboxSender<T>, MailboxReceiver<T>)`;
  `MailboxReceiver::try_swap_in_latest(&mut T, &mut RtRetirementSink<T>)`
  swaps in queued states only while the sink has a free slot, retiring each
  superseded state, so the realtime thread never drops one.
- The design's `copy struct RtRetirementSink` would permit two producers on
  one SPSC ring; here it is a unique handle (recorded correction).
- `RtOutputPort` / `RtBufferLease` (the design's `RtOutputLease`; contract
  name kept): preallocated periods; frames must be written in order;
  `commit` succeeds only when every frame is written (otherwise the lease is
  handed back); `emergency_silence_and_commit` zero-fills the rest; a lease
  from another port is refused; a dropped lease is counted by
  `LeaseAuditor::leaked`. Non-Clone, non-Copy, `#[must_use]`.

### Events (`events`) and kernels (`kernels`)

- `RtParamEvent` encodes/decodes the stated 16-byte little-endian
  `RtParamEventWire` (the fields add up to 16 bytes, so the layout is kept).
  `decode_events` writes into a caller buffer (no allocation) and rejects
  trailing bytes, over-budget counts and non-finite targets.
- `process_with_events` validates the whole event list first (offset inside
  the block, non-decreasing, known parameter), then splits the block at each
  distinct offset and applies events at exact sample offsets. `ParamRamp`
  ramps linearly and lands exactly on the target. The design's
  `.min(64)` clamp is replaced by an `OffsetOutOfBlock` error.
- `SvfBank`: 16-voice AoSoA `SvfVoiceTile` (64-byte-aligned hot lines),
  TPT state-variable low-pass, per-sample ramps of gain, `g` and `k`, equal
  pan to stereo.
- `SynthEngine::process_quantum`: lease a period, swap in the latest
  `Box<VoicePatchBank>` and retire the old one, decode ≤ 32 events, split and
  render, write the lease, commit; malformed events emergency-silence the
  period and return a typed error.
- `PidController`: 1 kHz PID with derivative on measurement, integrator
  anti-windup clamp and torque clamp; non-finite inputs and bad gains are
  typed faults (caller commands safe torque).

### ConstDenorm scope (`denorm`)

`with_const_denorm(f)` sets MXCSR.FTZ|DAZ (x86_64) or FPCR.FZ (AArch64) with
inline assembly, runs `f`, and restores the caller's exact control word on
exit, including on panic. `availability()` reports `DenormUnavailable` on
other architectures. This module is the crate's only `unsafe` (the crate is
`#![deny(unsafe_code)]`; the module opts in with SAFETY comments).

## Lean (`lean/ResonanceSpsc.lean`)

Model `vesper.resonance.spsc-pdc-model/1`; 11 theorems, no `sorry`/`admit`/
axioms, standard axioms only (checked by `tests/lean_model.rs` under
`VESPER_LEAN`).

- SPSC: `spsc_ring_occupancy_bounded` (§32.4), `slot_distinct`,
  `producer_slot_not_readable`, and a concurrent model where each side's
  access is split into start/end steps that interleave arbitrarily:
  `step_preserves`, `reachable_inv`, and `spsc_no_shared_slot` — in every
  reachable state, if both sides are inside their slot access, the slots
  differ.
- PDC: `pdc_equalizes_parallel_branch_latency` (§32.4), `le_maxList`,
  `compiled_is_compensated` (the compiler's max/sum equations give a
  compensated DAG), `path_latency` (every path `s → t` has latency
  `out t - out s`) and `pdc_paths_equal` (all paths between two nodes have
  equal latency after compensation).

These are theorems about the models. They are not proofs about the Rust code
or about `rtrb`; the Rust tests check the same properties on concrete runs
(random-DAG path enumeration, cross-thread stress).

## Tests

`crates/vesper-resonance/tests/` (55 tests):

- `confinement.rs` (14): allocation two calls deep with its path, the
  allowlist counterexample, every forbidden effect kind, unknown callees,
  indirect calls unresolved/resolved/resolved-to-bad, loop policy,
  recursion, worst-case stack exactly at and over budget, u64 overflow,
  a 200,000-deep chain (no native recursion), independent roots, malformed
  graphs.
- `cranelift.rs` (8): verifier-checked IR built with `cranelift-frontend`:
  transitive `malloc` three calls deep with the root's call instruction,
  stack slots and budget, tail call to a mutex, undeclared symbol and
  libcall, `call_indirect` with and without listed targets, loops via
  `LoopAnalysis`, declared effect rows, typed errors.
- `pdc.rs` (9): compensation values, an impulse through a 37-sample
  lookahead branch arriving aligned, block executor = per-sample reference
  for block sizes 1/7/16/64, delayed feedback = reference, cycle and
  feedback errors, delay limits, buffer reuse, and 40 random shuffled DAGs
  with feedback where every enumerated path has equal compensated latency
  and the executor matches the per-sample reference at block sizes 1 and 32.
- `spsc_stress.rs` (4): 1,000,000 heap values retired across threads through
  a 64-slot ring, in order, none dropped on the realtime thread; a
  200,000-state mailbox stress with monotone swaps and exact accounting.
- `realtime.rs` (11): lease rules, wire layout bytes, malformed events,
  ramps, 500 random event sets split vs per-sample reference (bit-exact),
  SVF bank vs an f64 reference (max error < 1e-4 over 4,096 samples with
  ramps), the quantum (swap, retirement, silence, budget, no period), PID vs
  an f64 reference (< 1e-3) tracking setpoint steps on a simulated motor,
  PID faults.
- `alloc.rs` (5): a counting global allocator (per-thread) shows 0
  allocations and 0 frees over 1,000 warmed-up quanta that include 50
  patch-bank swaps and retirements, 10,000 PID steps, 1,000 compiled-graph
  blocks with feedback and compensation, and realtime-side retirement of
  4 KiB boxes (the janitor is where they are freed).
- `denorm.rs` (2): a decaying IIR produces a subnormal outside the scope and
  exactly 0.0 inside, a subnormal input reads as zero inside, the control
  word is restored exactly (also on panic).
- `lean_model.rs` (2): source audit and, under `VESPER_LEAN`, compilation and
  axiom report.

### Hardware evidence

- x86_64 Linux (cloud host): MXCSR FTZ|DAZ exercised, test passes in debug
  and release.
- AArch64 Linux under QEMU user mode (`qemu-aarch64`): FPCR.FZ path
  exercised and the whole crate's tests pass. QEMU emulates FZ; real AArch64
  hardware is exercised by the macOS (AArch64) CI job.
- No audio device, PipeWire/CoreAudio/WASAPI or EtherCAT hardware was used.
  No latency or throughput numbers are claimed.

## Checks run

Toolchain: Rust 1.100.0 (stable, as in CI), Lean 4.24.0.

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: clean.
- `VESPER_LEAN=lean cargo test -p vesper-resonance`: 55 passed.
- `cargo test -p vesper-resonance --release --test denorm`: passed.
- `cargo test -p vesper-resonance --target aarch64-unknown-linux-gnu` under
  `qemu-aarch64`: 55 passed.
- `VESPER_LEAN=lean cargo test --workspace --locked`: 2,307 passed, 0 failed
  (302 test binaries).
- `lean lean/ResonanceSpsc.lean` (from `lean/`): compiles; 11 axiom report
  lines, standard axioms only.

## Root patch

Separate commit: `crates/vesper-resonance` added to root `members` and
`default-members` (before `experiments/native-pulse`), `Cargo.lock`
regenerated (adds `rtrb 0.4.1`). No other root edits.

## Decisions and corrections

- Confinement is reachability over declared effects (not an allowlist of
  names); unknown symbols and unresolved indirect calls are rejected.
- Cranelift frame bytes are IR stack slots only; spills and the ABI frame are
  added after register allocation and are not counted (lower bound).
- `RtRetirementSink` is a unique handle, not `copy` (SPSC needs one producer).
- Event offsets outside the block are errors rather than clamped to 64.
- Feedback delay must be ≥ block length for block execution.
- The ring is `rtrb`, not a hand-rolled one, because the lane's unsafe
  allowance is only the FTZ/DAZ register.

## Remaining gaps

- No compiler `eff [Realtime]` / `@realtime` source enforcement: roots,
  effect rows and loop bounds come from `RealtimePolicy` / `SymbolEffects`.
  Integration B (#136) runs this verifier on `src/native.rs` output.
- WCSD here is IR-level (stack slots), not the Silicon machine-frame WCSD.
- No egglog buffer fusion, no TiledSeq slab packing, no SIMD intrinsics (the
  SVF loop is plain Rust over 16-lane arrays).
- No BIBO-stability or torque-clamp Lean proofs; no linearizability proof of
  `rtrb` itself.
- No audio/MIDI/CLAP/EtherCAT drivers.
- Linear owners rely on Rust ownership plus a leak counter; this is not the
  Vesper source `linear` typing rule.
