# Naming the magic constants (#1159)

The goal is to replace unexplained numeric literals in the Rust compiler,
the crates and the `.ves` packages with named constants: Rust `const`
items, and `.ves` module constants (`const NAME: T = e;`, manuscript §2.2,
since #1115). Values a specification defines get a name whose comment
cites it (the manuscript section for Vesper's own values, the RFC or draft
for protocol values).

Out of scope: trivial literals (0, 1, 2, obvious indices and bit widths),
test fixtures, lock digests and golden data, and the strict Stage 1 lock
corpus, which must stay byte-identical.

## Survey

A rough count of non-trivial numeric literals outside `const`/`static`
items, tests, fixtures and examples (`main` at e29e63d). It overcounts:
lookup tables, layout assertions in bindings and wire-format byte values are
included, and many of them are fine as they are.

| Area | Literals | Notes |
| --- | ---: | --- |
| `crates/vesper-serde` | 3839 | mostly Grisu/Ryu power tables and SIMD masks: tables stay, scattered thresholds get names |
| `packages/vesper-compiler` | 2695 | the Vesper compiler package: after the SH1 lanes, and only where Stage 1 lock digests do not change |
| `src` (compiler) | 2065 | SH1 lanes are editing native lowering, diagnostics, parser, fmt, lint and forge; last |
| `crates/vesper-prism-h3` | 1368 | QPACK static table and Huffman tables (data), RFC 9114/9204 codes |
| `crates/vesper-ebpf` | 1202 | opcode encodings: named after the kernel's `BPF_*` macros |
| `crates/vesper-tensor` | 798 | GGUF/quant block sizes |
| `crates/vesper-grammar` | 736 | Protobuf/DER tags |
| `crates/vesper-aegis` | 685 | CBOR major types, GPA layouts |
| `crates/vesper-silicon` | 652 | AIA/DTB register offsets |
| `crates/vesper-kinetic` | 607 | |
| `crates/vesper-pact` | 586 | |
| `crates/vesper-ignis` | 533 | |
| `packages/vesper-front` | 467 | SH1 lanes 9-12 own it; last |
| other crates | under 400 each | |

## Order

1. BBR (`crates/vesper-warp-bbr`), with the inliner fold below.
2. Port crates and their `.ves` ports that no SH1 lane edits, one crate per
   PR: Warp, warp-tcp, Prism, Quarry, Aura, eBPF, silicon, and so on.
3. Toolchain crates the SH1 lanes do not touch.
4. `src/`, `packages/vesper-front`, `packages/vesper-compiler` and the seed
   crates once the SH1 lanes that edit them have merged.

## Named constants must be free

A use of a module constant is a call of its function `const$NAME`. In the
BBR port, naming about 40 constants cost 2.0% more instructions on the ack path
(`examples/ack_path_abba.rs measure 200000`, callgrind), because each use
was a call the inliner charged against its growth budgets, which moved
other inlining decisions. `ir_inline::inline` now first replaces every call
of a constant whose function returns one literal with that literal. With
it, the named BBR port runs within 0.1% of the literal one (385.48 M vs
385.89 M instructions).

The fold runs only where the inliner runs (optimized native code), so the
verified backend, the Stage 1 lock corpus and `vesper build` executables
see what they saw before.

## Gaps found

- A constant's initializer must be a literal tree: `GAIN_UNIT - LOSS_THRESH`
  is refused (`E0401`), because `-` on `u64` may raise. Derived constants
  are written as their literal value with a comment.
- A constant used as a divisor (`x / MILLIS_PER_SEC`) is not known to be
  non-zero at check time, so `/` needs `Fail` there. The BBR port uses its
  `div` helper instead.
