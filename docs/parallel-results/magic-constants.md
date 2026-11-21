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
| #1211 | Kinetic physics (Rust only): hull, separating-axis and edge tolerances, segment and tangent minimums, unit-quaternion and unit-vector checks, contact merge and manifold tolerances, broadphase box rounding and inflation, the barrier spin count. Config defaults (`dt`, substeps, margins) stay in their `Default` impls, where the field names already say what they are | Rust-only code, no port change |
| #1213 | Pact port: every node tag and kind/flag constant now uses the `term.ves` tag functions (`t_ctor()` ... `e_env()`) instead of bare numbers, child-reference masks, ASCII bytes, the FNV-1a, Fibonacci-hashing and SplitMix64 multipliers and slot shifts, and the default solver limits (as in the Rust `SolverConfig::default()`). Diagnostic codes and the derivation wire codes in `out.ves` stay documented numbers | `abba once` Ir: fixture 7,516,603 -> 7,440,968 (-1.0%), large 213.90 M -> 206.96 M (-3.2%) |
| #1214 | Ignis: the D3D12 enhanced-barrier `SYNC_*`/`ACCESS_*` bits, Vulkan `VkPipelineStageFlagBits2`/`VkAccessFlagBits2` and Metal 4 `MTLStages` bits in the port (replacing the comment tables), the split-free sync mask, and the queue, subresource, lifecycle and recorder ceilings (Rust reference and port). `D3D12_BARRIER_LAYOUT` values in `layout_raw` and the layout ranks stay literal | `abba profile` (256 passes, 20 ops) Ir: build 22.550 M -> 22.553 M, plan 331.181 M -> 331.183 M, lower 33.442 M -> 33.467 M (+0.08%); `abba digest` unchanged |
| #1215 | Quarry port: SQL keyword ids (`kw_select()` ... `kw_using()`) and `Tok` kinds (`tk_word()` ... `tk_op()`) as shared functions, per-module ASCII bytes, the i64 literal bounds, the hard `Limits` ceilings (as in `Limits::default`), the identifier length and word-byte `find_not_in` bitmaps, binary64 fields and the sign bit, and the access-path cost model (page/row/index costs, rows per page, index fanout and unknown-parameter selectivities, spec/planner-ddl.md); Rust `MAX_IDENTIFIER_LEN`. `Cmp`, `Raw` kinds, value tags, error kinds and word-stream layouts stay documented numbers (lib.ves), and the Rust store (`src/store`, WAL, pages) is left for its own PR | `planner_abba window 20` Ir: 28,187,669 -> 28,182,522 |
| #1216 | Warp: the RFC 9000 §19 frame types and the port's frame kinds, long-header types and header-protection masks (RFC 9001 §5.4.1), varint prefixes, limits and masks (§16), packet-number encoding windows (§17.1, A.2), the ECN codepoints, the application space, `ack_delay_exponent` ceiling (§18.2) and every-second-packet ACK threshold (§13.2.2), kPacketThreshold (RFC 9002), and the address-validation token kinds and plain-text layout. The Rust reference gets the same names (`MAX_ACK_DELAY_EXPONENT`, `PN_MAX_LEN`, `ADDR_LEN`, `PLAIN_*`, the protected-bit masks, `MAX_CID_LEN` in frames and tokens). The `token_open` result codes and the address family bytes stay documented numbers; the Rust endpoint is left for a later PR | `abba once <arm> 20` Ir: frames 166.50 M -> 166.44 M, ack 178.104 M -> 178.113 M (+0.005%), recovery and buffers unchanged, transfer 509.90 M -> 509.89 M |
| #1220 | Aura port: widget kinds, properties, events, value tags and patch tags as shared `codes.ves` functions (`k_*`, `p_*`, `e_*`, `v_*`, `op_*`), recorded event tags as module constants, the patch width, the `encode_view_tree` layout flags (as `sim_aura::layout_flags`) and u32/u16 ceilings, the close states (Vesper and `src/ported/session.rs`), the White_Space code points, UTF-8 lead bytes and the Width sign bit. `codec.rs`'s enum/code tables are the code definitions and stay; result codes stay documented numbers | `abba measure 200` Ir: 1,442.56 M -> 1,437.49 M (-0.35%) |
| #1229 | prism-h3 QPACK (Rust + `.ves` port): the RFC 9204 §4 first-byte patterns and prefix lengths of every field line and encoder/decoder instruction (new `src/qpack/wire.rs`, mirrored as module constants in `decoder.ves`/`encoder.ves`), the §3.2.1 entry overhead, the entry-share scale, the pending-instruction ceiling, the RFC 7541 §5.1 continuation bits and the shortest Huffman code, the Huffman table layout (code-word fields, trie and nibble-table fields, padding depth), FNV-1a and the compaction thresholds. HTTP/3 frames, settings, capsules, priorities and messages are part 2 | `abba once <w> 6` minus `once <w> 1` Ir (port): decode 169.832 M -> 169.832 M, encode 78.563 M -> 78.591 M (+0.04%), frames and messages unchanged |
| #1233 | prism-h3 HTTP/3 (Rust + `.ves` port): the RFC 9114 §7.2.4.1 and RFC 9204/9220/9297/WebTransport setting identifiers, the reserved HTTP/2 frame types and settings (§7.2.8, §11.2.2), the grease base and step (§7.2.8, §8.1, also in `H3Error::is_reserved`/`webtransport_code`), the frame reader modes, the RFC 9000 §16 varint prefixes and limits, the CLOSE_WEBTRANSPORT_SESSION code length, Unicode Table 3-7 in `valid_utf8`, the RFC 9218 urgency range and default, RFC 8941 bare-item kinds, digit limits and ASCII bytes, the scheduler word layout, the RFC 9110 tchar/field-value/pseudo-value `find_not_in` bitmaps, pseudo-header ids, content-length digit cap and status range. Field-name lengths beside their literal names, frame-type functions and error codes stay | `abba once <w> 6` minus `once <w> 1` Ir (port): decode, encode, frames and messages unchanged (169.832 M, 78.588 M, 61.555 M, 99.640 M) |
| #1235 | Serde part 1, binary64 parse and print (port, plus `src/encode.rs`): Eisel-Lemire/Clinger (the fast_float SWAR digit masks and multipliers, the power-of-five table range and index bias, the product mask and safe-q window, binary64 fields, round-to-even window, Clinger's exact-int and exact-power limits, JSON number ASCII), Grisu (Rust `flt2dec` gamma, `CACHED_POW10` exponent bias/span/steps and k bias/step, `MAX_SIG_DIGITS`), Dragon (`Big32x40` limb layout, 10^9 steps, the log10(2) scaling factor, binary64 decode, digit text) and the JSON encoder (word-stream tags, raw token kinds and record widths, `HARD_MAX_DEPTH`, `FLOAT_TEXT_BYTES`/`FLOAT_FORMAT_PASS_WORK`, encoded widths and escape bytes). The pow5 and cached-power tables stay data; result paths stay documented numbers; JSON index, stage 2, binary and tiles are part 2 | `serde_abba window <w> 6` minus `window <w> 1` Ir (port): f64_encode 59.040 M -> 58.960 M (-0.14%), f64_decode 109.004 M -> 109.304 M (+0.27%: the same instructions with a different register assignment in `decode_bits`) |
| #1236 | Serde part 2, JSON text (port): the scalar parser and tape helpers (`index.ves`: JSON ASCII bytes, the past-the-end byte and end-of-string marks, Unicode Table 3-7 UTF-8 bounds, lead tags and payload masks, the 2/3/4-byte code point thresholds, the eight-byte ASCII word mask, UTF-16 surrogate ranges and shift, `\uXXXX` digits and hex offsets, the per-character key compare work and its overflow bound, `TOKEN_BYTES` and the stage 1 entry size), stage 2 (`stage2.ves`: ASCII bytes, literal lengths, the container stack's first capacity) and the schema helpers (`helpers.ves`: the 7-byte SWAR key limit, word and pack sizes, `JsonSizeBound` escape and punctuation widths and overflow bound). Escape error kinds, surrogate charge modes and literal ids stay documented numbers; binary and tiles are part 3 | `serde_abba window <w> 6` minus `window <w> 1` Ir (port): parse_scalar 196.764 M and parse_simd 170.801 M, unchanged |
| #1256 | Serde part 3, binary and tiles (port): StreamVByte (`binary.ves`: values per control byte, tag bits and mask, the 1/2/3-byte limits, decode work per byte) and the relative-pointer archive (magic bytes, version, tree schema, every header and node field offset, header and node sizes, relative pointer and slice widths, alignment mask, node words, edge work, `U32_MAX`), plus the NDJSON tile ingest (`records.ves`: the `HARD` record limits, input and record work charges, the per-character key compare work, i64/u32/u16 bounds, the trace id width, UTF-8 tags and thresholds, NDJSON whitespace and number ASCII, the line-feed byte). Fault codes and tile slots stay documented numbers | `serde_abba window tiles 6` minus `window tiles 1` Ir (port): 576.161 M, unchanged; parse_simd 170.801 M, unchanged |
| #1266 | Loom calculus and solver (ports), the first areas on value enums (#1239): shared `pub enum X: _` groups in `util.ves`, imported by each module, for the diagnostic `Code`s, the upgrade codes (byte order of their code strings), `AuthorityScope`, forbid words, comparator ops, the merge sort's order kinds, `ord` results (`Order::Less/Equal/Greater` in place of 0/1/2) and the ASCII bytes of names, capabilities, paths and versions; module consts for the solution schema version, the per-node `nfirst` layout and edge triple widths. The two literal subject lengths (`4` for `root`, `26` for `package.capability_ceiling`) are now measured with `since`, and the unused `code_manifest_syntax` is removed. The solver's codes become `SolverCode` (from 16, after the calculus `Code`), and its error kinds, result tags, objectives, metric words, sort kinds, search step results and snapshot marks become value enums too; it imports the calculus `Code`, `Order` and `Ascii`, and names its catalog schema version, decimal base and dedup slack | `abba window 6` minus `window 1` Ir (calculus port): 127.547 M -> 127.513 M (-0.03%); `solver_abba window 4` minus `window 1` (solver port): 386.259 M -> 386.177 M (-0.02%) |
| #1268 | Loom audit (port), on the calculus enums: `ErrorKind` (the Rust `Error` order) replaces the five `e_*()` fns; new value enums `Identity` (Vesper or native), `AuditSort`, `License` and `Exception` (the supported SPDX ids in license.rs order) and `LicenseFail`; the calculus `Order` and `Ascii` (now with whitespace and parentheses) are imported. Module consts name the safe parser limits (64/64/1024) and the grant code layout (4 per license, 80 grants); the two `u64::MAX` sentinels use `none()` | `audit_abba window 4` minus `window 1` Ir (port): 72.624 M -> 72.554 M (-0.10%) |
| #1272 | Crucible (port), one module: value enums for `DrawCode`, `Outcome`, `ParseCode`, the shrinker's 19 pass points (`Pc`, in place of `pc` numbers), the fuzzer's `Verdict`/`Observed` codes, feature and draw kind tags, comparison tags, the ten `Mutation`s, the DPOR process tags (`Proc`) and `search_choose` codes (`Choose`), and a local `Order`; consts for the hex text format, the whitespace bytes, the chance scale, mutation bounds, the lane solver's limits, comparison word layout and process bit fields. The SplitMix64 constants and the Unicode White_Space encodings stay as spec literals; the tape header stays byte pushes (a `try_byte_at` loop over the literal cost +0.19% Ir) | `abba measure 120` minus `measure 20` Ir (port): 7613.954 M, unchanged |
| #1304 | Bench (port): value enums for the contract gate's `Status` and `Overall` codes, the `@bench` `Field` ids and `Flag` tri-state, the `Limit` and `ConfigCode` error codes (in place of the `limit_*()` fns and `invalid(n)` numbers), `analyze_pairs`' `Reason`, the layout `Verdict`, the gate `Outcome`, paired-difference `Exactness`, the wire `Field` indices and the attribute parser's `Ascii` bytes. Shared consts in util (`BP_SCALE`, `MAX_PAIRS` in place of `max_pairs()`, `MAX_EPOCHS`, the i64 bounds, `NO_BYTE`); module consts for the layout plan, `RunConfig` limits, the ABBA window counts, the 5% and 2.5% tail scales, the wire layout and the 128-bit word helpers. SplitMix64 stays as spec literals | `abba` estimators 200 minus 20 batches Ir (port): 47 514.55 M, +180 Ir; statistics 6400 minus 640 samples: 9.229 M, unchanged |
| #1318 | Resonance (port): value enums for the confinement verifier's `fns`/`entries`/`roots`/link record words, `Loops`, `Rule`, `Status` and DFS `Color`; the schedule compiler's graph `Node`/`GraphInput` words, `InputKind`, `PdcCode`, schedule `Header`/`Op`/`Input`/`Line` words and the shared record sizes (`Words`); the delay slab's `Slot` words, `Totals` and `SlabWords`; the ramp bank's `RampSlice`/`RampBase` offsets, the PID `Gain` and `Pid` state words; the event wire `At` byte offsets; the test graph's processor `Kind`. Consts for the tile geometry, the u64/isize/u32 bounds, the f32 exponent mask, the generator source, the key packing and the realtime roots' sample arguments (`ONE_BITS` etc.); `lanes()`, `six()`, `none()`, `umax()`, `isize_max()` and `max_delay()` became consts | `abba` port Ir, 2000 minus 200 blocks: kernel 3974.98 M -> 3971.74 M (-0.08%), graph 722.32 M -> 721.41 M (-0.13%) |
| next | Semver (port): value enums for the checker's flat encoding: the input header (`Input`), list, interface, external type, declaration, field, generic, type, function, parameter, projection and capability record words (`List`, `Iface`, `Ext`, `Decl`, `FieldList`, `Gen`, `Ty`, `Func`, `Param`, `Proj`, `Cap`), the `DeclTag`/`TypeTag`/`Prim` tags and the `Mode`, `Resumption`, `Pass`, `Grade`, `GenKind`, `Fields` and `ProjKind` codes; issue path nodes (`PathWord`, `PathKind`, `Side`), `IssueKind`, the `Found`/`Adapt` scratch words and strides; `Class` is a `pub enum` in `util.ves` shared by `check.ves` and `version.ves`. The version parser's `ErrorKind`, `Pos`, `Ascii`, `Bump`, `Side` and `Issue`, the digit and UTF-8 bounds; `schema_version()`, `semantics_version()`, `i64_min()` and `i64_max()` became consts, and an `is_nominal` helper replaces the repeated struct-or-enum test | `tests/port_bench.rs` cases, port only, 10 rounds, callgrind: 54.985 M -> 54.981 M Ir (-0.007%) |

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

