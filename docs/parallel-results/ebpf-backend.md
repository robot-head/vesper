# Wave seven lane 26: compiler eBPF target (XDP and tc from Vesper source)

Claim: #349. Base: `origin/main` 283c811 (descendant of the required
baseline c025b9a). Spec sections §27 and §7 of `docs/specification.txt`.

## What was built

`vesper build --target bpf <file.ves> [-o out.o] [--entry name]` compiles a
restricted, checked Vesper source subset into a verified, loadable BPF ELF
object:

1. **Lowering** (`src/ebpf_backend/lower.rs`): the checked Place IR
   (`vesper::ir::lower_world`) is lowered to `vesper_ebpf::Asm`. Every IR
   value and local lives in an 8-byte stack slot (`Option` takes two: tag,
   payload); values used only inside their defining block share a per-block
   slot pool; constants and field-less enum constructors are immediates.
   Reachable source functions become BPF-to-BPF subprograms (context in
   `r1`, up to four scalar parameters in `r2`-`r5`, result in `r0`; an
   `Option` result of at most 32 bits is packed as `tag << 32 | payload`).
2. **Source surface** (`src/ebpf_backend/surface.rs`), with no new syntax:
   * the entry takes no parameters and returns `XdpAction` (XDP) or
     `TcAction` (tc classifier, `BPF_PROG_TYPE_SCHED_CLS`), declared with the
     kernel's variants in return-code order (checked, `ebpf.action`);
   * `effect Packet { load_u8, load_u16be, load_u32be }`: bounds-checked
     packet views returning `Option`. Each lowers to the verifier-canonical
     §27.1 item 2 sequence `r2 = data; r2 += off; r4 = r2 + size;
     if r4 > data_end goto none` (a variable offset is first bounded by
     `MAX_PACKET_OFF`), so out-of-range reads are `None`;
   * `effect Bpf { ktime_get_ns, smp_processor_id, prandom_u32 }`: helpers,
     checked against the program type's helper set;
   * any other effect built from `lookup/update/delete/add` is a BPF map;
     its kind and size come from a constant `fn <snake>_map() -> BpfMapSpec`
     (Hash, Array, PerCpuHash, PerCpuArray, LruHash). `add` is an atomic add
     on the looked-up value, inserting the delta on a hash miss.
