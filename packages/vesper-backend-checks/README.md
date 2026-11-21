# @vesper/backend-checks

The checks that run on emitted code, written in Vesper (self-hosting wave
SH4 lane 17, #1291). Part 1 ports `src/native_verifiers.rs`: the four
Stage-5 verifiers over the Cranelift functions that native lowering emits.

| Module | Rust it mirrors |
| --- | --- |
| `input.ves` | the decoder of `tests/common/backend_checks_input.rs` |
| `consttime.ves` | `vesper_vault::verify_constant_time` |
| `stack.ves` | `native_verifiers::measure_emitted` and silicon's prologue decoders |
| `stack_bound.ves` | `stack_bound::stack_bound_report`, silicon `analyze_stack`, `stack_bound::render` |
| `realtime.ves` | resonance `call_graph_from_clif`; the confinement check is `@vesper/resonance` |
| `detfloat.ves` | `vesper_kinetic::detfloat::audit` |
| `verify.ves` | `verify_emitted`, `capability_verifiers`, `policy_from_capabilities` |
| `lib.ves` | `render` and the entries |

Rust stays the default build. The Vesper package is a test of the compiler:
`tests/self_host_backend_checks.rs` runs both over the verifier fixtures,
the capability fixtures and the corpus and compares the reports byte for
byte.

## Input

Everything Cranelift computes is input: the CLIF tables (values, blocks,
instructions with opcode properties), the machine code compiled with the
object ISA, and `verify_function`'s verdict. The verifiers' logic is not.
`input.ves` documents the word layout.

## Entries

- `backend_verify(in input: Str) -> ChunkSeq[u64]`: the report dump packed
  by `pack_text` (`render`'s text, `--`, then one line per report field), or
  `error <message>`.
- `backend_verify_count(in input: Str) -> u64`: findings plus notes, for
  timing.
- `backend_verify_decode(in input: Str) -> u64`: decoding only, the timing
  baseline.

## Reusing the stack analysis

`stack_bound.ves` is word-level so other ports can wrap it (lane 16 adds the
eBPF-origin checks): build a `StackGraph` (`graph_new`) and a `StackPol`
(`pol_new`), call `analyze_stack(in names, in graph, in policy, inout
report)`, and read `report.sr` / `report.sf` (layout in the module header),
or print them with `render`.
