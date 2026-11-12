# Vesper Nexus (§30): wave-five lane 6 report

Claim: #126. Baseline: `af03122ea74d403f203170926945f9d35786b289` (origin/main
at start). New crate `crates/vesper-nexus`, Lean model
`lean/NexusHandles.lean` with its stdin fixture `lean/NexusHandles/Fixture.lean`.
Depends on `vesper-ffi` (read-only) and Wasmtime 50.0.1, already in the lockfile.

## What was built

| §30 item | Delivered | Evidence |
| --- | --- | --- |
| 30.5 generational WIT table | `table::WitResourceTable<T>`: fixed capacity, O(1) insert/lookup/borrow/drop/transfer, slot index + generation + tenant tag, `num_lends` | `tests/table.rs`: 64 seeds × 2,000 random ops against a reference model, every outcome class seen >100 times; leak, foreign-owner, teardown tests |
| 30.2 linear `own`/`borrow` | `WitOwn`, `WitBorrow`: non-Clone, `#[must_use]`, exit only via `drop_own`/`transfer`/`lower_to_guest`/`end_borrow`; failed consuming calls return the owner; dropped owners are counted leaks | `dropped_owners_and_borrows_are_counted_leaks` |
| 30.2 component host | `host::NexusEngine`: Wasmtime 50 component host for the `PLUGIN_WIT` world; host resource `blob` backed by the table; per-tenant stores sharing one table | `tests/components.rs`, 8 tests on a real hand-written component (`guests::BLOB_PLUGIN`) |
| 30.1(4) fuel/epoch | Fuel budget per instance; `run_scheduled` preempts every `quantum` fuel (`fuel_async_yield_interval`) and resumes on the next turn; `QuantumScheduler` trait shaped like SimPulse's `next_task`/`yield_task`/`finish`; `RoundRobin`; epoch deadline counted in virtual scheduler slices | `tests/fuel.rs`: interleaving, identical trace/usage/results over repeated runs, out-of-fuel and epoch traps, a custom priority scheduler |
| 30.1(3), 30.3 shared ring | `ring::SharedPluginRing` over one fixed-size host-created Wasm memory; 24-byte `PluginSliceGrantWire` written and read through `vesper-ffi` wire views; linear `SlotLease`; `StreamFilterBroker` running two tenants' filters over one ring with per-call fuel and §30.3 `TenantUdfMetrics` on happy and trap paths | `tests/ring.rs`: in-place transform at the same address, typed errors, **0 host allocations over 10,000 warmed-up lease→fill→filter→read→release cycles** (counting global allocator, thread-scoped, self-checked) |
| 30.1(1) WasmFX | `wasmfx::YIELD_SUM_WAT`: a one-shot `Yield` effect and handler hand-lowered to `cont.new`/`suspend`/`resume`; `wasmfx::probe()` | `tests/wasmfx.rs`: **exercised** on x86_64 Linux (handler returns 1+2+3 = 6); expected **unavailable** on macOS AArch64 and Windows, where Wasmtime 50 rejects stack switching |
| 30.4 Lean | `lean/NexusHandles.lean`, 15 audited theorems, no `sorry`/`admit`/axioms beyond `propext`/`Quot.sound` | `tests/lean_model.rs`: compiles under `VESPER_LEAN`, checks the axiom report, and replays 4,809 random Rust table operations through the Lean `step` with every result equal |

## Boundary behaviour (real components)

Two layers check handles. Wasmtime's per-instance handle tables reject double
`resource.drop`, use after the own moved into `consume`, and borrowing a
dropped handle (`unknown handle index`). The Nexus table adds what Wasmtime
does not model: tenant tags, host lends that outlive a call (`lend`/`unlend`),
and a raw 64-bit integer path (`peek-raw`, `unlend`) on which a guest can
present any integer.

* `resource.drop` while a host lend is outstanding traps with
  `WitTableError::ResourceHasActiveBorrows`; the resource survives and is
  reclaimed at teardown.
* Tenant B presenting tenant A's exact integer, wrong-generation and
  wrong-index neighbours, 0 and `u64::MAX` all return `-1`. Stale,
  forged and cross-tenant integers deliberately share one guest-visible
  code so a guest cannot probe other tenants' slots; the host audit log
  keeps the precise error.
* Lend tickets are tenant-scoped and one-shot: B cannot end A's lend.
* `destroy_instance` releases the tenant's lends, then its resources
  (including owns the guest never dropped), and the old integers go stale.
  A dropped `PluginInstance` is a counted leak.
* A trapped component instance is poisoned (Wasmtime refuses re-entry);
  `call_run` reports `PluginError::Poisoned`.

## Lean: what is proved, about the model only

`lookup_ok_sound` (non-forgery: any `(idx, gen)` pair resolves for tenant `t`
only to a live slot at `idx` with generation `gen` owned by `t`, and likewise
`beginBorrow_ok_sound`, `endBorrow_ok_sound`, `dropOwn_ok_sound`,
`transfer_ok_sound`); `wit_handle_tenant_isolated` (§30.4 theorem 1, for any
presented generation); `wit_drop_own_rejects_active_borrows` (§30.4 theorem 2),
`dropOwn_ok_no_borrows`, `transfer_ok_no_borrows`; `step_gen_mono`,
`run_gen_mono` (no operation lowers a generation), `stale_stays_stale`,
`run_stale_stays_stale` (a stale handle never resolves again for any tenant
over any run), `dropOwn_ok_stales`, `transfer_ok_stales`.

