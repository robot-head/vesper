# Linear UMEM frames and AF_XDP four-ring model

Wave-four lane 3 of design §35 (Vesper Warp): §35.1 item 1, §35.2
`XdpNic`/`UmemFrame`, §35.4 theorem 1, §35.5 `XdpUmemAccounting`. Issue #39.
Branch `claude/project-thread-05gni5` (the prompt suggested
`wave4/warp-umem-rings`; this session may push only to the former). Base
`a5fa024b8d6f62eaa336337d825a2be2ec8972b4` (origin/main at start, exactly the
required baseline).

## Files

| Path | Content |
| --- | --- |
| `crates/vesper-warp-umem/src/wire.rs` | `XdpDescWire`, the 16-byte `struct xdp_desc`, declared through `vesper-ffi`'s wire registry; encode/decode through its checked views. |
| `src/config.rs` | `UmemConfig`: frame count, power-of-two frame size (2 KiB–32 KiB, default 4 KiB), headroom, four ring sizes; checked arithmetic. |
| `src/frame.rs` | `UmemFrame<RxHarvested / TxStaged / Submitted>`, `InFlightQuicPkt`, and the leak `Ledger`. |
| `src/ring.rs` | `Ring<T>` SPSC model plus `ProducerRing`/`ConsumerRing` traits. |
| `src/driver.rs` | `Xsk<B>` driver, `XdpNic` trait, `XskBackend` trait. |
| `src/soft.rs` | `SoftNic`: deterministic software NIC (loopback and scripted), fault injection. |
| `src/accounting.rs` | `XdpUmemAccounting::check_conservation`, `UmemAuditor`, `audit_driver`. |
| `src/trace.rs` | `Location`/`TransitionKind`, shared by name with the Lean model. |
| `src/af_xdp.rs` | Feature `af-xdp`, Linux only: real AF_XDP backend and XDP redirect loader. The crate's only unsafe code. |
| `tests/{rings,wire,soft_nic,zero_alloc,lean_model,af_xdp}.rs`, `tests/common/mod.rs` | Acceptance tests. |
| `lean/WarpUmem.lean`, `lean/WarpUmem/Fixture.lean` | Count model, theorems, executable fixture. |
| Root `Cargo.toml`/`Cargo.lock` | Registration of `crates/vesper-warp-umem` in `members` and `default-members` (own commit). No new workspace dependencies. |

Nothing else was edited.

## Public API

* `XdpDesc { addr, len, options }`, `XdpDescWire::{new, layout, encode, decode}`.
* `UmemConfig::{with_frames, validate, umem_len, chunk_base}`.
* Owners: `UmemFrame<S>` (`chunk_idx`, `data_offset`, `pkt_len`) and
  `InFlightQuicPkt` (`pkt_num_full`, `edt_departure_ns`, `held_frame`,
  `chunk_idx`). Non-Clone, non-Copy, `#[must_use]`; no public constructor.
* `trait XdpNic` (the §35.2 effect minus `park_for_ack_or_pto`, which is lane
  4): `harvest_rx`, `view_frame_bytes`, `recycle_to_fill_ring`,
  `turn_around_tx_in_place`, `submit_paced_tx`, `harvest_completions`,
  `retire`.
* `Xsk<B: XskBackend>` adds `replenish_fill`, `alloc_tx`, `abandon_tx`,
  `rx_bytes_mut`, `tx_bytes_mut`, `turn_around_tx_at` (new data offset),
  `census`, `stats`, `location`, `free_list`, `leaked_owners`.
* `Ring<T>` with `reserve/write/unreserve/submit` and
  `peek/read/unpeek/release`, `push_one`, `pop_one`.
* `SoftNic::{new, xsk, step, deliver, queue_ingress, inject, kernel_census,
  enable_trace, trace, take_trace}`, `Fault`, `SoftNicMode`.
* `XdpUmemAccounting::{new, get, sum, check_conservation, apply}`,
  `UmemAuditor::{audit, counts}`, `audit_driver`.
* `af_xdp::{probe, AfXdpBackend, BindMode, XdpRedirect, AfXdpError}`.

## Decisions

* **Linearity.** Consuming calls take the owner by value; every failure
  returns it in `Refused { owner, error }`. A dropped owner records its chunk
  in a shared ledger (an `Arc` clone per frame: an atomic increment, no
  allocation). The driver then quarantines the chunk as `Leaked` and the
  auditor reports it; it is never put back on the free list. Rust ownership
  here is host-library discipline. **It is not evidence that the Vesper
  source typing rules for `linear` are implemented.**
* **Lease semantics.** `submit_paced_tx` returns the in-flight packet holding
  the frame in a `Submitted` state (no byte access, cannot be resubmitted).
  The chunk is freed only when both the NIC completion has been harvested
  and the lease is retired (ack or loss), in either order. A lease dropped
  before its completion is flagged and quarantined when the completion comes.
  The lease carries `pkt_num_full` (checked `< 2^62`) and
  `edt_departure_ns`. The EDT is carried but not enforced here: AF_XDP has no
  per-descriptor launch time in this backend, and pacing is lane 2's and lane
  4's job.
