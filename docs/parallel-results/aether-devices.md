# Aether: shared-memfd minor faults, hot-set prefetch and microVM devices (§37) — wave seven lane 13

Claim issue #336. Base `283c8115d2203232d0e9b473cc4bb0312c3a8509` (main after
plan PR #354; descendant of the required `c025b9a`). Edited only
`crates/vesper-aether/**`, `lean/AetherMemory.lean` and this report. No root
patch: no new third-party dependencies, no new crate, `Cargo.lock` unchanged,
and `python3 tools/bazel/sync_build_files.py` leaves the existing
`BUILD.bazel` unchanged.

## What was built

### 1. Zero-copy `UFFDIO_CONTINUE` over a sealed shared memfd (`host`, `linux_host`)

- `SealedMemfd::build` / `from_source` write a frozen page set into a
  `memfd_create(MFD_ALLOW_SEALING)` file, `fallocate` every page (so a first
  touch is always a *minor* fault, never a silent hole fill), and seal it with
  `F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE`, then re-read
  `F_GET_SEALS`. Writes and writable shared mappings then fail with `EPERM`
  (tested).
- `MinorRegion::map_private(&Arc<SealedMemfd>)` maps the memfd `MAP_PRIVATE`,
  opens a userfaultfd with `UFFD_FEATURE_MINOR_SHMEM`, registers
  `UFFDIO_REGISTER_MODE_MINOR` and requires `UFFDIO_CONTINUE`. Its `serve`
  session resolves each minor fault with `UFFDIO_CONTINUE`, which maps the
  memfd's page-cache page in place: no copy, and every region over the same
  memfd shares one physical page. A write takes an ordinary copy-on-write
  fault and stays private to that region; the memfd and sibling regions are
  unchanged.
- The fault server is now shared by both region kinds (`Registered` plus a
  `Resolve` strategy: copy-from-source or continue-in-place), keeping the
  wave-six behaviour (fault budget, out-of-region and unexpected-event errors,
  panic safety, one session at a time). A MINOR region rejects MISSING fault
  messages and vice versa.
- Bug found and fixed while testing: the kernel's `UFFDIO_UNREGISTER` wakes
  only MISSING-mode waiters, so a failed MINOR session hung its toucher. The
  failure path now issues an explicit `UFFDIO_WAKE` over the range after
  unregistering (`first_write_to_an_unmapped_page_is_a_minor_fault_then_cow`
  covers it).
- `MinorRegion::frame_number` / `LazyRegion::frame_number` read
  `/proc/self/pagemap`, which is how the tests show the sharing (privileged
  readers only; unprivileged processes get zeros and the test says so).

### 2. Hot-set prefetch (`hotset`, `linux_host`)

- `HotSet` is a sorted, duplicate-free page set, built from a session's fault
  order (`serve_recording` on either region kind returns every fault's page
  index in arrival order) with `HotSet::from_fault_order`. `runs(batch)`
  coalesces it into contiguous runs of at most `PREFETCH_BATCH_PAGES` = 512
  pages (2 MiB). `encode`/`decode` is a validated on-disk form (magic, page
  count, strictly increasing in-range indices, no trailing bytes).
- `MinorRegion::prefetch` issues one `UFFDIO_CONTINUE` per run;
  `LazyRegion::prefetch(source, hot)` assembles each run into a buffer and
  issues one multi-page `UFFDIO_COPY`. Pages already present (`EEXIST`) are
  retried one by one and counted as raced.
- The design streams batches through io_uring; this host refuses
  `io_uring_setup` (`EPERM`), so batches are plain ioctls on the calling
  thread. That is a substitution, not an io_uring implementation.

### 3. Fork clones that remap the snapshot `MAP_PRIVATE`, with RDRAND/RNDR entropy (`mapped_fork`, `entropy`, `hw_rng`, `fork`)

- `SealedSnapshot::seal(&SnapshotImage)` writes the frozen image into one
  sealed memfd. `fork_mapped` issues a fresh epoch/generation (the same
  `ForkAuthority` as the host model) and gives the clone its own
  `MinorRegion` over that memfd.
- `MappedClone::serve` refuses every guest touch (`NotReseeded`) until
  `SnapshotResumeTicket::reseed_mapped` has run. That writes the clone's
  24-byte `AetherVmGenIdWire` block into its private mapping inside a serving
  session (a copy-on-write store; the sealed snapshot still holds the parent
  page), verifies it reads back, and mints the clone's `Sealer`. A ticket for
  another clone is handed back unconsumed.
