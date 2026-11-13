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

Hardware branch tracing (Intel PT, CoreSight), native activation
checkpoints and the `vesper replay` CLI are unavailable (`Unavailable`).
Rust ownership of `ReplaySession` is library discipline, not an
implementation of Vesper's linear typing rules. `lean/ChronoReplay.lean`
proves properties of a model, not of this code.