These are theorems about the Lean model in `lean/NexusHandles.lean`, not about
the Rust code. The differential test is evidence that the Rust table and the
model agree on the replayed runs only. Retirement at the generation ceiling is
modelled and differentially tested; that a retired slot is never reissued is
tested in Rust, not proved.

## Corrections to the §30 sketch

* **Generation wrap.** The §30.5 sketch does `wrapping_add(1).max(1)`, which
  reissues generation 1 after 2^32 reuses (ABA). The table retires a slot at
  its generation ceiling instead (`retired_slots()`); `transfer` at the
  ceiling is `GenerationExhausted`.
* **`end_borrow` tenant check.** The sketch's `end_borrow` takes no tenant, so
  tenant B could end tenant A's lend and unblock a drop. It now requires the
  owner.
* **No owner disclosure.** `UnauthorizedTenant` carries only the caller's
  tenant, not the owner's (`expected` in the sketch).
* **Fuel errors** live in `fuel::FuelError`, not the table error. Over-budget
  charges drain the meter.
* **Fixed capacity.** Tables and rings allocate everything at construction;
  `TableFull` and `Exhausted` are typed errors. `insert_own` returns the value
  on failure.
* **`PluginSliceGrantWire`** is 24 bytes as stated; its fields imply exactly
  that, verified against the `vesper-ffi` layout.
* **Fuel overshoot.** Wasmtime checks fuel at loop headers and calls, so a slice
  can exceed its quantum by one straight-line block (observed: 46 units on a
  2,000-unit quantum over 90 slices). The slice count and totals are still
  deterministic.
* **Epoch deadline** is engine-wide virtual time: one tick per scheduled slice
  of any tenant, so a deadline of N bounds a call to N slices of elapsed
  schedule, not N of its own slices.
* expansion-37-review (§30): "existing Wasmtime components do not imply this
  support" — WasmFX is claimed only where `tests/wasmfx.rs` runs it, and the
  component tests use real component binaries compiled by Wasmtime.

## Not done / unavailable

* **Rust ownership is not the Vesper source typing rules.** `WitOwn` and
  friends are host-library discipline; `Mode::Linear` checking of guest Vesper
  code, `[T-CallProj]` borrow projections and Theorem 2 for guests are not
  implemented here.
* **No compiler Wasm target.** Nothing lowers Vesper source to
  `wasm32-wasip2`/`wasip3` or to WasmFX. `YIELD_SUM_WAT` is a hand lowering;
  Iris-WasmFX validation is not attempted.
* **WIT compilation.** `PLUGIN_WIT` is documentation the guest is written
  against; no `.wit` → Vesper capability trait generator exists, and the test
  guest is hand-written component text (no guest toolchain needed in CI).
* **Ring isolation.** The Component Model cannot import a memory, so ring
  filters are core modules, not components. Every module importing the ring
  can address all of it: leases stop the host granting one tenant's slot to
  another but Wasm does not stop a filter writing other slots. Mutually
  distrusting tenants need one ring each. Domain-1 hardware protection (MPK),
  `DmaSlab` page alignment and io_uring DMA buffers are not exercised.
* **0 alloc** is measured only for the host-side ring path above (debug
  build). Component calls (`call_run`, `run_scheduled`) allocate (boxed
  futures, Wasmtime internals) and make no such claim.
* **Pulse integration.** Preemption is driven through `QuantumScheduler`; no
  adapter onto a real `PulseWorkerShard` or SimPulse run queue is included
  (the contract forbids the dependency). The "~25 µs per 50,000 fuel" figure is
  a design proposal; nothing was timed.
* **Lean extraction.** The Lean model is written by hand, not extracted by
  `vesper-verify-lean`; the host-import capability linker is not modelled.

## Cargo features

`wasmtime` gains `stack-switching`, `gc` and `gc-null` (needed by
`wasm_stack_switching`, `wasm_exceptions`, `wasm_function_references`). Cargo
unifies these into the workspace's single Wasmtime build; `gc` requires a
collector, hence `gc-null`. Without `gc-null`, every `Engine::new` in the
workspace fails when `gc` is on, so this is load-bearing for
`vesper-loom-sandbox` too; its tests pass with the unified features.

## Root patch

Separate commit: `crates/vesper-nexus` added to root `members` and
`default-members` before `experiments/native-pulse`; `Cargo.lock` regenerated
(new package entry only, no version changes). No `src/**`, CI or global docs
edits.

## Checks

Recorded with rustc/cargo 1.97.0 and Lean 4.24.0 on x86_64 Linux:

```sh
cargo fmt --all -- --check
VESPER_LEAN=lean cargo test -p vesper-nexus --locked
VESPER_LEAN=lean cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
lean lean/NexusHandles.lean
```

Results are in the PR description.
