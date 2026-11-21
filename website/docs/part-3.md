# 13 Strata regions and memory layout

A parser may allocate thousands of nodes that all expire at the end of one request. A region gives them a common lifetime. Their layout determines how much unrelated data the processor has to load when it scans them.

## 13.1 Decide who can allocate

An allocation capability makes allocation visible in a function’s effects. A region groups objects that can be reclaimed together. A general heap allocation has a different lifetime and cost. Consider both the longest-lived reference and the fields the program repeatedly visits when selecting a layout.

## 13.2 Keep a region reference within its scope

A region introduces a fresh lifetime and an allocation capability. Objects within it can refer to each other, even cyclically, if no reference survives reclamation. To rewind to a checkpoint, first establish that every reference into the reclaimed portion has expired. Returning an owned scalar summary can be simpler than returning the temporary parse tree.

## 13.3 Match the allocator to the request

A sharded slab serves repeated small allocations from size classes, keeping common operations local. A virtual arena reserves address space and advances a cursor as callers request storage. Reclaiming a whole arena is cheap when lifetimes align. Consider stranded capacity and peak resident memory as well as the cost of one allocation.

## 13.4 Reuse a consumed constructor

Functional-but-in-place (`@fip`) rewriting consumes an old data constructor and reuses suitable storage for its replacement. For a tree transformation, the visible result can be a new tree even when individual nodes occupy their old addresses. Reuse depends on ownership and layout compatibility.

## 13.5 Store frequently scanned fields together

An array of structures keeps each record together. A structure of arrays puts each field in a separate run. An array of structures of arrays (AoSoA) groups records into tiles and stores each field contiguously within a tile. Updating positions for eight objects can then load one tile’s positions without loading all eight objects’ metadata.

A hot/cold split takes infrequently accessed fields out of the scan path. A chunked sequence grows by adding chunks. A transient collection lets a unique owner perform a series of changes before publishing a stable value. Choose based on how often the workload scans, grows, or shares the collection.

## 13.6 Follow a region operation into the runtime

The allocator supplies the storage behind region operations. Check where a cursor advances, which alignment a request needs, and when pages can be reused. A fast cursor update does not establish escape safety. The allocator checks that the request fits. The language check determines whether any surviving value could still refer to the reclaimed storage.

# 14 Vector instructions and portable SIMD

Single instruction, multiple data (SIMD) applies one operation to several lanes. A portable vector loop must choose an instruction set available on the machine and handle a final group shorter than a full vector.

## 14.1 Keep inactive lanes out of the computation

A lane mask identifies the elements that participate in an operation. For a ten-element input and a width of four, only two lanes are active in the third group. Masked memory operations must also honor the mask when accessing memory. Ignoring the unused results would come too late for an out-of-bounds load.

## 14.2 Require an instruction-set profile

An `Isa[P]` capability records which instruction-set architecture profile a block may use. Dispatch selects a suitable implementation after establishing hardware support. Keep a fallback for a machine with a narrower profile. For example, select a narrower implementation when the CPU lacks an instruction required by the wider one.

## 14.3 Choose fixed or scalable vectors

`Vec[T, N]` has a fixed lane count. A scalable `VVec` loop processes a runtime-sized strip each time, with a predicate for the remaining elements. Structure the computation around active elements so the answer stays the same when a target chooses a different vector length.

## 14.4 Validate a vector rewrite

An optimizer can explore several equivalent-looking implementations and choose a cheaper one. Equality saturation retains candidate expressions together. A bit-vector proof can check a proposed rewrite at a specified width. Overflow, signedness, masks, and floating-point semantics determine which equalities actually hold.

## 14.5 Lower an operation for a target

The lowering engine selects instructions or a fallback according to the active profile. Use the same scalar input as a reference, including an empty input and a partial tail. Agreement on those examples provides a concrete check against accidentally processing an inactive lane.

# 15 Pulse fibers, completion, and cancellation

While a fiber waits for I/O, another fiber can run on the same thread. The runtime must keep the waiting fiber’s stack and any buffer still in use by the device alive until it can safely reclaim them.

## 15.1 Separate submission from completion

Submitting I/O creates outstanding work. A later completion tells the runtime what happened. Pulse’s design schedules lightweight fibers around those boundaries. A completion token must retain enough identity to wake the right fiber and return the right storage to its owner.

    Submit read       → device may write the buffer
    Request cancel    → original read may still be in flight
    Terminal read CQE → runtime may retire the read and recover its buffer

CQE means completion queue entry. If the event you just received belongs to the cancellation request, look up the original read before returning its storage to the pool.

## 15.2 Identify cancellation points

Network, storage, clocks, scopes, and cancellation appear as lexical capabilities. A suspension can promise to resume a continuation. An explicit cancellation point can abandon it. Before adding a cancellation point, identify who will consume each linear resource if the task stops there.

## 15.3 Keep device buffers live through the terminal completion

Pipelining lets a fiber submit several requests before awaiting their results. With direct memory access (DMA), the device can still touch a submitted buffer. The cancellation request can finish before the original I/O request. Keep the buffer live until the original request’s terminal completion. A multishot operation can report several completions before its final one.

