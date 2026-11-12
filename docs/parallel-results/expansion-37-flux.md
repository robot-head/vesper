# Flux host DBSP foundation (§34)

Implemented exclusively in new `crates/vesper-flux/**` and
`lean/FluxDbsp.lean`. Reviewed the refreshed full sections 30–34 plus
`docs/rfcs/expansion-37-contracts.md`. No shared schema, compiler, root manifest,
CI, CLI, existing runtime, Quarry, WAL or SimPulse file is modified.

## Executable contract

- `ZSet` has private canonical storage of nonzero signed i64 multiplicities.
  Consolidation borrows raw entries, validates width and bounds, sums with
  checked i128 arithmetic, and checks final fit into i64. Intermediate i64
  overflow that cancels within an accumulator is accepted. There is no
  saturation or wrapping of relational weights.
- `CircuitBuilder` uses append-only shared node IDs with private circuit origin.
  Rows are fixed-width all-i64 schemas. Typed predicates (`Equal`, `AtLeast`),
  projection/constant map fields, union and column-index equijoin are validated
  when constructed. Joins concatenate both rows. This is a host relational API,
  not SQL text, source syntax, macros, effects or egglog/compiler integration.
- Compilation builds an actual differentiated DAG. A join generates the three
  terms `join(dA, prevB) + join(prevA, dB) + join(dA, dB)`; previous values refer
  to the executor's prior integrals of the corresponding original nodes.
  Each differentiated node is evaluated once. Original subgraphs and delta
  subgraphs are referenced by index, never expanded recursively by cloning.
  One original join adds six differentiated nodes, so expansion is linear.
- `Executor::tick` executes only that differentiated program, then integrates
  its deltas at every original node. All tentative integrals are local until
  every operator, stored weight, output and budget check succeeds. No batch
  query recomputation occurs inside `tick`. Failed ticks leave all prior
  integrals and tick count unchanged; borrowed `TickInput` remains caller-owned.
  The separate `Circuit::batch` evaluator executes the original DAG.
- Bounds cover input entries, row width, distinct accumulator/stored rows,
  original and differentiated nodes/depth, each join Cartesian product, charged
  evaluation work and both output delta/materialization row counts. Work charges
  node visits, examined pairs, row/field processing and accumulation. BTreeMap
  comparisons and allocator overhead are additionally bounded by these sizes;
  this is not a cycle-exact real-time fuel claim. All declared nodes belong to
  a circuit and are evaluated, including nodes outside the selected output.
- Arithmetic follows `vesper.arith.checked/1`. Representable accepted
  computations are exact over integers. Intermediate accumulator/i64-node
  overflow or configured budgets may reject a computation whose eventual
  mathematical result would fit. There is no total Int-group implementation
  claim for finite i64 storage. Host allocator failure is not converted into
  a recoverable Flux error.
- `FrontierTicket` is a non-Clone Rust owner with private origin, slot, generation
  and timestamp. `Frontier` registers every live hold, publishes a conservative
  monotonic minimum, rejects regression and foreign/stale owners, supports
  explicit fork with capacity backpressure, advance and retirement. Errors
  consuming an existing owner return it in `TicketError`. Retiring the final
  hold retains the last published lower bound; it does not publish infinity.
  Exhausted generations never wrap or reuse an exhausted slot, and a ticket
  at the generation maximum can still be retired. Dropping/forgetting a ticket
  leaves the hold registered: affine Rust ownership is not a liveness proof.

This library allocates bounded BTreeMaps and vectors. AoSoA, SIMD, zero-copy CDC
wire decoding, zero allocation, windows, recursion, eviction, source cancellation,
persistence, checkpoint barriers, WAL/crash recovery, Quarry transactions,
network/distributed exactly-once execution and SimPulse journals are outside this
slice. No native runtime, throughput, latency or hardware claim is made.

## Verification evidence

Validated on this Linux host with a private ~5 MiB target, one build job, no
debug information and no incremental cache:

```sh
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/vesper-flux-target CARGO_INCREMENTAL=0 \
  cargo test --manifest-path crates/vesper-flux/Cargo.toml
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/vesper-flux-target CARGO_INCREMENTAL=0 \
  cargo clippy --manifest-path crates/vesper-flux/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/vesper-flux/Cargo.toml -- --check
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/vesper-flux-target CARGO_INCREMENTAL=0 \
  cargo run --manifest-path crates/vesper-flux/Cargo.toml --example incremental_join
lean lean/FluxDbsp.lean
```

