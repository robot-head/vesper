# Rust exit SH6 lane 14: Tracing, PMU, AF_XDP and KVM on syscalls

Claim #1409. Prompt: `docs/agent-prompts/self-host-sh6/14-os-tools.md`.
Part 1 (this report's first version) is the audit: what each crate needs
from the OS or a C library, what the compiler can express today, and the
gaps, each with a minimal repro run against the compiler at `ab965f2a`.

## Summary

| Crate | Non-test Rust | Rust that touches the OS | Who links it | Can leave Rust now? |
|---|--:|---|---|---|
| `vesper-warp-umem` | 3.5k | `af_xdp.rs` (935 lines; socket, setsockopt, mmap'd rings) | warp, chorus, prism-h3 (+interop), sim-session | Yes for the model (soft NIC, rings, accounting, driver); the AF_XDP backend loses zero-copy frame views (G3) |
| `vesper-bench-pmu` | 3.2k | `linux.rs` (perf_event_open, read, mmap), `rdpmc.rs` (RDPMC / `mrs`), `kpc.rs` (dlopen of kperf), `alloc.rs` (a Rust `GlobalAlloc`) | vesper-tools (bench), vesper-hybrid | Partly: perf_event via `read(2)` yes; RDPMC (G4), kpc (G5) and the allocator counter (G7) no |
| `vesper-chrono` | 8.0k | `capture.rs` (perf_event, mmap AUX), `tracked_arena.rs` (via vesper-fiber-abi write watches), `ipt_flow.rs` (iced-x86) | root compiler (`top`, `replay`, anvil sim) | Seed pattern (compiler-linked). Decoders, bundle, ring, replay, lineage are pure logic; iced-x86 needs a Vesper length/flow decoder (part 3); write tracking waits on lane 13 (fiber-abi) |
| `vesper-aether` | 10.0k | `linux_host.rs` (userfaultfd, memfd, KVM through kvm-ioctls), `seccomp.rs`, `hw_rng.rs` (RDRAND/RNDR), `vmm_thread.rs` (threads) | nobody (tests only) | Devices, virtio, PVH, slot plan, fork, snapshot: yes. KVM/uffd via ioctl: yes with workarounds. Threads (G5/G8) and RDRAND (G4): no |

About 80% of the four crates is ordinary logic (decoders, ring models,
virtio devices, accounting) that Vesper expresses today. The OS edges are
small, and most of them work with direct libc imports (G0). The blockers
are CPU instructions, C callbacks and long-lived threads.

## What works today (G0: direct libc imports, no Rust shim)

A library build (`vesper build --lib`) can import libc itself: a
`// @export vsys direct` block with `link = "dynamic", name = "c"` needs no
Rust module, and the Rust binary that links the object already links libc.
Probe (`getpid`, `sysconf`, `mmap`/`munmap` with `errno`, and `syscall(2)`
declared with six fixed arguments):

```vesper
// @export vsys direct
// | sys library Libc (target = "@TARGET@", abi = "@ABI@", link = "dynamic",
// |     name = "c", artifact = "blake3:00..00") {
// |     fn getpid() -> ForeignVal[i32];
// |     fn sysconf(name: i32) -> ForeignVal[i64];
// |     fn syscall(nr: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: i64) -> ForeignVal[i64] errno;
// |     fn mmap(addr: u64, len: u64, prot: i32, flags: i32, fd: i32, off: i64) -> ForeignVal[u64] errno;
// |     fn munmap(addr: u64, len: u64) -> ForeignVal[i32];
// | }
pub fn tid() -> i64 eff [Ffi[Libc]] {
    let (v, _) = Libc::syscall(186, 0, 0, 0, 0, 0, 0);
    v
}
```

Linked into a C `main` on x86_64 Linux: `pid 8988/8988 page 4096 map 0 tid
8988/8988` (each pair is Vesper/C). Also verified:

- **A wire struct by pointer** (`nocapture inout ws: Winsize`) to `ioctl`:
  `TIOCGWINSZ` returns `ENOTTY` (25) off a terminal and the row count on a
  pty. This is the shape of every KVM, userfaultfd and AF_XDP control call
  (`KVM_SET_REGS`, `UFFDIO_COPY`, `XDP_MMAP_OFFSETS`). `ByteArray[N]` and
  `ZeroPad[N]` fields cover the UAPI arrays and padding.
- **One C symbol under several signatures**: two libraries (`IoA`, `IoB`)
  both declare `ioctl`, one with a struct pointer and one with a `u64`
  argument, and both link to the same `ioctl`. So each request shape gets
  its own library.
- **`errno`** after every call (`-> ForeignVal[T] errno`).
- **Acquire loads on foreign memory through libatomic** (a C library):
  `fn __atomic_load_4(addr: u64, order: i32) -> ForeignVal[u32]` from
  `link = "dynamic", name = "atomic"` read 42 from a C `uint32_t`. This is the
  ring-index protocol of AF_XDP and perf mmap pages. The Rust binary needs
  `-latomic` (Linux has it; see G6).
- **Copying to and from an address**: a `CSlice` parameter expands to
  `(pointer, length)`, so `memcpy(dst: u64, nocapture in src: CSlice[u8])`
  writes a Vesper buffer to a mapped address and
  `bcopy(src: u64, nocapture inout dst: CSlice[u8])` reads one. With
  `mmap` returning the address as a `u64`, that is enough for the umem
  frame copies, kvm_run, uffd staging pages and guest memory, at the cost
  of one copy (G3).

## Gaps, with minimal repros

### G1: variadic C functions cannot be declared

`ioctl`, `fcntl`, `open` and `syscall` are variadic. The `.vsys` parser
refuses `...` ("variadics, attributes and imperative code are forbidden"),
so ports declare fixed arities (above). That matches the variadic
convention on x86_64 SysV (the callee ignores `%al` for integer-only
calls; glibc's `syscall` and `ioctl` are assembly) and on AArch64 Linux
(AAPCS64 passes variadic integers like fixed ones). It is **wrong on Apple
arm64**, which passes every variadic argument on the stack.

```vesper
// | sys library Fcntl (target = "@TARGET@", abi = "@ABI@", link = "dynamic", name = "c", artifact = "blake3:00..00") {
// |     fn fcntl(fd: i32, cmd: i32, ...) -> ForeignVal[i32];   // refused: variadics
// | }
```

Who needs it: bench-pmu's macOS path (`fcntl` probes), every crate's
`open`. Linux-only crates (umem's AF_XDP, aether's KVM/uffd, perf_event)
are fine with fixed arities. Fix: `...` in `.vsys` with the platform's
variadic lowering (on Apple arm64 the call site stores the variadic words
on the stack itself). Owner: `vesper-ffi` is
lane 13's (#1408); reported there, not fixed here.

### G2: no address of a Vesper buffer

A Vesper `Slice` can be lent to C as a `CSlice` for one call, but its address
cannot be put into a struct field. `UFFDIO_COPY` (`src: u64`),
`KVM_SET_USER_MEMORY_REGION` (`userspace_addr`) and `xdp_umem_reg`
(`addr`) all take an address inside a struct.

```vesper
let buf = ChunkSeq::with_capacity[u8](4096);
let copy = UffdioCopy { dst: fault_page, src: ???, len: 4096, mode: 0, copy: 0 };
```

Workaround (no compiler change): put every such buffer in memory the port
maps itself (`mmap` returns its address as a `u64`) and fill it with
`memcpy`. That is what the ports will do; it costs one copy per uffd fault.

### G3: no in-place view of foreign memory

Mapped memory (AF_XDP UMEM frames, the perf AUX buffer, guest RAM) can be
copied into a Vesper buffer but not viewed as a `Slice[u8]` in place.
`vesper-warp-umem`'s `frame()`/`frame_mut()` give Rust callers `&[u8]` into
the UMEM itself; through Vesper they become a copy in and a copy out of
each frame. For the soft NIC (the only backend CI runs) this changes
nothing; for the real AF_XDP backend it gives up zero copy on the payload.
Fix: a `.vsys` form that returns a bounded, lifetime-scoped `Slice[u8]`
view of a mapping the library owns (a `linear extern type Mapping` with
`fn mapping_bytes(inout m: Mapping) -> CSliceView[u8]`). Manuscript change
needed (§16.3); not attempted here.

### G4: no CPU instructions (inline assembly or intrinsics)

| Crate | Instruction | Why |
|---|---|---|
| bench-pmu `rdpmc.rs` | `lfence; rdpmc; lfence` (x86_64), `isb; mrs pmccntr_el0 / pmevcntrN_el0; isb` (AArch64) | user-space counter reads inside the perf page's seqlock |
| aether `hw_rng.rs` | `rdrand` (x86_64), `mrs rndr` (AArch64) | hardware entropy for the guest's virtio-rng |

Vesper has no `asm`, and no intrinsic for these. libc has no wrapper for
`rdpmc` (`rdrand` has none either; `getrandom(2)` is a different source).

```vesper
pub fn counter(index: u32) -> u64 {
    rdpmc(index)   // E0201 unknown name: there is no such builtin
}
```

Fix: a small set of target intrinsics in the compiler (`rdpmc`,
`rdtsc`, `rdrand64`, `aarch64_mrs[REG]`, `lfence`, `isb`). Cranelift has
no inline assembly, so each needs its own lowering in the backend. That is compiler work in
`src/` (codegen), owned by the SH4 lanes; reported to the coordinator.
Until then, `rdpmc.rs` and `hw_rng.rs` stay Rust, or the RDPMC provider
is dropped in favour of `read(2)` on the perf fd (slower: a syscall per
read; measured in part 2).

### G5: no C callbacks or calls through a function pointer

- bench-pmu `kpc.rs` (macOS) `dlopen`s the private `kperf` framework and
  calls its functions through `dlsym` pointers.
- aether's VMM thread and uffd handler thread start with
  `std::thread::spawn` (`pthread_create` takes a C function pointer).
- chrono's `tracked_arena` catches write faults with a `SIGSEGV` handler
  (through vesper-fiber-abi's `write_watch`, lane 13's crate).

`.vsys` has no function-pointer type, and a Vesper function cannot be
handed to C.

```vesper
// | fn pthread_create(nocapture inout t: u64, attr: u64, start: fn(u64) -> u64, arg: u64) -> ForeignVal[i32];
//   refused: `fn(..)` is not a .vsys type
```

kpc has a workaround: link `kperf.framework` directly
(`link = "dynamic", name = "kperf"` with the private-framework search path)
instead of `dlopen`. Threads and signal handlers need the compiler: an
exported Vesper function usable as a C function pointer
(`extern fn` with a fixed C signature and an empty effect row), plus a
`.vsys` `CFn[sig]` parameter type. Manuscript change (§16.3) and compiler
work; reported.

### G6: atomics on foreign memory come from libatomic

`Atomic[T]` is an owned cell; it cannot be placed over mapped memory. The
libatomic route (G0) works and is C, not Rust, but: macOS has no
libatomic (compiler-rt provides only the generic `__atomic_load`), and the
Rust crates that link the object must add `-latomic` on Linux. Fix:
`.vsys` atomics on an address (`fn load_acquire_u32(addr: u64) -> u32`
as compiler builtins), or `Atomic` views of a mapping (with G3). For the
Linux-only crates here, libatomic is enough.

### G7: a Rust global allocator stays Rust while Rust uses it

bench-pmu's `alloc.rs` is a `#[global_allocator]` that counts the
allocations of the Rust benchmark binary itself. It has no Vesper meaning:
Vesper code counts its own allocations (`live_allocations()`), and the
allocator goes when the last Rust bench that installs it goes. Not a
compiler gap; it is listed so the census does not count it as portable.

### G8: no long-lived worker with effects

`parallel_for` bodies must have an empty effect row (`E0823`), so a vCPU
loop (`KVM_RUN` is `Ffi[Kvm]`) or a uffd handler cannot run on a worker.
This is a blocker, not a design choice: a guest access to a missing page
blocks the vCPU thread inside `KVM_RUN` until another thread resolves the
fault (`Region::serve` in `linux_host.rs` runs the resolver concurrently
for that reason), and `immediate_exit` is checked only when `KVM_RUN`
starts, so one thread that polls between runs never gets to serve the
fault. Without G5 (a `pthread_create` callback) or an effectful worker,
aether's userfaultfd-backed lazy memory stays Rust; the only single-thread
alternative is a different memory strategy (populating guest memory
eagerly before `KVM_RUN`, which gives up lazy restore).

### G9: chrono's x86 decoder (iced-x86)

`ipt_flow.rs` uses iced-x86 only for instruction length, `FlowControl`
(next, branch, conditional, indirect, call, return, exception, interrupt)
and the near branch target. That is a length decoder plus a flow
classifier, not a full disassembler: the one-byte, two-byte (`0F`) and
three-byte (`0F 38`, `0F 3A`) opcode maps with their ModRM/SIB/immediate
size rules, the legacy, REX, VEX, EVEX and XOP prefixes, and a flow table.
Part 3 compares a Vesper port of those tables against Capstone's C API
(`cs_disasm_iter` plus `cs_insn_group`), and picks the one within 1.5x.
Capstone adds a C dependency to the compiler's closure; the Vesper table
does not.

## Plan for parts 2-4

In order, one crate per PR:

1. **`vesper-warp-umem`** first: the smallest, most of it pure, and four
   Rust crates use it, so its binding is the pattern for the rest. The soft
   NIC, rings, frames, accounting and driver go to Vesper with the
   crate's tests as the differential; the AF_XDP backend goes to direct
   libc imports (G0) with frame copies (G3) and libatomic (G6). The
   sim-session lane (#1342) keeps porting its own umem/warp models against
   the Rust crate until this lands.
2. **`vesper-aether`** devices, virtio, PVH, slot plan, fork and snapshot
   (pure); then uffd, memfd, seccomp and KVM over direct imports. Threads
   (G5/G8) block the uffd fault resolver and `vmm_thread.rs`, and RDRAND
   (G4) blocks `hw_rng.rs`; those stay Rust until the gaps close.
3. **`vesper-bench-pmu`**: events and the perf_event provider (read(2));
   RDPMC (G4), kpc (G5) and the allocator (G7) stay Rust until their gaps
   close.
4. **`vesper-chrono`**: compiler-linked, so it flips with the seed pattern
   (#1102). The IPT/ETE decoders, bundle, ring, lineage, replay and
   checkpoint go first; the x86 decoder is part 3; `capture.rs` uses
   G0; `tracked_arena` waits on lane 13's fiber-abi port.

Each flip pastes the flip-gate verdict (0 differences over the crate's
tests; ABBA upper bound at or below 1.5x).

## Part 2a: `vesper-warp-umem` in Vesper (no flip yet)

The rings, soft NIC, driver ledger and conservation audit are Vesper
(`crates/vesper-warp-umem/vesper/src/*.ves`, about 2,400 lines), behind the
`vesper-port` feature with the usual harness: a committed binding,
`port/test-port.sh`, Bazel port targets, and shard 7 of
`scripts/test-port-harness.sh`. The AF_XDP backend (`af_xdp.rs`) is part
2b and stays Rust; under `vesper-port` it is compiled out.

Function, checked on this branch:

- All of the crate's tests pass against both builds: rings 5, soft NIC 14,
  wire 3, Lean count model 3, zero allocation 1 (the port's steady-state
  echo loop allocates nothing).
- The consumers pass against the port: `vesper-warp` and
  `vesper-sim-session` with `--features vesper-warp-umem/vesper-port`.
- Divergence check: one bug per module (`DIVERGENT_EDITS` in build.rs)
  fails rings 3, soft NIC 7, zero allocation 1 and Lean 1.
- Errors map back to the Rust types (`UmemError`, `ConservationError`) with
  the same first error the Rust auditor reports.

Speed: `tests/abba.rs` runs the echo loop of `zero_alloc.rs` (step,
harvest, turn around, submit, retire, complete, replenish, then an exact
audit) on both builds in one process; the Rust build is compiled beside
the port as `rust_reference`.

| Version | Vesper/Rust median | 95% interval |
|---|---|---|
| straight port | 2.43x | [2.36, 2.47] |
| fast audit path, epoch stamps | 2.10x | [2.05, 2.13] |

The bar is 1.5x, so the crate does not flip. The fast audit decides "clean"
without building any error, and falls back to the exact pass otherwise, so
error reports are unchanged. Instruction counts (callgrind, whole run) put
Vesper at 1.87x Rust's instructions. Each function costs 1.5x to 2.6x its
Rust twin with the same algorithm: audit 1.8x, device step 1.8x,
`replenish_fill` 2.6x, `harvest_rx` 1.6x. The cause is generated code,
not the port. Every `Option` that a `try_get` or a helper returns lives in
a stack slot and is tested again at each `match`, and live values spill
across loop bodies (a 3 KB frame in the audit). That is gap G10 below. The
port is ready to flip once G10 closes; re-run the ABBA test then.

## Part 2b: `vesper-aether` slot plans, PVH parsing and hot sets in Vesper (no flip yet)

`slot_plan` (the KVM memory-slot plan verifier and the cross-VM host
check) is Vesper (`crates/vesper-aether/vesper/src/{util,slot_plan}.ves`)
behind `vesper-port`. The public `verify` and `verify_cross_vm` forward to
the port, and the Rust code stays as `verify_reference` and
`verify_cross_vm_reference` (hidden) for the differential test and the ABBA
test. The flip deletes those.

- The whole aether suite passes on the port.
- `tests/slot_plan_port.rs` compares both builds on 40,000 random plans
  that hit every rule, error by error.
- The divergence check (touching ranges count as overlapping) fails the
  slot-plan tests and the differential test.
- One known difference that no test can observe: when the cross-VM check
  sees several regions with the same host range, the port reports them in
  input order. The Rust sort leaves that order unspecified.

Speed: `tests/abba_slot_plan.rs` verifies four 32-slot plans, then runs
the cross-VM check, at 4.8x Rust with a heap sort and 4.0x
([3.97, 4.12]) with run-and-merge sorts. The sort costs about 400
instructions per element in Vesper against about 60 in Rust's small sort.
This is G10 again: every element access goes through an `Option` kept in
memory. The port waits on the same codegen fix as umem.

`pvh::parse_pvh_elf` (the bounds-checked PVH ELF parser and note scan)
is Vesper too (`vesper/src/pvh.ves`), with the Rust kept as
`parse_pvh_elf_reference`. The boot tests pass on the port.
`tests/pvh_port.rs` compares both builds on every truncation and single
bit flip of the test kernel, plus 20,000 images with random header and
note fields. The divergence check (a segment may end one byte past the
file) fails that test. `load_pvh` (the writes into guest memory) stays
Rust for now.

PVH speed (`tests/abba_pvh.rs`: parse the test kernel and every
truncation of it) is 10x Rust ([9.8, 10.2]). Each call is about 100 ns,
so the fixed cost of crossing the binding is a large share, and the parser
body alone costs 5.7x the Rust instructions. Reading fields through
`try_read_u64_le` and returning segments in a caller buffer (instead of
an `inout ChunkSeq`, which allocates a sequence per call) took it down
from 17x.

`hotset::HotSet::decode` (the untrusted on-disk form) and `runs` (the
prefetch batches) are Vesper (`vesper/src/hotset.ves`), with the Rust kept
as `decode_reference` and `runs_reference`. The hotset and hotset_store
tests pass on the port, `tests/hotset_port.rs` compares both builds on
5,000 random sets and 5,000 mutated encodings, and the divergence check
(equal indices count as increasing) fails both. ABBA (`tests/abba_hotset.rs`:
decode a 1,000-page set, then build its runs) is 1.9x ([1.87, 1.93]), the
closest yet. Part of the gap is copying pages between the crate's `usize`
and the port's `u64` at the boundary, which a safe-code crate cannot avoid.

## Part 2c: the packed virtqueue capture in Vesper (no flip yet)

`virtio::PackedQueue::capture` (the bounded packed-ring model of
virtio 1.2 §2.8) walks its chain in Vesper (`vesper/src/packed.ves`).
Rust keeps the queue state and the checks before and after the walk: the
ring length, `Full`, the lease identity and the buffer id. The walk itself
is in Vesper: phase bits, flags, the read-after-write rule, the
authorization search, byte and work budgets, and the cursor advance. It
writes the chain into a scratch buffer that the queue owns.
`capture_reference` keeps the Rust walk.

- `tests/virtio_packed.rs` passes on the port.
- `tests/packed_port.rs` runs 6,000 random queues through the same
  random captures, completions and driver writes in both builds. It
  compares every lease, error, state and ring byte.
- The divergence check (a used descriptor counts as available) fails that
  test.

ABBA (`tests/abba_packed.rs`, which captures and completes two laps of
two-descriptor chains on a 256-entry ring) was 1.7x ([1.70, 1.81]) and is
1.57x ([1.55, 1.59]) on the main of 2026-11-22 06:05 UTC. Two changes
brought it down from 3.2x:
- The output buffer belongs to the queue, so a call no longer zeroes a
  fresh buffer.
- One `try_read_u64_le` now reads each descriptor's length, id and flags.

Under callgrind the Vesper walk costs 720 instructions per chain against
390 for Rust. The listing shows G10: each `try_read` result goes to a
stack slot and is tested twice.

## Part 2d: the virtqueue engine walks flip to Vesper

The engine behind the MMIO devices (`devices::queue::VirtQueue`, split and
packed rings with indirect tables, event index and in-order completion)
captures every chain through Vesper (`vesper/src/vq.ves`) in every build.
Vesper reads the avail ring or the packed descriptors, follows the chain
and any indirect table, and applies every check: flags, the
read-after-write rule, the region authorization, the descriptor, byte and
work budgets, loops, busy ids and descriptors, and the hint bound. Rust
copies the readable bytes out once and commits the chain. Completion,
flushing and notification suppression stay Rust; they are not walks.

`GuestMemory` now keeps guest RAM in one buffer with a region table, so the
walk sees it as one slice.

The Rust walks left the build. They live on as the unit tests' reference
(`src/devices/queue/reference.rs`, `#[cfg(test)]`), as vesper-flux keeps
its differential.

- `reference::differential` runs 8,000 random queues (both formats,
  random limits, features, hints and driver writes) through both walks
  and compares every chain, error, stat and ring byte.
- The divergence check (a chain may hold one buffer more than the limit)
  fails it.
- `reference::abba` captures, completes and flushes 64 three-descriptor
  chains per lap on a 256-entry split ring and a 256-entry packed ring.

Flip gate (`crates/vesper-aether/flip-gate.toml`):

```text
flip-gate vesper-aether: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 2/2 pass | differential 0 differences | ABBA vesper/rust virtqueue 1.44x [1.36x, 1.47x] n=30 | RUSTFLAGS=-C target-cpu=native
| `vesper-aether` (PASS) | SH6-14 | 0 differences; tests 2/2 pass | virtqueue 1.44x [1.36x, 1.47x] n=30 |  | |
```

The margin is small. Two things made it: the walk keeps its running
totals in caller locals with pure helpers instead of `inout` scalars, and
the queue owns the output buffer. The rest is G10.

## Part 2e: the hardware RNG in Vesper (G4 closed)

`hw_rng` (RDRAND on x86_64, RNDR on AArch64, used by the clone entropy)
was the crate's only unsafe code outside `linux_host`. It is now Vesper
over `@vesper-std/cpu` (#1443, #1473), detection included, and its Rust
is gone. There is one package per target in `vesper-rng/`, and all three
export the same two functions, so one binding (`port/rng_binding.rs`)
serves them:

- `x86_64`: CPUID leaf 1 for RDRAND (Linux, macOS and Windows).
- `aarch64-linux`: `getauxval(AT_HWCAP2) & HWCAP2_RNG`.
- `aarch64-macos`: the `hw.optional.arm.FEAT_RNG` sysctl.

Other targets keep the stub that reports no instruction. The unit tests
check that the detection agrees with the standard library's and that a
present generator delivers distinct words. Speed does not matter here
(four words per clone).

## Part 2f: hot sets, the packed capture and the seccomp evaluator flip; the other aether ports are measured

The Claude thread that took the lane over on 2026-11-25 measured every
aether port that was still Rust by default on that day's main (84764cd),
with a randomized differential for the eight modules the paused agent had
ported with only hand-picked cases (`src/port_abba.rs`, under
`vesper-port`). The differential found one divergence: on a status write
that offers FEATURES_OK and DRIVER_OK at once with unacceptable features,
the Rust transport counts the rejected feature set before failing with
`DriverOkWithoutFeaturesOk`, and the port did not. The fault now carries
`rejected_features`, and the transport counts it (`vesper/src/mmio.ves`).

Three modules pass and flip. Their Rust is now `#[cfg(test)]` reference
code, and their differential and ABBA tests moved from `tests/` into the
crate (`src/port_reference_tests`), since integration tests cannot reach
test-only items:

- hot sets (`HotSet::decode`, `HotSet::runs`; `vesper/src/hotset.ves`),
  which were 1.98x on 2026-11-22 and are 1.35x now;
- the packed-ring capture (`PackedQueue::capture`; `vesper/src/packed.ves`),
  which already ran Vesper in every build and kept its Rust under
  `vesper-port`;
- the seccomp evaluator (`seccomp::evaluate`; `vesper/src/seccomp.ves`).

`python3 scripts/ci/flip_gate.py vesper-aether` (release,
`-C target-cpu=native`, 4-core cloud host):

```text
flip-gate vesper-aether: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 2/2 pass | differential 0 differences | ABBA vesper/rust virtqueue 1.27x [1.19x, 1.34x] n=30; hotset 1.35x [1.29x, 1.38x] n=30; packed 1.47x [1.40x, 1.50x] n=30; seccomp_evaluate 1.24x [1.17x, 1.37x] n=30 | RUSTFLAGS=-C target-cpu=native
```

The packed capture's upper bound sits on the bar. The others stay Rust by
default (`cargo test --release -p vesper-aether --features vesper-port
--lib port_abba -- --ignored --nocapture`, and `tests/abba_*.rs`):

| Port | Vesper/Rust | 95% interval | Why |
|---|---|---|---|
| seccomp compile | 1.67x | [1.65, 1.68] | was 2.79x: the output buffer is now sized to the profile, and the program is written straight into it |
| virtio-mmio status | 2.6x | [2.26, 2.79] | one crossing per status write |
| virtio-net TX check + control | 2.4x | [2.32, 2.49] | one crossing per frame |
| virtio-blk span + discard | 3.0x | [2.88, 3.04] | one crossing per request |
| guest memory `locate` | 3.8x | [3.42, 4.00] | one crossing per lookup |
| slot plans | 3.6x | [3.61, 3.71] | G10 |
| PVH parsing | 8.5x | [8.36, 8.60] | G10 and the crossing |
| vsock header codec | 27x | [26.3, 29.1] | was 40x: fixed-width reads and writes instead of bytes; a crossing per 44-byte header |
| vmgenid + fork wire codecs | 28x | [27.9, 28.4] | a crossing per 16- or 24-byte value |

The small codecs and checks cost the Rust caller one Rust-to-Vesper
crossing per call (slice views, a result buffer and its decode), while the
Rust code they replace compiles to a few loads and stores inlined into the
caller. They reach the bar only when their callers are in Vesper too: the
devices (`devices/{block,net,vsock,vmgenid}.rs`) and the transport's
register file as whole units, not their helpers.

## Part 2g: the whole vsock device flips to Vesper

Part 2f measured the vsock header codec at 27x: one crossing per 44-byte
header. Ported as a whole device, the same crossing pays for a whole
packet's worth of work, and the device passes the bar.

`vesper/src/vsock.ves` now holds the device state as one Vesper value
(`VsockState`):

- the listeners;
- the connection table (sorted keys over stable slots);
- credit and the seqpacket message counts;
- the connection state machine;
- the outbox (headers plus one payload byte FIFO);
- the event queue and the stats.

`src/devices/vsock.rs` keeps the public API, the virtio queue loops, and the
bytes each connection received until the host takes them, stored by slot.

Each event costs one crossing:

- a guest transmit packet: `vsock_guest_packet` validates it, runs the state
  machine and says what to do with its payload;
- a receive buffer: `vsock_rx_fill` writes the header and as much payload as
  fits;
- a host socket call: one call each.

The Rust device moved to `src/devices/vsock/reference.rs` and exists only
under `#[cfg(test)]`.

| Check | Result |
|---|---|
| Differential (`port_reference_tests::vsock`): 300 seeds x 400 events of guest packets (valid, malformed, spoofed, oversized, wrong type), host connect/send/recv/message/shutdown/listen calls, receive steps at random buffer sizes, transport resets and device resets | 0 diffs in results, received bytes, stats, connection tables and outbox length |
| Device tests (`tests/devices.rs`, `device_features.rs`, `vsock_port.rs`) on the Vesper device | pass, with and without `vesper-port` |
| ABBA `vsock_device`: 64 stream connections, 16 rounds of 512-byte guest packets, host reads with credit updates, 1 KiB replies, receive drain | 0.94x, 95% CI [0.92, 0.96] |

The header codec (`VsockHeader::encode`/`decode`) is still Rust by default.
It is now only a host convenience: the device itself decodes and encodes in
Vesper.

## Part 2h: the net and block devices and the MMIO register file flip to Vesper

Part 2f measured the net and block helpers at 2.4x and 3.0x and the MMIO
status machine at 2.6x: each was one crossing for a few comparisons. Ported
the vsock way (part 2g), each is now one Vesper value that owns the
decisions, state and counters, and one crossing pays for a whole request,
frame or register access.

- `vesper/src/net.ves` (`NetState`): MAC, MTU, link state, negotiated
  features, queue pairs, per-pair receive backlog counts, the control queue
  (`VIRTIO_NET_CTRL_MQ`), transmit validation, the receive plan (single or
  merged buffers) and the stats. `src/devices/net.rs` keeps the frame bytes,
  the backlog frames and the queue loops.
- `vesper/src/block.ves` (`BlkState`): every request type (IN, OUT, FLUSH,
  GET_ID, DISCARD, WRITE_ZEROES and unsupported ones) runs in Vesper over
  the disk and durable bytes, which Rust lends as slices; read-only, MQ and
  the per-queue counters too. `src/devices/block.rs` keeps the disk buffers
  and the queue loop.
- `vesper/src/mmio.ves` (`MmioState`): the whole register file (every
  register's decode, selectors, feature negotiation, the status machine,
  per-queue registers, interrupt status, configuration generation and the
  transport counters). `src/devices/mmio.rs` keeps the device model, the
  live rings and guest memory, and acts on what the register file answers
  (run a queue, build a ring, reset or activate the device).

The Rust devices and transport moved to `src/devices/{net,block,mmio}/reference.rs`
and exist only under `#[cfg(test)]`.

One deliberate difference: the net device counts a received frame as
delivered when Vesper plans it, not after the ring write succeeds. The two
differ only on a failed completion, where the device then needs a reset
anyway.

| Check | Result |
|---|---|
| Differential `port_reference_tests::net`: 200 seeds x 300 events (frame injection, receive posts, transmits, control commands, link changes, polls; random pairs, MRG_RXBUF/MQ/CTRL_VQ, split and packed) | 0 diffs in used entries, bytes, stats, backlog and config |
| Differential `port_reference_tests::block`: 200 seeds x 200 requests of every type, valid and malformed, read-only and MQ, split and packed | 0 diffs in statuses, data, disk and durable bytes, stats |
| Differential `port_reference_tests::mmio`: 200 seeds x 300 steps of driver sessions (random features, queue sizes, block traffic, `NOTIFICATION_DATA`, malformed chains) mixed with register reads and writes at any offset and width, plus 50 runs of net link changes and config-space accesses | 0 diffs in results, values, status, counters, queue stats, disk and RAM |
| Device tests (`tests/*`) | pass, with and without `vesper-port` |
| ABBA `net_device`: 4 pairs, rounds of 12 received 1514-byte frames and 12 transmits | 0.90x, 95% CI [0.875, 0.924] |
| ABBA `block_device`: 4 KiB reads and writes on a 2048-sector disk | 0.93x, 95% CI [0.893, 0.973] |
| ABBA `mmio_transport`: driver session over the block device, 32 rounds of four reads, notify, ISR acknowledge, drain | 0.99x, 95% CI [0.972, 1.004] |

The net and block ABBAs go through the real MMIO transport, identical in
both arms; the mmio ABBA uses the same Vesper block device in both arms.

Port ABI gap found on the way: a `Slice[u16]` passed from Rust reads 0 past
its first element in Vesper, so the register file takes the queue sizes as
little-endian bytes.

## Compiler gaps (for the coordinator)

| Gap | Blocks | Owner of the fix |
|---|---|---|
| G1 variadics in `.vsys` | macOS `fcntl`/`ioctl`/`open` | lane 13 (`vesper-ffi`) |
| G2 address of a Vesper buffer | nothing (workaround: mapped staging buffers) | none needed |
| G3 in-place views of mappings | AF_XDP zero copy, perf AUX in place | manuscript §16.3 + `vesper-ffi` |
| G4 CPU instruction intrinsics | closed (#1443, #1473): aether's RNG uses them (part 2e); RDPMC for bench-pmu next | done |
| G5 C function pointers / callbacks | threads, signal handlers, `dlsym` | manuscript §16.3 + compiler |
| G6 atomics on foreign memory | macOS rings (none here) | compiler builtins |
| G8 effectful workers | aether's vCPU and uffd threads (lazy guest memory needs a concurrent fault resolver) | manuscript §15 + compiler |
| G10 `Option` results kept in memory and retested; heavy spills in loops | the 1.5x bar for umem (2.1x), aether slot plans (4.0x), PVH parsing (10x) and the packed virtqueue (1.57x), likely every branchy port | compiler codegen (native backend) |

## Census rows

`vesper-aether` stays "c-api" (KVM ioctls and userfaultfd are its Rust
wrappers over the OS). Its virtqueue walks are Vesper-only since part 2d,
hot sets, the packed capture and the seccomp evaluator since part 2f, and
the vsock device since part 2g, and the net and block devices and the MMIO
register file since part 2h;
slot plans and PVH parsing wait on G10, and the small device codecs on
porting their callers.

## Part 7a: Chrono links its Vesper package; the CoreSight formatter flips

The paused agents had ported every Chrono module to `@vesper/chrono`
(`crates/vesper-chrono/vesper/`, parts 1-6, #1728-#1766), checked only by
the root interpreter test (`tests/chrono_port.rs`). Nothing linked it. This
part does the hookup with the seed pattern, since the compiler links
Chrono (`vesper top`, `vesper replay`, the anvil simulator):

- `vesper-chrono` gets a build script (`vesper-port-harness`) that compiles
  the package natively and checks `port/binding.rs`; Bazel builds the object
  from `//crates/vesper-chrono/vesper:port_package`.
- `crates/vesper-chrono/seed` (package `vesper-chrono-seed`, library
  `vesper_chrono`) builds the same sources with `cfg(vesper_port)` off. The
  root crate and `vesper-tools` depend on it; every other crate gets the
  port-backed package.
- Flipped modules forward to Vesper under `cfg(vesper_port)`. Their Rust
  stays for the seed and as the in-crate differential reference
  (`src/port_differential.rs`).

The first flip is `formatter::deformat`. Its export is a new boundary
function, `deformat_into`, which writes the trace bytes, one run word
(`id | start << 8`) per run of one source, and the counters into
caller buffers, so the Rust side appends whole runs instead of one byte at
a time. A frame without ID changes under an open run takes a word-at-a-time
path: it is two 8-byte loads, the auxiliary bits spread into the
flag-carrying bytes, and two 8-byte stores.

- Differential: 20,000 generated captures in both modes, with ID changes,
  null, flush, trigger and reserved IDs, random auxiliary bits, truncated
  and extended tails, bit flips and small output budgets. 0 differences
  from the Rust deformatter.
- Divergence: `formatter` (the trigger ID counted as a trace source) fails
  `coresight_flow::hand_built_frame_exercises_every_slot_rule`.
- ABBA (`flip-gate.toml`, release, `RUSTFLAGS="-C target-cpu=native"`, 30
  epochs): a 64 KiB ETR buffer of two interleaved sources with padding and
  a source switch every four frames or so, **1.03x [1.02, 1.04]**. With an
  ID change on one flag-carrying byte in eight (about one per frame, so the
  word path rarely applies), it is 1.46x [1.44, 1.47]. The byte-at-a-time
  path is slower than Rust's because of G10 (constants and locals spilled
  to the stack).

The other ported modules (the PT and ETE packet decoders, ring, timeline,
lineage, redact, bundle, replay, arena, checkpoint) are next. Each needs an
export-shaped entry point like `deformat_into`, because the existing
functions return `ChunkSeq` trees written for the interpreter test. Capture
(`perf_event_open`), the write-tracking arena (fiber-abi) and the x86 flow
walk (iced-x86, G9) stay Rust.