## 15.4 Join children and share state deliberately

A nursery bounds the lifetime of child fibers. A join handle represents the obligation to collect a child. Siblings can receive disjoint writable projections or communicate through a linear channel. Portable work can move between workers, while a pinned shard stays attached to its thread.

An `Atomic[T]` has one owner and can be shared through reads of the cell. Atomic loads, stores, swaps, and compare-exchange specify a memory order. The order determines what other memory accesses become visible between workers. `parallel_for` joins scoped workers before the data loan ends. The worker body and available execution backend constrain which programs can use this operation.

## 15.5 Suspend with a stack and handler evidence

A fiber’s saved machine state records where execution can resume. An evidence vector locates the current lexical handlers. A synchronously completed, tail-resumed operation may avoid a stack switch. If the request is still pending, the runtime must keep its continuation and stack live until completion.

## 15.6 Reproduce a schedule in SimPulse

SimPulse substitutes simulated clocks, completions, and scheduling decisions for live ones. A seed can reproduce the same sequence for a failing scenario. Live telemetry instead observes fibers in a running system. Use the event history to distinguish a fiber waiting for a completion from one waiting to run.

## 15.7 Trace the runtime bookkeeping

Follow a request through registration, completion delivery, wakeup, and retirement. The runtime must associate each event with the correct generation of the task. Reusing a stack or slot too early would give a delayed completion access to somebody else’s work.

# 16 Bridge wire layouts and foreign calls

Before Vesper can read a foreign value, it needs to know how the bytes are laid out, which bit patterns are valid, and how long the backing storage will live. Bridge describes that contract for device data and calls into other languages.

## 16.1 Ask what the foreign boundary guarantees

Check size, alignment, byte order, valid bit patterns, and ownership separately. A foreign integer that encodes a Boolean may contain a value outside the accepted representation. An opaque handle may need exactly one close even if its machine representation fits in one word.

## 16.2 Read a wire record without copying it

A `wire struct` lays out fields at explicit byte offsets and uses endian-aware scalars. Explicit padding makes every byte accountable. A view must cover enough bytes for the entire record. A packed field may be unaligned, so reading it must use an access appropriate to that layout.

## 16.3 Describe an import and validate its result

A package can declare `.vsys` imports in its manifest. A signature can describe a noncaptured string or slice argument, an opaque handle, or a foreign result requiring validation through `ForeignVal[T]`. Lending a slice saves a copy only for the agreed duration. A callee that retains the pointer needs a different ownership contract.

## 16.4 Choose an isolation boundary

A direct call shares the process and relies on the foreign library. Hardware protection keys or permission overlays can limit accidental cross-domain access. A threat model involving hostile native code requires stronger confinement. Decide which party is trusted before choosing the boundary.

## 16.5 Validate the layout before creating a view

A layout verifier computes each field offset and inspects gaps and valid bit patterns. To interpret arbitrary bytes as a value without checking their contents, every bit pattern must be valid for that type. To expose a value as bytes, check that every byte you expose is initialized. Checking a format definition once still leaves a length check for each input buffer.

## 16.6 Export a function to C or Rust

`vesper build --lib` produces an object for supported public functions. The export rules define an application binary interface (ABI). It specifies symbol names, word layouts, and how a caller receives failures. A caller must use the agreed result storage and failure convention. Consult the current export rules for unsupported parameter modes, generic functions, and effects.

# 17 Nexus WebAssembly plugins

A plugin that filters messages needs access to a message and a budget for the work. Nexus describes how a host can lend a resource to a guest, meter execution, and recover the resource when the invocation ends.

## 17.1 Separate suspension, ownership, and scheduling

The chapter proposes mapping one-shot suspensions to WebAssembly stack switching. Component resources represent owned or borrowed handles. A host also needs a scheduling budget so one tenant cannot occupy a worker indefinitely. Suspending a guest preserves its unfinished resource obligations.

## 17.2 Interpret a WIT resource handle

WebAssembly Interface Types (WIT) describes a component’s imports and exports. An owned handle carries responsibility for release. A borrowed handle permits limited use while the owner remains live. A shared buffer view also needs a range, a tenant identity, and a bounded lifetime.

## 17.3 Walk a message through a tenant filter

The host lends a message view, gives the plugin fuel, and collects its decision. On success, trap, or exhaustion, the host must end the loan and release invocation resources before recycling the buffer. Keeping the bytes in place saves copying only if the host and guest honor that interval.

## 17.4 State the isolation property precisely

A table lookup should resolve a handle only for its tenant, at the right generation, while the entry is live. Dropping an owner requires the outstanding loan count to reach zero. A proof about this table transition supports those checks. Other parts of a WebAssembly runtime need separate isolation arguments.

## 17.5 Reject a stale slot and charge for work

A generational table pairs a slot number with a generation. Recycling slot 4 increments its generation, so an older handle for slot 4 fails lookup. A separate fuel counter limits how much work the guest can do, including while its handles are valid.
