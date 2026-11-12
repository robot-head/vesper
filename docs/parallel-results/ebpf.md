# Wave five lane 4: Vesper eBPF pre-flight verifier and XDP programs

Design coverage: §27.1 items 2, 3 and 5, §27.2 wire structs, §27.3 XDP
SYN-flood mitigator and user-space ring consumer, §27.4 firewall proof,
§27.5 pre-flight abstract verifier. Issue #124.

- Base: `476f18c` (origin/main, contains `af03122`).
- Branch: `claude/project-thread-kpjb4h`. It continues the unreviewed
  work-in-progress commit `887aaf3` from `claude/project-thread-hcn7d6`
  (codec, scalar domain, verifier draft, interpreter, wire structs), which
  was untested; this lane tested it, fixed what the tests found and added
  everything else.

## What was built

New crate `crates/vesper-ebpf` (depends only on `object` and, on Linux,
`libc`; no internal dependencies) and `lean/EbpfFirewall.lean`.

| Module | Contents |
| --- | --- |
| `insn` | Decoder/encoder for every ALU/ALU64 op (incl. ISA v4 `sdiv`/`smod`/`movsx`, `bswap`), JMP/JMP32 incl. `gotol`, LDX/ST/STX incl. `memsx`, every `BPF_ATOMIC` op, `LD_IMM64` with `BPF_PSEUDO_MAP_FD`, helper/BPF-to-BPF/kfunc calls, `EXIT`. Malformed slots are `DecodeError`s; `disassemble` for diagnostics. |
| `program` | `Asm` two-pass assembler (labels, map references, local calls), `Program`, `MapDef`/`MapKind` (hash, array, per-CPU hash/array, LRU hash, ringbuf) with the kernel's map size rules. |
| `scalar` | Tristate numbers (after `kernel/bpf/tnum.c`) reduced with unsigned and signed 64-bit intervals. |
| `verifier` | The §27.5 abstract interpreter; `verify` returns `VerifyReport` or a `VerifyError { insn, rule, message }`; `findings` returns the contract's `Vec<VerifierFinding>`. |
| `callgraph` | Contract-shaped `CallGraph { functions: Vec<FnNode>, calls: Vec<(FnId, FnId)> }` with `FnId(u32)`, per-function frame bytes, indirect edges rejected; worst-case stack over all call paths. |
| `interp` | Concrete XDP interpreter with provenance on every value: any access outside the region a pointer came from, uninitialised read, use after release or reference still held at exit is an `InterpError`. Kernel ring-buffer byte layout and `RingBufReader`. |
| `elf` | BPF ELF writer and reader (`object` 0.37.3): `xdp`, `.relxdp` (`R_BPF_64_64`), `license`, BTF-defined `.maps` and a minimal `.BTF`. |
| `mitigator` | The §27.3 program, built with `Asm`, plus `reference_decision`. |
| `wire` | `EthIpv4TcpWire` (54 bytes), `SynDropAlertWire` (16 bytes), `XdpAction`. |
| `kernel` (Linux, the only `unsafe` module) | Raw `bpf(2)`: map create, program load (log collected on rejection), `BPF_PROG_TEST_RUN`, map lookup, `BPF_BTF_LOAD`, ring buffer `mmap`, CPU pinning. `probe()` reports `Unavailable` when the host refuses. |

### The verifier

Per-register types: scalar, `ctx` (`struct xdp_md`), packet pointer (id,
constant offset, variable offset), `pkt_end`, stack pointer (frame,
offset), map pointer, map value, map value or null, ring-buffer record
(acquired reference), socket (acquired reference), and the or-null forms.
Stack: 64 slots per frame with per-byte initialisation and 8-byte spills.
Packet accesses need a range established by comparing a pointer of the
same id against `data_end`. References from `bpf_ringbuf_reserve` and
`bpf_sk_lookup_tcp` must be submitted, discarded or released on every path
before the program (or the subprogram that acquired them) exits. Paths are
explored depth-first with pruning against fully explored states
(`state_includes`); an in-progress ancestor reached again with an equal
state is `loop.unbounded`, and a loop without a usable bound runs into the
1M processed-instruction limit (`complexity.limit`). BPF-to-BPF calls:
at most 8 frames, no recursion, 512 bytes combined stack over the call
graph (frames rounded to 32 bytes as the kernel does). Supported helpers:
`map_lookup_elem`, `map_update_elem`, `map_delete_elem`, `ktime_get_ns`,
`get_prandom_u32`, `get_smp_processor_id`, `xdp_adjust_head`,
`sk_lookup_tcp`, `sk_release`, `ringbuf_output`, `ringbuf_reserve`,
`ringbuf_submit`, `ringbuf_discard`. Every other helper, kfuncs,
`LD_ABS`/`LD_IND`, pseudo `LD_IMM64` forms other than map fd, and program
types other than XDP are rejections.

