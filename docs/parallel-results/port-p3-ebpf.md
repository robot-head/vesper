# Port wave P3a lane 6: vesper-ebpf (#1041)

Status: **part 2 of 4** (this PR; part 1 is #1048, merged as `5ff0c7a`).
Part 1 base `3c075a4`, part 2 base `5ff0c7a`.

The crate is 12.4k lines with 82 tests in 15 files, so the port lands in four
parts, as Crucible's did:

1. **Part 1 (#1048):** the seed package, the instruction codec (`insn.rs`), the
   wire structs (`wire.rs`), the tnum and interval domain (`scalar.rs`) with
   the verifier's domain functions (`scalar_alu`, `refine_jmp`, `sext_imm`),
   the concrete ALU, jump and byte-order semantics of `interp.rs`, the helper
   tables (`helpers.rs`), the map and program kinds and `MapDef::validate`
   (`program.rs`) and the stack bound (`callgraph.rs`).
2. **Part 2 (this PR):** the verifier (`verifier.rs`), see
   [Part 2: the verifier](#part-2-the-verifier).
3. BTF parsing, BTF.ext and CO-RE (`btf.rs`, `btf_ext.rs`, `core.rs`).
4. The interpreter, the mitigator and the builders and assembler
   (`interp.rs`, `mitigator.rs`, `builder.rs`, the `Asm` half of
   `program.rs`).

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
passes. All nine were observed failing:

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

## Remaining (parts 3-4)

BTF/BTF.ext/CO-RE, the interpreter (its maps become owned
state threaded through the run, per the lane prompt), the mitigator and the
builders. `tests/lean_model.rs` passes against both builds; locally its one
Lean compile step is skipped without `VESPER_LEAN` (the other three tests
check the emitted programs and the model text), and Bazel runs it with Lean
against the Rust build.
