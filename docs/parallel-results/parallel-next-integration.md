# Independent milestones alongside external wave three

Six implementation lanes are integrated from baseline
`1ffd0aec27dfcc24c5564585ae9068255490bf61`. The user's other agent owns all
wave-three production paths. A diff check confirms that this integration changes
none of those frontend/native/macro/simulation/SIMD/fiber/Prism paths.
Locally started duplicates were stopped and preserved as
[unmerged spikes](paused-wave-three-spikes.md); none is part of this delivery.

The app exposed saved cloud-environment discovery but no cloud-session creation
action. These six tasks used actual subagents with isolated git worktrees and
build targets on this executor. They were not separately provisioned cloud
sessions. Root registration, CLI wiring and validation were coordinated on main.

| Milestone | Integrated commits | Result and remaining boundary |
| --- | --- | --- |
| Bridge sys-specs | `e62e6f1`, follow-up `c7233d3` | Bounded `.vsys` grammar, explicit target C ABI/link contracts, checked raw returns/slice arguments and preserved owners. Real Linux C/Rust execution; source calls/loading/handle finalizers/isolation remain separate. [Report](bridge-sys-specs.md) |
| Strata slab | `3dc5424` | Real bounded local slabs, 48 classes, aligned initialized byte owners, recoverable resize/transfer/free and backing lifetime after pool drop. Remote frees/typed slabs/source allocation typing unsupported. [Report](strata-slab.md) |
| Aura delta | `9d50117` | Stable keyed widget/slot identities, sparse patches, transactional recording backend, subscriptions and recoverable close/shutdown. Native toolkit/FFI/event loop and source typing remain separate. [Report](aura-delta.md) |
| Ignis lifetimes | `120a213` | Real planner output drives owned command submission, checked fence retirement and completion-gated scratch. Uncertain/partial submissions retain all owners. Executing CPU backend; D3D12/DirectStorage/IOCP hardware execution remains separate. [Report](ignis-lifetimes.md) |
| Offline audit | `bac6338`, follow-up `7cff276` | Validated reachable Vesper/native graph, local advisory/SemVer and explicit SPDX-subset policy, root paths and complete input digests. Local catalogs are caller trust inputs. [Report](loom-audit.md) |
| Artifact admission | `eb1283d` | Fresh configured-key quorum and actual owned blob verification precede CAS creation/import; bounded revalidated retrieval and no-clobber copy. No persistent trust bit, build-cache approval or package installation. [Report](artifact-admission.md) |

The three new crates are registered in both workspace lists, for **28 workspace
packages** total. Temporary standalone workspaces/lockfiles were removed.
Existing dependency versions and sandbox guest workspace are preserved. The
shared lock changes add only path packages/dependency edges; there is no
registry dependency version migration.

## CLI integration

- [`vesper audit`](../package-audits.md) emits versioned JSON. Clean exits 0;
  findings, unknown assessments, invalid inputs and exhausted budgets exit 1.
  It validates nonempty roots and writes no files or package execution state.
- [`vesper attest import`](../builder-attestations.md#importing-verified-blob-bytes)
  consumes a bounded regular artifact file and freshly checks complete expected
  identity/current policy/bundle/clock and actual content before CAS writes.
  Rejection does not create a store. Existing `attest verify` remains read-only.
  Admission does not create derivation-index or permanent cache-trust records.

## Independent review and integration regressions

Review found and fixed these issues before publication:

- Bridge host matching now requires an actual 64-bit little-endian data model;
  architecture/OS/environment alone misidentified x32/ILP32. Enum descriptors
  share immutable backing, preventing expansion amplification from repeated
  references. An adversarial case covers 6,144 parameters sharing 256 variants.
- Aura transactions and shared borrows are thread-confined. Recording-backend
  limits independently cover text/depth and final teardown cost even when a
  session uses looser limits. Every accepted state remains explicitly closeable
  within its backend patch budget.
- Empty audit roots are rejected in the library, including programmatic DTOs;
  the underlying calculus permits empty graphs and cannot alone enforce this
  command's meaningful audit boundary.
- Artifact import rejects nonregular/final-symlink inputs and uses Unix
  nonblocking/no-follow opening. A subprocess regression verifies that FIFO
  artifacts fail promptly without initializing the store.
- Ignis tests include genuine partial multi-queue publication: one queue retires
  while another never signals, so shutdown stays incomplete and retains every
  owner. Acknowledgements or uncertain acceptance never substitute for terminal
  original completion reports.

## Validation

- All 16 focused audit/import/existing-attestation CLI regressions passed.
- Windows GNU compilation passed for root CLI, Bridge, Strata, Aura, Ignis,
  audit and admission, all targets, with portable BLAKE3. This is compilation,
  not Windows runtime execution.
- Full workspace tests: **1,172 tests/doctests across 131 suites passed**,
  zero failures or ignores, including actual pinned Lean 4.24 and the real
  Linux C/Rust ABI fixture. Existing kernel tests have skip-on-unavailable
  branches; Cargo totals alone do not establish kernel execution.
- Strict workspace all-target Clippy passed with `--locked --offline -- -D warnings`.
- Workspace formatting and diff whitespace checks passed.

Commands use an isolated root `target/parallel-next`, one build job, disabled
incremental compilation and zero development/test debug information. Windows
uses a distinct `target/parallel-next-windows`. Logs are retained locally as
`/tmp/vesper-next-workspace-tests.log`, `/tmp/vesper-next-cli-tests.log`,
`/tmp/vesper-next-clippy.log` and `/tmp/vesper-next-windows-check.log`.

Host protocols are conditional on trusted adapter behavior and supplied policy
inputs. Thread confinement is not proof of OS main-thread identity. Rust Drop
and public generic payloads do not prove Vesper source linear exhaustion,
native resource uniqueness or authentic hardware completion. No native GUI/GPU,
source integration, process-crash durability, provenance, latency, allocation
rate or compiler-correctness theorem is inferred from these milestones.
