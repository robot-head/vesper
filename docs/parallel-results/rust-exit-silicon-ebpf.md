# Rust exit SH6 lane 10: vesper-silicon and vesper-ebpf

Claim #1405 (continues #1041, eBPF, and #1042, silicon). Base: main at
ab965f2a (2026-11-21). This report grows with each part of the lane.

## Part 1: eBPF speed-ups

What changed (all in `crates/vesper-ebpf`, port build only; the Rust build
and the seed are unchanged):

- **Interpreter** (`vesper/src/interp.ves`, `src/port.rs`): the machine is a
  Vesper handle (`Mach`) that lives in the Rust `Env` between runs. Decoded
  code is reused when the same program runs again, and map storage is
  saved by appending new entries instead of renumbering every value.
  New differential test `port_matches_rust_when_programs_share_an_environment`
  (tests/interp_port.rs): insert, read and delete programs over shared maps,
  6 sessions of 40 programs.
- **Verifier** (`vesper/src/verifier.ves`): each frame keeps a low-water mark
  below which every stack slot is empty, so scans, packing and comparisons
  skip the empty part. Checkpoint records are stored packed. The record and
  worklist pools (`Pools`) are kept by the Rust caller in a thread-local and
  reused between runs. The current state is packed once per checkpoint, not
  once per compared record. The register-link check skips unlinked registers
  instead of testing all 55 pairs.
- **Scalar domain** (`vesper/src/scalar.ves`): `normalize` stops at its
  fixpoint (each round is a function of the state alone, so a round that
  changes nothing is final), with the tnum intersect inlined. `refine_jmp`
  handles the negated and mirrored comparisons in one pass instead of by
  recursion. New differential test tests/scalar_port.rs: `normalize` of raw
  component tuples, every binary ALU transfer function at both widths, every
  branch refinement on both outcomes, and `meet`, 20,000 random cases each,
  compared exactly against the Rust seed.
- **CO-RE** (`vesper/src/core.ves`, `src/core.rs`): one port call per
  relocation (`core_target_value`: candidates, values and combine together),
  borrowing the BTF name index and access path without converting them, and
  encoding the local type into a reused buffer. The split-BTF path is
  unchanged.
- **ABBA output** (tests/abba.rs) now prints the distribution-free 95%
  interval of the median ratio (order statistics x(10)..x(21) of 30 epochs)
  and whether its upper end meets the flip bar.

### Measurements

Release build, this cloud host (shared, noisy), `cargo test --release -p
vesper-ebpf --features vesper-port --test abba -- --ignored --nocapture
--test-threads 1`. Vesper/Rust time per operation, 30 epochs:

| Workload | Before (main) | After, median | After, 95% interval | Flip bar |
|---|---|---|---|---|
| interp | 3.50 | 0.78 | 0.78..0.79 | pass |
| concrete | 1.16 | 1.13 | 1.11..1.14 | pass |
| ext (.BTF.ext) | 1.27 | 1.26 | 1.20..1.28 | pass |
| core | 1.68 | 1.28 | 1.23..1.31 | pass |
| btf | 1.10 | 1.37 | 1.35..1.38 | pass |
| scalar | 1.81 | 1.49 | 1.47..1.58 | fail (marginal) |
| verify | 2.52 | 1.38-1.53 | 1.48..1.55 (last run) | fail (marginal) |
| decode | 1.88 | 1.91 | 1.86..2.03 | fail |

The host's timings drift between runs (Rust's own `verify` time moved from
25 to 37 µs between two runs of the same binary), so the marginal rows need
a quiet machine before a flip verdict. `btf` did not change in this part; its
movement is the same drift.

Callgrind (instructions, the same workloads): verify 1.90M to 1.09M per two
calls (Rust 0.69M); scalar 8.8M to 6.6M (Rust 4.6M); core steady state 26.1M
per ten calls before (Rust 15.1M).

### Compiler gaps (part 1)

1. **Short-circuit conditions materialize booleans.** `a && b` and `a || b`
   lower to `setcc; movzx; test; jne` per operand instead of a branch chain.
   In `scalar_normalize` the six-field fixpoint test cost about 30
   instructions until it was rewritten with `^` and `|`. Repro:
   `fn f(a: u64, b: u64, c: u64, d: u64) -> bool { a == b && c == d }`.
2. **Small structs and options return through memory.** `tnum_range` and
   `tnum_intersect` (two-word `Tnum`, `Option[Tnum]`) return through stack
   slots and are reloaded, about 40 instructions per call in a hot loop.
   Repro: `fn r(a: u64) -> Option[Tnum] { Option::Some(Tnum { value: a, mask: 0 }) }`
   called in a loop.
3. **Enum matches are a linear compare chain.** `insn::encode` tests the
   `Insn` tag against each variant in order with a materialized boolean per
   test, and copies the 7-word argument to the stack first. This and the
   per-instruction export call (the binding puts and gets 7 and 11 words)
   are why `decode` stays near 1.9x. Repro: a 12-variant enum and a
   `match` that returns a different constant per variant.
4. **The export boundary is per call.** `decode`, `scalar` and parts of
   `verify` are measured through the public Rust API, which crosses the C
   ABI once per instruction or per scalar operation. The fused CO-RE call
   above shows the pattern: fewer, larger calls.

### What is left

- eBPF: `decode` and the marginal `scalar` and `verify` rows; part 5 of the
  port (mitigator, builders and `Asm`, BTF and .BTF.ext writers, the ELF
  writer, syscalls through lane 13's direct imports); the flip.
- Silicon: everything in "Not ported yet" of
  docs/parallel-results/port-p3-silicon.md, the Cranelift analyses through
  the shim's function read-back, the `aplic-direct` and `imsic` speed gap,
  and the flip.
