# Aether: UFFD lazy page restore and KVM slot plans (§37) — wave six lane 12

Claim issue #254. Base `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (main after
plan PR #268; descendant of the required `cda092f`). Edited only
`crates/vesper-aether/**`, `lean/AetherMemory.lean`, this report, and the
root `Cargo.lock` (new third-party packages; separate commit).

## What was built

### 1. KVM EPT slot plans (`slot_plan`)

- `SnapshotMap::kvm_slot_plan(vm_id, log_dirty_private)` turns one VM's
  validated slots into `kvm_userspace_memory_region` records
  (`PlannedRegion`, same field names). `ReadOnlyCas` slots get
  `KVM_MEM_READONLY`; private slots optionally `KVM_MEM_LOG_DIRTY_PAGES`.
- `KvmSlotPlan::verify(&KvmHostLimits)` is the only way to get a
  `VerifiedKvmSlotPlan`, and the KVM backend installs only verified plans. It
  rejects, with a typed `PlanError` naming the slot(s): empty plans, too many
  regions, zero size, misalignment of GPA/HVA/size, endpoint overflow, null
  host address, unknown flags, read-only without `KVM_CAP_READONLY_MEM`, dirty
  logging on a read-only slot, slot ids at or above `KVM_CAP_NR_MEMSLOTS`,
  regions past the guest physical address width, duplicate slot ids, and any
  guest-physical or host-virtual overlap.
- Overlap detection sorts by start and keeps a running maximum end
  (O(n log n)); a test compares it with the quadratic definition on all
  1,728 triples of a small grid.
- `verify_cross_vm` checks plans that will share one host: distinct VM ids,
  and host memory shared between VMs only as identical read-only ranges
  (deduplicated CAS pages); partial or writable aliasing is refused.

### 2. CAS UFFD page resolver (`host`, `page_source`, gated `linux_host`)

- `probe_userfaultfd()` reads `/proc/sys/vm/unprivileged_userfaultfd` and
  `CAP_SYS_PTRACE` (from `CapEff`), then really opens a descriptor and runs
  the `UFFDIO_API` handshake. Routes tried in order: plain `userfaultfd(2)`,
  `userfaultfd(2)` with `UFFD_USER_MODE_ONLY`, `/dev/userfaultfd`
  (`USERFAULTFD_IOC_NEW`). Failure is `Unavailable::UserfaultfdDenied
  { unprivileged_userfaultfd, cap_sys_ptrace, errno }` (or `MissingFeature`,
  `UnsupportedPlatform`, `FeatureDisabled`).
- `LazyRegion::new(pages)` maps anonymous private memory and registers it in
  MISSING mode, requiring `UFFDIO_COPY`, `UFFDIO_ZEROPAGE` and `UFFDIO_WAKE`.
  `LazyRegion::serve(source, fault_budget, touch)` runs a fault-serving
  thread (poll on the uffd and a stop eventfd) while `touch` runs on the
  calling thread with a bounds-checked, atomic-byte `Toucher`. Each fault asks
  a `PageSource` for the page and resolves it with `UFFDIO_COPY` or
  `UFFDIO_ZEROPAGE`; `EEXIST` races are counted and woken. A source error,
  unexpected event, out-of-region fault or budget overrun unregisters the
  region (which wakes the blocked thread), and the session returns a typed
  `UffdError`; the region then refuses reuse. A panic in `touch` still stops
  the server (drop guard), so nothing hangs.
- `SnapshotMap::slot_source(store, vm, slot)` is the CAS page source: each
  page re-reads and re-hashes its BLAKE3 blob through the existing
  `SnapshotMap::resolve`, then copies the 4 KiB page.

### 3. Gated KVM execution

`probe_kvm()` opens `/dev/kvm` (API 12) and reports `KvmHostLimits`
(`KVM_CAP_NR_MEMSLOTS`, `KVM_CAP_READONLY_MEM`, 36-bit GPA width).
`run_port_write_guest(value)` (Linux x86_64, `kvm` feature) maps a private
code page at GPA 0x1000 and a read-only data page at 0x2000 holding `value`,
verifies that two-slot plan against the probed limits before any memory
ioctl, creates a VM and one vCPU in real mode, runs
`mov al, [0x2000]; out 0x10, al; hlt`, and returns the port and byte seen,
within a 16-exit budget. **This has never run**: the cloud host has no
`/dev/kvm`, and CI runners probably do not either. There it returns
`Unavailable::DeviceAbsent { path: "/dev/kvm" }`, which the test records.

