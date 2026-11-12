# Parallel implementation lanes

Base checkpoint: `c5328d0`. Integration branch: `main` (the `work` branch was merged in PR #9). Remote: https://github.com/robot-head/vesper.
The expanded design lives in [specification.txt](specification.txt); the implemented core baseline is [core-specification-baseline.txt](core-specification-baseline.txt).

## Next implementation batch

The user has launched all seven assignments in the [next-wave prompt index](agent-prompts/next-wave/README.md). Forge landed through remote PR #14 and checked arithmetic through PR #17; the other paths remain reserved for their active sessions. Do not launch duplicate implementations. Only the checked-arithmetic lane edits compiler semantics. Source-native suspension and source trait/model syntax follow that migration.

| Active lane | Requested branch | Reserved paths |
| --- | --- | --- |
| Checked arithmetic (landed) | Actual branch `claude/project-thread-vmzghp`, PR #17 | Core semantics integrated; coordinator SemVer/CI/Lean AST follow-up |
| Native lifecycle | `next/native-lifecycle` | Fiber ABI, native experiment, compatible scheduler changes if needed |
| Cached sandbox builds | `next/loom-cached-builds` | New `crates/vesper-loom-build/**` |
| Pact compiler bridge | `next/pact-compiler-bridge` | New `crates/vesper-pact-bridge/**` |
| Strata collections | `next/strata-collections` | `crates/vesper-alloc/**` |
| Source simulation | `next/source-simulation` | New `crates/vesper-sim-session/**` |
| Forge hygiene/authority (landed) | Actual branch `claude/project-thread-1z4nx8`, PR #14 | `crates/vesper-forge/**`; coordinator registration/testing |

The names above are the requested branch names, not confirmed remote refs. Record actual branches/PRs, base SHAs, commits and validation evidence when handoffs arrive. The individual prompts define exact ownership, including report files and fixture-only exceptions.

The coordinator retains root workspace/dependency registration, lockfile, CLI/module wiring, CI and global documentation. Do not pre-register unfinished crates or connect macros to caching before the provenance/authority gate is reviewed. Recheck compiler consumers against the arithmetic migration before integration; do not merge a partial arithmetic backend update.

While these sessions run, the coordinator owns read-only locked-resolution validation in `crates/vesper-loom-solver`, `src/package_commands.rs`, CLI routing/help and the new `tests/locked_resolution_cli.rs`. This work uses manifest/catalog DTOs and does not edit the active compiler, fiber, allocator or new integration-crate lanes.

The coordinator also owns offline package-graph inspection in the new `src/package_graph.rs` and `tests/package_graph_cli.rs`, with CLI routing and documentation. It consumes the existing calculus graph without changing compiler or active library APIs.

Release-version checking in `crates/vesper-semver`, the package-command adapter and the new `tests/release_cli.rs` is also coordinator-owned. It does not change the active source-interface exporter or arithmetic semantics.

Local Lean artifact auditing is coordinator-owned in the new `src/proof_commands.rs`, `tools/proof-audit/**` and `tests/proof_audit_cli.rs`. This uses an independent trusted Lean helper and does not edit the reserved extractor, shared IR or `lean/CheckedArithmetic.lean`. It does not authorize package publication or proof-cache reuse.

Declared-permission upgrade checks in `crates/vesper-loom-calculus/src/upgrade.rs`, the package-command adapter and `tests/upgrade_cli.rs` are coordinator-owned. Offline builder-attestation verification is being implemented separately in the new `crates/vesper-loom-trust/**`; neither task edits the active cached-build, Forge or compiler lanes.

CycloneDX export in `src/sbom_commands.rs`/`tests/sbom_cli.rs` and the independent native scanner milestone in new `crates/vesper-simd/**` are also coordinator-owned. The scanner does not edit source ISA capability typing or the reserved native compiler backend.

The independent offline TUF verifier in new `crates/vesper-loom-tuf/**` and bounded
HTTP/1.1 framing in new `crates/vesper-prism/**` are coordinator subagent lanes.
Neither is a source compiler or runtime integration. Root registration remains
with the coordinator after review and validation.

Structured Lean syntax is being implemented independently in new
`crates/vesper-lean-syntax/**`. It preserves the pinned Lean toolchain. After the arithmetic handoff landed,
scalar literals began using its nodes; full emitter migration is coordinator-owned.

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

All seven original feature handoff sessions are complete and their contributions are merged into `main`. The additional platform-CI handoff (`claude/project-thread-hlbvro`) is integrated too. The prompts below record their original scope; current reservations are listed in the active batch above.

| Assignment | Prompt |
| --- | --- |
| Native continuation ABI prototype | [native-pulse-abi.md](agent-prompts/native-pulse-abi.md) |
| Lean effects and mutable places | [lean-effects-places.md](agent-prompts/lean-effects-places.md) |
| Strata region allocator | [strata-regions.md](agent-prompts/strata-regions.md) |
| Pact canonical trait goals | [pact-goals.md](agent-prompts/pact-goals.md) |
| Loom content-addressed store | [loom-cas.md](agent-prompts/loom-cas.md) |
| Hermetic WASI macro/build host | [forge-wasi.md](agent-prompts/forge-wasi.md) |
| Ignis barrier graph core | [ignis-barrier-graph.md](agent-prompts/ignis-barrier-graph.md) |

The integration checkpoint registers every delivered library plus the native experiment in the root workspace. Focused review uses separate `integration/review-*` worktrees; shared manifests, lockfile and CI remain coordinator-owned.

| Completed handoff | Result and remaining boundary |
| --- | --- |
| Native ABI | [ABI report](rfcs/native-pulse-abi.md); hand-written CLIF on pinned stacks, source lowering pending |
| Lean mutable effects | [Report](parallel-results/lean-effects-places.md); executable extraction, explicit rejection of unsupported alias cases |
| Strata | [Report](parallel-results/strata-regions.md); host-region library, source lifetime/alias rules pending |
| Pact | [Report](parallel-results/pact-goals.md); solver/coherence DTOs, source integration pending |
| Loom CAS | [Report](parallel-results/loom-cas.md); private immutable store, build integration pending |
| Forge WASI | [Report](parallel-results/forge-wasi.md); component host, compiler hygiene and confinement pending |
| Ignis | [Report](parallel-results/ignis-barrier-graph.md); barrier model, Windows execution pending |

## Integration rules

1. Fetch `origin/main` and create a separate branch/checkout. Rebase once before integration rather than repeatedly rewriting a running lane.
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
lean lean/EffectPlaces.lean
scripts/test-aarch64.sh
scripts/test-native-host.sh
```

The current host denies `io_uring_setup` with `EPERM`; actual kernel integration must also run on a suitable host.


## Coordinator second wave

The coordinator owns source-interface export, CLI adapters, and offline local-catalog resolution. These do not overlap the seven external assignments above.

| Lane | Branch | Ownership |
| --- | --- | --- |
| Checked interface export | `parallel/interface-export` | Integrated: `src/interface.rs`, 13 focused tests |
| Offline solver | `parallel/loom-solver` | Integrated: `crates/vesper-loom-solver/**`, 19 focused tests |
| CLI/integration | `main` | Integrated: source-interface/SemVer/offline resolve commands, 7 end-to-end tests |

Usage and guarantees are in [package-workflows.md](package-workflows.md).

External Strata milestone from PR #1 was merged upstream during the second wave and preserved on integration. The coordinator registers its host-runtime crate for workspace checks; source-level Region lowering and the proposed v0 failure semantics remain design proposals pending compiler-contract review.

The [completed handoff integration report](parallel-results/handoff-integration.md) records review fixes, combined validation, and the next source/build integration boundaries.