Eleven tests pass: one module-level adversarial stale/generation test and ten
integration tests. These include a 128-tick deterministic differential execution
of a shared/nested `filter → map → union → join → join → map` circuit with
negative weights and both join inputs changing. At every tick independently
integrated emitted deltas equal both the separate batch query and executor
materialization. Focused tests check simultaneous retractions, wide consolidation
cancellation, concrete filter/projection/union row results, early operator and
late integral overflow rollback, schema/entry/row/column/node/depth/work/product/
output limits, linear program size despite shared recursive-looking DAGs,
all-live-hold progress, foreign/stale owners, backpressure and forgotten holds.

The self-checking `incremental_join` example emits deltas `+6`, `-5`, `-1`,
compares integrated output with batch at each tick, and explicitly retires its
frontier owner. It is ready for coordinator registration as:

```sh
cargo run -p vesper-flux --example incremental_join
```

The dependency-free crate currently has a temporary standalone `[workspace]`
for independent tests. Coordinator should remove that stanza, register the crate
in root members/default-members as appropriate and update the root lockfile.
No leaf Cargo.lock is included in this handoff.

## Lean model and actual axioms

`lean/FluxDbsp.lean` compiles under repository-pinned Lean **4.24.0** (commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`) with `import Std`. It has no sorry,
new axioms, `native_decide` or `bv_decide`. Actual `#print axioms` output:

| Theorem in `Vesper.FluxDbsp` | Axioms |
| --- | --- |
| `bilinear_join_exact` | `propext` |
| `retraction_cancels` | `propext`, `Quot.sound` |
| `integrate_delta` | `propext`, `Quot.sound` |
| `frontier_chain_monotonic` | `propext`, `Quot.sound` |
| `advance_preserves_live_bound` | `propext`, `Quot.sound` |
| `retirement_preserves_live_bound` | `propext` |

These kernel-checked handwritten lemmas prove scalar exact-integer DBSP algebra,
cancellation/integration and preservation of a published bound across a list of
live holds. They do not prove finite-support relational execution, Rust machine
arithmetic/state rollback, compiler differentiation or generated source artifacts.
Those implementation properties have the executable evidence described above.

## Refreshed design review findings (§§30–34)

- **§30 Nexus:** `PluginSliceGrantWire` is labeled 24 bytes, but its listed wire
  fields total 28 bytes. A host generational table/fuel counter does not establish
  WasmFX lowering, zero-copy sharing, tenant hardware isolation, zero allocation,
  timing claims or static WIT resource soundness. Generation wrap and unchecked
  borrow counters in sample Rust need additional bounds/lifecycle handling.
- **§31 Chrono:** append-only `writes` and snapshot vectors must enforce sorted
  timestamps for `partition_point` queries; the displayed Rust only comments on
  ordering. Lineage terminality needs a complete protocol, not only rejection
  after a terminal hop. A pure replay model does not validate Intel PT/CoreSight,
  cross-shard capture completeness, redaction, platform checkpoints or overhead.
- **§32 Resonance:** a string effect allowlist is not transitive compiler effect
  confinement. The shown topological scheduler rejects all cycles, while the
  prose permits feedback behind unit delays; duplicate IDs and latency overflow
  also need rejection. A lease's linear discharge does not prove every sample
  was initialized. Timing, wait freedom, zero allocation, FPU flag handling and
  audio/robotics device execution need separate implementation evidence.
- **§33 Aegis:** normalized policy equality is not cryptographic signature/TCB
  certificate verification or hardware attestation. An eight-byte binding prefix
  is not full TLS transcript authentication. GPA range arithmetic, bounded work
  and atomic failure need explicit checks. Snapshot substitution is a pure model
  lemma, not a proof of physical encrypted/shared mappings or host-copy machinery.
- **§34 Flux:** the displayed `saturating_add` contradicts signed Int equivalence
  and can make consolidation order dependent. The example's single
  `delta_orders join prior_risk` term assumes risk is fixed; changing both sides
  needs all three differentiated terms. The shown `FrontierTracker` lacks a
  method signature and cannot represent multiple live holds. Linear ownership
  alone cannot guarantee eventual advance, absence of deadlock or distributed
  watermark completion. Exact-once checkpointing requires durable atomicity,
  aligned channels and recovery evidence outside this lane. The host library
  corrects the arithmetic, differentiation and live-hold tracking within the
  bounded scope above.
