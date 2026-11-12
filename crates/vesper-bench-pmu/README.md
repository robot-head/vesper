# vesper-bench-pmu

Opt-in real hardware observations for `vesper-bench::run_abba_observed`.
`PerfGroup::open()` creates one current-thread cycles/instructions perf-event
group, then the safe `WindowObserver` implementation enables/disables it around
each actual ABBA window. No external command, privilege adjustment or clock
fallback is used. This crate depends on `vesper-bench`; the safe benchmark core
has no platform dependency and retains `forbid(unsafe_code)`.

The initial ABI profile is **little-endian 64-bit Linux x86_64/aarch64**.
Other targets return structured `UnavailableReason::UnsupportedPlatform`.
Actual Linux permission denial, absent events, unscheduled intervals, OS errors,
invalid group reads, identity violations and counter regressions also return
structured `Unavailable`. A passing fixture or conditional live test is not
proof that a machine made counters available.

```rust,no_run
use vesper_bench::{BenchCase, RunConfig, run_abba_observed};
use vesper_bench_pmu::PerfGroup;
fn measure<A: BenchCase, B: BenchCase>(a: &mut A, b: &mut B) {
    let mut group = match PerfGroup::open() {
        Ok(group) => group,
        Err(unavailable) => { eprintln!("{unavailable}"); return; }
    };
    let measured = run_abba_observed(RunConfig::default(), a, b, &mut group);
    if let Ok(run) = measured {
        for window in run.observations() {
            let counts = &window.observation;
            println!("{:?} warmup={} cycles={} instructions={}",
                window.invocation.arm, window.invocation.warmup,
                counts.cycles(), counts.instructions());
        }
    }
}
```

The provider identity is `linux.perf_event_open.group_read`; cycles and
instructions are raw hardware events, separate from wall-clock nanoseconds.
Kernel/hypervisor execution is excluded. Counts include enable-return/dispatch,
Instant readings, safe boundary bookkeeping and after-hook dispatch until the
disable syscall. The syscall-read provider does not claim RDPMC, a 12-cycle
measurement boundary, zero allocations in user callbacks, SIMD validation or a
performance guarantee. No subtraction or hardware confidence gate is supplied.

Every group read validates the exact 56-byte Linux ABI, two independently
queried event IDs (either entry order), enabled/running metadata and all checked
u64 deltas. A usable window requires positive hardware counts and positive
`delta_enabled == delta_running`; no scaling/rounding of partially scheduled
counts is accepted. Earlier scheduling loss outside a window is allowed. The
kernel exposes cumulative u64 accounting; this provider assumes no full u64
wrap between snapshots and no outside reset/reconfiguration. It does not guess
a physical counter width or infer how many wraps occurred.

FDs are owned immediately, including partial construction failures. CLOEXEC,
private origin/generation tokens, non-Send/non-Sync ownership and OS pid/tid
checks prevent reuse across tasks, fork, groups or windows. Foreign/stale tokens
cannot disable another current window. An uncertain read/control transition
poisons the group; Drop attempts disable in the original task and closes both
FDs. Dropping a token alone can leave its group enabled until group Drop; there
is no liveness proof or global scheduler/affinity claim.

The observer runs after input setup/capacity preparation, before Instant starts,
and after Instant ends, before discard/teardown. Once before succeeds, after runs
once on normal Result paths, including body/clock failure. All available inputs
and outputs are then cleaned. Primary callback/cleanup and clock errors retain
secondary observer errors and completion metadata. Panics/aborts in callbacks
are not caught. `WindowCompletion` describes body/clock state before teardown.
Warmups are observed and explicitly identified, while clock report samples
exclude them. Harness storage and hook counts are bounded by `RunConfig`; user
callback/observer runtime and their internal allocations remain their own work.

Run `cargo run -p vesper-bench-pmu --example probe` after workspace registration.
It executes a real bounded workload, verifies positive counters/full scheduling
on success, and exits **2** with an explicit unavailable reason otherwise.
