# Independent foundations from the 37-section design

The coordinator refreshed the entire Google Doc, including all 37 sections,
from the [recorded revision](../specification-source.json). The source snapshot
preserves document order rather than silently renumbering/reordering it. Six
readers covered the whole document; [the consolidated review](../rfcs/expansion-37-review.md)
records changed requirements, scaffold defects and remaining dependencies.
The upstream document was not edited.

Six implementation subagents used separate branches/worktrees and isolated
build targets. Their new production paths do not overlap externally owned wave
three. The coordinator merged the five wave-three deliveries already present
at upstream `33bf21b` without modifying their production code, then reran
combined checks. That merge is `7d18e1b`; earlier local duplicate spikes remain
unmerged. The external wave-three source/native work and standalone macro runner
remain under their owner's integration process.

| Milestone | Integrated commits | Working slice |
| --- | --- | --- |
| §20 Wire/Json | `8281d52` | Strict whole-input JSON index/cursor, zero-copy plain strings, caller-scratch escapes, checked encoding and SWAR/size helpers; [report](expansion-37-serde.md) |
| §22 Beacon | `12b22ff`, `a2a4492` | W3C v00 trace codec, bounded owned spans/ring/columns, explicit cancellation records and full-u64 checked histogram; [report](expansion-37-beacon.md) |
| §24 Quarry | `b97455e`, `18aa9c2` | Parsed typed SQL/catalog, real bounded row execution, schema-bound plans and NULL/outer-join-safe pushdown; [report](expansion-37-quarry.md) |
| §28 Tensor | `f8618e5` | Bounded symbolic shapes, checked host IR, actual FP32/Q4_0 kernels and arithmetic-order-preserving fusion; [report](expansion-37-tensor.md) |
| §34 Flux | `b0768d3` | Exact bounded Z-sets, shared DBSP differentiation, transactional ticks and owned frontier holds; [report](expansion-37-flux.md) |
| §37 Aether | `e954f15` | Checked immutable snapshot maps, explicit sharing identities and actual bounded CAS-page verification; [report](expansion-37-aether.md) |

All six new crates are registered in workspace members and default-members;
the registered workspace now contains 34 packages. Temporary standalone tables,
profiles and leaf lockfiles from these new lanes were removed. Existing
sandbox/guest workspaces and external ownership boundaries were preserved.
No registry dependency version migration was needed.

## Runnable interfaces and workflows

The coordinator added two bounded host-data commands:

```sh
cargo run -- json check input.json --json
cargo run -- traceparent 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01 --json
```

JSON checks the whole input, including unknown subtrees and decoded-equivalent
duplicate keys. CLI files are capped at 1 MiB and opened as regular inputs with
no-follow/nonblocking Unix flags. TraceContext IDs are emitted as exact hex
strings, flags are preserved and only bit zero is interpreted as sampled.
Argument and content errors produce consistent JSON when requested. Neither
command modifies input or creates tracing/application state.

`scripts/test-expansion-foundations.sh` executes six self-checking examples:
`bounded_json`, `bounded_trace`, `quarry`, `cpu_fusion`, `incremental_join` and
`snapshot_pages`. Assertions exercise actual codec bytes, completed telemetry,
SQL rows, fused/unfused CPU bit patterns, incremental/batch equality and verified
4 KiB CAS pages, respectively.

The new [Expanded foundation contracts workflow](../../.github/workflows/expansion-foundations.yml)
runs focused tests, strict Clippy and examples on Ubuntu, Windows and macOS,
plus a pinned Lean 4.24 model job. It has read-only repository permissions,
path-filtered push/PR triggers and manual dispatch. PR supersession cancels its
older run; separate main pushes retain distinct concurrency identities.
The snapshot check and seven regressions validate source bytes, all numbered
section identities/digests and document order. Local consistency is not
Google-source authentication or a proof of freshness.

## Review fixes

Independent peer/coordinator reviews produced concrete changes before release:

- Mixed JSON size bounds distinguish an upper ceiling on statically bounded
  contributions from dynamic total length. A 14-byte encoding regression
  prevents interpreting an accumulated ceiling as a lower bound.
- Beacon cancellation enqueues `Error(Cancelled)` and releases its slot only
  after success. Queue-full/backwards-clock failures return the same live owner.
  Caller-supplied remote/local span-ID namespaces remain explicit; this library
  does not validate complete distributed ancestry.
- Quarry dataset updates borrow incoming rows, validate fully and clone only
  after preflight. Failed updates preserve both caller inputs and prior data.
- Tensor rechecks every previously proven shape predicate against final bindings,
  preventing stale partial-environment evidence from authorizing changed shapes.
- The revision checker binds each inventory entry to its numbered heading;
  copying section 36's inventory onto section 37 now fails a regression.
- CLI review removed a strict-Clippy warning and made TraceContext argument
  errors use the same requested JSON output contract as parse failures.

## Local validation

- **1,338 workspace tests/doctests across 157 suites passed**, zero failures or
  ignores, after merging upstream wave-three deliveries through `33bf21b`.
- **99 focused new-library tests/doctests**, five new CLI tests and seven Python
  snapshot regressions passed. Corpus iterations are included within those
  tests, not counted as separate test cases.
- All six self-checking examples executed successfully. Tensor debug/release
  tests compare actual ordered FP32 outputs; no speedup is inferred.
- Strict workspace all-target Clippy, formatting and diff whitespace passed.
- Root CLI and all six new crates compiled for Windows GNU, all targets,
  using portable BLAKE3. This is compilation evidence, not native runtime.
- Both new Lean files compile under actual pinned Lean 4.24: six DBSP/frontier
  statements and seven interval/page statements. Axiom reports contain only
  `propext`, `Quot.sound` and (one Aether theorem) `Classical.choice`. There are
  no admissions/new axioms/compiler-trust tactics. These are model proofs.
- Official Actionlint 1.7.7 checked the new workflow after verifying its release
  archive against the publisher's checksum list; shell syntax and YAML parsing
  were checked too.

The registered workspace total does not imply that the externally owned
standalone Forge runner was registered or tested by this batch. Existing kernel
tests can explicitly skip unavailable APIs; Cargo totals alone do not establish
kernel/device execution.

Logs are local under `/tmp/vesper-expansion37-{workspace,focused,cli,clippy,windows,examples}.log`
and the model-specific Lean logs. Root source builds use the root's private
`target/parallel-next`; examples use `target/expansion37`; Windows checks use
`target/parallel-next-windows`. No source-divergent compiler worktrees share a target.

## Hosted workflow evidence

Pending the first published workflow run. Local validation above is complete;
native Windows/macOS results will be recorded separately when observed.

## Remaining boundaries

These are host foundations with explicit supported slices. Source derives,
compiler effects/region/linearity, vector/egglog lowering, Arrow transport,
durable SQL/storage/stream checkpoints, complete LLM inference and native
KVM/UFFD/encryption/entropy are not implied. Rust Drop remains possible.
Numeric interval checks are not physical/EPT isolation; BLAKE3 byte identity is
not provenance. No zero-global-allocation, nanosecond throughput, distributed
progress, cross-ISA determinism, boot latency or compiler-correctness theorem
is claimed from these tests.
