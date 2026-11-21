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

## Part 2b: `vesper-aether` slot plans in Vesper (no flip yet)

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

## Compiler gaps (for the coordinator)

| Gap | Blocks | Owner of the fix |
|---|---|---|
| G1 variadics in `.vsys` | macOS `fcntl`/`ioctl`/`open` | lane 13 (`vesper-ffi`) |
| G2 address of a Vesper buffer | nothing (workaround: mapped staging buffers) | none needed |
| G3 in-place views of mappings | AF_XDP zero copy, perf AUX in place | manuscript §16.3 + `vesper-ffi` |
| G4 CPU instruction intrinsics | RDPMC, RDRAND/RNDR | compiler codegen (SH4) |
| G5 C function pointers / callbacks | threads, signal handlers, `dlsym` | manuscript §16.3 + compiler |
| G6 atomics on foreign memory | macOS rings (none here) | compiler builtins |
| G8 effectful workers | aether's vCPU and uffd threads (lazy guest memory needs a concurrent fault resolver) | manuscript §15 + compiler |
| G10 `Option` results kept in memory and retested; heavy spills in loops | the 1.5x bar for umem (2.1x) and aether slot plans (4.0x), likely every branchy port | compiler codegen (native backend) |

## Census rows

No crate has flipped yet; the census rows stay "unported" until the first
flip PR.
