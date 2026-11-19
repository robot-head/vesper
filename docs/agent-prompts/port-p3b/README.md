# Port wave P3b: networking, dataflow and numeric crates move to Vesper

> Plan written 2026-11-18 against `main` at `8212ddf`. The
> [manuscript](../../manuscript/README.md) is the sole specification. The crate
> classes come from the [port survey](../../maintenance/vesper-port-survey.md)
> (#869), and every port lane follows the
> [porting recipe](../../maintenance/porting-a-crate.md) and the
> [P1 lane rules](../port-p1/README.md#rules-for-every-lane). Confirm live
> ownership on the issue board before acting.

[P3a](../port-p3/README.md) (#1036-#1043) ported six analysis crates and added
second-class `dyn` (#1036), boxed objects (#1057) and atomics with scoped
workers (#1037). The Pulse pool (#599) is done. This wave ports the six crates
P3a's "Not in this wave" table left for P3b, and adds the three compiler lanes
those ports need most:

- **Effects and function values through objects (F7c).** Chorus stores
  `Box<dyn Transport>` over transports that do I/O, Flux sources and sinks
  perform storage effects, and Kinetic passes a `dyn FnMut` contact pass. Today
  a slot's handler effects are specialized at the call, and closures are not
  objects.
- **Boxes owned across loops.** Chorus sessions and Raft nodes, Warp's
  endpoint and Kinetic's world own a boxed object and loop on it, which the
  acyclic Box drop elaboration refuses.
- **Opaque Bridge handles (F9).** Warp's endpoint holds AEAD sealers and
  openers, a rustls session and UMEM frames. That needs `linear extern type`
  handles in `.vsys` imports; it is the import-handle half of wave-nine lane 34
  (#611), which has had no PR since 2026-11-16.

The remaining `dyn` gaps (associated types, several models per pair,
`Rc`/`Arc` of objects, `dyn` in trait methods) are not needed by these ports
and stay out of this wave. Linkable Pulse in executables (#585) is not needed
either: every port keeps its I/O driver in Rust.

## Standing owner rule: Rust stays the default

Rust stays the default build of every crate in this wave and is **never
deleted** until the Vesper build matches it in both functionality and
performance. Each port lane proves the tests pass against both builds, adds the
`vesper-port` build next to the Rust one, measures, and stops. It does not
switch any dependent to the port and it does not delete Rust code. The owner
decides each switch after the lane reports parity.

Kinetic and Resonance are linked by the root `vesper` package (`cargo tree -p
vesper`), so lanes 8 and 9 use the **seed pattern** from
[the recipe](../../maintenance/porting-a-crate.md#crates-the-compiler-links)
(`crates/vesper-beacon` is the worked example). Warp, Prism H3, Chorus and
Flux are outside the compiler closure, so lanes 4-7 add the `vesper-port`
feature to the crate itself, as `crates/vesper-warp-tcp` did.

## Lanes

Claims are #1063-#1071 (lane n is issue 1062+n).

| Lane | Prompt | Claim | Work (src LOC, tests) | Needs | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [Effects and function values through objects (F7c)](01-dyn-effects-closures.md) | #1063 | compiler | - | now | effect/evidence hunks of `src/dyn_trait*`, NEW `src/dyn_fn.rs` and `src/dyn_fn/**`, evidence-call hunks of `src/native_handlers.rs`, `src/native.rs`, `src/runtime.rs`, `tests/dyn_effects*`, `tests/dyn_fn*` |
| 2 | [Own boxes in functions with loops](02-loop-owned-boxes.md) | #1064 | compiler | - | now | Box drop elaboration in `src/fip.rs` and `src/fip/**`, its loop checks in `src/check.rs`, the sink-lend refusal hunk, `tests/loop_boxes*` |
| 3 | [Opaque Bridge handles (F9)](03-bridge-handles.md) | #1065 | compiler | - | now (check #611 first) | handle and slice hunks of `src/ffi_lowering*`, `crates/vesper-ffi/**` handle declarations, extern-handle hunks of `src/check.rs`, `tests/ffi_handles*` |
| 4 | [vesper-warp QUIC core](04-warp.md) | #1066 | 13,882, 149 | F9 (part 2) | now (part 1); part 2 after lane 3 | `crates/vesper-warp/**` |
| 5 | [vesper-prism-h3 protocol core](05-prism-h3.md) | #1067 | 8,392, 93 | - | now | `crates/vesper-prism-h3/**` |
| 6 | [vesper-chorus](06-chorus.md) | #1068 | 9,861, 83 | F7c, lane 2 (part 2) | now (part 1); part 2 after lanes 1 and 2 | `crates/vesper-chorus/**` |
| 7 | [vesper-flux dataflow core](07-flux.md) | #1069 | 11,967, 91 | F7b (merged) | now | `crates/vesper-flux/**` |
| 8 | [vesper-kinetic](08-kinetic.md) | #1070 | 15,867, 170 | F12 (PR #1062 for threads) | now (single-threaded) | `crates/vesper-kinetic/**` (not `gpu/**`) |
| 9 | [vesper-resonance DSP core](09-resonance.md) | #1071 | 4,080, 80 | F12 atomics (merged) | now | `crates/vesper-resonance/**` |

Every lane can start now. Lanes 4 and 6 start with their pure half and finish
after their compiler lanes merge; lane 8's threaded paths wait for PR #1062.
Stacking a branch on an open dependency is allowed.

**What each port keeps in Rust, in short:** Warp keeps `vesper-warp-crypto`,
`-tls`, `-umem`, `-shard` and its token AEAD/HMAC; Prism H3 keeps its
connection driver over Warp and quinn; Chorus keeps the durable file log, the
Warp/rustls network and RDMA; Flux keeps compression, TLS/SASL, the Kafka
client and the file store; Kinetic keeps the Cranelift scanner, intrinsics and
the GPU crate; Resonance keeps the Cranelift JIT, the `rtrb` rings and the
denormal control registers.

**Related work that stays where it is:**

- #611 (wave-nine lane 34) keeps pointer results, narrow integers and the
  sandbox stack; lane 3 takes only its import-handle half and stops if #611
  has an open PR.
- #585 (linkable Pulse) is not needed by this wave.
- The port speed wave (#1021-#1028) owns the native backend's performance
  hunks. Lanes 1-3 add new constructs and leave those hunks alone.
- PR #1062 (scoped workers, #1037) is still in CI; lane 8 builds on it and
  does not edit it.

## Rules for every lane

The [P1 rules](../port-p1/README.md#rules-for-every-lane) apply to lanes 4-9,
with these changes:

- **Claim.** The lane's claim issue exists; link it from the PR ("Closes #NNN").
  Search open issues and PRs for the crate first.
- **No deletion and no switch** (owner rule above). The P1 rule "Deleting the
  Rust code" does not apply.
- **Seed pattern** for lanes 8 and 9, in its own commit, with the root patch
  rule from P1. Lanes 4-7 need no seed.
- **Parts.** A lane may land as several PRs, each green on its own; only the
  last closes the claim.
- **Performance.** Each lane reports an ABBA comparison of both builds on the
  crate's heaviest test or a new `examples/abba.rs`, with instruction counts
  (callgrind) next to wall clock, because wall clock on the cloud hosts varies
  by up to 30%. Report the ratio and the compiler gaps that cause it, each with
  a repro, as earlier lanes did.
- **Report.** `docs/parallel-results/port-p3b-<lane slug>.md`: API inventory,
  what stays Rust and why (file, reason, the feature that unblocks it),
  unit-test treatment, divergence checks, ABBA numbers, platforms run.

Lanes 1-3 follow the compiler-lane rules of the
[port speed wave](../port-speed/README.md#rules-for-every-lane), including the
Stage 1 guard: never edit `src/verified_codegen/**`, `lean/**`, `bootstrap/**`
or `packages/vesper-compiler/**`, and show the Stage 1 lock still reproduces.

## Merge order and collisions

| Shared file | Lanes | Rule |
|---|---|---|
| Root `Cargo.toml`/`Cargo.lock` | 8, 9 (seed packages), any lane adding a dependency | Serialize. Merge `main` and regenerate the lock right before merging. |
| `fixtures()` skip predicate in `tests/fixtures/self_host/driver.rs` | all | One line per lane. |
| `scripts/test-port-harness.sh` | 4-9 | Append only. |
| `src/dyn_trait*`, `src/native.rs`, `src/native_handlers.rs`, `src/check.rs` | 1, 2, 3 | Disjoint new hunks; whichever merges second merges `main` first. |
| `packages/vesper-std/**` | none | Ports needing a std function put a private helper in their own package. |

Suggested order: lane 9 first (smallest, proves the seed and the boxed
processor loop), then 5, 7 and part 1 of 4 and 6 as they go green, then 8. Lanes
1-3 merge whenever they are green; parts 2 of lanes 4 and 6 follow them.

## Not in this wave

| Crate or part | Waits on |
|---|---|
| Quarry buffer pool, latches, SSI | linkable Pulse (#585); atomics and workers are now available |
| Serde SIMD JSON | source `VVec` (#587) |
| Warp shard tile table | source `VVec` (#587) |
| `dyn` associated types, several models per pair, `Rc`/`Arc` of objects | a port that needs them |
| Wave P4 toolchain crates (about 65k) | self-hosting Stage 2 |
