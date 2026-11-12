# Kernel-checked SIMD scanner algorithm and bounds model

Wave-three lane 5. Branch `claude/project-thread-x8ymjp` (prompt suggested
`wave3/simd-kernel-model`; this session's push permission is limited to the
former). Base `1ffd0aec27dfcc24c5564585ae9068255490bf61` (origin/main, a
descendant of the required `02b3a1d`). Implementation commit `f393dc3`; this
report is the following commit on the same branch.

**What this proves:** the Baseline128 delimiter-count/XOR *algorithm*, as
transcribed into Lean from the Cranelift IR that `crates/vesper-simd/src/lib.rs`
builds, equals the scalar reference for every input and only reads inside the
logical slice. **What it does not prove:** Cranelift IR construction matching
the transcription in general (only a structural CLIF test guards it),
Cranelift/x86_64/AArch64 instruction selection and lowering, register
allocation, the emitted machine code or ELF objects, object linking, CPU
feature discovery, the Rust `unsafe` call boundary, source `eff [Isa[P]]`
typing, scalable vectors (SVE2/RVV) or performance. The model identifier
`vesper.simd.baseline128.algorithm-model/1` and its claim string are carried by
both the Lean file and the Rust fixture and are checked to agree, so the model
cannot be mistaken for a machine-code certificate.

## Files (all new, all inside the lane boundary)

| Path | Content |
| --- | --- |
| `lean/SimdScanner.lean` | Model, reference, assumptions and all theorems (699 lines). No recursion of its own, no `partial`/`unsafe`. |
| `lean/SimdScanner/Fixture.lean` | Unaudited stdin driver (`partial` IO) evaluating the modeled program for the Rust differential test. |
| `crates/vesper-simd/model/mod.rs` | Std-only Rust transliteration of the model and reference, model identifiers, xorshift fixture generator, Lean runner. |
| `crates/vesper-simd/tests/model.rs` | Differential tests against the real `JitScanner`; auto-discovered integration test, so no manifest change. |
| `docs/parallel-results/simd-kernel-model.md` | This report. |

Nothing outside these paths was edited. Scanner codegen, existing scanner
tests/examples, the compiler Lean emitter, the proof-audit helper and CI were
only read.

## The model

The Lean `scan` follows the IR block by block (the Rust test
`jit_clif_matches_modeled_program_and_metadata` checks the JIT's actual CLIF has
exactly this instruction sequence, with value numbers normalized):

* `entry`: `full = len &&& BitVec.ofInt 64 (-16)` (the sign-extended
  `band_imm` immediate), `needle = splat delim`, state `(0, 0, splat 0)`.
* vector loop, guard `off <u full`: unaligned `load16` at `input + off` (lane
  `i` = byte at `input + off + i`), lane-wise `icmp eq` giving `0xff`/`0x00`,
  sixteen sequential `count + uextend64 (lane &&& 1)` additions, lane-wise
  XOR into the accumulator, `off + 16`.
* `reduce`: `x = 0; x ^= lane i` for lanes 0..15.
* scalar loop, guard `off <u len`: `load8`, scalar `icmp eq` giving `i8` 1/0,
  `uextend64`, `count +`, `x ^= byte`, `off + 1`.
* `finish`: return `(count, x)`.

Words are `BitVec 64` with wrapping `+`, bytes are `BitVec 8`, memory is a flat
`BitVec 64 → BitVec 8` map. Both loops are a generic guarded `runLoop` over core
`Nat.repeat` with an explicit running/exited state; fuel only makes the
definitions total. `runLoop_zero`/`runLoop_succ` are proved recursion
equations, and a fuel-exhausted run returns `none`, never a result. Every load
appends a `Read.vector off addr` or `Read.scalar off addr` to an explicit read
log.

The reference is `refCount data d = data.count d` and
`refXor data = data.foldl (· ^^^ ·) 0`, matching `src/tests.rs`' `reference`.

### Assumptions (hypotheses, not axioms)

* `MemoryModel mem input len data`: `data.length = len.toNat` and
  `mem (input + i) = data[i]` for each `i < data.length`. Length is a `u64` by
  construction of `BitVec 64`.
* `NoWrap input len`: `input.toNat + len.toNat ≤ 2^64`, which a valid Rust
  slice or C object provides. Only the address-level bounds use it.

Counter, offset and loop-increment representability are **proved**: the
invariants carry `off = 16j ≤ len/16*16` and `count ≤ off`, and each machine
`+` is shown not to wrap (`toNat_add_of_lt`) before it is related to `Nat`.

## Theorems