- `CloneEntropy::gather()` takes four 64-bit words from the CPU generator
  (`RDRAND` with the recommended 10 retries on x86_64, `RNDR` on AArch64 with
  FEAT_RNG, both after runtime feature detection) and 32 bytes from
  `/dev/urandom`, and absorbs both into one BLAKE3 `derive_key` context, so
  the hardware instruction never weakens the result below the OS source. It
  fails only if neither source exists. `EntropySources` records what
  contributed (on this host: `RDRAND` and the OS).
- Both reseed paths (`reseed_after_fork` for the host model and
  `reseed_mapped`) now take a `CloneEntropy` by value, and clone keys are
  `derive_key("vesper.aether.fork-epoch/2", root ‖ gen_lo ‖ gen_hi ‖ epoch ‖ entropy)`.
  Boot sealers keep the `/1` context without entropy. Nonce uniqueness still
  comes only from distinct epochs (Lean `fork_clones_nonce_disjoint`); the
  entropy adds key separation if the epoch bookkeeping were ever wrong.
  `ReseededVaultEpoch` gained an `entropy: EntropySources` field.

### 4. VirtIO 1.2 MMIO transport and block/net/vsock devices (`devices`)

- `devices::mmio::MmioTransport<D: VirtioDevice>` implements the version-2
  MMIO register file (§4.2.2): magic, version, device/vendor ids, 64-bit
  feature negotiation through the selectors, queue select/size/address/ready,
  notify, interrupt status/acknowledge, the status state machine (§2.1, §3.1.1),
  shared-memory selectors (none; length reads −1) and the configuration
  generation. Access width rules (aligned 32-bit below 0x100; 1/2/4/8 bytes in
  config space), read-only and write-only registers, queue registers written
  while ready, out-of-range queue sizes, rings outside RAM or misaligned,
  status bits cleared without a reset, `DRIVER_OK` without `FEATURES_OK`, a
  driver setting `DEVICE_NEEDS_RESET`, and notifications before `DRIVER_OK` are
  all typed `MmioError`s.
- Packed rings only: the device offers and requires `VIRTIO_F_VERSION_1` and
  `VIRTIO_F_RING_PACKED`; a driver accepting anything else (or less) sees
  `FEATURES_OK` cleared on read-back. On each notification the descriptor
  ring is copied out of guest memory once and chains are captured from the
  copy by the existing `PackedQueue` (no double fetch); readable bytes are
  gathered once into the `Chain`. Completion scatters the response into the
  writable descriptors and publishes the used descriptor with id/len first and
  flags last. A malformed chain (out-of-RAM buffer, readable after writable, …)
  or a device protocol error sets `DEVICE_NEEDS_RESET` and raises a
  configuration interrupt; nothing more is processed until status 0.
  `GuestAuthorization::for_guest_ram` authorizes device access to exactly the
  VMM-owned guest RAM regions.
- `block::BlockDevice` (§5.2): `IN`, `OUT`, `FLUSH`, `GET_ID`; offers
  `BLK_SIZE`, `FLUSH` and `RO` for read-only disks. Out-of-range sectors (with
  overflow checks), partial sectors and writes to a read-only disk complete
  with `VIRTIO_BLK_S_IOERR` and leave the disk unchanged; unknown types get
  `UNSUPP`; the status byte is always the last writable byte. `durable()` is
  the disk as of the last `FLUSH`.
- `net::NetDevice` (§5.1): RX/TX queues, `MAC`, `STATUS`, `MTU`; no offloads,
  so TX headers must be `flags = 0`, `GSO_NONE`; bad frames are dropped and
  counted with their buffers returned. RX frames wait in a bounded backlog;
  receive buffers too small for a whole frame (no `MRG_RXBUF`) are returned
  empty and counted. Link changes bump the config generation and interrupt.
- `vsock::VsockDevice` (§5.10): RX/TX/event queues, stream sockets, host CID 2.
  Guest packets are validated before any effect (44-byte header, `len` equal to
  the payload and ≤ 64 KiB, `src_cid` = guest, `dst_cid` = host; spoofed or
  malformed packets are dropped and counted). Guest- and host-initiated
  connect, RW, credit update/request, shutdown and RST are implemented, with
  credit flow control enforced in both directions (a guest exceeding its
  grant is reset). Replies queue in a bounded outbox; when it is full the
  device stops taking TX chains (back-pressure, never dropped control
  packets). `transport_reset()` drops every connection and posts
  `VIRTIO_VSOCK_EVENT_TRANSPORT_RESET`, which a restored clone needs.
