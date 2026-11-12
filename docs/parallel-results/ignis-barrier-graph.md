# Ignis barrier and pass graph core (design §19.4, §19.7)

Lane: Ignis barrier graph core. Branch: `claude/project-thread-26zqwc`, based on `origin/work` at `72493b8`.
Owned paths: `crates/vesper-ignis/**` and this file. No root workspace, lockfile, CLI, CI, compiler, or shared IR file is changed.

## What is implemented

`crates/vesper-ignis` is a standalone library with no dependencies and a temporary `[workspace]` table. It is GPU-independent: it never calls D3D12. It turns a typed render graph into a deterministic plan of D3D12 enhanced-barrier batches and cross-queue fence signals/waits.

| Requirement | Implementation |
| --- | --- |
| Resource identity | `BufferHandle` and `TextureHandle` are distinct types tagged with a process-unique graph id; using another graph's handle is `GraphError::ForeignHandle`. |
| Subresource/range overlap | Buffers are tracked as byte-range segments (split on use boundaries). Textures are tracked per subresource (mip, array slice, plane) using D3D12 subresource numbering. Disjoint ranges never create edges or barriers, with one exception: writers of the same buffer are serialized even on disjoint ranges, because the spec allows at most one concurrent writer per buffer. |
| Read/write access | `Access`/`Sync` are bit sets with the `D3D12_BARRIER_ACCESS`/`D3D12_BARRIER_SYNC` values. Write accesses: RT, UAV, DSV write, stream-out, copy/resolve dest, AS write (`UNORDERED_ACCESS` is always treated as a write). |
| Layouts | `Layout` mirrors `D3D12_BARRIER_LAYOUT` (common, read, write, and direct/compute queue-specific variants). A use may give an explicit layout or let the planner keep the current one when compatible, otherwise pick one from a fixed preference list. |
| Queue ownership | Queues are `Direct`, `Compute`, or `Copy`; any number of each. Per-queue access, sync, and layout support is validated at `add_pass`. |
| Hazards | RAW, WAR, WAW, and layout transitions (treated as writes) on overlapping ranges. Same-queue hazards produce barriers; cross-queue hazards produce fence waits. |
| Read reuse | Reads in a compatible layout whose sync/access scope is already covered by an earlier barrier emit nothing. A read in a scope the earlier barrier did not cover gets its own barrier. Its `SyncBefore`/`AccessBefore` include the earlier barrier's after-scope, as sequential barriers in one ECL scope require. |
| Cross-queue sync | One timeline fence per queue, values 1, 2, … in stream order. Waits are elided using vector clocks when a queue already knows directly or transitively that the producer finished. Concurrent reads on several queues in a shared layout are allowed. |
| Queue handoff | When the consumer queue cannot leave the current layout (for example, compute after `RENDER_TARGET`, or any copy-queue texture transition), the single queue that last used the subresource transitions it (to the target, or to `COMMON`) after its last use, with `SyncAfter = NONE`, before signaling. |
| Long-distance sync | Optional split barriers (`SyncAfter = SPLIT` after the last touch, `SyncBefore = SPLIT` before the consumer) when other passes run on that queue in between. Splits are not used when the barrier also depends on accesses on another queue, because those fence waits sit before the end half only. Begin/end halves carry identical access/layout/subresources. |
| Final layouts | `TextureDesc::final_layout` (for example `PRESENT`) ends the plan with a transition on the most recent user's queue, joining other queues with fence waits first. |
| Ordering and cycles | Declaration order is the sequential meaning. `add_dependency` adds explicit edges; Kahn's algorithm (lowest index first) gives a deterministic global order. Cycles return `PlanError::Cycle` with the cycle's passes in edge order. |
| Determinism | All maps are ordered (`BTreeMap`/`BTreeSet`), barrier batches are sorted, and `Plan::render` produces a stable text form used by golden tests. |

## Checking the §19.7 scaffold against the enhanced-barrier spec