| Theorem | Statement (abridged) |
| --- | --- |
| `scan_correct` | For any `mem`, `input`, `len`, `data`, delimiter `d` with `MemoryModel` and any fuel `vf ≥ len/16`, `sf ≥ len%16`: `scan` returns `some o` with `o.count.toNat = refCount data d`, `o.count = ofNat 64 (refCount data d)`, `refCount data d < 2^64`, `o.xor = refXor data`, and every logged read `Legal`. Covers both count and XOR across the whole block+tail loop, all remainders and zero length. |
| `scan_exact_fuel` | Exactly `len/16` vector and `len%16` scalar iterations suffice. |
| `vecLoop_insufficient_fuel` | Less vector fuel than the remaining blocks yields `none` (never a wrong success). |
| `fullLen_toNat` | `(len &&& -16).toNat = len.toNat / 16 * 16` (bitwise proof via `getLsbD`). |
| `mask_lane_bit`, `scalar_eq_bit` | Per-lane mask conversion: `uextend(band(0xff,1)) = 1`, `uextend(band(0,1)) = 0`; scalar `icmp` 1/0 extends to 1/0. |
| `maskCount_block` | The sixteen unrolled additions add exactly the block's match count, without wrapping, when `count + 16 < 2^64`. |
| `reduceXor_block` | XOR composition: `reduce(acc ⊕ chunk) = reduce(acc) ^^^ refXor block`. |
| `VInv`/`SInv` with `vecBody_inv`, `scalarBody_inv` | Block and tail invariants: offset, bound, count and XOR equal the reference on `data.take off`, all reads legal. |
| `Read.Legal`, `vector_read_offsets` | A vector read has `addr = input + off` and `off + 16 ≤ len` (whole width inside the slice, no padding overread); a tail read has `off < len`. |
| `legal_read_in_slice`, `scan_reads_in_slice` | Under `NoWrap`, each read's natural address range `[addr, addr+16)` or `[addr, addr+1)` lies in `[input, input+len)`. |
| `vector_lane_address` | Under `NoWrap`, lane `i < 16` of a vector read is at natural address `input + off + i < input + len` (the 64-bit address addition does not wrap). |
| `scan_zero_length` | Length 0: no reads, count 0, XOR 0, for any fuel. |
| `block_tail_decomposition` | `full + len%16 = len`, `len%16 < 16`, `full % 16 = 0`. |
| `reference_block_tail` | The reference splits into the `len/16` full blocks plus the tail for both count and XOR. |

Unaligned reads are permitted by construction (no alignment hypothesis
anywhere). Proofs use core `BitVec`/`List`/`Nat` lemmas, induction, `omega`,
`simp`, `ac_rfl` and kernel `decide` on closed 64-bit constants. There is no
`sorry`, admission, project axiom, `native_decide` or `bv_decide`
(`Lean.ofReduceBool` does not appear). Two mutations were checked to break the
build: immediate `-15` instead of `-16`, and vector stride 15 instead of 16.

## Evidence

Toolchain: Lean `4.24.0` (`797c613eb9b6`, x86_64 Linux release from the
official `v4.24.0` archive, matching `lean-toolchain`). Rust 1.97.0. Host:
x86_64 Intel Xeon with SSE4.2 (Baseline128 available). No QEMU or AArch64
cross binutils were installed in this environment.

| Command | Result |
| --- | --- |
| `lean lean/SimdScanner.lean` | Success; no errors or warnings. Axiom report: `scan_correct`, `scan_exact_fuel`, `vecLoop_insufficient_fuel`, `scan_reads_in_slice`, `vector_lane_address`, `legal_read_in_slice`, `block_tail_decomposition`, `fullLen_toNat`: `[propext, Classical.choice, Quot.sound]`; `scan_zero_length`, `reference_block_tail`, `mask_lane_bit`, `maskCount_block`, `reduceXor_block`: `[propext, Quot.sound]`. |
| `lean -o SimdScanner.olean SimdScanner.lean` then `vesper proof-audit SimdScanner --root <artifact dir> --theorem …×14 --allow-axiom propext --allow-axiom Classical.choice --allow-axiom Quot.sound --lean <real lean> --timeout 120` | **accepted**; 313 kernel-replayed constants; per-theorem axioms as above (`vector_read_offsets`: none). The artifact directory held only the `.olean`. |
| Same audit, default empty policy, `--theorem Vesper.SimdScanner.scan_correct` | rejected: `unapproved axiom` for each of the three standard axioms, as the policy intends. |
| `VESPER_LEAN=lean cargo test -p vesper-simd --locked` | 7 existing unit tests (including guard-page and seeded unaligned tests) + 6 new model tests pass; doctests unchanged. |
| `cargo clippy -p vesper-simd --all-targets --locked -- -D warnings` | Clean. |
| `cargo fmt --all -- --check` | Clean. |

