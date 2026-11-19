# Port wave P3a: the next crates move to Vesper, and `dyn Trait`

> Plan written 2026-11-18 against `main` at `3c075a4`. The
> [manuscript](../../manuscript/README.md) is the sole specification. The crate
> classes come from the [port survey](../../maintenance/vesper-port-survey.md)
> (#869), and every port lane follows the
> [porting recipe](../../maintenance/porting-a-crate.md) and the
> [P1 lane rules](../port-p1/README.md#rules-for-every-lane). Confirm live
> ownership on the issue board before acting.

Port wave P1 ported every crate the survey classed as port-first. What is left
of the survey is wave P3 (port-later crates, about 110k lines with the Tensor
and Quarry remainders) and wave P4 (the toolchain, which moves with
self-hosting Stage 2). This plan splits P3 in two:

- **P3a, this wave:** the six port-later crates whose portable core needs
  nothing the compiler lacks today, plus two compiler lanes that unblock the
  rest.
- **P3b, a later wave:** the crates that need `dyn Trait`, opaque Bridge
  handles, linkable Pulse or atomics first (see "Not in this wave").

The owner approved this split on 2026-11-18.

## Standing owner rule: Rust stays the default

Rust stays the default build of every crate in this wave and is **never
deleted** until the Vesper build matches it in both functionality and
performance. Each port lane proves the tests pass against both builds, adds the
`vesper-port` build next to the Rust one, measures, and stops. It does not
switch any dependent to the port, and it does not delete Rust code. The owner
decides each switch after the lane reports parity.

All six crates in this wave are linked by the root `vesper` package (they are in
`cargo tree -p vesper`), so each lane uses the **seed pattern** from
[the recipe](../../maintenance/porting-a-crate.md#crates-the-compiler-links) ("The seed-copy rule"): the
frozen Rust becomes `crates/<crate>/seed` as package `<crate>-seed`, and the
root and every other crate inside the compiler closure depend on it by rename.
`crates/vesper-beacon` is the worked example.

## Lanes

Claims are #1036-#1043 (lane n is issue 1035+n). "Needs" uses the survey's
feature names; F7b and F12 are this wave's compiler lanes.

| Lane | Prompt | Claim | Work (src LOC, tests) | Needs | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 1 | [`dyn Trait` (F7b)](01-dyn-trait.md) | #1036 | compiler | - | now | NEW `src/dyn_trait.rs` and `src/dyn_trait/**`, the `dyn` hunks of `src/check.rs`, `src/native.rs`, the JIT path and `src/native_export/**` (refusal only), `tests/dyn_trait*.rs`, `tests/fixtures/dyn_trait/**`, manuscript §7.1-§7.2 `dyn` text |
| 2 | [Atomics and scoped workers (F12)](02-atomics-workers.md) | #1037 | compiler | - | now (atomics); scoped workers after #599 | NEW `src/atomics.rs` and `src/atomics/**`, the atomic hunks of `src/check.rs` and `src/native.rs`, NEW `packages/vesper-std/sync/**`, `tests/atomics*.rs`, `tests/scoped_workers*.rs`, a new manuscript subsection |
| 3 | [vesper-pact](03-pact.md) | #1038 | 4,593, 54 | F1, F2, F5, F7a | now | `crates/vesper-pact/**` (not `-binding`, `-bridge`) |
| 4 | [vesper-loom-audit](04-loom-audit.md) | #1039 | 973, 16 | F2, F5 | now | `crates/vesper-loom-audit/**` |
| 5 | [vesper-bench statistics](05-bench.md) | #1040 | 4,066, 77 | F1, F4, F5 | now | `crates/vesper-bench/**` (not `vesper-bench-pmu`) |
| 6 | [vesper-ebpf core](06-ebpf.md) | #1041 | 12,375, 82 | F1, F2, F3, F5 | now | `crates/vesper-ebpf/**` (not `src/ebpf_backend/**`) |
| 7 | [vesper-silicon models](07-silicon.md) | #1042 | 12,317, 141 | F1, F2, F3, F5 | now | `crates/vesper-silicon/**` |
| 8 | [vesper-aegis parsing](08-aegis.md) | #1043 | 9,329, 94 | F1, F2, F3, F5 | now | `crates/vesper-aegis/**` |

Every lane can start now. Lane 2's scoped-worker half waits on the Pulse pool
(wave-nine lane 22, #599); its atomics half does not.

**Callbacks and trait objects in this wave's crates.** Pact takes
`&dyn Fn(&InferCtxt) -> Goal`, Silicon stores `Box<dyn FnMut>` interrupt
handlers, and Aegis takes `&mut dyn FnMut` evaluators and a static table of
`&dyn SignatureVerificationAlgorithm`. Ports do not wait for lane 1. Use the P1
patterns: a closed enum of the callers' goal builders, returned event or
command lists that a thin Rust adapter dispatches to the handlers, and
signature checks behind a `.vsys` extern. If lane 1 merges while a port lane is
open, the lane may switch to `in dyn`, but it does not have to.

**What else is related, and stays where it is:**

- The rest of Tensor (attention and the pure parts of llama) belongs to port P1
  lane 8 (#905), which is still open.
- Wave-nine lane 14 (#591) is the eBPF *compiler backend* (`src/ebpf_backend/**`),
  not the `vesper-ebpf` crate. Lane 6 does not edit it.
- Wave-nine lane 22 (#599) owns the Pulse work-stealing pool. Lane 2 builds on
  it and does not edit Pulse.
- The port speed wave (#1021-#1028) owns the native backend's performance
  hunks. Lanes 1 and 2 add new constructs and leave those hunks alone.

## Rules for every lane

The [P1 rules](../port-p1/README.md#rules-for-every-lane) apply to lanes 3-8,
with these changes:

- **Claim.** The lane's claim issue exists; link it from the PR ("Closes #NNN").
  Search open issues and PRs for the crate first.
- **No deletion and no switch** (owner rule above). The P1 rule "Deleting the
  Rust code" does not apply.
- **Seed pattern** for all six crates, in its own commit, with the root patch
  rule from P1. `vesper-bench` is a dev-dependency of every earlier port's ABBA
  benchmark: those keep using the Rust build through the seed.
- **Performance.** Each lane reports an ABBA comparison of both builds on the
  crate's heaviest test or a new `examples/abba.rs`, with instruction counts
  (callgrind) next to wall clock, because wall clock on the cloud hosts varies
  by up to 30%. Report the ratio and the compiler gaps that cause it, each with
  a repro, as earlier lanes did.
- **Report.** `docs/parallel-results/port-p3-<lane slug>.md`: API inventory, what
  stays Rust and why (file, reason, the feature that unblocks it), unit-test
  treatment, divergence checks, ABBA numbers, platforms run.

Lanes 1 and 2 follow the compiler-lane rules of the
[port speed wave](../port-speed/README.md#rules-for-every-lane), including the
Stage 1 guard: never edit `src/verified_codegen/**`, `lean/**`, `bootstrap/**`
or `packages/vesper-compiler/**`, and show the Stage 1 lock still reproduces.

## Merge order and collisions

| Shared file | Lanes | Rule |
|---|---|---|
| Root `Cargo.toml`/`Cargo.lock` | 3-8 (seed packages) | Serialize. Merge `main` and regenerate the lock right before merging. |
| `fixtures()` skip predicate in `tests/fixtures/self_host/driver.rs` | 3-8 | One line per port. |
| `scripts/test-port-harness.sh` | 3-8 | Append only. |
| `src/check.rs`, `src/native.rs` | 1, 2 | Disjoint new hunks; whichever merges second merges `main` first. |
| `packages/vesper-std/**` | 2 (new `sync` module) | Ports needing a std function put a private helper in their own package. |

Suggested order: lane 4 first (smallest, proves the loom-calculus import), then
3, 5, 6, 7 and 8 as they go green, one root patch at a time. Lanes 1 and 2 merge
whenever they are green.

## Not in this wave (P3b and later)

| Crate or part | src LOC | Waits on |
|---|--:|---|
| `vesper-warp` QUIC core | 13,882 | F9 opaque Bridge handles (#611) for crypto, TLS and UMEM |
| `vesper-prism-h3` | 8,392 | `vesper-warp` |
| `vesper-chorus` | 9,861 | `vesper-warp`, F7b (lane 1), linkable Pulse (#585) |
| `vesper-flux` | 11,967 | F7b (lane 1), F9 (#611) for compression and TLS |
| `vesper-kinetic` | 15,867 | F12 (lane 2), `f32` bit-determinism tests |
| `vesper-resonance` | 4,080 | F12 (lane 2); the realtime rings and JIT stay Rust |
| Quarry buffer pool, latches, SSI | rest of 13,405 | F12 (lane 2), linkable Pulse (#585) |
| Serde SIMD JSON | `src/simd/**` | source `VVec` (#587) |
| Wave P4 toolchain crates | about 65k | self-hosting Stage 2 |

When lanes 1 and 2 have merged and #611 is close, the next plan writes P3b from
this table.

P3b is planned in [port-p3b](../port-p3b/README.md) (#1063-#1071).
