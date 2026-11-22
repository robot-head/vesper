# 38 Aura native interfaces

A stream inspector can receive a row while its user is selecting another. Aura separates the table’s state from its native controls so it can insert the row and preserve the selection without rebuilding the entire view.

## 38.1 Define state, view, and lifecycle

A view reads application state. An update changes the state in response to input. When the view goes away, its owner must unregister its native event subscriptions. A virtualized table can keep controls only for visible rows while the underlying data set is much larger.

## 38.2 Patch the part of a view that changed

A state lens identifies the portion a subview reads or updates. Computing a view delta produces changes such as replacing one label or inserting a row. Stable identities let a backend retain focus and selection while applying such patches. Two updates may proceed independently when their write footprints are disjoint.

## 38.3 Lower one view to native controls

WinUI 3, GTK 4, and SwiftUI have different object, subscription, and update conventions. A backend maps a common view description to those conventions. Preserve the native ownership and notification rules at each boundary so a deleted widget leaves no active subscription.

## 38.4 Keep UI work on its owning thread

A native widget handle can be thread-affine. A background fiber may prepare data and hand an update back to the UI event loop. A close request can carry a linear decision to finish saving and close or to keep the window open. The close handler must consume the decision exactly once, including when a save fails.

## 38.5 Build the stream inspector’s update path

A background source adds records. A reducer updates the table model, and a view delta tells the backend which visible rows changed. The selection can refer to a stable row key even as new records arrive. Closing the inspector must also end its subscriptions.

## 38.6 Test semantics without opening a window

A headless backend can record a widget tree and accessibility tree for a sequence of inputs. Check a control’s accessible name, focus, and state after a selection change. Such tests exercise the model. Native accessibility integration and appearance still need checks on each toolkit.

# 39 Ignis render graphs and GPU lifetimes

A render graph describes which passes read or write each resource on the graphics processing unit (GPU). Ignis uses that information to plan synchronization and identify when memory can safely return to its owner.

## 39.1 Describe who reads and writes each frame resource

A component query can name disjoint columns in an entity-component system (ECS). GPU passes similarly declare which buffers and textures they access. A command list moves from recording to closed to submitted. Submission creates an outstanding obligation until the relevant GPU fence completes.

## 39.2 Match shader bindings to host data

The proposed build runs the DirectX Shader Compiler (DXC) in isolation, then inspects the reflection metadata it produces for a High-Level Shading Language (HLSL) program. Reflection describes what the compiled shader expects. Compare field offsets and resource types with host declarations. A typed bindless index associates a descriptor slot with its intended resource kind.

## 39.3 Keep work-graph backing memory live

A Direct3D 12 (D3D12) work graph can schedule nodes on the GPU. Its backing allocation may remain in use after the CPU submits the graph. A lease tracks that storage through submission and fence completion. Reusing it for the next graph too early would overlap two owners.

## 39.4 Insert a barrier at the dependency

A pass writing a texture followed by one sampling it needs an ordering and visibility transition appropriate to the target. Two compatible reads may need no transition. A split barrier can begin synchronization near the producer and finish near the consumer while independent work proceeds.

## 39.5 Keep a streamed asset resident

Asset streaming can read and decompress content asynchronously before the GPU consumes it. DirectStorage submission, I/O completion, residency, and final GPU use are separate events. Releasing a resource requires evidence for its last relevant event.

## 39.6 Walk through one D3D12 frame

Update disjoint kinematics columns, record work that consumes them, close the command list, and submit it. The CPU may prepare another frame while the first frame is in flight. Keep per-frame temporary memory attached to the fence that actually covers its final use.

## 39.7 Compile dependencies into barriers

For each resource, track its last access and the access required by the next pass. A render-graph compiler uses these facts to generate enhanced barriers or fence coordination. Test a write followed by a read on another queue as well as repeated reads on the same queue.

## 39.8 Compile a shared shader interface for several targets

A Slang pipeline can express a shader once and emit target-specific modules. Compare reflected bindings across outputs and preserve data layouts at the host boundary. A common source file can still require different synchronization or feature selection on each device.

## 39.9 Adapt submission to Vulkan and Metal

Vulkan and Metal expose different resource states, command structures, and synchronization primitives. The proposed native bindings translate a common frame description into each backend’s API. Features such as work graphs and cooperative matrices require capability checks and defined fallback behavior.

## 39.10 Define equivalence by what a frame observes

A portable frame loop acquires resources, runs passes, presents, and retires work in a defined order. Compare the observable dependencies and results across backends. Equal numbers of barriers are unnecessary if the backends enforce the same required ordering.

## 39.11 Emit and inspect a target plan

The backend emitter turns abstract hazards into concrete barrier operations. Look up a producer and consumer in the emitted plan and check which operation connects them. Missing transitions risk stale data, while unnecessary transitions can serialize work that was independent.

# 40 Kinetic deterministic physics and rollback

When a late input arrives, a multiplayer simulation may need to restore an earlier tick and replay it. Reproducing the result requires the saved state, the intervening inputs, and rules for arithmetic and update order.

## 40.1 Decide what must repeat