54 rule ids; `tests/verifier_rules.rs` checks that every rule that real
input can reach has a corpus program rejected with exactly that rule (three
are defensive and unreachable: `prog.unsupported_type`, `call.graph`,
`internal.no_successor`).

### Deliberate differences from the kernel

The direction that matters is: a program accepted here is accepted by the
kernel and cannot fault. This verifier is stricter than a privileged
kernel in these places (all checked against the kernel in
`kernel_agrees_on_the_rule_corpus`):

- variable-offset stack pointers, pointer comparisons other than packet vs
  `pkt_end` and null checks, partial reads of spilled pointers,
  misaligned stack access;
- pointer leaks that `CAP_PERFMON` allows: storing a pointer into a map,
  returning a pointer, an atomic whose operand is a pointer;
- reads of uninitialised stack (allowed with `CAP_PERFMON`);
- `bpf_sock` field access;
- adding a variable that may exceed `0xffff` to a packet pointer (the
  kernel allows the arithmetic and rejects a later access);
- helpers outside the list above.

To keep accepts kernel-compatible, it is also *less* precise than it could
be exactly where the kernel is: division and modulo results, and shifts by
a non-constant or out-of-range amount, are fully unknown 64-bit values
(even for 32-bit ops), a fresh `ctx->data` load has no range, and no range
is established for a packet pointer whose variable part plus offset may
exceed `0xffff` (`find_good_pkt_pointers`). The kernel differential found
each of these; the first three were cases where this crate was correctly
more precise than the kernel, which would have made a pre-flight accept a
kernel rejection.

## Bugs found and fixed in the inherited draft

- Unsound pruning: ranges established for `ctx->data` survived in the state
  after no register held the pointer, so a state with a smaller range was
  pruned against one with a larger range and a later reload of `data`
  inherited the larger range. Found by `tests/soundness.rs` (an accepted
  program read past the packet in the interpreter). Fixed twice over: a
  fresh `ctx->data` load gets a fresh id with no range (the kernel's
  behaviour), and pruning also compares the range of id 0.
- Atomics: no alignment check on map values (kernel: always strict), no
  operand-size check, and an atomic on a spilled pointer was allowed.
- The precision mismatches above.
- Interpreter: releasing transient regions was quadratic in the number of
  runs on one environment.

## Item 3 (linear `BpfRingSlot`/`BpfSocketRef` builders): not delivered

The previous thread's attempt to write this module was stopped by an
automated safety filter, and the project owner then said to skip the
builder and finish the rest. This lane did not attempt it again. The
mitigator is assembled with `Asm`, and the release obligation is enforced
on the emitted bytecode by the verifier's `ref.leak` rule (rejection
corpus: `reject/ringbuf_leak`, `reject/socket_leak`; random programs that
forget a release are rejected with `ref.leak`). This is not a Rust-typed
linear API and not an implementation of the Vesper source typing rules
(Theorem 2, [T-Let]).

## The mitigator (§27.3)

Per-CPU hash `syn_counts` (u32 source address in network order → u64
count) and ringbuf `syn_alerts`. Short (< 54 bytes), non-IPv4 and non-TCP
packets pass; a packet passes unless its flags (`data_offset_flags & 0x3f`)
have SYN set and ACK clear; otherwise the source's count is incremented (or
inserted as 1) and the packet drops once the count exceeds the threshold,
with a `SynDropAlertWire` reserved, filled and submitted when the ring has
room (a full ring still drops). 60 instructions, 16 bytes of stack (32
after kernel rounding), 68 instructions processed by this verifier.

Corrections to the design text: the threshold is a build-time constant
(`MitigatorConfig::pps_threshold`) baked in as a 64-bit immediate because
an XDP program receives only its context; `percpu_increment_u64` is
open-coded as lookup / in-place add / insert; if the counter map is full a
new source cannot be inserted and passes (fail open); counts are never
reset by the program. Wire structs match their stated sizes (54 and 16
bytes), so no layout correction was needed.

## Kernel evidence (this cloud host: Linux 6.18, root, `CAP_BPF`)

- The kernel verifier accepts the mitigator; `BPF_PROG_TEST_RUN` over 200
  fixture packets agrees with the interpreter on every return value and
  output packet, the per-CPU counters (summed over CPUs) equal the
  interpreter's, and the alerts read from the kernel's `mmap`ed ring
  buffer with `RingBufReader` equal the interpreter's alerts.
