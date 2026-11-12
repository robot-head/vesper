# Parallel implementation lanes

Base checkpoint: `c5328d0`. Integration branch: `work`. Remote: https://github.com/robot-head/vesper.
The expanded design lives in [specification.txt](specification.txt); the implemented core baseline is [core-specification-baseline.txt](core-specification-baseline.txt).

## First-wave results

| Lane | Branch | Exclusive ownership | Status |
| --- | --- | --- | --- |
| Workspace extraction | `parallel/workspace` | Root workspace + core/syntax/Pulse extraction | Integrated as `76f1a0a`; 267 baseline tests passed |
| SemVer | `parallel/semver` | `crates/vesper-semver/**` | Integrated: §9 interface compatibility; 25 tests |
| Bridge wire layouts | `parallel/ffi-wire` | `crates/vesper-ffi/**` | Integrated: §13.2 byte layouts/views; 9 tests |
| Loom calculus | `parallel/loom-calculus` | `crates/vesper-loom-calculus/**` | Integrated: manifest/solution validation; 17 tests + doctest |
| SimPulse | `parallel/simpulse` | `crates/vesper-simpulse/**` | Integrated: deterministic scheduling/replay; 18 tests + 2 doctests |
| Contract audit | `parallel/contracts` | `docs/rfcs/parallel-contracts.md` | Reviewed: [contract audit](rfcs/parallel-contracts.md) |

Each agent used a distinct worktree under `/workspace/vesper-worktrees/`. Agents commit only to their own branch. The coordinator reviews and cherry-picks; a completed subagent turn does not by itself mark a design section complete.

## External agent prompts

These lanes are reserved for user-launched cloud sessions or other agentic systems. No cloud-session creation API is exposed in the current toolset. Copy the full text prompt from one file per independent agent:

| Assignment | Prompt |
| --- | --- |
| Native continuation ABI prototype | [native-pulse-abi.md](agent-prompts/native-pulse-abi.md) |
| Lean effects and mutable places | [lean-effects-places.md](agent-prompts/lean-effects-places.md) |
| Strata region allocator | [strata-regions.md](agent-prompts/strata-regions.md) |
| Pact canonical trait goals | [pact-goals.md](agent-prompts/pact-goals.md) |
| Loom content-addressed store | [loom-cas.md](agent-prompts/loom-cas.md) |
| Hermetic WASI macro/build host | [forge-wasi.md](agent-prompts/forge-wasi.md) |
| Ignis barrier graph core | [ignis-barrier-graph.md](agent-prompts/ignis-barrier-graph.md) |

Do not assign a prompt to two agents simultaneously. Record the external branch/PR and owner here once launched. Do not give an external agent a currently active internal lane.

## Integration rules

1. Fetch `origin/work` and create a separate branch/checkout. Rebase once before integration rather than repeatedly rewriting a running lane.
2. Keep root `Cargo.toml`, `Cargo.lock`, shared compiler representations, CLI routing, and CI under one integration owner.
3. New independent crates may temporarily declare their own `[workspace]` to test before registration. Integration removes that table and nested lockfile, registers the crate in both workspace members/default-members, and updates the root lockfile.
4. Transport DTOs and experimental contracts must carry a schema/ABI version. Do not pretend they are already the compiler's canonical `.vhir` representation. Adapters are separate integration milestones.
5. Preserve existing `vesper::` module reexports during extraction. Avoid dependency downgrades from illustrative design scaffolds.
6. Every contribution needs meaningful positive/negative tests, formatting, Clippy, explicit limits, and a commit SHA. Native changes need evaluator comparisons and AArch64 object validation; Lean changes need real Lean compilation/execution.
7. Kernel/hardware tests must report unavailable validation. Mocks validate the modeled protocol, not hardware isolation or kernel behavior.
8. Keep native codegen and Pulse ABI changes coordinated. Saved raw stack pointers do not alone establish relocation safety.
9. The user-approved field premise `k_i ⊑ m_i` remains binding. Newly expanded design requirements such as checked arithmetic need coordinated migration across all backends; preserve the current checkpoint until that migration is reviewed as a whole.
10. Core/workspace integration precedes source-language APIs for new libraries. Prism/Aura/Ignis production frameworks depend on stable traits, allocation, FFI, and runtime contracts; the Ignis external prompt is intentionally a GPU-independent graph milestone.

## Acceptance commands

```sh
cargo fmt --all -- --check
VESPER_LEAN=lean cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
lean lean/Foundations.lean
lean lean/RuntimeFrames.lean
lean lean/SpecificationCounterexample.lean
scripts/test-aarch64.sh
```

The current host denies `io_uring_setup` with `EPERM`; actual kernel integration must also run on a suitable host.


## Coordinator second wave

The coordinator owns source-interface export, CLI adapters, and offline local-catalog resolution. These do not overlap the seven external assignments above.

| Lane | Branch | Ownership |
| --- | --- | --- |
| Checked interface export | `parallel/interface-export` | Integrated: `src/interface.rs`, 13 focused tests |
| Offline solver | `parallel/loom-solver` | Integrated: `crates/vesper-loom-solver/**`, 19 focused tests |
| CLI/integration | `work` | Integrated: source-interface/SemVer/offline resolve commands, 7 end-to-end tests |

Usage and guarantees are in [package-workflows.md](package-workflows.md).

External Strata milestone from PR #1 was merged upstream during the second wave and preserved on integration. The coordinator registers its host-runtime crate for workspace checks; source-level Region lowering and the proposed v0 failure semantics remain design proposals pending compiler-contract review.