The policy admits exactly Lean's three standard axioms, because
`Classical.choice`/`propext`/`Quot.sound` enter through core `omega`, `simp`
and `List`/`BitVec` lemmas; nothing else is admitted and `sorryAx` cannot be.
The first audit attempt was rejected because the compiler emits
`_unsafe_rec` helpers for recursive definitions; rather than weaken the
auditor, the loops were rebuilt on core `Nat.repeat` and the IO driver moved to
the separate unaudited `Fixture.lean`.

### Differential tests (`crates/vesper-simd/tests/model.rs`)

* `model_reference_and_jit_agree_on_all_tails_offsets_and_delimiters`: all 256
  delimiters × lengths 0..=80 × 16 start offsets of one allocation (asserting
  all 16 address residues mod 16 occur): reference = Rust model = real JIT.
* `model_reference_and_jit_agree_on_seeded_large_inputs`: seeded slices up to
  290 000 bytes including 65 535/65 536/65 537/131 089/262 143, unaligned
  starts, extreme and occurring delimiters, and a 70 001-byte all-match slice.
* `model_read_log_matches_block_tail_decomposition`: for every length 0..=200
  the Rust model logs exactly `len/16` vector reads at offsets `0,16,…` and
  `len%16` scalar reads to `len - 1`, all legal; negative legality cases
  (overread by one, offset overflow, tail at `len`).
* `jit_clif_matches_modeled_program_and_metadata`: the JIT's real CLIF equals
  the modeled instruction sequence; `CodeMetadata` profile, width, tail strategy
  and ABI match the model's constants; x86_64 and AArch64 ELF objects export
  `vesper_simd_scan_baseline128_v1` with the same metadata.
* `lean_and_rust_model_identifiers_agree`: identifiers/claim agree; with
  comments stripped, the Lean source has no `sorry`, `admit`, `native_decide`,
  `bv_decide`, `ofReduceBool`, `axiom`, `partial` or `unsafe`; 13 axiom reports.
* `lean_model_matches_real_jit` (requires `VESPER_LEAN`, as CI sets): compiles
  the real Lean file (re-checking every proof and requiring every axiom line to
  list only standard axioms), runs `Fixture.lean` on 458 cases (every remainder
  for lengths 0..=48 at four base addresses including the top of the 64-bit
  address space, all 256 delimiters on a slice containing every byte value, six
  seeded 100–2 099-byte slices), and checks the Lean modeled program's count,
  XOR, read count and in-bounds flag against the Rust model and the real JIT.

## Coordinator patch

No manifest, lockfile or module wiring is needed: the new test is an
auto-discovered integration test of the already-registered `vesper-simd` crate
and uses only `std` plus that crate. Requested CI addition (both the Linux job
and the platform matrix, next to the existing Lean steps):

```diff
       - run: lean lean/CheckedArithmetic.lean
+      - run: lean lean/SimdScanner.lean
```

`cargo test --workspace` with `VESPER_LEAN: lean` already runs
`lean_model_matches_real_jit`; on the macOS AArch64 matrix lane that will be the
first real AArch64 execution of this differential. Optionally add an audit
step:

```sh
out=$(mktemp -d) && cp lean/SimdScanner.lean "$out/" && (cd "$out" && lean -o SimdScanner.olean SimdScanner.lean) && rm "$out/SimdScanner.lean"
cargo run --locked -- proof-audit SimdScanner --root "$out" \
  --theorem Vesper.SimdScanner.scan_correct --theorem Vesper.SimdScanner.scan_reads_in_slice \
  --theorem Vesper.SimdScanner.vector_lane_address --theorem Vesper.SimdScanner.scan_zero_length \
  --allow-axiom propext --allow-axiom Classical.choice --allow-axiom Quot.sound
```

Global docs (`docs/parallel-work.md`, `docs/implementation.md`, the scanner
README's "no formal proofs" line) can then point to this report.

## Remaining gaps

* The CLIF-to-Lean correspondence is a manual transcription guarded by an exact
  CLIF shape test, not a proof. Cranelift lowering, the ISA instruction
  semantics of the selected `pcmpeqb`/`cmeq`, `pextrb`/`umov` etc., register
  allocation and object/JIT linking are unverified.
* The memory model assumes reads return the slice bytes and have no side
  effects, the input is unchanged during the call, and the output store is
  ignored; the Rust `unsafe` boundary that establishes these is not modeled.
* AArch64 JIT execution of the new differential tests was not run here (x86_64
  host, no QEMU). The existing QEMU object harness is unchanged and was not
  re-run in this environment. Windows/macOS runs of the new test are pending CI.
* CPU feature discovery, source `Isa` effect typing, SVE2/RVV and performance
  remain out of scope.
