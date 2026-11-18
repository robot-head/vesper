# Port wave P3a lane 6: vesper-ebpf (#1041)

Status: **part 4 of 5** (this PR; part 1 is #1048, merged as `5ff0c7a`;
part 2 is #1056, merged as `f0a64be`; part 3 is #1061, merged as `322ca4a`). Part 1 base
`3c075a4`, part 2 base `5ff0c7a`, part 3 base `f0a64be`, part 4 base `322ca4a`.

The crate is 12.4k lines with 82 tests in 15 files, so the port lands in
parts, as Crucible's did (four were planned; the interpreter took a part of
its own):

1. **Part 1 (#1048):** the seed package, the instruction codec (`insn.rs`), the
   wire structs (`wire.rs`), the tnum and interval domain (`scalar.rs`) with
   the verifier's domain functions (`scalar_alu`, `refine_jmp`, `sext_imm`),
   the concrete ALU, jump and byte-order semantics of `interp.rs`, the helper
   tables (`helpers.rs`), the map and program kinds and `MapDef::validate`
   (`program.rs`) and the stack bound (`callgraph.rs`).
2. **Part 2 (#1056):** the verifier (`verifier.rs`), see
   [Part 2: the verifier](#part-2-the-verifier).
3. **Part 3 (#1061):** BTF parsing, BTF.ext and CO-RE (`btf.rs`,
   `btf_ext.rs`, `core.rs`), see [Part 3](#part-3-btf-btfext-and-co-re).
4. **Part 4 (this PR):** the interpreter (`interp.rs`), see
   [Part 4](#part-4-the-interpreter).
5. The mitigator and the builders and assembler (`mitigator.rs`,
   `builder.rs`, the `Asm` half of `program.rs`) and the BTF and `.BTF.ext`
   writers.

Rust stays the default build, nothing is deleted and no dependent is
switched (the owner's standing rule). `elf.rs` (the `object` crate) and
`kernel.rs` (`bpf()`, `/sys` reads, `unsafe`) stay Rust for good.

## Packages (seed-copy rule)

- `crates/vesper-ebpf/seed` is the package `vesper-ebpf-seed`, the Rust
  build of `../src` under the library name `vesper_ebpf`, with no build
  script. The root depends on it by rename (`vesper-ebpf = { path =
  "crates/vesper-ebpf/seed", package = "vesper-ebpf-seed" }`); the root is
  the only crate in the compiler closure that uses eBPF.
- `crates/vesper-ebpf` builds the Rust code by default. Under its
  `vesper-port` feature, `build.rs` builds the Loom package `@vesper/ebpf`
  in `crates/vesper-ebpf/vesper/` with `vesper_port_harness::Port`, checks
  `port/binding.rs` and sets `cfg(vesper_port)`; each ported Rust item is
  `#[cfg(not(vesper_port))]`, next to a `#[cfg(vesper_port)]` item with the
  same signature that forwards to the port. The Rust bodies are unchanged.
  `IMPLEMENTATION` says which build is linked.
- The seed repeats no tests: the default build of `vesper-ebpf` is the same
  Rust code, and `tests/lean_model.rs` finds `lean/` relative to the
  package directory.
- The closure guard (`vesper-port-harness` `tests/closure_guard.rs`) passes.

## What was ported

| Rust | Vesper module | Public API now forwarding to the port |
| --- | --- | --- |
| `insn.rs` | `insn.ves` | `decode`, `decode_all`, `encode`, `slots_to_bytes`, `slots_from_bytes` |
| `scalar.rs` | `scalar.ves`, `common.ves` | `Tnum::{range, contains, includes, add, sub, and, or, xor, lsh, rsh, arsh, intersect, truncate}`, `Scalar::{urange, srange, unknown_bits, contains, includes, normalize, add, sub, mul, and, or, xor, lsh, rsh, arsh, zext32, truncate, sext, hull, meet}`, `sext_const` |
| `verifier.rs` (domain) | `scalar.ves` | `scalar_alu`, `refine_jmp`, `sext_imm` |
| `interp.rs` (semantics) | `concrete.ves` | `alu_concrete`, `cmp_concrete`, `endian` |
| `helpers.rs` | `helpers.ves` | `allowed`, `gpl_compatible` |
| `program.rs` (kinds) | `program.ves` | `MapKind::{kernel_type, from_kernel_type, is_array}`, `ProgramType::{kernel_type, from_kernel_type, has_packet, is_tracing}`, `MapDef::validate` |
| `callgraph.rs` | `callgraph.ves` | `check_stack` |
| `wire.rs` | `wire.ves` | `WireFrame::{encode, decode}` of `EthIpv4TcpWire` and `SynDropAlertWire` |

Every Rust caller of these (the verifier, the interpreter, the builders, the
mitigator, ELF reading) runs the ported code in the port build, so every test
file exercises it, not only the codec and domain tests.

Boundary: the Vesper enums `AluOp`, `JmpOp`, `Endian`, `Size`, `AtomicOp`,
`Imm64`, `CallTarget`, `MapKind`, `ProgramType`, `WireError` and the structs
`RawInsn`, `Tnum`, `Scalar`, `SynDropAlertWire` have the crate's own layout
and names, so the binding converts them directly (`src/port.rs` names them
for it). Four types differ and `src/port.rs` converts them: instructions and
operands carry registers as `u8` (the crate's `Reg` has a private field),
`DecodeError::ReservedFieldSet` names its field by number, and
`EthIpv4TcpWire` holds each MAC address in the low 48 bits of a word. Errors
are `Fail[DecodeError]` and `Fail[WireError]`; `MapDef::validate` returns the
number of the failing rule and the wrapper formats the crate's message;
`check_stack` returns a word list (outcome, bytes or id, path).

Stays Rust in this part: constants and tables (`class`, `pseudo`,
`helpers::SUPPORTED`, `KFUNCS`, `helpers::name`), the trivial accessors and
`const fn`s (`RawInsn::{new, to_bytes, from_bytes, class}`, `Reg`,
`Insn::slots`, `JmpOp::{negate, swap}`, `Size::{bytes, from_bytes}`,
`AtomicOp::fetches`, `Tnum::{konst, is_const, umin, umax}`,
`Scalar::{konst, as_const}`, `MapDef` constructors, `XdpAction::from_u64`,
`Display` impls) and `disassemble`, which formats `Debug` output over the
ported decoder.

No `unsafe` in the port; the Rust side keeps `deny(unsafe_code)` with only
the generated binding allowed (and `kernel.rs`, unchanged).

## Tests

Unit tests in the crate: none. Integration tests: 82 in 15 files, all
unchanged. This part adds `tests/port_surface.rs` (6 tests: every
`MapDef::validate` rule, the kind numbers, the helper table per program type,
the GPL list, every `check_stack` outcome, and `IMPLEMENTATION`), because no
existing test pinned those down, and `tests/abba.rs` (one equality test plus
the ignored benchmarks).

`cargo test -p vesper-ebpf` (Rust) and `cargo test -p vesper-ebpf --features
vesper-port` (port) both pass all 88 tests plus the ABBA equality test
(Linux x86_64, kernel 6.18, with `CAP_BPF`, so `kernel_diff.rs` and the
other kernel tests really load programs). Both builds agree on every verdict
and abstract state the soundness tests print.

CI: the Linux "Vesper ports" job runs `crates/vesper-ebpf/port/check.sh`
(both builds and the divergence checks); Bazel builds the port object and
every divergent object, and runs `codec`, `port_surface`, `scalar_soundness`
and `soundness` against the port. macOS and Windows build and test the Rust
default, as before.

## Divergence checks

One edit per ported module (`EDITS` in `build.rs`, applied one at a time with
`VESPER_EBPF_DIVERGENT`, mirrored in `BUILD.bazel`). `port/check.sh` asserts
that each edit fails the named test while another test of the same file
passes. All twelve were observed failing:

| # | Module | Edit | Caught by |
| --- | --- | --- | --- |
| 0 | insn | `lsh` encodes as `rsh` | `codec::every_family_round_trips` (also `decodable_random_slots_are_canonical`, CO-RE and interpreter tests) |
| 1 | scalar | tnum addition drops the second operand's unknown bits | `scalar_soundness::alu_transfer_functions_contain_concrete_results`, `kernel_diff` |
| 2 | interp | 64-bit arithmetic shift is logical | `scalar_soundness::alu_transfer_functions_contain_concrete_results`, `kernel_diff` |
| 3 | verifier | refining `a > b` claims `a > b + 1` (unsound) | `scalar_soundness::branch_refinement_keeps_every_value_on_its_branch`, `soundness::accepted_random_programs_never_fault`, `kfuncs_widening`, `interp_semantics` |
| 4 | helpers | tc may call `bpf_xdp_adjust_head` | `port_surface::helper_table_matches_the_kernel_protos`, `verifier_rules::corpus_verdicts` |
| 5 | program | a 2048-byte ringbuf is valid | `port_surface::map_definitions_are_validated_rule_by_rule` |
| 6 | callgraph | stack limit plus 8 bytes | `port_surface::stack_bound_reports_every_outcome` |
| 7 | wire | source address read from the destination | `mitigator::wire_layouts_are_exact` |
| 8 | verifier (part 2) | a packet access may end one byte past the checked range (unsound) | `verifier_rules::corpus_verdicts` (and `findings_use_the_contract_shape`), `soundness::accepted_random_programs_never_fault` (both tests), `kernel_diff`, `lean_model::bounds_model_verdicts_match_the_verifiers`, `verifier_port` |
| 9 | btf (part 3) | an unknown kind 20 is read as a pointer | `btf_port::port_matches_rust_on_mutated_btf` |
| 10 | core (part 3) | members are not found through anonymous aggregates | `core_port::port_matches_rust_on_random_relocations` (not caught by `btf_core`) |
| 11 | btf_ext (part 3) | line info keeps 9 bits of the column | `ext_port::port_matches_rust_on_objects_and_mutations` (not caught by `btf_core`) |
| 12 | interp (part 4) | the byte just before `data` is readable | `interp_port::port_matches_rust_on_chaos_programs` |

Edit 3 is the verifier divergence the soundness tests must catch in part 1
(the domain functions); edit 8 is the one in the ported verifier itself, and
the soundness tests catch it too.

## Performance (ABBA)

Setup: cloud VM, Intel Xeon @ 2.30GHz, 4 vCPUs, Linux 6.18; `cargo test
--release -p vesper-ebpf --features vesper-port --test abba -- --ignored
--nocapture --test-threads=1`; `vesper-bench` ABBA, 30 epochs after 2 warm-up
epochs; ratios are Vesper/Rust. Instruction counts: callgrind over one call
of the workload in each build (`callgrind_one_workload`,
`--toggle-collect='*<build>::<workload>*'`). Wall clock on these hosts varies
by up to 30%.

| Workload | Rust per op | Vesper per op | Wall ratio (median, min-max) | Instructions Rust | Instructions Vesper | Instruction ratio |
| --- | --: | --: | --: | --: | --: | --: |
| decode + encode, 4096 slots | 61.6 ns | 149.4 ns | 2.43 (1.78-2.96) | 2,265,034 | 5,710,041 | 2.52 |
| scalar domain, 4096 steps | 92.0 ns | 194.3 ns | 2.10 (2.02-4.33) | 4,555,569 | 10,007,439 | 2.20 |
| concrete ALU and jumps, 4096 | 28.1 ns | 32.0 ns | 1.13 (1.04-1.24) | 288,341 | 764,612 | 2.65 |
| `verify` of the mitigator | 43.6 us | 51.1 us | 1.19 (1.09-1.72) | 466,634 | 592,207 | 1.27 |

The port is about 2.1-2.5x slower than Rust on its own hot paths and 1.2x on
the verifier that calls it (the verifier itself is still Rust). The
concrete-ALU wall ratio is low because both builds wait on the same divides;
the instruction ratio shows the call overhead.

## Compiler gaps

Each with a repro from this port:

1. **Aggregate locals live in stack slots.** `scalar_normalize` keeps its
   `mut s: Scalar` (six words with the nested `Tnum`) in memory and reloads
   each field per use; it is 64% of the port's scalar instructions, about 450
   instructions per call where Rust spends about 150 for the same loop. Repro:
   `vesper/src/scalar.ves` `scalar_normalize`, `objdump -d` of its
   `vesper_function_*`. This is the stack-slot locals gap the BBR and Quarry
   parity lanes reported, now for nested copy structs.
2. **Aggregate results are built in a temporary and copied.** Every
   `Option[Scalar]` return writes the seven words to a stack temporary and
   then copies them to the result pointer (the same function). Struct literal
   temporaries, as reported by the BBR lane.
3. **Boundary conversion per call.** Each domain operation is one exported
   call: the binding writes `Scalar` to words and reads them back
   (`<Scalar as Abi>::put` and the wrappers are 6% of the scalar workload),
   and Rust callers cannot inline it. Export ABI v3 mirrors would lend the
   values in place; the larger fix is porting the verifier (part 2), which
   makes these calls internal to Vesper.
4. **Sequences of enums cross by conversion.** `decode_all` returns
   `ChunkSeq[Insn]` of a payload enum: the binding copies the `RawInsn`
   slice into a packed buffer, then converts each `Insn` (`PortInsn::get`,
   3%) into a fresh `Vec`; malloc and free are 16% of the decode workload.

## Decisions

- Four parts, like Crucible, because the verifier alone is 3.6k lines.
- The seed runs no tests of its own (see above); `cargo test -p vesper-ebpf
  -p vesper-ebpf-seed` still builds it.
- `tests/port_surface.rs` is new; the existing tests are unchanged.

## Part 2: the verifier

`verifier.rs` (3.6k lines) is `vesper/src/verifier.ves` (4.4k lines), and
`verify` forwards to it in the port build. The kprobe `PT_REGS` check and
`MapDef::validate` stay in the Rust wrapper (they run before exploration and
call code that is already ported). `src/port.rs` packs the maps (kernel
type, sizes, name) and kfunc references (slot, kfunc code, name) over one
text pool, calls `verifier_verify`, and rebuilds the `VerifyError` or
`VerifyReport` from the outcome words. Rule names come back as text and are
mapped to the crate's `&'static str` rules (`RULES`, 73 entries).

Layout: a state is two flat sequences instead of `Vec<Frame>`, `Vec<Slot>`
and the reference, packet-range and irq lists. `rs` holds 76 entries per
frame (11 registers, 64 stack slots, the callback argument); `ws` holds an
8-word header, 17 words per frame, then references, packet ranges and irq
ids. Explored states are records in pools with per-instruction linked lists,
pending paths are a work stack of packed copies (the leading empty stack
slots are skipped, as Rust's stack only grows to the deepest slot used), and
a rejection raises `Fail[VErr]` after writing the rule and message.

Exactness: `tests/verifier_port.rs` (new) runs both builds on every corpus
case (as is and retargeted to each of the 4 program types), every depth and
kfunc case, and 400 random programs plus a retargeted copy each, under four
policies (the default limits at a 200,000-instruction budget, the same
without widening, 2,000 instructions with 2 states per instruction, and 300
instructions), and compares the `Debug` output: verdict, rule, instruction,
message and every count of the report. They agree on all of it. A corpus
case that runs out of a large budget is compared retargeted under the small
budgets only (it takes about a second per run in either build). The suite
takes 63 s in release and 153 s in debug. All 88 earlier tests and the ABBA
equality test also pass against the ported verifier.

Memory: before packing pending paths, `accept/iter_num_nested` without
widening at a 1,000,000-instruction budget peaked at 6.7 GB in the port
against 0.84 GB in Rust (the work stack held full 64-slot frames of 88-byte
entries). With packing it peaks at 1.55 GB and runs as fast as Rust (107 s
against 110 s for the case retargeted to every type).

### Performance (part 2)

Same setup as above, `abba_verify` (the mitigator under the default policy,
the whole verifier now ported):

| Workload | Rust per op | Vesper per op | Wall ratio (median, min-max) | Instructions Rust | Instructions Vesper | Instruction ratio |
| --- | --: | --: | --: | --: | --: | --: |
| `verify` of the mitigator | 28.7 us | 132.5 us | 4.74 (3.56-5.47) | 465,001 | 1,428,700 | 3.07 |

The ported verifier is about 3x Rust's instructions and 4.7x its time.
Rust stays the default. The flat 88-byte entry (`R`) for every register and slot is this
port's choice, not a compiler limit; a compact state is the first thing to
try for parity. Where the time goes (callgrind, self cost):
`memcpy` 24% (state copies: a fork copies the whole flat state, 6.7 KB per
frame, and each record stores one, where Rust clones only the stack slots
in use), the exploration loop 16%, state inclusion 11%, malloc and free 5%.

### Compiler gaps (part 2)

5. **`Option[E]` results of `handle` have no native layout** ("native
   layout is not yet defined for _"): the verifier's sub-results return
   `u64` codes instead. Repro: a `handle` block whose result type is
   `Option[VErr]`.
6. **Division, remainder and indexing raise `Fail[RuntimeError]`**, which
   an `eff [Fail[VErr]]` function cannot pass through (E0400), so the port
   uses shifts, masks, a `udiv` helper with its own `handle`, and
   `try_get`/`try_set` with a default everywhere.
7. **Nested reads inside an `inout` call's arguments are refused** (E0301):
   `f(inout st, g(in st))` needs a local first, so `verifier.ves` hoists
   such reads throughout.
8. **No `continue`, `while` or `wrapping_neg`.** Loops are `loop` with
   `if`/`break`; negation is `wrapping_sub(0, x)`.

## Part 3: BTF, BTF.ext and CO-RE

Ported (Vesper module, then what forwards to it in the port build):

| Rust | Vesper | Forwarding |
| --- | --- | --- |
| `btf.rs` reader | `btf.ves` | `Btf::parse`, `Btf::parse_split`: the header, every kind, references and names (with UTF-8 checks) |
| `core.rs` | `core.ves` | `CoreSpec::{local_value_with, candidates, target_value_with}`, `value_over`, `combine`, `patch` |
| `btf_ext.rs` readers | `btf_ext.ves` | `parse_ext`, `local_type`, `maps_from_btf` |

Stays Rust: the BTF and `.BTF.ext` writers (`BtfBuilder`, `btf_ext::build`,
`local_root_ids`, `assign_local_ids`, which part 4 takes with the other
builders), host file reads (`host_vmlinux`, `host_modules`),
`resolve_kfuncs`, the by-name index (a `HashMap` the wrapper fills from the
port's records), the trivial accessors (`name`, `get`, `by_name`,
`func_id`, `skip_mods`, `size_of`) and the CO-RE spec constructors.

Layout: the port returns each type as a seven-word record plus entries; in
the port build `Btf` keeps those records (`table`, with `index` from type
id to record) next to `types`, so the CO-RE matcher and `.BTF.ext` reader
read the target BTF in place (borrowed slices, no conversion per call). A
local type crosses as a preorder word tree with its names in a text pool.

One Rust fix, in both builds: `local_type` multiplied a malformed member
size by 8 in `u32` (a panic in debug builds on a mutated `.BTF`, found by
the mutation test); a size above 8 bytes is now a "bad bitfield member"
error, as any width above 64 bits already was.

Exactness (new tests, port build only):

- `tests/btf_port.rs`: a blob with every kind (bitfields, signed and
  64-bit enums, names of every UTF-8 length, a named pointer), split BTF
  on it, the host's vmlinux and module BTF, and 6,000 mutated or truncated
  copies; types, names, by-name ids, base length and every one of the 11
  error messages agree.
- `tests/core_port.rs`: 6,000 random relocations of every kind against 30
  random targets (anonymous members, bitfields, arrays, modifiers, forward
  declarations, flavours, colliding names) with 0-2 split modules, and 400
  reshaped relocations against vmlinux's own structs; local values,
  candidates, target values and errors (all of `NoMatch`, `Ambiguous`,
  `Bitfield`, `BadAccess`) agree; plus `patch` on 5,000 random
  instructions.
- `tests/ext_port.rs`: objects written for every corpus program with line
  info and CO-RE relocations added, read for three section names, and 60
  mutations of each object's `.BTF.ext` or `.BTF`; `parse_ext` and
  `maps_from_btf` agree, as does `local_type` on 3,000 vmlinux ids.

All 102 tests pass against the port and the 94 of the default build
against Rust.

### Performance (part 3)

Same setup; instruction counts are now one warm call (two calls under
callgrind minus one, `EBPF_ABBA_CALLS`), which leaves out building each
workload's fixed input. The verify row is re-measured the same way.

| Workload | Rust per op | Vesper per op | Wall ratio (median, min-max) | Instructions Rust | Instructions Vesper | Instruction ratio |
| --- | --: | --: | --: | --: | --: | --: |
| `Btf::parse`, 4096 types | 636.7 us | 860.7 us | 1.30 (1.13-1.58) | 9,909,297 | 10,837,543 | 1.09 |
| CO-RE target value, 512 relocations | 193.4 ns | 315.6 ns | 1.63 (1.46-1.78) | 1,554,565 | 2,666,883 | 1.72 |
| `parse_ext`, 256 lines + 64 relocations | 62.7 us | 88.4 us | 1.41 (1.20-1.46) | 655,428 | 914,032 | 1.39 |
| `verify` of the mitigator (warm) | | | | 225,435 | 697,665 | 3.09 |

The parse is close to Rust because most of it is building the `Vec`s and
the by-name `HashMap`, which stays Rust; the port's part (decode and
validate) is 5.1 of 9.7 million instructions. CO-RE was 3.5x before the
root's encoding was hoisted out of the per-candidate calls (one encoding
per relocation instead of two); what is left is three boundary calls per
relocation, each returning a fresh sequence.

No new compiler gaps in part 3. The small results cross as a
`ChunkSeq[u64]` per call (an allocation each); returning a copy struct
through the export ABI instead is the next thing to try for CO-RE.

## Part 4: the interpreter

| Rust | Vesper | Forwarding |
| --- | --- | --- |
| `interp.rs` (the run) | `interp.ves` | `run`, `run_xdp`, `run_skb`, `Env::new`, `Env::map_get`, `Env::map_keys`; `install_tail_call` validates in Rust and keeps the target's slots for the port |

The whole run is in the port: decoding, the provenance and bounds checks of
every load, store and atomic, the context layouts, every helper (maps with
their flags and limits, ring buffers, sockets, `bpf_loop` frames, tail calls
with the 33-call limit, the task helpers) and the kfuncs. The Rust side
builds the inputs (the `__sk_buff` bytes of `SkbMeta`, the configuration
words), formats the fault strings from codes and applies the ring writes.

**State.** The export ABI has no `inout` parameters and no handles, so an
environment cannot keep a Vesper value between calls. The port build's
`Env` instead holds the interpreter's persistent state in the port's own
layout (`interp.ves` header): one word sequence (map entry table, open
addressing hash index, the map values' regions), the key bytes, the region
bytes and one spill word per 8 bytes. A run borrows them (`in` slices),
works on copies and returns the new state, which replaces the old. Only
map values persist; at the end of a run every other region is dropped, the
values still in a map are renumbered and every spilled pointer to a dropped
region points at one dead region, so a pointer kept in a map value from an
earlier run faults exactly as the Rust build's released region does. Ring
buffers stay in Rust (`Env::ring_mut` hands out `&mut RingBuf`): a run gets
each ring's size, producer and consumer and returns the new producers and a
log of its writes, applied in order. The copies cost O(state) per run where
Rust's `HashMap` is O(1) per operation, which is the price of the ABI; see
the gaps below.

`Env::map_keys` (sorted keys of a hash or array map) is new in both builds,
so the comparison tests can check every entry.

### Tests (part 4)

`tests/interp_port.rs` runs both builds side by side over one pair of
environments per program, several runs each so state carries over, and
after every run compares the result or fault (`Debug`, so every detail
string), every map key and value, the ring bytes, producer and consumer and
the prandom state:

- every corpus and depth case and 150 random programs, each also retargeted
  to other program types (3,000 runs);
- 1,500 "chaos" programs over six maps (hash, array, ring buffer, program
  array, per-CPU array, LRU hash), clean (arguments that work, so state
  builds up) or wild (any map, size, pointer or helper), with tail-call
  targets installed and removed, and mutated copies (17,000 runs, every
  fault kind);
- pointer lifetimes: a frame pointer kept in a map value until the next
  run, a hash value renumbered by deletes, socket fields, a double release,
  `bpf_get_current_comm` into the context and a tail call from a
  subprogram.

No Rust-side panics occur in these runs (the test would skip such a case;
Rust overflows on absurd helper sizes in debug builds). The existing
interpreter tests (`interp_semantics`, `program_types`, `kfuncs_widening`,
`mitigator`, `builders`, `soundness`, `kernel_diff` and the rest) pass
against the port unchanged.

Divergence check 12 (`interp.ves`): the byte just before `data` is
readable. Caught by `interp_port::port_matches_rust_on_chaos_programs`;
`port_matches_rust_on_pointer_lifetimes` still passes.

### Performance (part 4)

Workload `interp`: the mitigator run 256 times (256 SYN and ACK packets
from 64 sources) in a fresh `Env`, with the ring buffer drained after each
run. Instructions are one warm call, as in part 3.

| Workload | Rust per op | Vesper per op | Wall ratio (median, min-max) | Instructions Rust | Instructions Vesper | Instruction ratio |
| --- | --: | --: | --: | --: | --: | --: |
| `run` of the mitigator, 256 packets | 1972.8 ns | 7867.1 ns | 3.94 (3.81-4.14) | 4,718,149 | 28,412,655 | 6.02 |

The first build was 8.11x. Building the per-run zero and one buffers with
bulk extends instead of 8,704 pushes took it to 5.36x; pre-sizing the
state, output and table sequences to 4.14x; decoding straight into the
interpreter's slot tables (`decode_slots`, no intermediate `ChunkSeq[Insn]`)
to 3.94x.

Where the 111 thousand instructions of a port run go: decoding the
program 35 thousand (Rust decodes per run too, but the port's
`decode_slot` costs about 390 instructions per slot), executing it 16.5
thousand, reading the state in 13 thousand and writing it out 15 thousand,
and the Rust side's word building and result formatting about 10 thousand.
So the instructions themselves run at close to Rust speed; the gap is the
per-run setup.

Compiler gaps (part 4):

- No handles or `inout` at the export boundary, so the whole interpreter
  state (regions, map entries, the hash table, value bytes and spill
  words) is copied into the run and back out on every call. A handle that
  lives across export calls would remove about a quarter of a run.
- `Insn` is a large enum passed and pushed by value; decode is dominated
  by building and copying it.
- No bulk fill or `resize` for `ChunkSeq`, and `extend` cannot read from
  the sequence it writes (so buffers cannot double themselves).

## Remaining (part 5)

The mitigator, the builders and assembler, and the BTF and `.BTF.ext`
writers. `tests/lean_model.rs` passes against both builds; locally its one
Lean compile step is skipped without `VESPER_LEAN` (the other three tests
check the emitted programs and the model text), and Bazel runs it with Lean
against the Rust build.