* **Fill ring sizing.** `fill_size >= frame_count` is required, so returning a
  frame to the fill ring cannot fail for lack of space. The driver still has
  a free-list fallback (`RejectToFree`/`RecycleToFree`), which this makes
  unreachable; the tests check it never fires.
* **Turn-around hygiene.** Bytes of the new transmit window that the
  received packet did not cover are zeroed, and `alloc_tx` zeroes its window,
  so stale bytes from earlier packets are never transmitted.
* **Descriptor validation** (rx, from the device): outside the UMEM
  (`OutOfUmem`, no chunk named), chunk not kernel-owned (`NotKernelOwned`,
  nothing changed), option bits set (multi-buffer unsupported), zero length,
  data offset inside the headroom, data past the chunk end. In every case
  with an identifiable kernel-owned chunk, the chunk goes straight back to
  the fill ring (`recycled: true`), so no frame is lost. Completion entries
  that name no kernel-tx-side chunk are skipped and counted. On transmit, the
  driver validates the window before publishing a descriptor.
* **Software NIC divergence, documented in `soft.rs`.** Linux drops an invalid
  tx descriptor without completing it. Our driver never publishes one, and
  the software NIC completes such a descriptor anyway and counts it in
  `tx_invalid` (tests assert 0). An injected out-of-UMEM rx descriptor is
  spurious: it consumes no fill buffer, since the model cannot know which
  buffer a buggy kernel would have lost. The NIC takes a tx descriptor only
  when a completion slot is guaranteed, as the kernel does.
* **vesper-alloc is not used.** A UMEM must be one contiguous,
  page-aligned region registered with the kernel. `ShardSlab` hands out
  separate 64 KiB slabs with at most 16 KiB size classes, and `ChunkBacking`
  returns raw pointers that would need unsafe code outside the one allowed
  module. The software UMEM is a safe `Box<[u8]>`; the Linux UMEM is an
  anonymous `mmap` inside `af_xdp.rs`.
* **vesper-ffi in the hot path.** The rings carry decoded `XdpDesc` values.
  The wire codec is available and tested, and the Linux backend converts to
  and from `libc::xdp_desc`, whose layout a test checks against the
  `XdpDescWire` declaration.

## The auditor

`XdpUmemAccounting` holds a count per location: free list, fill ring, NIC rx
pool, rx ring, app-held rx, app-held tx, tx ring, NIC tx, completion ring,
in flight (completed, lease live), and leaked. `check_conservation` requires
that they sum to `frame_count` and that nothing leaked.

`UmemAuditor::counts` builds the counts from the real structures: the
software NIC's committed ring entries and internal pools, the driver's free
list and location table. It then checks:
* the driver's collapsed "kernel receive side" and "kernel transmit side"
  counts equal the device's fine-grained counts;
* every chunk appears exactly once across all structures, and the device
  side never holds a chunk the driver places elsewhere;
* the total.

`UmemAuditor::audit` adds the leak checks, including dropped leases whose
transmit has not completed yet. Its scratch buffer is reused, so a warmed-up
audit allocates nothing. `audit_driver` is the coarse check for any backend,
used with the real kernel socket.

## Lean (`lean/WarpUmem.lean`)

Compiles with the pinned `leanprover/lean4:v4.24.0`. It has no
`sorry`/`admit`/`axiom`, and its eleven `#print axioms` lines list only
`propext`, `Classical.choice` and `Quot.sound`; the Rust test checks the
report. Proved **about the count model** (eleven `Nat` buckets, 21
transitions each moving one frame, failing on an empty source):

* `apply_preserves_total`, `run_preserves_total`: every transition and every
  finite run preserves the total.
* `umem_step_conserves_total_frames` (§35.4): the modelled receive step
  `quicStep` (malformed descriptor, header-split failure, CID miss or
  reply) preserves `fill + rx + tx`, leaves app-held counts unchanged and
  preserves the total.
* `quicStep_failure_replenishes_fill`: every malformed-packet path returns
  exactly the frame the rx ring lost to the fill ring.
* `quicStep_reply_submits`, `quicStep_progress` (never stuck when rx is
  nonempty), `quicStep_eq` (closed form), `quicStep_none_of_empty`,
  `apply_none_iff`, `total_set`, `get_set_same`.

**These are not theorems about the Rust code.** The connection is a
differential test (`tests/lean_model.rs`). Randomized software-NIC runs
(faults, backpressure, early and late retires, and deliberate drops of rx,
tx and both kinds of lease) record every driver and NIC transition. They
also snapshot the auditor's counts, which come from the real rings, after
every NIC step and every application operation. The log is replayed through
Lean's `apply` (via `Fixture.lean`) and through the Rust count model, and
all three agree. On this host that was 8,555 lines with 5,322 auditor
checkpoints, plus hand-written rejection and `quicStep` cases. A
vocabulary test checks that the Lean `Trans`/`Loc` names, source and
destination of every transition and the render order match the Rust enums.
The Lean model leaves out ring capacity, descriptor contents and timing.