- A `pub const` can't be imported from another module: `use self::term::{REF_A}`
  fails with E0111 ("module self::term has no item"). The pact port keeps
  its zero-argument tag functions for shared tags and declares per-module
  constants (ASCII bytes, hash multipliers) where they are used.
- Named constants in a hot byte loop can cost a little: in the `.BTF.ext`
  access-string parser (`parse_u32`), writing `ASCII_0`/`ASCII_9`/`U32_MAX`
  instead of literals costs about 14 K Ir per warm `ext` call (1.5%). The
  constants are single literals, so the inliner's fold should make them free;
  it does not reach these comparisons yet.
- A zero-argument code function in a struct-literal argument of an inlined
  callee is not free: in Aura's `slots.ves`, `record(inout s, lim,
  op(ev_slot_bound(), slot, 0, 0, 0))` cost 24 M Ir more than the literal
  (+1.7% on `abba measure 200`), while the same functions in `if` tests and
  in `plan.ves`'s `emit(.., op(op_create(), ..))` were free. A module
  `const` in the same place costs nothing, so Aura's recorded event tags
  are constants in `slots.ves` and `widgets.ves`.
- A tuple of constants is not free either: prism-h3's encoder picking all
  three N bits at once with `let (a, b, c) = if sensitive { (X, Y, Z) }
  else { (0, 0, 0) }` cost 0.9% Ir on `encode`; one `u8` and two shifts
  cost nothing.
- Subtracting a negative constant is not free: Serde's Grisu cached-power
  index `(gamma - CACHED_POW10_FIRST_E) * 80 / 2126` with
  `CACHED_POW10_FIRST_E = -1087` cost 3.2% Ir on `f64_encode`, where
  `gamma + CACHED_POW10_E_BIAS` (1087) costs nothing. The port names the
  positive bias. A unary minus on a constant (`-EXPONENT_OFFSET`) is
  avoided the same way.
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
- A `pub const` imported into another module is not folded: each use is a
  call. In the resonance graph loop the imported record sizes cost +0.035%
  Ir until they became members of a shared value enum (`Words`), which fold
  to literals like local consts do. Share cross-module numbers as enum
  members.