Extended Position-Based Dynamics (XPBD) resolves physical constraints by adjusting predicted positions. Different orders of floating-point operations can produce different answers. A cross-machine replay contract must define rounding, fused operations, NaNs, reductions, and any math functions used by the solver.

## 40.2 Save state needed for the next tick

A wire snapshot records the state required for deterministic restoration. AoSoA tiles store the same body attributes together for vector operations. A rollback ticket accounts for the lifetime of a retained snapshot. Asynchronous GPU work must finish before its backing memory is reused.

## 40.3 Solve disjoint constraints together

Treat bodies as vertices and constraints between them as edges. Give conflicting constraints different colors. Constraints of one color can update disjoint bodies in parallel, then the solver advances to the next color. For a chain A–B–C, the constraints A–B and B–C both update B and need different colors.

## 40.4 Check disjointness and spatial ordering

A graph-coloring property can show that two constraints in one batch share no writable body. A Morton code interleaves coordinate bits to group nearby cells in a spatial index. Validating the physical model requires comparing simulated motion with the behavior of the system being modeled.

## 40.5 Inspect the numerical pipeline

A deterministic-float verifier checks the operations allowed by the numerical profile. A Morton encoder supports the broad-phase search for candidate contacts, and the colorer schedules the constraints. For rollback testing, compare saved and replayed states tick by tick to locate the first divergence.

# 41 Resonance audio and control loops

An audio callback receives a fixed interval in which to produce its next buffer. Work that waits for a lock or allocates unpredictably can miss that deadline even when its average runtime is small.

## 41.1 Keep unbounded work outside the callback

A realtime effect policy restricts blocking, allocation, and other unpredictable operations. Prepare new graph state on a control thread and transfer it through a bounded channel. Very small floating-point values can have target-dependent costs, so the design also specifies a subnormal-handling scope.

## 41.2 Lend a buffer for one processing interval

A linear buffer lease accounts for access to a block of samples. A timestamped event identifies its exact position within the block. A single-producer, single-consumer (SPSC) ring carries parameter updates or retired resources between threads without requiring the callback to perform reclamation.

## 41.3 Split a block at an event

Suppose a filter change falls at sample 17 of a 64-sample block. Process the first 17 samples with the old setting, apply the event, and process the remainder with the new setting. The same timing discipline applies to a periodic controller consuming timestamped measurements. Deadlines need measurements on the actual target.

## 41.4 Align parallel signal paths

If one branch of an audio graph delays its output by several samples, summing it directly with a faster branch mixes different instants. Plugin delay compensation adds the missing delay to the faster path. A ring-safety proof concerns publication and reclamation, while a filter stability argument needs its own signal and coefficient assumptions.

    Branch A    32 samples of processing delay + 32 samples of compensation
    Branch B    64 samples of processing delay +  0 samples of compensation

An impulse sent into both branches now reaches the mixer at sample 64 on each path. Recompute those delays when an edit changes the graph.

## 41.5 Compile a bounded processing schedule

A graph compiler orders dependencies, computes compensation delays, and can fuse compatible buffer operations. An effect verifier checks callbacks for operations outside the realtime policy. A cycle needs an explicit delay or feedback rule before a block schedule is meaningful.

# 42 Tensor shapes, fusion, and the KV cache

During inference, Tensor checks whether the dimensions of a matrix multiply agree. It also tracks leases on pages of cached attention keys and values (the KV cache), so a cancelled request can release its pages.

## 42.1 Unify the contracted dimensions

Multiplying a 2-by-3 matrix by a 3-by-4 matrix gives a 2-by-4 result. The two inner dimensions must agree. Named axes distinguish quantities such as batch, heads, and sequence length even when their sizes happen to match. Unknown dimensions may require a runtime check against the contract.

## 42.2 Interpret a quantized weight block

Quantized formats store compact values together with scales or other decoding information. A mapped tensor view must respect the file’s dimensions, block layout, alignment, and lifetime. A `KvPageLease` instead represents ownership of a page of cached attention keys and values for a live sequence.

## 42.3 Trace one token through attention

A decode step forms queries, keys, and values, appends the new keys and values to the sequence cache, and attends to the retained history. Grouped-query attention lets several query heads share a key/value head. Temporary projections can live in a step arena. Persistent KV pages must survive the step and be returned on completion or cancellation.

In the manuscript example 32 query heads share eight key/value heads in groups of four. Query heads 0 through 3 use key/value head 0, and heads 4 through 7 use head 1. The final group uses head 7, which fits the cache’s eight-head dimension. The next token can reuse those cached keys and values. At the end of the request, the cache must return the pages to the pool.

## 42.4 Check the shape and the page pool independently

A contraction theorem relates input axes to the output shape. A page-conservation property accounts for every page as free or held by a permitted lease. Streaming softmax updates partial statistics across blocks. Its numerical behavior depends on the chosen arithmetic. For a cancelled request, check that its KV pages return to the pool even if every tensor shape was valid.

## 42.5 Fuse operators while preserving the contract

A shape unifier collects dimension equalities and rejects incompatible contractions. An epilogue fuser can combine operations such as a projection and an activation to avoid writing an intermediate tensor. Any rewrite must preserve output shape, layout, and the numerical guarantees promised for that operator.