### 4. Snapshot fork with nonce reseeding (`fork`)

- `quiesce_and_snapshot(old_sealer, memory)` consumes the running VM's
  `Sealer` (`sink old_sealer`) and freezes memory into a shared
  `SnapshotImage`; on a malformed layout the sealer is handed back.
- `SnapshotImage::fork(&mut ForkAuthority)` returns a copy-on-write
  `ForkClone` (pages shared via `Arc` until first write) and a linear
  `SnapshotResumeTicket` with a fresh fork epoch and 128-bit generation id
  (`lo` = epoch, `hi` = authority instance).
- `SnapshotResumeTicket::reseed_after_fork(master_key, &mut clone)` writes the
  24-byte `AetherVmGenIdWire` (LE fields, §37.2) into the clone's generation
  page and mints the clone's `Sealer` with a key from BLAKE3 `derive_key`
  over root key, generation id and epoch. A ticket for a different clone is
  handed back unconsumed. Until reseeded, the clone's `PageSource` refuses
  every page (`Error::NotReseeded`), so no restored guest can run with the
  snapshot's nonce state.
- Nonces are `epoch (LE u64) || counter (LE u32)`; the counter's 2^32 limit is
  a typed `NonceExhausted`. Epochs come from `ForkAuthority` and are never
  reissued (`EpochExhausted` at the limit), so nonces are unique among all
  sealers of one authority independently of the key derivation.
- `Sealer` and `SnapshotResumeTicket` are non-Clone, `#[must_use]`, and count a
  leak (`leaked_obligations()`) when dropped armed. Rust ownership here is
  not evidence that the Vesper source typing rules (`[T-Let]`, Theorem 2) for
  these types are implemented; that is compiler work outside this lane.

## Tests (where they ran)

| Test file | What | Ran on |
| --- | --- | --- |
| `tests/slot_plan.rs` | every `PlanError`, overlap differential, cross-VM sharing, SnapshotMap→plan | everywhere |
| `tests/fork.rs` | COW sharing, generation block, distinct keys, nonce uniqueness over 4 generations × 32 clones (2,112 nonces), ticket binding, epoch exhaustion, leak counting | everywhere |
| `tests/uffd.rs` | real userfaultfd: CAS restore out of order with a write fault, 4 concurrent touchers over 64 pages, source error and budget overrun without hangs, two reseeded clones restored with their own generation block, un-reseeded clone refused | **Linux 6.18 cloud host, as root (route `Syscall`) and as uid 65534 with all capabilities dropped (route `SyscallUserModeOnly`, sysctl 0)** |
| `tests/kvm.rs` | guest run where `/dev/kvm` opens; otherwise records `Unavailable` | recorded `DeviceAbsent` on the cloud host |
| `tests/lean_model.rs` | Lean hygiene; with `VESPER_LEAN`, compile and axiom audit | Lean 4.24.0 locally |

UFFD and KVM tests print what they exercised; where the kernel refuses they
print the typed reason and pass. `VESPER_REQUIRE_UFFD=1` /
`VESPER_REQUIRE_KVM=1` turn that skip into a failure. Under Bazel on
BuildBuddy, and on GitHub's Linux runners, whether userfaultfd is permitted
depends on the sandbox; read the test log line to know which happened.

Unprivileged run, for the record:
`setpriv --reuid=65534 --regid=65534 --clear-groups --inh-caps=-all --bounding-set=-all <uffd test binary> --nocapture`
→ 5 passed, every probe `route: Ok(SyscallUserModeOnly)`.

No performance claims: nothing was benchmarked.

## Lean (`lean/AetherMemory.lean`)

Added, all with `#print axioms` (only `propext`, `Classical.choice`,
`Quot.sound`), no `sorry`/`admit`/axioms:

- `running_reach_overlap`, `slot_end_within_width`: the two arithmetic steps
  the Rust slot-plan verifier relies on.
- `nonce_injective`, `distinct_epochs_distinct_nonces`,
  `issued_epochs_nodup`, `fork_clones_nonce_disjoint`: in the model where a
  nonce is `epoch * 2^32 + counter` with `counter < 2^32` and an authority
  issues `start, start+1, ...`, distinct clones never share a nonce. No
  injectivity of BLAKE3, HKDF or any key derivation is assumed (the design's
  `hkdf_vmgenid_injective` axiom would be unsound and is not used).
- `cow_write_reads_back`, `cow_write_other_pages`: a copy-on-write store
  changes only its page.

These are theorems about the Lean models, not about the Rust code.

## Decisions

- **No `userfaultfd` crate.** `userfaultfd-sys` 0.6 always runs bindgen
  (libclang plus kernel headers) at build time, which the hermetic Bazel
  build cannot provide. The small, stable UAPI (five structs, seven ioctl
  numbers) is declared in `linux_host.rs`, with size assertions; the ioctl
  numbers were checked against `<linux/userfaultfd.h>` on the host.
- **`kvm-ioctls =0.25.1` / `kvm-bindings =0.14.2`** (rust-vmm; pure Rust,
  pre-generated bindings; adds `vmm-sys-util 0.15.0`), only for
  `cfg(all(target_os = "linux", target_arch = "x86_64"))` behind the `kvm`
  feature.
- Features `uffd` and `kvm` are off by default; the crate's dev-dependency on
  itself turns them on for every test and clippy run (as in
  `vesper-warp-umem`), since CI passes no features.
- The crate is `#![deny(unsafe_code)]`; `linux_host` is the one module that
  allows it, compiled only on Linux x86_64/AArch64 with a feature on. All
  other modules are `#![forbid(unsafe_code)]`.
- Touched memory is read with `AtomicU8` operations so that a racing
  `UFFDIO_COPY` is never a Rust data race.
- On a serving error the region is unregistered to release blocked threads;
  they then read zeros, but the session returns the error, never success.
- Shared CAS pages are also served with `UFFDIO_COPY` into private memory.
- Clone memory is a host model (`BTreeMap` overlay over `Arc` pages), not
  `MAP_PRIVATE` of a memfd.

## Checks

Commands with `CARGO_TARGET_DIR` outside the checkout, `CARGO_INCREMENTAL=0`,
`CARGO_PROFILE_{DEV,TEST}_DEBUG=0`:

- `cargo fmt --all -- --check`: CHECK_FMT
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: CHECK_CLIPPY
- `cargo test --workspace --locked`: CHECK_TEST
- `cargo test -p vesper-aether` with
  `VESPER_LEAN=lean-4.24.0-linux/bin/lean` (release zip): all pass.
- `cargo clippy -p vesper-aether --features blake3/pure --all-targets --locked
  --target {aarch64-apple-darwin, x86_64-pc-windows-msvc,
  aarch64-unknown-linux-gnu} -- -D warnings`: clean (checks the non-Linux
  stubs and the AArch64 UFFD-only build).
- `lean lean/AetherMemory.lean` (4.24.0): compiles, standard axioms only.

## Remaining gaps

- `UFFDIO_CONTINUE` over a shared, sealed memfd (minor faults) for zero-copy
  cross-VM CAS pages, and 2 MiB io_uring prefetch of the hot set (§37.1.3).
- Serving a KVM guest's memory through userfaultfd (vCPU faults come from
  the kernel, which a `UFFD_USER_MODE_ONLY` descriptor does not report).
- PVH boot, VirtIO MMIO devices, seccomp lockdown, VMGENID ACPI device,
  macOS Hypervisor.framework / Windows WHPX backends, and AArch64 KVM guests.
- The KVM guest path is untested on real hardware.
- Fork clones are a userspace model; a real fork would remap the snapshot
  `MAP_PRIVATE` per clone and lazily restore it, and RDRAND/RNDR entropy is
  not mixed into the key (the generation id and epoch are).
- No `vesper build --unikernel`, no Vesper-source `AetherVm` effect checking.