- `devices::vmgenid::VmGenIdDevice`: the Microsoft VM Generation ID device
  (`VMGENCTR` / `microsoft,vmgenid`). The 16-byte id lives at an 8-byte-aligned
  address that does not cross a page; `regenerate` writes the new id to guest
  memory before raising the notification, refuses the all-zero id and any id
  it already published. The ACPI `ADDR` package and the device-tree node are
  returned as values; no AML is assembled.

### 5. PVH boot (`pvh`, `guest_mem`)

- `guest_mem::GuestMemory` is a bounded RAM model (≤ 16 page-aligned disjoint
  regions, ≤ 1 GiB); every access must fit one region.
- `parse_pvh_elf` validates an x86_64 `ET_EXEC` ELF64: header, program header
  table bounds, each `PT_LOAD` (file range, `filesz ≤ memsz`, overflow, inside
  RAM, not over the loader's own boot structures, no overlaps), and the Xen
  `XEN_ELFNOTE_PHYS32_ENTRY` note (4- or 8-byte descriptor, below 4 GiB,
  exactly one, inside an executable segment). A byte-flip and truncation sweep
  over a test image never panics.
- `load_pvh` writes the segments (zeroing bss), a Firecracker-style boot GDT,
  `hvm_start_info` v1 (magic `0x336ec578`, command line, memory map, no
  modules, `rsdp_paddr = 0`), the `hvm_memmap_table_entry` table and the
  NUL-terminated command line, and returns the PVH initial vCPU state
  (`CR0.PE`, paging off, `CR4 = EFER = 0`, flat 4 GiB code/data segments, a
  32-bit TSS, `IF` clear, `EBX` = start-info address).

### 6. VMM seccomp-bpf profile (`seccomp`, `linux_host`)

- `SeccompProfile::vmm(arch, default)` allowlists 26 calls the vCPU/device
  threads need after setup and restricts `ioctl` to eleven KVM run-loop and
  userfaultfd requests (matched on the low 32 bits, as the kernel's `ioctl`
  takes an `unsigned int`). `compile` emits classic BPF: arch check
  (another audit arch → `KILL_PROCESS`; x32 numbers never match), then a
  `jeq` chain, then the `ioctl` request chain; it refuses programs whose
  8-bit jump offsets would overflow. `validate` applies the kernel's structural
  checks and `evaluate` interprets exactly the emitted subset. Syscall
  numbers are tabled for x86_64 and AArch64 and checked against `libc` on the
  build host.
- `host::install_seccomp` (feature `seccomp`, Linux) sets
  `PR_SET_NO_NEW_PRIVS` and calls `seccomp(SECCOMP_SET_MODE_FILTER)`.
  `seccomp_probe` runs a fixed set of probe calls for tests.

### 7. Gated KVM paths

`run_pvh_guest()` (Linux x86_64, feature `kvm`) boots a real PVH image through
`pvh`: 2 MiB of RAM at GPA 0 in an anonymous mapping, a verified one-slot plan,
segment registers, GDT, control registers and `RIP`/`RBX`/`RFLAGS` from the
returned `BootCpuState`; the 32-bit entry code reads `hvm_start_info.magic` and
`.version` through `EBX` and writes their low bytes to port 0x10. The test
expects `[0x78, 0x01]`. **This has never run**: the cloud host has no
`/dev/kvm`; the test records `Unavailable::DeviceAbsent`. The wave-six
`run_port_write_guest` is unchanged and also never ran.

Rust ownership of `Chain`, `SnapshotResumeTicket`, `Sealer` and `CloneEntropy`
here is not evidence that the Vesper source typing rules (`VirtqDescLease`,
`[T-Let]`, Theorem 2) are implemented; that is compiler work outside this
lane.

## What ran where

| Test file | What | Ran on |
| --- | --- | --- |
| `tests/devices.rs` (13) | register file, width/RO/WO rules, feature negotiation (§3.1.1), queue validation, block IN/OUT/FLUSH/GET_ID and every error status, ring wrap-around over 40 rounds, `DEVICE_NEEDS_RESET` and recovery, net TX/RX/drops/link change, vsock connect/RW/credit/shutdown, spoofing/malformed/credit violation, host connect, transport-reset event | everywhere (pure model, simulated driver in `tests/common`) |
| `tests/boot.rs` (5) | PVH load and vCPU state, every `PvhError`, truncation/byte-flip sweep, `GuestMemory` validation, VMGENID ordering and reuse | everywhere |
| `tests/seccomp.rs` (5) | compiled program semantics on both arches (exhaustive over syscall numbers 0..1024), malformed programs, numbers vs `libc`; **live install** in a re-executed child: after install `getpid` and `socket` return `EPERM`, an allowed `ioctl(-1, KVM_RUN)` reaches the kernel (`EBADF`), `ioctl(-1, TCGETS)` is stopped (`EPERM`) | live install: **Linux 6.18 cloud host, as root and as uid 65534 with all capabilities dropped** |
| `tests/shared_memfd.rs` (7) | seals; `UFFDIO_CONTINUE` into two private clones with **pagemap showing the same physical frame for every page in both**; COW write private to one clone (its frame changes, the sibling's and memfd's do not); first write as a minor fault; fault budget; hot set recorded from 701 faults, 4 runs, prefetch then **0 faults** on the same touches; copy-path prefetch; mapped fork clones reseeded with their own generation block and distinct keys; entropy sources | **Linux 6.18 cloud host, as root (route `Syscall`, frames visible) and as uid 65534 with all capabilities dropped (route `SyscallUserModeOnly`, sysctl 0; frames hidden, so sharing is not observed directly there)** |
| `tests/hotset.rs` (4) | coalescing, 2 MiB splits, 200 pseudo-random sets covered exactly, fault-order limit, decode validation | everywhere |
| `tests/kvm.rs` (2) | port-write guest and PVH boot where `/dev/kvm` opens; otherwise records `Unavailable` | recorded `DeviceAbsent` on the cloud host |
| `tests/fork.rs`, `tests/uffd.rs`, `tests/slot_plan.rs`, `tests/snapshot.rs`, `tests/virtio_packed.rs` | wave-six suites, updated for the entropy argument | as before |
| `tests/lean_model.rs` | Lean hygiene; with `VESPER_LEAN`, compile and axiom audit of all 21 theorems | Lean 4.24.0 (release zip) locally |

Measured, not benchmarked: in `hot_set_prefetch_removes_first_touch_faults`
(1,200-page sealed memfd, a 702-touch working set with 701 distinct pages)
the cold session took 701 minor faults; after prefetching the recorded hot set
(701 pages in 4 `UFFDIO_CONTINUE` calls) the warm session took 0 faults on the
same touches and 1 on an extra cold page. No timing was measured, and no
performance claim is made.

`VESPER_REQUIRE_UFFD=1`, `VESPER_REQUIRE_KVM=1`, `VESPER_REQUIRE_SECCOMP=1`
and `VESPER_REQUIRE_HW_RNG=1` turn the corresponding skip into a failure.
Whether userfaultfd minor faults and seccomp are permitted under Bazel on
BuildBuddy, GitHub's runners or the Windows/macOS jobs depends on the sandbox;
the test logs print which path ran.

## Lean (`lean/AetherMemory.lean`)

Added six theorems, all compiled with Lean 4.24.0 and audited with
`#print axioms` (only `propext`, `Classical.choice`, `Quot.sound`), no
`sorry`/`admit`/axioms:

- `scan_allows_iff_mem`, `scan_denies_unlisted`: the model of the compiled
  allowlist (a `jeq` chain falling through to the default) allows a syscall
  number exactly when it is in the list.
- `chunks_bounded`, `chunks_cover`: splitting a contiguous run of `len` pages
  into batches of at most `batch > 0` pages yields batches no larger than
  `batch` whose lengths sum to `len`.
- `advance_in_range`, `advance_wraps_iff`: advancing a packed-ring cursor by at
  most the ring size keeps the index in range and flips the wrap counter
  exactly when it passes the end.

These are theorems about the Lean models, not about the Rust code (the Rust
`HotSet::runs`, `SeccompProfile::compile` and `RingCursor::advance` are
tested against the same properties, not extracted).

## Decisions

- **No new dependencies.** The minor-fault, memfd and seccomp UAPI pieces
  (one more userfaultfd struct and ioctl, register mode and feature bits,
  `sock_fprog`) are declared in `linux_host.rs` next to the wave-six
  declarations, with size assertions; numbers were checked against the host's
  `<linux/userfaultfd.h>`, `<linux/seccomp.h>` and `<linux/audit.h>`.
- **Unsafe code** stays in `linux_host` (userfaultfd, memfd, seccomp, KVM) plus
  one new module, `hw_rng`, which the assignment's RDRAND/RNDR item requires:
  one call of a `#[target_feature(enable = "rdrand")]` function after runtime
  detection, and one `mrs` of `RNDR` after detecting FEAT_RNG. Every other
  module is `#![forbid(unsafe_code)]`.
- New Cargo feature `seccomp` (off by default, enabled for tests through the
  existing self dev-dependency, like `uffd` and `kvm`).
- OS entropy is `/dev/urandom` on Unix; on Windows only the hardware source is
  used (no OS RNG is reachable from `std` without a new dependency), and
  `gather` fails with `EntropyError::NoSource` if that is missing too.
- `MappedClone` uses MINOR mode only: the memfd is fully allocated before
  sealing, so there are no holes and no MISSING faults to serve.
- The VMGENID device publishes the id as `lo ‖ hi` little endian, matching the
  `VmGenId` the fork authority issues; it is a separate 16-byte page location
  from the 24-byte `AetherVmGenIdWire` block.
- The vsock RX path splits a payload across receive buffers smaller than the
  payload (stream semantics), but refuses buffers below the 44-byte header as
  a driver protocol error.
- The seccomp `ioctl` request list targets the x86_64 KVM run loop (AArch64 KVM
  uses `KVM_GET/SET_ONE_REG`, not in the list); the profile is a tested
  component, not yet installed by a running VMM.

## Checks

Commands with `CARGO_TARGET_DIR` outside the checkout, `CARGO_INCREMENTAL=0`,
`CARGO_PROFILE_{DEV,TEST}_DEBUG=0`:

- `cargo fmt --all -- --check`: clean
- `cargo test -p vesper-aether --locked` with
  `VESPER_LEAN=lean-4.24.0-linux/bin/lean`: all 91 tests pass (unit, 12
  integration files, doctest)
- the `shared_memfd` and `seccomp` binaries again under
  `setpriv --reuid=65534 --regid=65534 --clear-groups --inh-caps=-all --bounding-set=-all`:
  all pass
- `cargo +1.100.0 clippy -p vesper-aether --locked --all-targets -- -D warnings`
  and the same with the local 1.97: clean
- `cargo clippy -p vesper-aether --features blake3/pure --all-targets --locked
  --target {aarch64-apple-darwin, x86_64-pc-windows-msvc,
  aarch64-unknown-linux-gnu} -- -D warnings`: clean
- `lean lean/AetherMemory.lean` (4.24.0): compiles, standard axioms only
- workspace runs: see the PR description

## Review fixes

Codex review on the first push found three real defects, fixed with tests:
the net TX backlog is now bounded (`TX_BACKLOG`, excess counted as
`tx_dropped_backlog`); vsock credit saturates when the guest shrinks
`buf_alloc` below the bytes in flight (it used to wrap into a fresh grant);
and the userfaultfd server loop calls `ppoll`, which the VMM seccomp profile
allows, instead of `poll` (syscall 7 on x86_64, which the profile kills).

## Remaining gaps

- No KVM path has run on real hardware (no `/dev/kvm` here); serving a KVM
  guest's memory through userfaultfd also needs a descriptor without
  `UFFD_USER_MODE_ONLY` (vCPU faults come from the kernel).
- The device models are not wired to a running guest: no ioeventfd/irqfd, no
  MMIO exit dispatch, no device-tree or ACPI table emission, no event
  suppression, `NOTIFICATION_DATA`, `IN_ORDER`, indirect descriptors,
  multiqueue, checksum/GSO offloads, `MRG_RXBUF`, block `DISCARD`/`WRITE_ZEROES`,
  vsock `SEQPACKET`.
- The seccomp profile is compiled, interpreted and installed in a test
  process, but no VMM thread installs it yet; AArch64 KVM ioctls are not in
  its list.
- io_uring batching for prefetch (refused here), 2 MiB huge-page memfds
  (`UFFD_FEATURE_MINOR_HUGETLBFS`), and persisting the hot set next to the CAS
  snapshot.
- macOS Hypervisor.framework / Windows WHPX backends, AArch64 KVM guests,
  `vesper build --unikernel`, and Vesper-source `AetherVm` effect checking.
