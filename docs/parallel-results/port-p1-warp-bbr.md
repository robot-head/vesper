# Port P1 lane 10: Vesper Warp BBR (#907)

Baseline: `main` at b69383e. The crate is `crates/vesper-warp-bbr`. Its
BBRv3 state machine, windowed filters, pacing arithmetic and event handlers
are now written in Vesper (`vesper/src/*.ves`, package `@vesper/warp-bbr`)
and called through the generated binding `port/binding.rs`.

**The Rust code was not deleted.** On the ABBA benchmark of the ack/loss
event path, the port is 11x to 16x slower than the Rust build (95% interval
of the median ratio). The plan's rule is that a port more than 2x slower
keeps the Rust code, so this lane reports the gap and stops before deletion.
Both builds stay, and the Rust build remains the default. The Vesper build
is the `vesper-port` feature.

## API inventory

Everything public at b69383e, and how each item is served in the Vesper
build.

| Item | Vesper build |
| --- | --- |
| `pacing_delay_ns`, `edt_step`, `clamp_target_cwnd`, `send_allowed` | ported (`pacing.ves`) |
| `WindowedMax::{new, update}`, `CycleMax::{update, advance}`, `SplitMix64::{next_u64, below}` | ported (`pacing.ves`) |
| `WindowedMax::get`, `CycleMax::get` | `get` stays a Rust `const fn` (field read); `CycleMax::get` also exported as `cycle_max_get` |
| `BbrConfig::quic`, `BbrState::is_probe_bw` | ported (`init.ves`) |
| `Bbrv3::new` | ported (`bbrv3_new`, `init.ves`) |
| `Bbrv3::cwnd_bytes`, `target_cwnd_bytes`, `inflight_hi`, `inflight_lo`, `bw_lo`, `is_app_limited` | ported (`saturate_u32`, `clamp_target_cwnd`, `finite`, `app_limited_marked`) |
| `Bbrv3::schedule_edt_ns` | ported (`schedule_edt_ns`, scalar form) |
| `Bbrv3::on_packet_sent`, `on_probe_packet_sent` | ported (`bbrv3_record_send`) |
| `Bbrv3::on_ack` | ported (`check_time`, `ack_check`, `ack_check_totals`, `bbrv3_ack_step`) with a Rust call protocol, see below |
| `Bbrv3::on_packet_lost`, `on_ecn_ce`, `on_persistent_congestion`, `on_app_limited`, `on_spurious_loss_detected` | ported (`inputs.ves`, `control.ves`) |
| `Bbrv3::can_send` | stays Rust `const fn` (same comparison as `send_allowed`) |
| `Bbrv3::on_event` | stays Rust (match over `CongestionEvent<'_>`, dispatches to the forwards) |
| 19 field getters (`pacing_rate_bytes_per_sec` .. `last_rate_sample`) | stay Rust `const fn` field reads, shared by both builds |
| `BbrError` and its `Display`, `CongestionEvent`, `Delivered`, the event structs | stay Rust types; the port mirrors them (`BbrFault`, `AckedPacket`, ...) |
| The 16 constants (`BETA`, `GAIN_UNIT`, `INFINITY`, ...) | stay Rust `pub const`; the port has its own literals |
| `Bbrv3FlowState`, `OffloadModel`, `RateSample` | Rust types, mirrored as Vesper structs that cross the ABI as `copy` values |

The public API is unchanged. One signature property changed in the Vesper
build: the forwarded `pacing::*` functions and `WindowedMax::new` are no
longer `const fn`, because a foreign call cannot be `const`. No caller in the
workspace uses them in a const context; `vesper-warp` does call `can_send`
from a `const fn`, which is why that one stays Rust.

## What was ported

About 1,980 lines of Vesper in eight parts, about the same size as the
1,260-line Rust state machine plus the filters and pacing:

| Part | Contents |
| --- | --- |
| `types.ves` | `BbrState`, `OffloadModel`, `BbrConfig`, `RateSample`, the filters, the event structs, `BbrFault`, `Bbrv3`, `AckProgress` |
| `arith.ves` | saturating and wrapping helpers, 64x64->128 multiply, restoring long division, `mul_div_sat`, `product_gt`, `mul_gain`, `inflight_at_loss` |
| `pacing.ves` | pacing delay, EDT step, target cwnd clamp, send guard, windowed max, cycle max, SplitMix64 |
| `init.ves` | `OnInit`, config defaults, startup, small outputs |
| `inputs.ves` | send recording, ACK validation, per-packet rate sampling, loss, ECN, persistent congestion, app-limited, spurious loss |
| `model.ves` | bandwidth and min-RTT models, BDP, max inflight, long-term model |
| `probe.ves` | ProbeBW cycle (down, cruise, refill, up), inflight_longterm probing |
| `control.ves` | ProbeRTT, drain, cwnd and pacing-rate update, `UpdateOnACK` |

