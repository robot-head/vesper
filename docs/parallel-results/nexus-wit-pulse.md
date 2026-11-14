# Vesper Nexus (§30): wave-seven lane 22 report

Claim: #345. Baseline: `283c8115d2203232d0e9b473cc4bb0312c3a8509` (origin/main
at start, a descendant of the required `c025b9a`). Edits are confined to
`crates/vesper-nexus/**` and this report, plus the root `Cargo.lock` entry
for the crate's new dependency edges (own commit).

New dependencies of `vesper-nexus`, all already in the lockfile:
`wit-parser =0.261.0` (the WIT parser Wasmtime 50's own bindgen uses;
`default-features = false, features = ["std"]`), `vesper-simpulse` (path),
and dev-dependencies `vesper-syntax` and `vesper-fmt` (path) to parse and
format-check the generated Vesper. No versions changed in `Cargo.lock`; the
only change is the `vesper-nexus` dependency list.

## What was built

| Assignment item | Delivered | Evidence |
| --- | --- | --- |
| 1. `.wit` → Vesper capability traits + Rust host bindings | `wit_gen::generate(source, world)` returns `WitBindings { world, vesper, rust }` | `tests/wit_gen.rs`, 14 tests |
| 2. Preemption driven by SimPulse's run queue | `pulse::SimPulseScheduler`, a `QuantumScheduler` on `vesper_simpulse::Simulator` | `tests/pulse.rs`, 6 tests |
| 3. One ring per tenant | `tenant_ring::TenantRings`: per-tenant `Store`, ring memory and filter instance | `tests/tenant_ring.rs`, 10 tests; zero-alloc steady state in `tests/ring.rs` |

### 1. WIT generator (`src/wit_gen.rs`)

`generate` parses one WIT package with `wit-parser`, selects a world (by
name, qualified name, or the only one) and lowers a single resolved model to
two texts:

* **Vesper.** A host resource becomes `linear struct Res { rep_idx: u32,
  generation: u32 }` (the §30.2 `WitOwn` shape). An imported interface
  becomes a `linear effect` with one operation per function: constructors
  are `res_new`, methods and static functions `res_<name>` (a method's
  receiver is an explicit `in res: Res` parameter), and each resource gets
  `res_drop(sink res: Res)`, so the only exits of a handle are consuming
  operations. `own<T>` (and any affine or linear value, such as `string`) is
  passed `sink`; `borrow<T>` is passed `in`; copy values are passed plainly.
  Records become `copy`/plain/`linear struct` by the join of their fields'
  modes; enums and variants become `enum` with payloads. World-level
  imported functions become `linear effect <World>Imports`. The world's
  exports become `trait <World>` and exported interfaces `trait
  <Iface>Export`, whose methods take `in self` and carry every imported
  effect in their row.
* **Rust.** One module per scope. An imported interface's module has
  `INTERFACE`, a type tag per resource, a `Host` trait (one method per
  function, constructor, method, static function, plus a destructor per
  resource) and `add_to_linker(linker, get: fn(&mut T) -> &mut dyn Host)`
  wiring it into a `wasmtime::component::Linker` (`resource`, `func_wrap`
  under the exact `[constructor]`/`[method]`/`[static]` names). Records,
  enums and variants derive Wasmtime's `ComponentType`/`Lift`/`Lower` with
  explicit WIT names. `Exports::new(store, instance)` looks up every export
  (including functions inside exported interfaces) as a `TypedFunc`.

Supported subset: all scalars, `char`, `string`, `option`, `result`,
`tuple`, records, enums, variants, `use`/aliases, resources with
constructors, methods and static functions, `own`/`borrow`, world-level
functions and types, exported functions and exported interfaces. Everything
else is a typed `WitGenError::Unsupported { item, construct }`: `list`,
`map`, fixed-length lists, `flags`, `future`, `stream`, `error-context`,
async functions, getters/setters, guest-exported resources, inline/renamed
interface imports, borrows stored in records/variants or returned. Also
typed: `TooLarge` (over 1 MiB), `Parse`, `World` (missing or ambiguous) and
`NameCollision` (two WIT items mapping to one generated name, e.g. a record
and a resource both called `thing`). Rust and Vesper keywords get a
trailing `_`, with the original WIT name kept in `#[component(name = ..)]`.

