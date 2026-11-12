# Aether direct packed-queue host slice — issue #51

Implemented only `crates/vesper-aether/**` and this report on
`continuation37/aether-virtio`, based on `5e699f8`. Existing snapshot/page
implementation and tests are preserved; `lib.rs` only exports the new `virtio`
module. No dependency, root manifest/lockfile, workflow, CLI or Lean changes.

## Standard and supported contract

Read [issue #51](https://github.com/robot-head/vesper/issues/51), §37.2 of the
refreshed design, and the complete authoritative VirtIO 1.2 packed-ring source:
[OASIS virtio-spec, v1.2-cs01, packed-ring.tex](https://github.com/oasis-tcs/virtio-spec/blob/e230375adde48ff39f734ea2783ef7b9d2ba346a/packed-ring.tex).
The source checkout is tag `v1.2-cs01`, commit
`e230375adde48ff39f734ea2783ef7b9d2ba346a`. Published specification:
[VirtIO 1.2 §2.8 Packed Virtqueues](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html).

The module implements a deliberately restricted direct-descriptor host model:

- Raw codec is exactly 16 bytes: LE64 address, LE32 length, LE16 buffer ID,
  LE16 flags. It uses explicit byte operations and no Rust-layout casts.
- Available direct descriptors support NEXT, WRITE, AVAIL (bit 7), USED (bit 15).
  INDIRECT and other flags are rejected; indirect tables, event suppression,
  feature negotiation, transport and IN_ORDER batch completion are absent.
- Ring size is 1–32768, including non-powers of two (§2.8 and Structure Size and
  Alignment). Cursors start at offset zero with wrap=true. Available means
  AVAIL=wrap and USED!=wrap; used means both equal wrap (§2.8.1). The raw flag
  predicates alone do not establish queue occupancy or completion.
- Direct chains occupy consecutive ring entries, possibly crossing the end.
  Every element must have the correct local phase bits, and the chain must end
  within both the queue size and device's configured descriptor limit.
  Device-readable elements precede writable ones (Scatter-Gather requirements).
  The buffer ID comes from the **last** element (§2.8.6 Next Flag), with preceding
  ID fields ignored. This implementation limits terminal IDs to queue size and
  rejects IDs already reserved; these are explicit local policies.
- Zero-length elements are explicitly unsupported as a conservative local
  policy, not asserted to be forbidden by VirtIO. Every nonzero element's
  checked half-open GPA interval must fit one authorized range. Authorization
  ranges must fit one already validated SnapshotMap slot, even when neighboring
  slots or ranges would otherwise have equal permissions. All overlaps in the
  authorization allowlist are refused. Writes require both caller write
  permission and a Private slot; readonly CAS slots cannot authorize writes.
- A device-side FIFO completion policy emits **one** used descriptor per chain
  at the next-used cursor, carrying the terminal ID and caller-declared written
  byte count. WRITE is set only for a nonzero written count. The cursor advances
  by the complete chain length, with wrap updated even when only the first
  descriptor is overwritten. Other chain entries stay unchanged. NEXT is cleared
  in generated used records. FIFO is this model's chosen completion order; it
  does not imply negotiation of the distinct IN_ORDER batching feature.

## Capture, ownership and failure

`GuestAuthorization::new` derives an immutable numerical permission snapshot from
one validated SnapshotMap, VM ID and explicit caller allowlist. `PackedQueue`
embeds that bounded snapshot; callers cannot swap an unrelated map into later
captures. The authorization does not own or pin any guest allocation.

`PackedQueue::capture` borrows an ordinary byte-ring, copies each descriptor's
fields once, validates the **entire** chain, and only then commits a reservation
and returns a non-Clone `DescriptorLease`. Its immutable `CapturedChain` exposes
VM ID, descriptors, terminal buffer ID and direction byte totals. It captures
metadata, not payload bytes. Safe Rust borrowing supplies stable source bytes
for the call; this is not a single-fetch guarantee for concurrently changing
DMA/MMIO or unsafe shared guest memory. Source changes after the call do not
change the owned snapshot.

Each lease has private queue-origin and generation identity. Origins and
per-capture generations use checked non-wrapping counters. Queue-full,
unavailable/incomplete/invalid chains, ID conflicts and limits leave committed
state unchanged and preserve the caller's borrowed ring. Completion rejects
foreign/stale/out-of-order owners, invalid written length and wrong ring size
before changing either ring bytes or queue state. `CompletionError` returns the
same lease for retry. Completion never refetches captured descriptor metadata.
Dropping/forgetting a lease keeps its reservation registered and can stall the
queue: non-Clone affine Rust ownership is not static linear discharge or a
liveness proof. Module-private forged duplicates are used only in adversarial
stale/duplicate-completion tests; there is no public lease import/constructor.

The external ring is a caller-selected ordinary host byte buffer of exactly the
configured size. This API does not prove the identity, address, alignment or
physical backing of a mapped guest ring. A written-count argument is a caller
declaration, not evidence that payload bytes were initialized. No live VirtIO
DMA, KVM/UFFD ioctls, VM boot, memory ordering, driver publication/notification,
device isolation, encrypted memory or cryptographic fork-safety claim is made.

## Bounds

Hard ceilings are 32768 ring entries, 4096 authorization regions, 32768
descriptors per chain, 64MiB declared bytes per chain and 16,777,216 charged
validation steps. Defaults are 256 regions, 1024 descriptors, 1MiB and 1,000,000
steps. Chain count is also bounded by free ring entries; occupied entries across
all live leases cannot exceed queue size. Each descriptor end and byte sum is
checked before admission; metadata growth occurs only after its limit check.
Authorization charges range and slot validation; capture charges descriptors and
binary-search permission comparisons. Sorting, allocation, bookkeeping and
completion scans have the additional fixed size bounds above, not a cycle-exact
fuel guarantee. Normal Rust allocator failure is not converted to QueueError.
Only descriptor metadata is allocated; advertised payload bytes are not copied.

## Validation

On this Linux host with private target
`/tmp/vesper-target-continuation37-aether-virtio`, one build job, development/test
debug info zero and incremental disabled:

```sh
cargo fmt -p vesper-aether --check
cargo test -p vesper-aether --all-targets --locked --offline
cargo test -p vesper-aether --doc --locked --offline
cargo clippy -p vesper-aether --all-targets --locked --offline -- -D warnings
cargo run -p vesper-aether --example packed_queue --locked --offline
```

Passed: 13 new integration tests, three new module adversarial tests and one
compile-fail doctest proving DescriptorLease lacks Clone; all existing 14
snapshot/page integration tests also pass. The new tests cover byte-for-byte LE
codec, phase truth table, terminal IDs, one used record per chain, both wrap
phases, non-power-of-two rings, full chains and free-slot backpressure, ID reuse,
malformed/partial chains, checked overflow, permissions/readonly memory, byte/
descriptor/work limits, hostile mutation after capture, owner recovery on failed
completion, stale/duplicate/foreign completion, generation/origin exhaustion and
drop-stalls behavior. A separate cursor reference loop exercises 576 chains
across ring sizes 1–9 and varying full/wrapping chain lengths.

The self-checking `packed_queue` example captures a two-element chain across a
three-entry ring's boundary, mutates the source bytes, verifies the immutable
snapshot, rejects an oversized completion without changes and retries with its
returned lease. It checks the resulting single used record and cursor phase.
Stable runnable command after integration is the example command above.

Windows `x86_64-pc-windows-gnu` cross-compilation also passed strict Clippy for
all targets with the same locked/offline build configuration. This is compile
coverage only; no native Windows/macOS execution or device execution was run.