The port is one compilation unit split into parts because `vesper fmt` stops
at 8192 tokens per file. `build.rs` (Cargo) and a `genrule` (Bazel)
concatenate the parts in the order listed in `vesper/Loom.toml`, following
the `packages/vesper-compiler` precedent. Once the harness can build a
package directory (#919), the concatenation can go.

The port has no `unsafe`, and its capability ceiling is `["Fail"]`.
Arithmetic that could overflow uses the `wrapping_*` and saturating helpers
with the same results as the Rust code. Every malformed input is a typed
`BbrFault`, converted to the same `BbrError` the Rust build returns. The
existing error tests check this in both builds.

### The call protocol in Rust (`src/bbr/forward.rs`)

Two ABI facts shape how the Rust build calls the port:

* `Fail` cannot cross an `inout` parameter (E0402). So every mutating entry
  point checks all of its preconditions first, then mutates, and returns
  `Result[.., BbrFault]` as a value. This keeps the Rust contract that a
  rejected event leaves the controller unchanged.
* The export ABI copies a slice of non-scalar elements into a heap buffer.
  Passing an ACK's packets as one slice would break the zero-allocation
  claim. So `on_ack` validates each packet with `ack_check` (no state
  change), checks the totals with `ack_check_totals`, and then feeds the
  packets one at a time to `bbrv3_ack_step`, which runs `GenerateRateSample`
  and `UpdateOnACK` on the last one. This keeps the all-or-nothing contract,
  because every check runs before the first mutation.

## What stays Rust, and why

| File | What | Reason | Unblocked by |
| --- | --- | --- | --- |
| `src/bbr/forward.rs` | `Bbrv3::can_send` | `vesper-warp` (`cc.rs`) calls it from a `const fn`; a foreign call cannot be `const` | const-evaluable exports, or `vesper-warp` dropping the `const` (not in this lane's boundary) |
| `src/bbr.rs` | the 19 field getters, `WindowedMax::get` | pure field reads kept `const fn`; forwarding them would only add ABI copies | none needed |
| `src/bbr.rs` | `on_event` | matches on `CongestionEvent<'_>`, which borrows a slice; it only dispatches to forwarded methods | borrowed slices of non-scalar elements across the ABI without a heap copy |
| `src/bbr/forward.rs` | `on_ack` call protocol, `From<BbrFault> for BbrError` | `Fail` cannot cross `inout`; slices of structs are heap-copied (see above); `BbrError::InvalidConfig` carries a `&'static str` | `Fail` across `inout`, and zero-copy slices of `copy` structs in export ABI v3 |
| `src/events.rs` | `BbrError` and its `Display` | public Rust error type; text formatting stays with the type | none needed |
| `src/bbr.rs` | the 16 public constants | Vesper has no `const` items to export | exported constants |
| `src/bbr/reference.rs`, `src/filter.rs`, `src/pacing.rs` | the Rust implementation | the port is more than 2x slower (see Benchmarks) | the codegen and ABI work in "What would close the gap" |

The binding module is `#[allow(unsafe_code)]`, as the generator emits it.
The crate is `forbid(unsafe_code)` in the Rust build and `deny(unsafe_code)`
in the Vesper build. Apart from the generated binding, there is no unsafe
code.

## Unit tests

All 36 in-crate `#[cfg(test)]` tests now run unchanged against both builds.
Nothing was deleted or rewritten. Several tests call private procedures
directly, such as `start_probe_bw_down`, `inflight_with_gain`,
`adapt_long_term_model`, `mul_gain` and `inflight_at_loss`. For those, the
Rust implementation's methods are `pub(super)`, and `forward.rs` has
`#[cfg(test)]` forwards with the same names that call the matching exports.
The tests themselves changed in only two ways: wider imports, and the
`state` field is now `bbr_state`, because `state` is a Vesper keyword.

The integration tests run unchanged in both builds:

| Test | Rust build | Vesper build |
| --- | --- | --- |
| unit tests (`src/**`) | 36 passed | 36 passed |
| `tests/lean_model.rs` | 2 passed | 2 passed |
| `tests/properties.rs` | 4 passed | 4 passed |
| `tests/simulator.rs` | 7 passed | 7 passed |
| `tests/zero_alloc.rs` | 3 passed | 3 passed |
| total | 52 | 52 |

`tests/zero_alloc.rs` uses a counting global allocator over the warmed-up
steady state. It passes against the port, so the zero-allocation claim holds
for the Vesper build: no heap allocation per send, ACK, loss or ECN event.
`tests/lean_model.rs` compares traces from the port with the Lean model,
using the repository's pinned Lean (v4.35.0, from `lean-toolchain`). No Lean
files changed.

`cargo test -p vesper-warp --features vesper-warp-bbr/vesper-port` passes:
`vesper-warp` runs its whole suite against the port.

## Divergence checks

The feature `vesper-port-divergent` (Cargo, `Port::edit`) and the target
`:bbr_vesper_divergent` (Bazel, `edits`) build the port with one edit in
each ported module:

| Module | Edit | Test that catches it |
| --- | --- | --- |
| pacing | ceiling division rounds up by 2, not 1 | `pacing::tests::pacing_delay_is_ceiling_and_defined_at_zero` |
| filters | `CycleMax::advance` resets to slot 1, not 0 | `filter::tests::cycle_max_keeps_two_cycles` |
| state machine | persistent congestion allows `smss + 1` | `bbr::tests::persistent_congestion_collapses_cwnd_then_floors_at_min_pipe` |

`tests/divergence.rs` (`:port_divergence_detected` in Bazel) runs the
divergent unit-test binary and asserts that exactly those tests fail. It
also asserts that a control test from each module still passes, so the
check cannot pass on a binary that fails everything.

## Benchmarks

`examples/ack_path_abba.rs` times the steady-state workload of
`tests/zero_alloc.rs`: send while the window allows, then ACK the oldest
packet, or on every 97th packet mark it CE and declare it lost. Each measured
window warms one controller into ProbeBW (5,000 steps) and then times
200,000 steps. The two builds are the same crate with different features, so
they cannot share a process. Each window therefore runs in a fresh child
process, in A B B A order per epoch, and `vesper_bench::analyze_pairs` runs
the paired analysis over the epochs.

Setup: release profile, Linux cloud container, 4 vCPU Intel Xeon @ 2.10 GHz,
12 epochs, 200,000 steps per window, Rust 1.97, `vesper` from this branch.

```
cargo build --release -p vesper-warp-bbr --example ack_path_abba               # -> rust
cargo build --release -p vesper-warp-bbr --example ack_path_abba --features vesper-port  # -> vesper
ack_path_abba abba <rust> <vesper> 12 200000
```

| | ns per step (median of epochs) |
| --- | --- |
| Rust build | about 69 |
| Vesper build | about 930 |
| ratio, 95% interval of the median | 11.1 to 16.2 |
| gate at 2x | Regression |

Per-epoch ratios ranged from 10.0 to 18.5. Two earlier runs on the same
host, before two optimizations to the port, gave intervals of 14.7 to 22.0
and 11.3 to 18.3. The two optimizations were a scalar `schedule_edt_ns` that
passes only the fields it needs, and 32-bit fast paths in `mul_div_sat` and
`product_gt`.

### Where the time goes

Callgrind over 25,000 steps of the same workload:

| | instructions |
| --- | --- |
| Rust build | 22.0 M |
| Vesper build | 251 M (293 M before the two optimizations) |

In the Vesper build, about 70 M instructions (about 28%) are the binding
copying `Bbrv3` (about 50 fields) and `RateSample` into and out of the
export ABI. `Abi::get` and `Abi::put` for `Bbrv3` are the two largest
entries, at 30.8 M and 18.7 M. The binding does this around every `inout`
call, and an ACK makes several such calls. The rest is the Vesper code
itself, which runs about 8x the Rust instruction count. The causes are no
inlining across small helpers, every tuple and struct return going through
memory, and redundant stores. The 128-bit helpers were the largest single
cost before the 32-bit fast path, at about 170 instructions per `mul_full`.

### What would close the gap

* An export ABI where an `inout` aggregate is passed by pointer into the
  caller's struct instead of copied each way. This removes about a quarter
  of the cost.
* Inlining of small non-recursive functions in the native backend, and
  register returns for small tuples.
* A native `u128`, or a 64x64->128 multiply intrinsic.

These are compiler and ABI changes outside this lane's boundary.

## Platforms run

* Linux x86-64 (cloud container): Cargo, both builds, all 52 tests, the
  divergence check, `vesper-warp` against the port, the benchmark, and
  `cargo +1.100.0 clippy --all-targets -D warnings` on both builds.
* Linux through Bazel and BuildBuddy, macOS AArch64 and Windows x86-64 MSVC:
  through the required CI checks on the PR. The macOS and Windows jobs run
  the port through `scripts/test-port-harness.sh`, which this lane extends
  with both builds and the divergent check.

## Was the Rust code deleted?

No. The port is 11x to 16x slower than Rust on the ack/loss path, far past
the 2x limit. Both builds stay. Rust is the default, and `vesper-port`
selects the port. `src/bbr/reference.rs` holds the Rust implementation,
moved out of `src/bbr.rs` unchanged apart from `pub(super)` visibility and
the `bbr_state` and `rng` field changes. When the gap is closed, deletion is
mechanical. Delete `reference.rs` and the `cfg(not(feature))` halves of
`filter.rs` and `pacing.rs`, move the reference docs in `pacing.rs` onto
the forwards, make `build.rs` unconditional, and drop the feature.

## Known gaps

* Bazel targets were checked with `tools/bazel/sync_build_files.py --check`
  but not built locally; CI builds them.
* The parts are concatenated by `build.rs` and a `genrule` until the harness
  can build a package directory (#919).
* The self-hosted parser corpus skips `crates/vesper-warp-bbr/vesper`
  (#880), like the other ports.

## Commands

```
cargo test -p vesper-warp-bbr                                   # Rust build
VESPER_LEAN=<lean> cargo test -p vesper-warp-bbr --features vesper-port
cargo test -p vesper-warp --features vesper-warp-bbr/vesper-port
VESPER_PORT_DIVERGENT_TEST=<divergent lib test binary> \
  cargo test -p vesper-warp-bbr --test divergence
VESPER_PORT_BLESS=1 cargo build -p vesper-warp-bbr --features vesper-port   # regenerate port/binding.rs
```
