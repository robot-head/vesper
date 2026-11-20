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

## Done

| PR | Area | Port speed |
| --- | --- | --- |
| #1162 | BBR (Rust + `.ves` port), inliner fold, checker facts | ack path +0.1% Ir |
| #1172 | warp-tcp: header offsets, option kinds and lengths, RFC 9293/7323/5961 limits (Rust reference + `.ves` port) | codec and transfer Ir identical |
| #1193 | warp-wheel: slot and span limits, lease mixing constants | single-literal constants, folded (no benchmark example) |
| #1193 | Prism: per-module ASCII constants, RFC 9110 tchar/field-value and RFC 3986 target masks, status codes, host limits | `abba measure 20` 62.87 M -> 62.85 M Ir |
| #1197 | silicon: the RISC-V AIA register map (APLIC offsets, IDC registers, `target[i]` and `msiaddrcfgh` fields, `sourcecfg.SM` codes, IMSIC iselects) | `abba one port` aia-intc, aplic-direct, imsic Ir unchanged |
| #1199 | eBPF instruction codec: the kernel's `BPF_*` opcode fields (class, op, jmp, source, size, mode, atomic) | decode workload 5.42 M -> 5.32 M Ir per warm call (-1.9%) |
| #1202 | eBPF interpreter: helper ids, map types, errnos, limits, ring buffer header bits, `xdp_md`/`__sk_buff` offsets, FNV and xorshift64* constants, region kinds (Rust and port) | interp workload 25.63 M Ir per warm call before and after (+0.01%) |
| #1203 | eBPF BTF, `.BTF.ext`, CO-RE and wire: BTF kinds and `info` fields, header layout, UTF-8 bounds, `line_col`, CO-RE relocation kinds, local tree tags, frame offsets, map types and limits (Rust and port) | warm call Ir: btf 10.574 M -> 10.593 M (+0.18%), ext 934.3 K -> 942.0 K (+0.82%), core 2.7407 M -> 2.7405 M |
| #1204 | eBPF verifier: register tags, frame and stack layout, stack and packet limits, `xdp_md`/`__sk_buff` context offsets, map types, socket tuple sizes, ASCII (Rust and port). Integer `match` arms stay literal (constants are not patterns yet) | warm call Ir: verify 679.6 K -> 681.4 K (+0.26%), scalar 8.7395 M -> 8.7394 M, concrete 763.7 K -> 766.0 K (+0.30%) |
| #1205 | Tensor: IEEE binary32/binary16/BF16 fields (new `host::ieee`), FP8 and E8M0 codes, GGML `nearest_int` magic, every GGML type id and block size, GGUF value types, host/K/V/shape ceilings, JSON surrogates and control range, UTF-8 (RFC 3629, Unicode Table 3-7), xorshift64* multiplier, FNV-1a, Q4_K scratch layout, `open_weights` codes, ASCII (Rust and port). Port result and tape codes stay documented numbers | whole-benchmark Ir: dequantize Q8_0 2.5038 G -> 2.5024 G, Q4_K 3.9149 G -> 3.9149 G, shape plan 16.550 G -> 16.550 G |
| #1206 | Grammar: X.690 identifier/length octets, universal tags and DER kinds, LEB128 and u24 frames, Protobuf wire types/field-number range/ZigZag masks, Cap'n Proto pointer fields, segment table and packing runs, FlatBuffers offsets and vtables, Arrow IPC framing/`MetadataVersion`/`MessageHeader`/struct sizes, grammar rule tags and metadata/capture layouts, Unicode Table 3-7, hard ceilings (Rust and port). FlatBuffer field ids, slot widths and error kinds stay literal | `grammar_abba window` (2,000 decodes) Ir: Protobuf 227.979 M -> 227.975 M, DER 189.242 M -> 189.237 M |
| #1207 | Aegis: SEV-SNP report versions, signature algorithm, FLAGS reserved bits, ETCB layout, Venice family/models and P-384 signature components; TDX quote versions, body types and lengths, attestation key and TEE type, certification data types; SGX quote and report body; Intel TCB info/QE identity versions; X.509 DER tags and TBS fields, SEC1 points; CCA REMs, RoTPK and digest lengths; CoRIM CDDL map keys; civil-time (Hinnant) constants; claims wire layouts, architectures and TCB components; port tail modes (Rust and port). Format codes, error kinds and field widths in layout order stay literal | `abba window port` (1,000 passes) Ir: snp 35,484,197 -> 35,483,973, tdx 43,505,390 -> 43,505,166, sgx 23,801,174 -> 23,800,961, policy 105,848,700 -> 105,848,494 |
| #1208 | Kinetic: the fdlibm `exp`/`log`/`sin`/`cos`/`atan2` coefficients and high-word thresholds (fdlibm names, as `_BITS` patterns in the port), IEEE binary64/binary32 fields, crmath formats, Machin and ln 2 series constants, working-precision and error-bound terms, GJK/EPA tolerances and iteration caps, the Morton magic-bits masks, LBVH child words, SAP radix and pair-key passes (Rust counterparts of the ported modules and the whole port; the rigid/world/diff physics tolerances are left for a follow-up). Status codes stay literal | `abba one port` (200 runs) Ir: crmath 9.067 G -> 8.959 G (-1.2%), lbvh 4.9820 G -> 4.9812 G, gjk 915.9 M -> 915.8 M, fastmath, morton, sap and colorer unchanged |
| next | Kinetic physics (Rust only): hull, separating-axis and edge tolerances, segment and tangent minimums, unit-quaternion and unit-vector checks, contact merge and manifold tolerances, broadphase box rounding and inflation, the barrier spin count. Config defaults (`dt`, substeps, margins) stay in their `Default` impls, where the field names already say what they are | Rust-only code, no port change |

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