Tests (`tests/wit_gen.rs`):

* Goldens for `PLUGIN_WIT` (`tests/golden/plugin.ves`,
  `plugin_bindings.rs`) and a second world exercising the rest of the
  subset (`tests/wit/kv.wit` → `kv.ves`, `kv_bindings.rs`). `VESPER_BLESS=1`
  rewrites them. Output is deterministic across runs.
* The generated Vesper parses with `vesper-syntax` with no diagnostics, is
  already canonical under `vesper-fmt` (CI's `vesper fmt --check` covers the
  goldens), and has the expected shape (one linear effect with the eight
  operations, one linear struct, one trait).
* **The generated Rust compiles and runs.** The test `include!`s both golden
  binding files. A host written only against the generated `plugin`
  `Host` trait (backed by `WitResourceTable`) runs the existing hand-written
  `BLOB_PLUGIN` component and returns the same results as the hand-written
  `NexusEngine` host on six scenarios, traps on the same four misuse
  scenarios (drop while lent, double drop, use after move, borrow after
  drop), and reaches the generated destructor binding. A second component
  (`KV_GUEST`, component text in the test) round-trips an enum through an
  imported function and an exported interface, a `result<u64, variant>`
  through a world export, and a `string` through a world-level import, all
  via the generated `kv` bindings. The `kv` `Exports::new` rejects the blob
  component with a named missing export.
* Typed rejections for each unsupported construct, malformed WIT, bad
  worlds, oversize input, name collisions and keyword escaping; 600 random
  mutations of both WIT sources never panic (both `Ok` and `Err` outcomes
  occur).
* The golden Rust files are `rustfmt`-clean (Bazel's rustfmt test checks
  them); the generator follows rustfmt's layout for the shapes it emits.

`vesper check` on the goldens (the root CLI, run by hand, not from the
crate's tests since library crates may not depend on the root package):
`kv.ves` checks. `plugin.ves` fails with one `E0101 unknown type 'i32'`,
because `peek-raw` returns `s32` and the current checker knows only `u8`,
`u32`, `u64`, `i64`, `f64`, `bool`, `String` among scalars. The generator
keeps the exact width rather than widening; this is a checker gap.

### 2. SimPulse adapter (`src/pulse.rs`)

`SimPulseScheduler::new(seed)` wraps a `Simulator<usize>`: `admit` spawns a
task whose payload is the job index, `next_job` is SimPulse's `next_task`
(one dispatch per fuel quantum), `preempted` is `yield_task` (the job stays
ready) and `finished` is `finish`. SimPulse's seeded, round-based selection
therefore decides the interleaving and records it in its versioned journal.
`SimPulseScheduler::replay(journal)` drives a run that SimPulse validates
transition by transition. Rejected transitions are recorded as
`SimPulseFault`s; `NexusEngine::run_scheduled` (unchanged) counts them as
scheduler violations and drains the remaining jobs round robin, so no
instance is stranded. The crate depends on `vesper-simpulse` only.

Tests (`tests/pulse.rs`): one SimPulse `Dispatch` per slice naming the
tenant that ran it, and journal counts of `Spawn`/`Dispatch`/`Yield`/
`Finish` matching the slice trace; per-round fairness (no tenant more than
one quantum ahead while all are runnable, 8 seeds); same seed ⇒ identical
schedule, different seeds ⇒ different interleavings, and for every seed the
results, slice counts and fuel per job equal the round-robin run; a JSON
round-tripped journal replays the identical trace with `finish_replay` OK; a
replay with different work reports faults and still completes every job;
out-of-fuel and guest traps leave the run queue (`is_complete`).

### 3. One ring per tenant (`src/tenant_ring.rs`)

`TenantRings` gives each tenant its own `Store<()>`, its own fixed-size
`SharedPluginRing` memory and its own filter instance linked against that
memory only; only the `Engine` (compiled code) is shared. Every host
operation names the acting tenant: a lease minted for another tenant is
`TenantRingError::CrossTenantLease { acting, owner }` (a rejected release
hands the lease back), and a lease from another `TenantRings` is a
`ForeignLease` ring error. The §30.3 filter call is shared with
`StreamFilterBroker` through one private `run_filter` (behaviour unchanged).

Tests (`tests/tenant_ring.rs`):

* The hazard, demonstrated on the shared broker: a spy filter finds tenant
  B's secret and a scribbler overwrites B's slot.
* Against `TenantRings` the same spy, run on every one of A's slots, never
  sees B's secret (and does find it when it is placed in A's own ring); the
  scribbler destroys A's ring and leaves every B slot byte-identical and
  still filtering; a store one byte past A's ring traps (`GuestTrapped`,
  counted in A's metrics) three times without affecting B.
