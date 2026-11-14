# vesper-chrono

Vesper Chrono (design §31): effect-boundary flight recording, time-travel
queries and linear-obligation provenance, as a host library.

* `wire`: the §31.2 `ChronoEventWireHeader` (32 bytes) and companion records
  as `vesper-ffi` wire layouts.
* `ring::FlightRecorder`: bounded, always-on record ring with overwrite-oldest
  semantics and snapshot epochs over caller-provided state images. No heap
  allocation after construction.
* `timeline::ChronoTimelineIndex`: ordered `inout` write index with O(log N)
  reverse watchpoints; unordered input is rejected or sorted with a recorded
  reason.
* `lineage::ObligationLineage`: monotonic obligation ids and full provenance
  chains (mint, sink moves, projections, handoffs, discharge, leak).
* `redact` and `bundle`: secret and `@pii` scrubbing before `.vchrono`
  serialization, and a keyed-BLAKE3-authenticated bundle format.
* `replay::ReplaySession`: the linear `ChronoReplay` session: reverse watch,
  lineage queries, step-back by replay from the nearest snapshot.
* `simpulse`: recording a SimPulse session and rebuilding its journal.

* `ipt` and `ipt_flow`: Intel PT packet decoder (every packet kind the
  instruction trace uses, all IP-compression modes) and a flow reconstructor
  that walks a code image with `iced-x86` to rebuild the branch trace.
* `ete`: Arm CoreSight ETE / ETMv4 instruction-trace element decoder
  (A-Sync, trace info, timestamps, context, addresses, atoms, exceptions).
* `formatter`: CoreSight trace formatter removal (16-byte TPIU / ETF frames,
  per-source-ID streams, full-frame sync).
* `ete_flow`: AArch64 flow reconstruction from ETE atoms over a code image
  (an A64 branch classifier walks to each P0 element).
* `hwpath`: maps reconstructed branches onto effect records by TSC interval
  so a reverse watchpoint names the instruction-level path to a write.
* `capture`: gated `perf_event_open` capture (Linux, `intel_pt` / `cs_etm` /
  `arm_spe` PMU); returns `Unavailable` with the reason when there is none.
* `checkpoint`: native fiber activation checkpoints stored as snapshot
  epochs. Fibers with a fixed stack address carry a byte image (stack bytes,
  register frame, context, data frame) restored at the same address without
  re-execution, rejecting a different entry argument or code; other fibers
  are restored by deterministic re-execution of the resume journal.
* `arena`: copy-on-write shard checkpoints of a sharded arena, incremental
  deltas as snapshot epochs, and a probe of the kernel write-tracking
  mechanisms (reported, not used: they need unsafe code).

The decoders are tested on independently encoded streams, not on traces
captured on hardware; capture has only run its unavailable path. Byte
images restore only in the capturing process and thread. Arena writes are
tracked at the arena API, not by the MMU (see `Unavailable`).
Rust ownership of `ReplaySession` is library discipline, not an
implementation of Vesper's linear typing rules. `lean/ChronoReplay.lean`
proves properties of a model, not of this code.
