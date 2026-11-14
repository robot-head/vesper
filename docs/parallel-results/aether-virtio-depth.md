# Aether: virtio queue features, VMM-thread seccomp, huge-page snapshots and stored hot sets (§37) — wave eight lane 27

Claim issue #476, plan PR #477. Base `1094e649c95e4d81eef4e705d13882ab1339542d`
(main after #477). Edited only `crates/vesper-aether/**`,
`lean/AetherMemory.lean` and this report. No root patch: no new
dependencies, no new crate, `Cargo.lock` unchanged, and
`python3 tools/bazel/sync_build_files.py` leaves `BUILD.bazel` unchanged.

**KVM still cannot run here.** The sandbox has no `/dev/kvm`, so no guest
booted and no KVM ioctl was issued. Everything below was exercised against the
in-process device models, a model driver written from the VirtIO 1.2 text, and
the real Linux kernel's userfaultfd, memfd, hugetlbfs and seccomp.

## What was built

### 1. One virtqueue engine for split and packed rings (`devices/queue.rs`)

The MMIO transport now drives every queue through `VirtQueue`, which handles
both ring formats (split when `VIRTIO_F_RING_PACKED` is not negotiated, packed
when it is) and these ring features:

- **`VIRTIO_F_INDIRECT_DESC`** (§2.7.5.3, §2.8.19). An indirect descriptor
  must be the only descriptor of its chain (`IndirectPlacement`), its table
  length must be a non-zero multiple of 16 inside RAM (`IndirectTableLength`),
  tables may not nest (`IndirectNested`), and indirect without the feature is
  `IndirectNotNegotiated`. Split tables follow `next` with loop detection;
  packed tables are read in order.
- **`VIRTIO_F_EVENT_IDX`**. Split: the device honours `used_event` with
  `vring_need_event` and publishes `avail_event` after each pass. Packed: the
  driver event suppression structure (ENABLE / DISABLE / DESC with
  offset+wrap) is honoured and the device structure is written. Interrupts
  avoided are counted (`interrupts_suppressed`).
- **`VIRTIO_F_IN_ORDER`** (§2.7.9, §2.8.20). Completions are batched: one used
  entry carrying the id of the last buffer of the batch, written at the
  batch's first slot. A batch closes at the first buffer not written to its
  full length (whose length must be reported) or at the end of the pass.
- **`VIRTIO_F_NOTIFICATION_DATA`** (§4.2.3.3). The notify value carries the
  queue index and `next_idx` (split) or `next_off`/`next_wrap` (packed). The
  hint is checked against the ring; a mismatch is counted, and the ring stays
  authoritative. Malformed values are `MmioError::InvalidNotification`.
- Hardening: descriptor chains are captured once (no double fetch), a chain is
  a non-`Clone`, `#[must_use]` value that must be completed exactly once, the
  driver may not overrun the ring (`AvailOverrun`), an out-of-range `next` is
  `InvalidNext`, a reused head is `DescriptorInUse`, and every walk is charged
  against a fixed work budget.
- `QUEUE_READY` before `FEATURES_OK` is now refused
  (`QueueReadyBeforeFeaturesOk`).

### 2. Device features

- **virtio-blk** `DISCARD` and `WRITE_ZEROES` (§5.2.6): segments are fully
  validated before any is applied. Unknown flags and `UNMAP` on a discard are
  `VIRTIO_BLK_S_UNSUPP`; zero or oversized counts, misaligned discards, too
  many segments and ranges past capacity are `IOERR`. Offered only for
  writable disks, with the limits in config space (the config is now 60
  bytes). **`VIRTIO_BLK_F_MQ`** with `with_queues(n)` (up to 8) and per-queue
  statistics.
- **virtio-net** **`MQ`** with **`CTRL_VQ`**: `with_queue_pairs(n)` (up to 8),
  `VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET` on the control queue (out-of-range counts
  are `VIRTIO_NET_ERR`), frames for inactive pairs dropped and counted, and
  per-pair receive backlogs (`inject_rx_on`). **`MRG_RXBUF`**: a frame larger
  than one buffer is spread over several chains, with `num_buffers` in the
  first header. If too few chains are posted, the frame waits and nothing is
  consumed. A buffer smaller than the header, or a read-only receive buffer,
  is a protocol error.
- **virtio-vsock** **`SEQPACKET`** (§5.10.6.6): `listen_seqpacket`,
  `connect_seqpacket`, `send_message(key, data, eor)` and `recv_message`, with
  `VIRTIO_VSOCK_SEQ_EOM`/`EOR` message boundaries. A message split across RX
  packets carries EOM/EOR only on its last part. Type mismatches are reset,
  and seqpacket without the feature is `VsockError::WrongType`. Sends are all
  or nothing (`WouldBlock`).

### 3. The compiled seccomp profile on a VMM thread (`vmm_thread.rs`, `seccomp.rs`)

- `spawn_vmm_thread(name, program, work)` starts a named thread. Before any
  VMM work runs, the thread installs the compiled filter on itself with
  `PR_SET_NO_NEW_PRIVS` and `seccomp(SECCOMP_SET_MODE_FILTER)`, without TSYNC,
  so only that thread and its children are bound. The caller learns the
  install result before `spawn_vmm_thread` returns, and a refused install is
  an error with the work never run.
- `DefaultAction::KillThread` (`SECCOMP_RET_KILL_THREAD`) was added next to
  `KillProcess`. `VmmThread::wait` reports `Killed` when the kernel task is
  gone without a result, using `/proc/self/task/<tid>`.
- `SeccompProfile::vmm_ioctls(Arch::Aarch64)` lists the AArch64 KVM run-loop
  ioctls: `KVM_RUN`, `KVM_GET_ONE_REG`, `KVM_SET_ONE_REG`, `KVM_IRQ_LINE`,
  `KVM_SIGNAL_MSI` and `KVM_GET_DIRTY_LOG`, plus the userfaultfd ioctls. The
  encodings were checked against the host `linux/kvm.h` and are pinned by a
  test. x86-64 keeps `KVM_RUN`, REGS/SREGS, `IRQ_LINE` and `GET_DIRTY_LOG`.

### 4. Hot sets stored next to the CAS snapshot (`hotset_store.rs`)

- `persist_hot_sets(store, slots, hot)` writes the snapshot *manifest* (a
  canonical encoding of every slot's id, guest range, permission and page
  identities) and one encoded hot set per slot. It binds them in an
  artifact-domain CAS tree (`manifest`, `hotset-<slot_id>`), the *bundle*.
  Persisting is content addressed, and a new recording is a new bundle;
  nothing is overwritten.
- `load_hot_sets(store, bundle, slots)` treats the bundle as untrusted. It
  must be an artifact tree. Its manifest must be byte-for-byte the manifest of
  the snapshot being restored, otherwise `ForeignSnapshot`. Entry names must
  be canonical `hotset-<n>` (`UnexpectedEntry`), each slot must exist, sizes
  are bounded before any read (`Oversized`), and every hot set is decoded and
  validated against its slot's page count.

### 5. 2 MiB huge-page snapshot memfds (`linux_host.rs`, `mapped_fork.rs`)

- `SealedMemfd::build_huge` creates `memfd_create(MFD_HUGETLB |
  MFD_HUGE_2MB | MFD_ALLOW_SEALING)`. hugetlbfs has no `write(2)`, so the
  memfd is filled through a temporary shared mapping, then sealed with the
  same seals as small-page snapshots.
- `build_preferring_huge` and `SealedSnapshot::seal_preferring_huge` use huge
  pages when the image is a whole number of 2 MiB pages and the pool has free
  pages, and fall back to small pages otherwise. `free_huge_pages()` reads
  `HugePages_Free`, and failures are `Unavailable::HugePages`.
- `MinorRegion::map_private` on a huge memfd registers with
  `UFFD_FEATURE_MINOR_HUGETLBFS` and resolves faults with `UFFDIO_CONTINUE` at
  2 MiB granules (`fault_granule()`). Hot-set prefetch widens to whole huge
  pages.

### 6. Lean (`lean/AetherMemory.lean`)

Four new theorems, about models of the Rust code rather than the Rust code
itself:

- `needEvent_iff_window` and `needEvent_empty_window`: the 16-bit
  `vring_need_event` test is exactly "the driver's event index lies in the
  window of entries just published", and an empty window never interrupts.
- `closeBatches_cover` and `closeBatches_skipped_full`: in-order batching puts
  every completion in exactly one batch, and every buffer the driver infers
  without its own used entry was fully written.

## Checks

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo +1.100.0 clippy -p vesper-aether --locked --all-targets -- -D warnings` | clean |
| `cargo clippy -p vesper-aether --locked --all-targets --target {aarch64-apple-darwin, x86_64-pc-windows-msvc, aarch64-unknown-linux-gnu} --features blake3/pure -- -D warnings` | clean on all three |
| `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings` | see PR |
| `VESPER_LEAN=<lean 4.24.0> cargo test -p vesper-aether --locked` | all pass |
| `cargo test --workspace --locked` | see PR |
| `lean lean/AetherMemory.lean` (4.24.0) | no errors; every theorem depends only on `propext`, `Classical.choice`, `Quot.sound` |
| `python3 tools/bazel/sync_build_files.py` | no changes |

New tests:

- `tests/virtqueue.rs` (12) runs spec-derived descriptor chains through the
  engine on both ring formats. It covers indirect tables and every way they
  can be malformed, event-index suppression on both formats, IN_ORDER
  batches, NOTIFICATION_DATA hints, loops and overruns.
- `tests/device_features.rs` (9) covers blk DISCARD/WRITE_ZEROES/MQ, net
  MQ/CTRL_VQ/MRG_RXBUF and vsock SEQPACKET.
- `tests/seccomp.rs` gains:
  - `denied_syscall_kills_the_vmm_thread_and_not_the_process`, run in a
    re-executed child. The VMM thread makes a denied call and dies there, and
    the process and its other threads keep running. It passed as root and as
    uid 65534.
  - `kill_process_default_takes_the_whole_vmm_down_with_sigsys`.
  - The AArch64 profile and ioctl encoding tests.
- `tests/huge_memfd.rs` (4): sealing, two-fault 2 MiB clones that share
  frames (confirmed through `/proc/self/pagemap`), copy-on-write of a whole
  huge page, and huge prefetch. They ran for real here with
  `vm.nr_hugepages=32` and `VESPER_REQUIRE_HUGEPAGES=1`. Without free huge
  pages they pass with a printed reason.
- `tests/hotset_store.rs` (3) covers the round trip, foreign snapshots and
  every malformed bundle. `tests/shared_memfd.rs` gains the small-page
  fallback test.

## Decisions

- **Zero-length buffers are refused** (`QueueError::EmptyBuffer`). The
  spec allows them, but no device here needs them and they make chain-length
  accounting ambiguous.
- **An indirect descriptor must be alone in its chain**, on both formats.
  This is stricter than split rings in the spec, which only forbid `NEXT` on
  an indirect descriptor (so direct descriptors could precede it). Linux
  never builds such chains, and refusing them keeps one capture path per
  format.
- **IN_ORDER batches only skip fully written buffers.** A partly written
  buffer always closes its batch so that its length is reported.
- **Packed `RING_EVENT_FLAGS_DESC` without `EVENT_IDX` is treated as ENABLE**
  and counted in `invalid_event_flags`, rather than failing the queue.
- **Notification hint mismatches are counted only for split rings**, once per
  notify where the hint differs from the ring. The ring is always
  authoritative.
- **The snapshot manifest excludes the host virtual address and VM id**, so the
  same snapshot restored into another VM or address loads the same hot sets.
  It includes everything that changes which bytes a page holds.
- **Huge private mappings reserve their pages** (no `MAP_NORESERVE`). A clone
  that copies on write cannot later die with `SIGBUS` when the pool is empty;
  instead the mapping fails up front and the caller falls back.
- Only `VERSION_1` is required from the driver. `RING_PACKED` and the ring
  features are offered and used when negotiated.

## Remaining gaps

- **KVM never ran**: no `/dev/kvm`. The AArch64 ioctl list is checked by
  encoding, not by running a guest under it. The seccomp profile was proven
  on a VMM thread doing non-KVM work.
- No wiring to a running guest kernel, so the virtio features were validated
  against a model driver written from the spec, not against Linux's virtio
  drivers.
- Not implemented: automatic net flow steering (`RSS`,
  `VIRTIO_NET_CTRL_MQ_RSS_CONFIG`), `VIRTIO_F_RING_RESET`, io_uring-backed
  block I/O, and vsock `SEQPACKET` credit edge cases beyond those modelled.
- Huge-page snapshots need a configured hugetlb pool. CI runners without one
  take the printed-skip path.
- The Lean theorems are about models of the batching and event-index logic,
  not the Rust code. Rust ownership (`Chain` being non-`Clone`) is not
  evidence of the Vesper typing rules.