* The two rings' memories are disjoint address ranges.
* Cross-tenant and foreign leases are refused on every operation; slots are
  per tenant (A exhausting its ring leaves B's free); unknown and duplicate
  tenants, bad geometry and bad modules are typed errors; dropped leases are
  counted.
* **0 alloc:** 10,000 warmed-up lease → fill → filter → read → release cycles
  across two tenants' rings allocate nothing on the host side (counting
  global allocator in `tests/ring.rs`, thread-scoped; measured in the same
  test as the shared broker so the two never overlap). Debug build.

## Public API (all additive)

`vesper_nexus::wit_gen::{generate, WitBindings, WitGenError, MAX_WIT_BYTES}`,
`vesper_nexus::pulse::{SimPulseScheduler, SimPulseFault}`,
`vesper_nexus::tenant_ring::{TenantRings, TenantRingError}`, re-exported at
the crate root. Example: `cargo run -p vesper-nexus --example wit_gen --
[file.wit [world]] [--rust]`. No existing item changed signature (lane 25
depends on this crate read-only).

## Decisions

* **wit-parser, not a hand parser.** It is the parser Wasmtime uses, it is
  already in the lockfile at the version Wasmtime 50 pins, and it resolves
  `use`, versions and world elaboration.
* **Exact widths.** WIT `s32` is `i32`, `u16` is `u16`, and so on; the
  generator does not widen to the checker's current scalar set.
* **Exports as traits, imports as linear effects.** The guest implements
  exports; the host provides imports as capabilities that appear in the
  guest's effect row.
* **Explicit destructors.** `res_drop(sink ..)` in the effect and
  `res_drop` in the Rust `Host` trait, so the Rust host decides drop
  semantics (the test host enforces `num_lends == 0` through the table).
* **Per-tenant stores.** A per-tenant memory inside one shared store would
  still let a module that imports two memories reach both; a separate store
  per tenant makes cross-tenant addressing impossible to express.

## Not done / boundaries

* Rust ownership is not the Vesper source typing rules. The generated
  Vesper declares linear handles and `sink`/`in` passing; whether the
  checker enforces `Mode::Linear` for them is the checker's property. The
  Rust bindings expose `wasmtime::component::Resource<T>`, which is not
  linear; linear discipline on the host is the `Host` implementation's (the
  test host uses `WitResourceTable`).
* No guest-side bindings: nothing lowers Vesper to a component, and the
  generated Vesper trait is not consumed by a compiler backend yet (lane 25
  owns the Wasm target).
* `list<T>`, `flags`, streams, futures and async functions are rejected,
  because the Vesper side has no faithful type for them today.
* The adapter drives SimPulse, the deterministic kernel; there is still no
  adapter onto a real `PulseWorkerShard`, no virtual-time cost per quantum,
  and the "~25 µs per 50,000 fuel" figure remains unmeasured.
* Ring isolation is Wasmtime's linear-memory bounds checking, not hardware
  Domain-1/MPK protection; `DmaSlab` alignment is not exercised.

## Checks

Recorded on x86_64 Linux with rustc/cargo 1.97.0 (and 1.100.0 for clippy):

```sh
cargo fmt --all -- --check                                   # ok (1.97 and 1.100)
rustfmt --edition 2024 --check crates/vesper-nexus/tests/golden/*.rs   # ok
cargo test -p vesper-nexus --locked                          # 67 passed
cargo +1.100.0 clippy -p vesper-nexus --locked --all-targets -- -D warnings   # ok
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings       # see PR
cargo test --workspace --locked                              # see PR
python3 tools/bazel/sync_build_files.py --check              # ok
vesper fmt --check crates/vesper-nexus/tests/golden/*.ves    # unchanged
vesper check crates/vesper-nexus/tests/golden/kv.ves         # checked
```