3. **Subset rules**, each a typed `EbpfBackendError::Unsupported` with a
   stable rule id (`vesper::ebpf_backend::rules`): `ebpf.entry`,
   `ebpf.action`, `ebpf.type`, `ebpf.effect`, `ebpf.map`, `ebpf.recursion`
   (anything but tail self-recursion, including mutual recursion),
   `ebpf.operation` (closures, handlers, indirect calls, tuples, structs at
   run time, indexing, signed division), `ebpf.fallible` (any operation the
   checker could not prove total, i.e. that may raise `Fail`), `ebpf.call`
   (inout, Option or more than four scalar parameters), `ebpf.stack`,
   `ebpf.generic`. Tail self-recursion becomes an open-coded loop (store the
   arguments into the parameter slots, jump back); it is accepted only if
   the verifier model proves it terminates (§27.1 item 3; unbounded loops
   fail with the model's `loop.unbounded`/complexity rule).
4. **Stack**: each frame and the worst BPF-to-BPF call chain (frames rounded
   to 32 bytes, as the kernel does) must fit 512 bytes, checked before the
   verifier and again by the verifier model's call-graph report.
5. **Verification and emission**: every compiled program goes through
   `vesper_ebpf::verify` before it is returned; the object is written by
   `vesper_ebpf::elf::write_object` and always carries `.BTF`, `.BTF.ext`
   (func info per subprogram and line info for every source line, with the
   source text) and BTF-defined `.maps`.

Public API (`vesper::ebpf_backend`): `compile_source`, `compile_world`,
`Options { entry, file, policy }`, `Compiled { program, report, maps,
action, frames, loops }` with `object()`, `map_index()`, `prog_type()`,
`EbpfBackendError` with `rule()`, `rules::*`, `ActionKind`, `MapBinding`,
and `build_command` used by the CLI.

## Examples and evidence

Fixtures in `tests/fixtures/ebpf_backend/` (canonically formatted):

| Program | Type | Insns | Model insns processed | Worst stack | Maps |
| --- | --- | --- | --- | --- | --- |
| `firewall.ves`: learning firewall, blocks a source after a telnet attempt, counts drops | XDP | 265 | 371 | 256 B | Hash u32->u8, LruHash u32->u64 |
| `counter.ves`: per-CPU protocol counters, IPv4 header checksum by a bounded loop | XDP | 299 | 1088 | 256 B | PerCpuArray u32->u64 |
| `tc_guard.ves`: last-seen time per source, shoots outside DNS | tc | 194 | 275 | 224 B | Hash u32->u64 |

Kernel evidence on this cloud host (Linux 6.18.44, x86-64, root, `bpf()`
permitted): the kernel verifier accepts all three programs, both plain and
with their program BTF, func info and line info (`kernel::load_with_ext`),
and `BPF_PROG_TEST_RUN` gives the same verdict as the `vesper-ebpf`
interpreter on every packet of each trace, with the same map contents
afterwards (drop counts, blocklist entries, per-CPU counter sums, last-seen
entries). Where `bpf()` is not permitted the kernel tests print
"unavailable" and return; they never claim success.

The interpreter models only `struct xdp_md`. For the tc program the test
rewrites the two context loads (`__sk_buff` offsets 76/80 to 0/4) before
interpreting; the verifier model and the kernel see the real tc program.

## Checks run

* `cargo test --test ebpf_backend`: 12 tests pass (verifier model,
  interpreter verdicts and maps for all three programs, kernel load with and
  without BTF, kernel test runs matching the interpreter, ELF round trip
  through `elf::read_object` with `.BTF`/`.BTF.ext`/`.maps`, 18 out-of-subset
  programs with their rule ids, an unbounded loop rejected by the model, a
  560-byte frame rejected, the CLI writing an object that `vesper ebpf check`
  accepts, and a no-panic sweep over truncated and edited sources).
* `cargo fmt --all -- --check`, `vesper fmt --check tests/fixtures/ebpf_backend/`.
* `cargo clippy -p vesper --all-targets --locked -- -D warnings` on Rust
  1.97 and `cargo +1.100.0 clippy` (same flags): clean.
* `cargo test --workspace --locked`: see the PR for the result on the final
  head.

## Decisions

* **No new syntax.** Program type, maps and helpers are recognised from
  ordinary declarations (action enums, effects, a constant spec function),
  because attribute syntax belongs to the §16.4 macro work and
  `crates/vesper-syntax` is not this lane's. `@xdp_prog(...)`-style
  attributes can later desugar to the same surface.
* **Source has no loop construct**, so "bounded loops" are tail
  self-recursion lowered to a back edge, and boundedness is the verifier
  model's proof (and the kernel's), not a syntactic `decreases` check.
* **Stack slots for every value** instead of register allocation: simple
  and verifier-friendly (8-byte spills keep scalar bounds), at the cost of
  code size. The per-block pool keeps frames small enough for the examples.
* **Packet views return `Option`** rather than a typed `wire struct` view:
  the language has no `wire struct` or 16-bit integer type yet, so
  `load_u16be` returns `u32`.
* **Fallible arithmetic is rejected**, not lowered to `XDP_ABORTED`:
  `wrapping_*` and checker-proved operations are the subset, so no hidden
  abort path changes a program's verdicts.
* `crates/vesper-ebpf` is used read-only (lane 17 owns it); the ELF writer
  was not touched.

## Root patch

No dependency or workspace changes (`vesper-ebpf` was already a root
dependency). Shared-file hunks, each additive: `pub mod ebpf_backend;` in
`src/lib.rs` (with the backend commit) and the `build --target bpf` routing
hunk in `src/main.rs` (its own commit).

## Remaining gaps

* Not implemented: `wire struct` packet views, `Packet::len` (`xdp_md` has no length field, and
  `data_end - data` is arithmetic on `PTR_TO_PACKET_END`, which the kernel
  verifier refuses), packet writes, `XDP_TX`/redirect helpers, ring buffers and the
  linear `BpfRingSlot`/`BpfSocketRef` protocol (excluded by the owner;
  PR #165), struct values at run time, signed division, `bpf_loop`
  lowering, tracepoint/kprobe entries, and kernel-side map pre-population
  (the interpreter and kernel modules expose lookup only, so the examples
  fill their maps themselves).
* The checks are of the emitted program and of the source subset. The
  backend does not implement the Vesper source typing rules for linear
  kernel references (Theorem 2, [T-Let]); Rust ownership in the compiler is
  not evidence of them.
* No Lean proof: §27.4's firewall theorem is not part of this lane.
* No performance was measured.