The scaffold was checked against the [D3D12 Enhanced Barriers specification](https://microsoft.github.io/DirectX-Specs/d3d/D3D12EnhancedBarriers.html). These points were changed rather than copied:

1. **WAR hazards were missed.** The scaffold only synchronized when there was a prior writer, so a read followed by a write in the same layout (for example a UAV read then a UAV write) emitted nothing. Here every write waits for all reads since the last write.
2. **Reader scopes were overwritten.** The scaffold kept only the last access, so the second of two readers in different stages was not recorded. Here readers accumulate, and the WAR barrier's `SyncBefore`/`AccessBefore` is their union.
3. **Read-after-read elision ignored sync scope.** One barrier from a compute write to a pixel-shader read does not order a later compute-shader read. The planner tracks the after-scope already covered and emits a chained barrier when a new scope is not covered. Aggregate scopes (`ALL`, `DRAW`, `ALL_SHADING`, `NON_PIXEL_SHADING`) are expanded as the spec's tables state. `ALL` is not assumed to cover the acceleration-structure build/copy/postbuild scopes, which the spec's table omits.
4. **Fences were chosen by pass distance.** The scaffold emitted a fence signal/wait when the consumer was three or more pass indices after the writer, without regard to queues or intermediate accesses. Here queue fences are used only between queues. Within a queue, the real mechanism for overlapping a barrier with intermediate work is the split barrier (`D3D12_BARRIER_SYNC_SPLIT`), which is what the planner emits. The spec does contain a Fence Barriers section (`SignalBarrier`/`WaitBarrier` on `ID3D12GraphicsCommandList11`, Tier 1 retail), but the exact signatures and Agility SDK version could not be confirmed. The "1.620" version in the design was not verified, so command-list fence barriers are not emitted.
5. **Buffers have no layout and whole-buffer barriers.** The spec requires `Offset = 0` and `Size` = the buffer size or `UINT64_MAX`. Ranges are used for hazard detection only; emitted `BufferBarrier`s cover the whole buffer. A split barrier on a buffer is not used if any other range of that buffer is touched in between.
6. **Single-valued sync/access enums cannot express unions.** These are now bit sets with the D3D12 bit values.
7. **Queue-type rules.** Both `LayoutBefore` and `LayoutAfter` must be valid on the queue that performs the barrier. Compute queues cannot use `RENDER_TARGET`/`DEPTH_STENCIL_*`/`RESOLVE_*`, and copy queues support only `COMMON` and cannot transition layouts. `DIRECT_QUEUE_*` layouts are rejected on compute and vice versa.
8. **`NONE`/`NO_ACCESS` pairing.** `SyncBefore = NONE` is always paired with `AccessBefore = NO_ACCESS`, and likewise for the after half. A transition from `UNDEFINED` uses `NONE`/`NO_ACCESS`. The planner never emits `LayoutAfter = UNDEFINED`, on which the spec is silent.
9. **Layout/access and access/sync compatibility** follow the spec's tables and are validated at declaration. For example, `UNORDERED_ACCESS` is rejected in `SHADER_RESOURCE`, and `RENDER_TARGET` access is rejected with `COMPUTE_SHADING` sync.
10. The scaffold's `FxHashMap`, `SmolStr` errors, and `transition_texture(pass_idx, …)` immediate API are replaced with a declarative graph, ordered maps, and structured error enums. That avoids new dependencies and nondeterministic iteration.

## Public API

```rust
pub const PLAN_CONTRACT_VERSION: u32 = 0;

// D3D12 vocabulary (bit values from d3d12.h)
pub struct Sync(u32);    // NONE, ALL, DRAW, …, SPLIT; bits, from_bits, contains, intersects, expanded, covers, |
pub struct Access(u32);  // COMMON, VERTEX_BUFFER, …, NO_ACCESS; is_write, incompatible_with_sync, |
pub enum Layout { Undefined, Common, GenericRead, RenderTarget, UnorderedAccess, DepthStencilWrite,
                  DepthStencilRead, ShaderResource, CopySource, CopyDest, ResolveSource, ResolveDest,
                  ShadingRateSource, DirectQueue*(6), ComputeQueue*(6) }  // PRESENT = Common; raw, allows, compatible_access
pub enum QueueKind { Direct, Compute, Copy }  // supports_layout, can_transition, supported_access, supported_sync

// Graph
pub struct RenderGraph;
impl RenderGraph {
    fn new() -> Self;
    fn add_queue(&mut self, QueueKind) -> QueueId;
    fn add_buffer(&mut self, BufferDesc) -> Result<BufferHandle, GraphError>;
    fn add_texture(&mut self, TextureDesc) -> Result<TextureHandle, GraphError>;
    fn add_pass(&mut self, PassDesc) -> Result<PassId, GraphError>;
    fn add_dependency(&mut self, before: PassId, after: PassId) -> Result<(), GraphError>;
    fn plan(&self) -> Result<Plan, PlanError>;
    fn plan_with(&self, PlanOptions) -> Result<Plan, PlanError>;
    fn queue_kind, resource_name, pass_name, pass_count;
}
pub struct BufferDesc { name, size }
pub struct TextureDesc { name, mip_levels, array_size, plane_count, initial_layout, final_layout }
    // simple, with_mips, with_array_size, with_final_layout
pub struct PassDesc;  // new(name, queue), buffer(h, ByteRange, Access, Sync), texture(h, SubresourceRange, Access, Sync, Option<Layout>)
pub struct ByteRange { offset, size }               // WHOLE, new
pub struct SubresourceRange { first_mip, mip_count, first_slice, slice_count, first_plane, plane_count }
    // ALL, mips, slices, single
pub enum ResourceRef { Buffer(BufferHandle), Texture(TextureHandle) }
pub struct QueueId, PassId;  // index()

// Plan
pub struct PlanOptions { split_barriers: bool }      // default: true
pub struct Plan { order: Vec<PassId>, dependencies: Vec<Dependency>, queues: Vec<QueueStream> }
    // barrier_count, render(&RenderGraph) -> String
pub struct QueueStream { queue, kind, commands: Vec<Command> }
pub enum Command { Wait { queue, value }, Barriers(Vec<Barrier>), Execute(PassId), Signal { value } }
pub enum Barrier { Buffer(BufferBarrier), Texture(TextureBarrier) }
pub struct BufferBarrier { resource, sync_before, sync_after, access_before, access_after }
pub struct TextureBarrier { resource, sync_before, sync_after, access_before, access_after,
                            layout_before, layout_after, subresources }
pub struct Dependency { before, after, kind: DependencyKind, resource: Option<ResourceRef> }
pub enum DependencyKind { ReadAfterWrite, WriteAfterRead, WriteAfterWrite, ReadOrder, Explicit }

// Errors
pub enum GraphError { ForeignHandle, InvalidResource, InvalidAccess, InvalidSync, AccessSyncMismatch,
                      AccessLayoutMismatch, AccessResourceKindMismatch, QueueUnsupported, InvalidRange,
                      ConflictingUses, SelfDependency }
pub enum QueueRestriction { Access(Access), Sync(Sync), Layout(Layout) }
pub enum PlanError { Cycle { passes }, NoLayoutForAccess, UnsupportedQueueHandoff, FinalLayoutUnreachable }
```

## Validation

Run from `crates/vesper-ignis` (standalone workspace):

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

Results on this host (Linux, cargo 1.97.0): formatting is clean; 38 integration tests and 1 doctest pass; Clippy reports no warnings.

The tests cover:
- Hazards on one queue: RAW (exact texture barrier fields), WAR on a buffer, WAW as a UAV barrier, and no barrier for a first read.
- Disjoint and adjacent buffer ranges, serialized buffer writers on disjoint ranges (same queue and cross-queue), a mip-chain downsample that syncs only adjacent mips, and box grouping of subresources (whole texture, one cube face).
- Read reuse: shared readers, and `GENERIC_READ` serving shader and copy reads. A new sync scope needs a chained barrier. Aggregate sync covers later reads.
- Layout transitions: RT↔SRV round trips, automatic `DIRECT_QUEUE_GENERIC_READ` for depth+shader reads, and final `PRESENT`.
- Split barriers, shown with a golden plan. No split across an intervening access to the same buffer, and none before a wait for a remote reader. Sequential barriers chain on the previous after-scope.
- Ordering: two-pass and three-pass cycles, reordering by explicit edges, and self-dependency rejection.
- Cross-queue: RAW and WAR fences, redundant and transitive wait elision, ping-pong fences, and a fence simulator that checks increasing values and no deadlock.
- Queue rules: concurrent reads on two queues, RT handoff to compute (golden), copy-queue transition errors and the COMMON path, a final-layout join across queues, and rejection of queue-specific layouts.
- Determinism and invariants on a mixed frame graph (direct, compute, and copy queues): identical renders across graphs, stream order consistent with the global order, matched split pairs, and every texture barrier's access/layout/queue validity.
- Every `GraphError` variant raised by declaration.

## Integration requirements

1. Register `crates/vesper-ignis` in root `members` and `default-members`, remove its temporary `[workspace]` table and `.gitignore`d nested lockfile, and regenerate the root `Cargo.lock`. There are no dependencies to add.
2. `PLAN_CONTRACT_VERSION = 0` versions the plan shape. A future Windows executor maps `Command`s to `ID3D12CommandQueue::Wait`/`Signal`, `ExecuteCommandLists` boundaries at each wait/signal, and `ID3D12GraphicsCommandList7::Barrier` batches (`Sync::bits`, `Access::bits`, `Layout::raw` pass through).
3. Source-language integration is separate: lowering `in`/`inout` frame-graph projections (§19.4) to `PassDesc` uses, typestate command lists, and `vesper verify` extraction are not attempted.

## Ambiguities and proposed decisions

| Question | Decision here |
| --- | --- |
| Cross-queue visibility | The spec says ECL completion guarantees "full pipeline sync and cache flush". The planner relies on queue fences plus that flush, and emits no barrier on the consumer queue for a same-layout cross-queue use. The spec's "Cross Queue Synchronization" section was not fully read; confirm at the Windows gate. |
| Concurrent reads of a non-simultaneous-access texture on two queues in a shared read layout | Allowed (no extra fence between readers). The spec is explicit only for buffers and simultaneous-access textures; confirm with the debug layer. |
| `SyncBefore = NONE` for a transition after a fence wait | Used only when no access on the same queue is outstanding. Any earlier same-queue access is separated by a wait/signal (an ECL boundary), which satisfies the spec's "no preceding accesses in the same ECL scope" rule. |
| "No other intervening barriers on the same resource" for split pairs | Interpreted per subresource for separate split pairs started in the same batch. Splits never straddle a pass that touches the resource. |
| Layout after `UNDEFINED` | Never emitted (spec is silent). |
| `ALL` vs acceleration-structure sync scopes | `ALL` is not treated as covering them (conservative). |
| UAV reads | Always treated as writes; the API has no UAV-read-only access bit. |
| Auto layout choice | Keep the current layout if it admits the access on this queue; otherwise take the first compatible entry of a fixed list (`SHADER_RESOURCE`, `COPY_SOURCE`, `COPY_DEST`, `UNORDERED_ACCESS`, `RENDER_TARGET`, `DEPTH_STENCIL_READ`, `DEPTH_STENCIL_WRITE`, resolve, shading rate, `GENERIC_READ`, `DIRECT_QUEUE_GENERIC_READ`, `COMMON`). Queue-specific layouts are only used when requested, except `DIRECT_QUEUE_GENERIC_READ` for read combinations no common layout admits. |
| Overlapping reads in one pass with one explicit layout | The explicit layout must admit the other use's access, or `add_pass` returns `AccessLayoutMismatch`. |
| Overlapping auto-layout reads | Kept in declaration order (`ReadOrder`) since a transition may occur between them; this only constrains explicit-edge reordering. |
| Explicit edges on one queue | Order only; no memory barrier is implied. |
| Fence barriers (design's "Agility SDK 1.620 D3D12_FENCE_BARRIER") | Not emitted; replaced with split barriers within a queue and queue fences across queues until the API shape and version are verified. |

## Limitations

- No D3D12 calls, no debug-layer or hardware validation, and no Windows execution. Bit values and tables follow the published spec and `d3d12.h` names as recorded here and must be checked against the Agility SDK headers at the Windows acceptance gate.
- The tests check the modeled rules. They are not a soundness proof, and no Lean extraction (§19.4 item 4) is provided.
- Not modeled: simultaneous-access textures, placed-resource aliasing, `DISCARD` flags, global barriers, video queues/layouts, `DIRECT_QUEUE_GENERIC_READ_COMPUTE_QUEUE_ACCESSIBLE`, implicit COMMON-layout barrier-free access, legacy resource states and promotion/decay, and external synchronization before the graph starts (the caller guarantees prior GPU work on imported resources is complete).
- Handoffs require that all outstanding accesses to the subresource are on one other queue; otherwise `UnsupportedQueueHandoff` is returned rather than inserting a mid-stream join.
- Pairwise hazard analysis is O(passes² × uses²). No performance targets or allocation claims are made; the planner allocates on the host heap.
- Not in scope: ECS, Work Graphs, DXR, DirectStorage, DXC/WASI shader compilation, swap chains, and UI integration.
