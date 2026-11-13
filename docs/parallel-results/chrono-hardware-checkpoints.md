# Wave-six lane 2: Chrono hardware trace decoding and activation checkpoints (§31)

Claim: #244. Branch `claude/project-thread-ae6uo5`, based on `main` at
`07322e3ba47890e939a48967a1ea8f62fdd7aac1` (a descendant of the required
baseline `cda092f`). Edits stay inside `crates/vesper-chrono/**`,
`lean/ChronoReplay.lean` and this report, plus the root `Cargo.lock` patch
for the new `iced-x86` dependency (own commit).

## What was built

| Item | Where | Evidence |
| --- | --- | --- |
| 1. Intel PT packet decoder | `src/ipt.rs` | `tests/ipt.rs`: every packet kind, all six IP-compression modes, PSB reset of the last IP, 18 typed error cases, fuzzed bytes never panic |
| 1. PT flow reconstruction over a code image (`iced-x86` 1.21.0) | `src/ipt_flow.rs` | `tests/ipt.rs`: a hand-assembled x86-64 image (loop, call, compressed and uncompressed `ret`, `jmp rbx`, `jmp rax`), async FUP+TIP, OVF, PSB+ resync and desync, 10 mismatch errors |
| 2. ETE / ETMv4 element decoder | `src/ete.rs` | `tests/ete.rs`: A-Sync, trace info, timestamps (partial and full, with cycle count), context, address-with-context, short / long / exact-match addresses (IS0, IS1), exceptions (one- and two-byte type), exception return, event, overflow, discard, every atom format 1-6; 15 error cases; fuzz |
| 3. Branches mapped onto the effect timeline | `src/hwpath.rs` | `tests/hwpath.rs`: `reverse_watch_path` returns the last writer of a place and the definite and boundary-uncertain steps since the previous record, from a PT trace and from ETE elements |
| 4. Gated capture | `src/capture.rs` (the crate's only `unsafe`) | `tests/capture.rs`: sysfs probe on a fake tree; on this host it reports unavailable (below) |
| 5. Native activation checkpoints as snapshot epochs | `src/checkpoint.rs` | `tests/checkpoint.rs` on real fibers, both Cranelift opt levels |
| 6. Lean checkpoint model | `lean/ChronoReplay.lean` | four new theorems, compiled and axiom-audited by `tests/lean_model.rs` |

### Public API (additive)

`ipt::{PtDecoder, PtPacket, PtPacketAt, PtError, Tnt, IpPacketKind,
IpCompression, PSB_BYTES, decode_packets}`, `ipt_flow::{CodeImage,
CodeSegment, reconstruct, FlowTrace, Branch, BranchKind, FlowLimits,
FlowError}`, `ete::{EteDecoder, EteElement, EteElementAt, EteContext,
EteError, InstructionSet, ASYNC_LEN, decode_elements}`,
`hwpath::{BranchTimeline, EffectAnchor, TraceStep, StepKind,
InstructionPath, WatchPath, PathError, reverse_watch_path}`,
`capture::{probe, capture_command, parse_format, PmuProbe, TracePmu,
CaptureConfig, CapturedTrace, CaptureError, SYSFS_PMU_ROOT}`,
`checkpoint::{ActivationCheckpoint, ParkPoint, JournalEntry, ResumeTag,
StackShape, CheckpointLimits, CheckpointError, AddressPolicy, capture_epoch,
checkpoint_from_bundle}` and, on direct-fiber targets,
`checkpoint::{RecordedActivation, Step, RejectedResume, Restored}`. No
existing item changed signature. `Unavailable` keeps its four variants; the
reasons of `HardwareBranchTrace` and `NativeActivationCheckpoint` now say what
remains. The crate attribute changed from `forbid(unsafe_code)` to
`deny(unsafe_code)` with one `allow` on `capture::linux`.

## Decisions

- **Independent encoders.** Every decoder test builds its stream with an
  encoder in the test file, written from the packet tables rather than from
  the decoder. Both were written by the same author from the same reading of
  the specifications, so a misreading shared by both would not be caught. No
  trace from real hardware was available to check against.
- **PT synchronisation.** `PtDecoder::new` requires a PSB first (after PAD);
  `sync` searches for one. The last IP resets to 0 at each PSB. Reserved
  bits that the SDM fixes at zero (CBR byte 3, TMA bytes 4 and 6, MODE bits
  [4:2]) are errors.
- **PT flow.** Return compression is assumed whenever a TNT bit is pending
  or next; otherwise a `ret` takes a TIP. A TIP.PGD without a preceding
  branch only disables tracing when it names the current IP. TSX is rejected
  as unsupported. The reconstructor stops at the first instruction that
  needs a packet after the packets end, and leftover TNT bits are an error.
- **Time.** Each branch carries the interval between the TSC packets around
  it. A branch is definitely between effect records `a` and `b` only when
  `tsc_lo > anchor(a)` and `tsc_hi <= anchor(b)`; intervals that straddle an
  anchor are returned separately as uncertain instead of being assigned. The
  recorder's `time_ns` field must hold the same TSC/CNTVCT clock when
  hardware tracing is on; nothing in the crate reads a hardware clock.
- **ETE encodings.** Address field layouts, the format-4/5 atom tables and
  the exception-type bit split follow this lane's reading of IHI 0064;
  cycle-count, conditional, commit, Q and data packets are
  `Unsupported`, since they appear only when those features are enabled.
  ETE flow reconstruction over an A64 image (walking atoms through code) is
  not implemented; atoms map onto the timeline at their latest address.
- **Capture.** The child waits on stdin in `sh -c 'read _; exec "$0" "$@"'`,
  the event is opened on its pid with `enable_on_exec`, and a newline
  releases it, so the traced program runs no untraced instruction.
  `EACCES`/`EPERM` and `ENOENT`/`ENODEV`/`EOPNOTSUPP`/`EINVAL` from
  `perf_event_open` become `Unavailable`. SPE output is returned raw.
- **Checkpoints by re-execution.** `vesper-fiber-abi` exposes no accessor
  for the stack bytes between the saved stack pointer and the top, no
  register frame, no fixed-address stack and no context write, and this lane
  may neither edit it nor use `unsafe` outside capture. So a checkpoint holds
  the park request (evidence by depth), generation, data frame, the shape of
  the self-pointing stack words (`Parked::stack_self_pointers`, relative to
  the lowest one) and the journal of resumptions with the data frame each
  resumer left. `restore` starts a fresh fiber of the same code, replays the
  journal checking each park, then checks request, generation, data frame
  and stack shape, and hands back a live activation that resumes to the same
  result.
- **Relocation.** The rebuilt stack is a new mapping. `AddressPolicy::
  SameAddress` rejects any restore whose stack anchor differs with
  `CheckpointError::Relocated`; `Reexecuted` accepts it, because re-execution
  rebuilt the pointers rather than copying them, and reports the
  displacement. Every rejected restore cancels the rebuilt continuation.
- **Epochs.** A checkpoint serialises to a versioned little-endian image with
  a BLAKE3 trailer (so a redacted or damaged image is `Corrupt`, never
  misparsed) and is stored with `FlightRecorder::capture_snapshot`.

## Checks

All on x86_64 Linux (the cloud host), Rust toolchain from `rust-toolchain`,
`CARGO_BUILD_JOBS=4`:

```text
cargo fmt --all -- --check                                         ok
cargo clippy -p vesper-chrono --all-targets --locked -- -D warnings ok
cargo test -p vesper-chrono                                        ok (all suites)
VESPER_LEAN=<lean 4.24.0> cargo test -p vesper-chrono --test lean_model   ok
lean lean/ChronoReplay.lean (4.24.0, GitHub release zip)           ok; new theorems use only propext
cargo test --workspace --locked                                    see PR checks
```

Hardware capture check on this host (from `tests/capture.rs`):

```text
hardware trace capture unavailable: no intel_pt, cs_etm or arm_spe PMU under
/sys/bus/event_source/devices, perf_event_paranoid = 2
```

macOS AArch64 runs `tests/checkpoint.rs` in the `macOS (AArch64)` CI job;
the result is recorded on the PR.

## Lean

`checkpointAt step s0 es e` is the state after the first `e` events.
Proved, about this model only and not about the Rust code:
`checkpoint_restore_replays_to_live` (restoring the checkpoint at epoch `e`
and replaying `es.drop e` equals the live run over `es`),
`reexecuted_checkpoint_replays_to_live` (a state rebuilt by re-executing a
journal equal to the recorded prefix replays to the live run),
`restore_rejects_relocation` and `restore_at_captured_base` (the byte-image
restore model succeeds only at the captured base). No `sorry`, `admit` or
new axioms.

## Remaining gaps

- No Intel PT, CoreSight or SPE trace was captured or decoded on hardware;
  the capture path past `probe` has not run. A host with `intel_pt` and
  `perf_event_paranoid <= 1` (or `CAP_PERFMON`) is needed, and small
  recorded traces should then be committed as fixtures.
- CoreSight frame-formatter (TPIU/ETF 16-byte frames) removal is not
  implemented; ETE from a TRBE sink is unformatted and decodes directly.
- No AArch64 flow reconstruction from ETE atoms over a code image.
- Byte-image checkpoints need an additive `vesper-fiber-abi` API in a later
  step: `Parked::stack_image() -> (saved_sp, top, bytes)` and a register-frame
  accessor, `FiberConfig::fixed_stack_base` (mapped with
  `MAP_FIXED_NOREPLACE`) and `Fiber::from_image(...)` restoring at the same
  address. Until then the entry argument and non-pointer stack words are not
  verified: restoring onto a fiber with a different argument is not detected
  (tested, it resumes to a different result), so callers must supply the
  same code and argument. Direct handlers run again during re-execution and
  must be deterministic.
- VirtualArena copy-on-write shard checkpoints (userfaultfd, Mach
  `vm_copy`, `MEM_RESET`) are not implemented.
- The `vesper replay` CLI and Lens integration are integration B (#136).
- `RecordedActivation` ownership is host-library discipline, not the Vesper
  linear typing rules.
