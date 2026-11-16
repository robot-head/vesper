# Verified direct codegen breadth (#583)

Base: `042c859` (origin/main). Lane 6 of wave nine, delivered in milestones.
This report is updated by each milestone PR.

## Milestone 1: enums and `match`

### What was built

- **Enum layout.** A non-recursive enum (generic enums after substitution)
  flattens to a tag word (the variant index, path `#tag`) followed by a
  union payload as wide as the widest variant (`#payload.N`). Each variant
  lays its fields out from word 1 in declaration order. Unused payload words
  are written as zero, so results are deterministic. A recursive enum
  (`enum L { Nil, Cons(i64, L) }`) has no finite layout without a heap and is
  rejected with `Unsupported::Enum`.
- **Constructors.** `Op::Data` of an enum variant writes the tag, the
  fields at their variant offsets and the zero padding.
- **`match`.** `Terminator::Match` lowers to a first-match chain: each arm's
  pattern becomes word tests that jump to the next arm on failure, then a
  jump to the arm block. Tag, integer literal (all four kinds, including
  `i64::MIN`), `bool` literal, tuple, struct and nested variant patterns are
  tested. A literal that does not fit the scrutinee's type never matches.
  After the last arm the chain traps with `Unreachable` (the checker
  proves arms exhaustive). The arm block's leading `Bind` of the scrutinee
  then only copies words; refutable patterns in any other binding (a `let`
  of a multi-variant enum constructor, a literal) remain
  `Unsupported::Match`.
- **Result decoding.** `decode` rebuilds enum values (reading the tag,
  the variant's fields, and skipping the payload's remaining words) so the
  differential tests compare with the evaluator's rendering.

The dispatch uses only existing slot-IR rows (`Const`, `Compare eq`,
`BranchZero`, `Jump`, `Label`, `Trap unreachable`). The instruction
selection table and its Lean counterpart are unchanged, and the
row-local simulation in `lean/VerifiedCodegen.lean` covers every emitted
row. Nothing new is proved about the lowering of patterns itself: as
before, Rust lowering is outside the Lean proofs.

### Checks

- `cargo test --locked --test verified_codegen --test verified_codegen_cli
  --test verified_codegen_lean`: 13 passed. New fixtures
  `tests/fixtures/verified_codegen/enums.ves` and `literals.ves` agree with the
  evaluator and the Cranelift JIT on x86-64 Linux (native) and AArch64 Linux
  (QEMU 8.2.2 user mode), with and without the IR optimizer. AArch64 macOS
  objects are built and checked for determinism but only run on Apple
  silicon (macOS CI).
- New typed rejections: recursive enum (`Enum`), string pattern (`String`),
  float pattern (`Float`).
- `cargo test --locked -p vesper-codegen-verified`: passed. The Stage 1 lock's
  recorded refusal for `parse_main` (`Effect`, effect handlers) is
  unchanged: this milestone only removes earlier refusals, so the first
  refusal on the frontend stays the handler.
- `cargo +1.100.0 clippy --locked -p vesper --all-targets -- -D warnings`
  and `cargo fmt --all -- --check`: clean. `vesper fmt --check` passes on the
  new fixtures.

### Remaining gaps in #583

`f64` arithmetic, strings and byte access, the RV64GC target, Windows PE/COFF
output with unwind data, and Lean encoder proofs against an ISA model are
not in this milestone. They follow as separate PRs.