## Constants as known values

The checker now treats a use of a constant whose initializer is an integer
literal as that integer (`Checker::integer_fact`). A constant divisor is
then non-zero (`x / MILLIS_PER_SEC` needs no `Fail`), and an initializer
built from such constants is in range (`const KEEP: u64 = GAIN_UNIT -
LOSS_THRESH;`). A zero constant still leaves `/` fallible, and `A - B`
that would underflow is still refused.

## ASCII codes in `.ves` ports

Vesper has no byte literals and module constants are not imported across
modules, so each module that compares bytes defines the ASCII constants it
uses (`SLASH: u8 = 47`, `DIGIT_0`, ...). The user chose this over adding byte
literals first (2026-11-20).

## Gaps left

- Named constants in a hot byte loop can cost a little: in the `.BTF.ext`
  access-string parser (`parse_u32`), writing `ASCII_0`/`ASCII_9`/`U32_MAX`
  instead of literals costs about 14 K Ir per warm `ext` call (1.5%). The
  constants are single literals, so the inliner's fold should make them free;
  it does not reach these comparisons yet.
- Constants are not patterns: `match c { JEQ => ... }` is refused
  ("unknown pattern constructor"). The eBPF port's decoders compare with
  `if` chains instead of matching integer literals.
- An index that starts with an uppercase name parses as type arguments:
  `b[LO..HI]` and `s[S_PACKED + j]` fail with E0002 ("expected RBracket"),
  because `name[Upper` reads like `ChunkSeq::new[u8]`. The tensor port
  writes `b[(LO)..HI]` or `s[j + S_PACKED]`; fmt keeps the parentheses.

- A constant derived from other constants (`GAIN_UNIT - LOSS_THRESH`) is
  not folded by the inliner (its function is not a single literal), so it
  stays a small call. The BBR port writes such values as literals with a
  comment until the fold covers them.
- The 0.1% left in the BBR port is one function, `vesper_function_3`
  (+403 k Ir over 200 000 steps); every other hot function has the same
  count as before.