- `kernel_accepts_what_we_accept_and_executes_identically`: random
  programs from `tests/common`; every program accepted here is loaded and
  must be accepted by the kernel, then run on 8 packets in both. Default
  1,500 programs; run here with 20,000 programs for each of three seeds
  (`VESPER_EBPF_DIFF_SEED=1,2,3 VESPER_EBPF_DIFF_PROGRAMS=20000`): 9,283
  accepted by both, 0 accepted here and rejected by the kernel, 8,060
  accepted only by the kernel, 74,264 identical test runs.
- `kernel_agrees_on_the_rule_corpus`: each corpus program's kernel verdict
  is as recorded in `tests/common/corpus.rs` (accept, reject, or
  "accepts but stricter here").
- The kernel validates the emitted `.BTF` (`BPF_BTF_LOAD`) and loads the
  program read back from the emitted ELF.

On hosts without `bpf()` access (CI runners, macOS, Windows) these tests
print that the evidence is unavailable and pass; checked here by running
the test binary as an unprivileged user.

## ELF

`aya-obj` 0.3.0 (dev-dependency, the aya project's object parser) parses
the mitigator object: license, both BTF-defined maps with their types and
sizes, the `xdp` program, and `relocate_maps` rewrites all three map loads
to the given fds. No `.BTF.ext` (line info, CO-RE): `@btf_core` is out of
scope. libbpf itself was not tried (not installed here).

## Lean (§27.4)

`lean/EbpfFirewall.lean`, checked with Lean 4.24.0 (`lean
EbpfFirewall.lean`), no `sorry`/`admit`/axioms beyond `propext` and
`Quot.sound`. It models the filter over a packet as a byte list and the
counters as a function; packet bytes are read only through an accessor
that demands a proof the index is in bounds, discharged from the same
54-byte guard. Proved about the model: `reads_within_checked_bounds`,
`short_packets_pass`, `filter_action`, `drop_iff_spec` (drop exactly when
IPv4 ∧ TCP ∧ SYN ∧ ¬ACK ∧ count after the packet > threshold),
`syn_mitigator_never_drops_ack_packets` (the §27.4 theorem),
`only_syn_only_packets_are_counted`, `count_increments_by_one`. Counts are
naturals (the program's u64 would wrap after 2^64 SYNs from one source).
These are theorems about the model, not about the Rust code or the
bytecode; `tests/lean_model.rs` compiles the file under `VESPER_LEAN`,
audits the axiom report and compares the model's fixture table (15
packets, threshold 2) with the real program in the interpreter.

## Tests

| File | What |
| --- | --- |
| `codec.rs` | every instruction family round-trips; 500k random slot sequences: every decodable one re-encodes to the same bytes, none panic |
| `scalar_soundness.rs` | ALU transfer functions, truncation/extension and branch refinement contain every concrete result (800k random cases) |
| `verifier_rules.rs` | accepted corpus programs; one rejection per rule with the exact rule id; contract-shaped findings |
| `soundness.rs` | 4,000 random programs (40,000 × 3 seeds run here): every accepted program runs over 12 random packets without an interpreter fault |
| `mitigator.rs` | wire layouts; fixtures (threshold, ACK never dropped, other sources, short/non-IPv4/non-TCP, full ring); 20k random packets against `reference_decision`; kernel run |
| `elf.rs` | round trip of the accepted corpus; aya-obj parse and relocation; BTF parser robustness; kernel BTF and object load |
| `kernel_diff.rs` | the two kernel differentials above |
| `lean_model.rs` | Lean source audit, compile, axiom report, fixture agreement |

Commands run (all passed):

```
cargo fmt --all -- --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
VESPER_LEAN=lean cargo test -p vesper-ebpf --locked
cargo test -p vesper-ebpf --release --test kernel_diff   (with the seeds above)
cargo test -p vesper-ebpf --release --test soundness     (VESPER_EBPF_FUZZ_SEED=11,12,13, 40000 programs)
```

## Root patch

Separate commit: `crates/vesper-ebpf` in root `members` and
`default-members` (before `experiments/native-pulse`), `Cargo.lock`
regenerated (adds `aya-obj` 0.3.0 and its dependencies for tests). No
`[workspace.dependencies]` changes.

## Remaining gaps

- Item 3 builders (see above).
- No compiler eBPF backend from Vesper source, no `@xdp_prog` lowering, no
  `vesper ebpf check` CLI (integration B, #136), no merge with the lane-3
  Cranelift stack analyzer (integration B).
- Program types other than XDP (tc, tracepoint, LSM), tail calls, kfuncs,
  `bpf_loop`, BTF CO-RE, `.BTF.ext`.
- The verifier is path-sensitive but has no precision tracking or scalar
  id linking, so it accepts fewer programs than the kernel (the
  differential's "kernel only" column).
- Performance numbers: none claimed or measured.
