# Paused duplicate wave-three spikes

The user clarified that all wave-three lanes were already assigned to their
other agent. Local duplicates were interrupted immediately and preserved on
separate branches. **None is merged or published, and none is a completed
handoff.** The external implementation remains authoritative.

| Paused local branch | WIP commit | State when stopped |
| --- | --- | --- |
| `wave3/source-traits-models` | `6069cc158bb6d603f130ee4efb95d762a926ced0` | Uncompiled frontend draft; no acceptance tests |
| `wave3/source-native-suspension` | `37cce5bc693113ed1f3ee5fb8b20e2720b89b064` | Initial library check passed; execution tests canceled |
| `wave3/cached-forge-runner` | `41448198aa11764ea661fd2157d087c9f471f055` | Unvalidated runner draft |
| `wave3/crash-wal-network-simulation` | `9dba774a6db6038a5eb60eb4250ed700aeb4a0e7` | Partial checkpoint draft references an absent module; does not compile |
| `wave3/simd-kernel-model` | `d1f446f` | Lean proofs compiled; Rust model/tests unvalidated |
| `wave3/macos-arm-fibers` | `c4b8c07` | Linux tests and Darwin library compilation passed; register probe unvalidated; no Darwin execution |
| `wave3/prism-connection-routing` | `990c632d926c6524284c1bb54a87adc3582e795a` | Incomplete codec/router draft; known budget-liveness gap |

Worktrees remain under `/workspace/vesper-worktrees/wave3-*`. Some retain
temporary uncommitted test wiring or generated caches; those are not delivery
patches. Do not automatically cherry-pick these commits alongside the external
owner's work. New independent tasks use `parallel-next/*` branches and different
paths, as recorded in [their contracts](../rfcs/parallel-next-contracts.md).
