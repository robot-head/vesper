# Parallel implementation lanes

Base checkpoint: `c5328d0`. Integration branch: `main` (the `work` branch was merged in PR #9). Remote: https://github.com/robot-head/vesper.
The expanded design lives in [specification.txt](specification.txt); the implemented core baseline is [core-specification-baseline.txt](core-specification-baseline.txt).

## Fourth wave: Vesper Warp (§35)

The [fourth-wave prompt index](agent-prompts/wave-four/README.md) splits design §35 (user-space QUIC/TCP networking, linear UMEM rings, BBRv3) into eight concurrent lanes under the [wave-four contracts](rfcs/wave-four-contracts.md). Each lane owns one new `crates/vesper-warp*` crate (lane 8 owns `crates/vesper-sim-session/**`), depends only on crates already on `main`, and commits its own root registration patch on its PR branch. An integration step in `crates/vesper-warp` follows once lanes 1–6 merge.

| Lane | Suggested branch | Exclusive boundary |
| --- | --- | --- |
| QUIC packet core and loss recovery | `wave4/warp-quic-packet-core` | New `crates/vesper-warp/**`, `lean/WarpPacketNumber.lean` |
| BBRv3 and EDT pacing | `wave4/warp-bbrv3` | New `crates/vesper-warp-bbr/**`, `lean/WarpBbr.lean` |
| Linear UMEM frames and AF_XDP rings | `wave4/warp-umem-rings` | New `crates/vesper-warp-umem/**`, `lean/WarpUmem.lean` |
| Timing wheel and EDT launch queue | `wave4/warp-timing-wheel` | New `crates/vesper-warp-wheel/**` |
| Packet protection and nonce tickets | `wave4/warp-packet-protection` | New `crates/vesper-warp-crypto/**` |
| Connection tiles, CID lookup, RSS | `wave4/warp-connection-shards` | New `crates/vesper-warp-shard/**` |
| RFC 9293 TCP | `wave4/warp-tcp` | New `crates/vesper-warp-tcp/**` |
| Source XdpNic and WAN simulation | `wave4/warp-source-simulation` | `crates/vesper-sim-session/**` |

## Current independent batch: sections 20–37

The [design snapshot](specification.txt) has been refreshed through section 37;
[revision metadata](specification-source.json) and a [full-section review](rfcs/expansion-37-review.md)
record its identity, changes and scaffold limitations. Six new isolated lanes
implement JSON codecs, Beacon, Quarry, Tensor, Flux and Aether under the
[expanded ownership contracts](rfcs/expansion-37-contracts.md). Their source paths
are independent of externally merging wave three. The coordinator supplies CLI
wiring, root registration and the new `Expanded foundation contracts` workflow.
[Integrated results and validation](parallel-results/expansion-37-integration.md)
record the completed slices and remaining boundaries.

| Lane | Branch | Exclusive implementation |
| --- | --- | --- |
| JSON/codecs (§20) | `expansion37/serde` | New `crates/vesper-serde/**` |
| Beacon (§22) | `expansion37/beacon` | New `crates/vesper-beacon/**` |
| Quarry (§24) | `expansion37/quarry` | New `crates/vesper-quarry/**` |
| Tensor (§28) | `expansion37/tensor` | New `crates/vesper-tensor/**` |
| Flux (§34) | `expansion37/flux` | New crate and `lean/FluxDbsp.lean` |
| Aether (§37) | `expansion37/aether` | New crate and `lean/AetherMemory.lean` |

## Earlier independent batch and external wave three

The previous seven sessions are complete and integrated. The [third-wave prompt index](agent-prompts/wave-three/README.md) is **assigned to the user's external agent**; its [frozen contracts](rfcs/wave-three-contracts.md) separate frontend, native and platform ownership. Do not assign duplicate writers to those lanes. Locally started duplicates were stopped and preserved as [unmerged spikes](parallel-results/paused-wave-three-spikes.md).

Six additional implementation lanes are implemented alongside that external wave;
their [integration report](parallel-results/parallel-next-integration.md) records
validation, review fixes and remaining boundaries:

| Independent lane | Branch | Exclusive boundary |
| --- | --- | --- |
| Bridge sys-specs | `parallel-next/bridge-sys-specs` | `crates/vesper-ffi/**` |
| Strata slab allocation | `parallel-next/strata-slab` | `crates/vesper-alloc/**` |
| Aura reconciliation | `parallel-next/aura-delta` | New `crates/vesper-aura/**` |
| Ignis command lifetimes | `parallel-next/ignis-lifetimes` | `crates/vesper-ignis/**` |
| Offline dependency audit | `parallel-next/loom-audit` | New `crates/vesper-loom-audit/**` |
| Signed artifact admission | `parallel-next/artifact-admission` | New `crates/vesper-loom-admission/**` |

Their [ownership and acceptance contracts](rfcs/parallel-next-contracts.md) keep wave-three paths read-only. Each uses a separate worktree and target; integration, root registration, CLI wiring and shared schemas stay coordinator-owned. These are subagents on this executor, not separately provisioned cloud sessions: the current app tool surface does not offer session creation.

| Next lane | Requested branch | Exclusive boundary |
| --- | --- | --- |
| Source traits/models | `wave3/source-traits-models` | Additive frontend AST/syntax, checker/interface and Pact bridge refactor |
| Source-native suspension | `wave3/source-native-suspension` | `src/native.rs`, new source-session helpers; fixed IR/ABI |
| Cached Forge macros | `wave3/cached-forge-runner` | New `crates/vesper-forge-runner/**` |
| Crash/WAL/network models | `wave3/crash-wal-network-simulation` | Source simulation and new model crate |
| SIMD kernel model | `wave3/simd-kernel-model` | New Lean/model proof files; scanner codegen read-only |
| macOS AArch64 fibers | `wave3/macos-arm-fibers` | Fiber platform implementation; preserve Linux APIs/ABI v2 |
| Prism connections/routing | `wave3/prism-connection-routing` | `crates/vesper-prism/**` |

Root manifests/lock, CLI/module wiring, existing CI/global docs and shared schema decisions remain coordinator-owned. So do `src/lean.rs`, `src/lean_ir.rs`, `src/lean_nodes.rs`, `crates/vesper-lean-syntax/**` and `crates/vesper-loom-tuf/**`. The complete Lean emitter now builds an AST and renders once; new lanes must not restore raw-source construction.

The [second-wave integration report](parallel-results/wave-two-integration.md) records all handoffs, actual branches, validation and limitations. Historical [second-wave prompts](agent-prompts/next-wave/README.md) are complete, not active reservations.

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