## Linux AF_XDP backend (`af-xdp`)

The backend does socket(AF_XDP), anonymous UMEM `mmap`, `XDP_UMEM_REG`, the
four ring-size options, `XDP_MMAP_OFFSETS` (requires the post-5.4 layout
with ring flags), four ring `mmap`s, and bind with `XDP_ZEROCOPY`, falling
back to `XDP_COPY` and recording why. Rings use acquire loads of the peer
index and release stores of our own. The unsafe invariants are written at
the top of `af_xdp.rs`. `XdpRedirect` loads a 19-instruction XDP program
through `bpf(2)`: an XSKMAP redirect for one IPv4/UDP destination port,
everything else `XDP_PASS`. It attaches through a BPF link in generic (skb)
mode and detaches on drop.

The crate's dev-dependency on itself with `af-xdp` means every Linux
`cargo test`/`clippy --all-targets` builds the backend; CI needs no flags.
The tests probe first and print `unavailable: <step>: errno ...` when the
host refuses. GitHub's unprivileged runners are expected to refuse.

**Executed on this cloud host** (Linux 6.18, root), `tests/af_xdp.rs` on
`lo`, queue 0:
* zero-copy refused (`bind(XDP_ZEROCOPY)`: errno 95, EOPNOTSUPP), then bound
  in **copy mode**;
* a kernel UDP socket's datagram went through generic XDP and the XSKMAP into
  the rx ring and was harvested;
* it was turned around in place in the same chunk and transmitted, completed,
  redirected back by the same program and harvested again;
* a fresh `alloc_tx` frame went through the same loop; leases were retired
  and `audit_driver` passed.

The kernel drops 127.0.0.0/8 packets that arrive on `lo` without an attached
route (martians), so our transmissions are observed through the XDP redirect,
not through a kernel UDP socket. **No claim** is made about NIC zero-copy,
DMA isolation, NIC drivers other than `lo`'s generic path, or line rate.
DPDK is not implemented.

## Acceptance

| Requirement | Evidence |
| --- | --- |
| Long randomized runs keep exact conservation | `soft_nic.rs`: 12 scripted and 12 loopback seeds of 400 steps, plus a 3,000-step coverage run, with random geometry, ring sizes, batch sizes, malformed packets, all fault kinds, tx/completion stalls and early/late retires. The auditor runs after every NIC step and every app operation. The coverage run asserts every rejection kind, backpressure, early and late retire, and rx drops all occurred. |
| Dropping an owner is detected | `dropped_owners_are_detected_and_quarantined`: rx frame, tx frame, lease before completion; `Leaked { first_chunk }`, chunk never recycled. The Lean differential runs also include drops. |
| Descriptor validation without losing the frame | `malformed_rx_descriptors_are_rejected_without_losing_frames`, `zero_headroom_overlap_fault_crosses_the_chunk_end`, `turn_around_rejects_bad_windows_and_returns_the_frame`, `spurious_completions_are_skipped_and_counted`. |
| 0 Alloc steady state | `zero_alloc.rs`: counting global allocator (with a positive control). After 200 warm-up iterations, 5,000 iterations of step → harvest → turn-around → submit → retire → completions → replenish **plus the full auditor** performed 0 allocations. This covers the driver and the software NIC only, not a kernel backend. |
| Real AF_XDP or explicit unavailable | Executed in copy mode on this host (above); explicit `unavailable:` messages otherwise. |

## Commands run (this host)

```sh
cargo fmt --all -- --check                                              # ok
cargo clippy --workspace --locked --all-targets -- -D warnings          # ok
VESPER_LEAN=lean cargo test --workspace --locked                        # ok
VESPER_LEAN=lean cargo test -p vesper-warp-umem                         # 25 tests, ok
lean lean/WarpUmem.lean                                                 # ok, standard axioms only
```

## Remaining gaps

* The Vesper source `linear` typing of `UmemFrame`/`InFlightQuicPkt` and the
  `XdpNic` effect are not implemented here (lane 8 and the checker).
* `park_for_ack_or_pto` and wheel parking are lane 4; EDT enforcement and
  NIC launch-time offload are not modelled.
* Linux backend: only `lo` copy mode was executed. Zero-copy, real NICs,
  `XDP_USE_NEED_WAKEUP`, shared UMEM, multi-buffer (`XDP_PKT_CONTD`),
  unaligned-chunk mode and multi-queue redirect (`XdpRedirect` is queue 0
  only) are not implemented or not exercised. Pre-5.4 kernels are rejected.
* The ring model is single-threaded. Real cross-core ordering is covered
  only by the Linux backend's atomics, which were exercised against the
  kernel on one host and not model-checked.
* The Lean model has no ring capacities or descriptor contents. Its link to
  the Rust code is the differential test, not a refinement proof.
