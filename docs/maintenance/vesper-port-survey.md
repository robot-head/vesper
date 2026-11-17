# Vesper port-readiness survey

Survey date: 2026-11-17, against `main` at 74195c0, with the open verified-codegen PRs #850 (front-end coverage) and #857 (generics and regions) read from their branches. Claim: #867.

This survey answers one question: what would it take to rewrite the workspace's Rust crates in Vesper source, and in what order? It changes no crate code.

The strategy it assumes was agreed in the project thread:

1. Freeze feature growth in Rust for every crate classified below as port-first or port-later. Rust gets bug fixes, thin OS/FFI shims and what the compiler itself needs.
2. Port whole crates, leaves first, in dependency order. Never keep two half implementations.
3. The crate's existing Rust tests and golden fixtures are the spec. The Vesper build has to pass them differentially against the Rust build.
4. Delete the Rust implementation once the differential suite passes.

## Summary

- **The language is the bottleneck, not the backend.** Today Vesper source has `u8`, `u32`, `u64`, `i64`, `f64`, `bool`, tuples, structs, enums, generics, static traits, closures, `Box[T]`, regions (`RegionBox`, `Watermark`), `TiledSeq`, effects and handlers, and loops only as tail recursion. It has no growable sequence, no byte buffer or slice, no string operations beyond literals, no map or set, no narrow integers (`i8`, `i16`, `i32`, `u16`, `usize`), no `f32`, and no dynamic dispatch. The self-hosted front end says so in its own header (`packages/vesper-compiler/src/lexer.ves`): its source is a function `source_byte(i)` and its CST is folded into a digest, because there is no buffer to hold either. Every crate below needs at least growable sequences and byte buffers. Most need maps and strings too.
- **There is no Vesper standard library yet.** No prelude, no `Option`/`Result` helpers, no collections, no formatting. Building one is the first step of the porting kit.
- **Ported library crates should target the native (Cranelift) backend first.** The verified backend matters for the bootstrap trust chain (the compiler and what it links). After #850, #857 and #859 it covers the scalar subset, structs, tuples, non-recursive enums and `match`, `f64`, abort-only `Fail[E]`, generics by monomorphization, and regions on Linux ELF. It still rejects closures, recursive enums, strings, resuming handlers, `f32`, FFI, vectors and heap memory on macOS and Windows. Library ports should not wait on it. Each verified-backend milestone widens what the compiler-adjacent crates (wave P4) can rely on.
- **70 crates, 335k lines of Rust source, 4,035 `#[test]` functions.** 16 crates (62k lines) are port-first, 13 are port-later (115k lines), 14 are compiler toolchain crates that move with self-hosting Stage 2 (53k lines), and 27 stay in Rust behind an `extern` boundary (106k lines). The root `vesper` package (134k lines in `src/`) is the compiler and is out of scope here: it is Stage 2 of the bootstrap.
- **Three crates already have port specs.** Strata (`vesper-alloc`, #847: `tests/collections_conformance.rs` and the CacheBTree/ChunkSeq traces), Quarry (#848: `crates/vesper-quarry/spec/planner-ddl.md` with `tests/fixtures/planner/{cases,golden}.txt`), and Tensor (#846: `crates/vesper-tensor/fixtures/reference/` with `tests/reference_fixtures.rs`). They lead wave P1, but each needs the porting kit and the F1 to F4 features below before a single line can move.

## What the compiler supports today

| Feature | Language / checker | Native backend (Cranelift) | Verified backend (main + #850/#857) |
|---|---|---|---|
| `u8 u32 u64 i64 bool` | yes | yes | yes |
| `i8 i16 i32 u16 usize` | no | no | no |
| `f64` | yes | yes | yes (IEEE `+ - * /`, compare; no `%`) |
| `f32` | no | no | no |
| Tuples, structs | yes | yes | yes, as words on the stack |
| Enums and `match` | yes | yes | non-recursive only |
| Recursive data | `Box[T]` | `Box[T]` handle | rejected |
| Generics | yes | yes (instances) | by monomorphization (#857) |
| Traits | static elaboration only (§7); bounds needing dictionary dispatch are rejected | elaborated away | elaborated away |
| Closures | yes | yes, not across the native entry | rejected |
| Loops | tail recursion | self tail call is a loop | self tail call is a loop |
| Strings | literals, `String` is affine | no operations | rejected |
| Sequences, buffers, slices | `TiledSeq`; `RegionSlice`/`SlotArena` in source are open (#588) | `TiledSeq` handle | dynamic tuple indexing only (#850) |
| Maps, sets | no | no | no |
| Regions | yes | yes | Linux ELF bump heap (#857) |
| Effects and handlers | full | handler ABI (#836) | abort-only `Fail[E]` (#859); `Region`/`SyntaxMemory` handlers still refused |
| FFI | Bridge `.vsys`, `Ffi[Lib]` (`src/ffi_lowering.rs`); linear extern types open (#611) | yes | no |
| Concurrency | Pulse fibers in the evaluator; linkable runtime open (#585) | session executables open (#585) | no |
| SIMD | `Vector` shapes (§14); `VVec` open (#587) | yes | no |
| Calling Vesper from Rust | JIT entry for tests and fuzzing (`native_coverage.rs`) | no stable exported C ABI | no |

The coverage inventory from #850 shows how far the verified backend is from the Stage 1 front end: stage1 compiles 153 of 159 functions, the checker 285 of 295, and the parser 280 of 423. The parser's 148 refusals are almost all the `SyntaxMemory` and `Region` effects, which V3 is closing.

## Method

For every crate under `crates/` the survey counted, in `src/` only: lines, `#[test]` functions (including `tests/`), `unsafe`, FFI and assembly sites (`libc::`, `windows_sys::`, `extern "C"`, `asm!`, `core::arch`), concurrency (`std::thread`, `Arc`, `Mutex`, atomics), collection types (`HashMap`, `BTreeMap`, `VecDeque`, ...), string use, trait declarations and `dyn`, float and narrow-integer types, and filesystem, network and process I/O. It then read each crate's crate-level docs and external dependencies. These are grep counts, so they overstate (a `usize` index is not a protocol field) and understate (a macro can hide a loop). They are good enough to rank crates, not to estimate effort to the week. Re-run them with the script in the appendix.

The classes are:

- **port-first**: pure logic or sans-IO state machines with few or no external dependencies. Moving them only needs the language features F1 to F6.
- **port-later**: mostly logic, but large, or built on another port, or needing dynamic dispatch, concurrency, `f32` determinism or SIMD. Their OS or crypto edges stay behind an `extern` boundary.
- **toolchain**: crates the compiler and CLI are built from. They move with self-hosting Stage 2, because a ported crate the compiler depends on only helps once the compiler itself can link Vesper code (feature F10). The Stage 1 front end in `packages/vesper-compiler` already overlaps `vesper-syntax`.
- **stays-Rust**: syscalls, GPU, hypervisor, Wasm hosts, JIT hosts, and every crate that wraps vetted cryptography. Hand-rolled crypto is forbidden in this repository, so `vesper-vault`, `vesper-warp-crypto`, `vesper-warp-tls`, `vesper-loom-trust` and `vesper-loom-tuf` stay Rust for as long as their vetted libraries are Rust. These crates get a stable `extern` surface (a `.vsys` file) instead of a port.

## Crate inventory

The "Needs" column uses: `map` collections, `str` string handling, `int` narrow integers or `usize`, `f32`/`f64` floats, `dyn` traits or trait objects, `conc` threads, atomics or shared state, `simd` architecture intrinsics, `os` FFI, syscalls or file/network I/O. "FFI/asm" counts FFI and intrinsic sites.

| Crate | Class | Wave | src LOC | tests | `unsafe` | FFI/asm | Needs | Notes |
|---|---|---|--:|--:|--:|--:|---|---|
| `vesper-quarry` | port-first | P1 | 12,436 | 92 | 1 | 0 | map str int f64 dyn conc os | Quarry #848 planner/DDL golden spec; planner+catalog first, buffer pool/O_DIRECT behind extern |
| `vesper-alloc` | port-first | P1 | 8,400 | 223 | 111 | 18 | str int dyn conc os | Strata #847 traces; collections port, ShardSlab/VirtualArena OS pages stay behind extern |
| `vesper-tensor` | port-first | P1 | 7,317 | 89 | 33 | 8 | map str int f32 conc simd os | Tensor #846 reference fixtures; formats/BPE/sampling first, memmap behind extern; needs f32/u16 |
| `vesper-warp-wheel` | port-first | P1 | 1,555 | 17 | 0 | 0 | int dyn conc | pilot; generic payloads need generics + fixed arrays |
| `vesper-beacon` | port-first | P1 | 919 | 12 | 0 | 0 | int dyn | pilot; no_std, no deps |
| `vesper-simpulse` | port-first | P1 | 712 | 18 | 0 | 0 | map | pilot; seeded scheduler model; serde only for traces |
| `vesper-ignis` | port-first | P2 | 5,084 | 69 | 1 | 0 | map str int dyn conc | render-graph hazard planner, no deps |
| `vesper-grammar` | port-first | P2 | 4,976 | 125 | 1 | 0 | int | binary parsers, no deps; needs byte slices |
| `vesper-crucible` | port-first | P2 | 4,120 | 59 | 0 | 0 | map str int dyn | choice tapes, shrinking; after Strata + SimPulse |
| `vesper-warp-tcp` | port-first | P2 | 3,926 | 63 | 0 | 0 | map int dyn | sans-IO RFC 9293; needs u16 and byte buffers |
| `vesper-warp-bbr` | port-first | P2 | 3,050 | 52 | 0 | 0 | map int | sans-IO state machine, no deps |
| `vesper-aura` | port-first | P2 | 2,491 | 28 | 1 | 0 | map int dyn | keyed view runtime, no deps; Backend contract needs trait objects |
| `vesper-prism` | port-first | P2 | 2,478 | 55 | 0 | 0 | map str int dyn | HTTP/2 framing, no deps |
| `vesper-semver` | port-first | P2 | 2,157 | 48 | 0 | 0 | map str | interface diff; semver crate is the only real dep |
| `vesper-loom-solver` | port-first | P2 | 1,285 | 36 | 0 | 0 | map str | deterministic resolver; maps |
| `vesper-loom-calculus` | port-first | P2 | 1,032 | 29 | 1 | 0 | map str | manifest validation; TOML/JSON parsing via ported Serde or extern |
| `vesper-kinetic` | port-later | P3 | 15,867 | 170 | 168 | 11 | str int f32 dyn conc simd | f32/f64 bit-determinism, threaded RigidWorld; Cranelift JIT part stays |
| `vesper-warp` | port-later | P3 | 13,882 | 149 | 0 | 0 | map int dyn os | QUIC core; crypto via vetted extern (warp-crypto), traits, maps |
| `vesper-ebpf` | port-later | P3 | 12,375 | 82 | 11 | 44 | map str int dyn conc os | codec/verifier/BTF portable; object-crate ELF + bpf() syscalls extern |
| `vesper-silicon` | port-later | P3 | 12,317 | 141 | 2 | 0 | map str int dyn conc os | devicetree/SVD/AIA models portable; Cranelift stack analysis stays |
| `vesper-flux` | port-later | P3 | 11,918 | 91 | 0 | 0 | map str int dyn conc os | DBSP over Quarry; compression/TLS stay extern |
| `vesper-chorus` | port-later | P3 | 9,861 | 83 | 1 | 0 | map str int dyn conc os | 12 traits, maps, Raft; after Warp |
| `vesper-aegis` | port-later | P3 | 9,329 | 94 | 0 | 0 | map str int dyn conc | quote/report parsing portable; X.509/webpki/COSE verification stays extern |
| `vesper-prism-h3` | port-later | P3 | 8,392 | 93 | 0 | 0 | map str int dyn | HTTP/3+QPACK on top of ported Warp |
| `vesper-serde` | port-later | P3 | 7,153 | 142 | 21 | 28 | str int f64 dyn simd os | scalar codecs portable; SIMD JSON needs source vectors (§14) or stays extern |
| `vesper-pact` | port-later | P3 | 4,593 | 54 | 0 | 0 | map str int dyn | trait resolution; recursive terms and maps; compiler-adjacent |
| `vesper-resonance` | port-later | P3 | 4,080 | 80 | 25 | 18 | map str int f32 dyn conc simd os | DSP; f32, realtime rings (rtrb) and JIT stay extern |
| `vesper-bench` | port-later | P3 | 4,066 | 77 | 0 | 0 | int dyn conc | ABBA statistics portable; timers extern |
| `vesper-loom-audit` | port-later | P3 | 973 | 16 | 1 | 0 | map str int | catalog audit logic; small |
| `vesper-sim-session` | toolchain | P4 | 9,490 | 65 | 0 | 0 | map str int f64 dyn os | drives the root compiler |
| `vesper-doc` | toolchain | P4 | 8,218 | 66 | 5 | 0 | map str int | Codex |
| `vesper-core` | toolchain | P4 | 7,091 | 81 | 0 | 0 | map str int | compiler AST and types; moves with self-hosting Stage 2 |
| `vesper-syntax` | toolchain | P4 | 6,437 | 94 | 0 | 0 | map str int conc | seed lexer/CST (logos, rowan); Stage 1 front end already exists in Vesper |
| `vesper-forge` | toolchain | P4 | 5,566 | 65 | 0 | 0 | map str int conc | macro hygiene/provenance gate |
| `vesper-highlight` | toolchain | P4 | 4,711 | 39 | 7 | 4 | map str int f32 os | lexical part ports; rasterizing (tiny-skia, rustybuzz, png) stays extern |
| `vesper-pact-binding` | toolchain | P4 | 3,438 | 0 | 0 | 0 | map str int dyn | Pact DTOs |
| `vesper-fmt` | toolchain | P4 | 2,221 | 54 | 0 | 0 | str int | formatter |
| `vesper-lsp` | toolchain | P4 | 1,763 | 24 | 0 | 0 | map str int conc | LSP framing |
| `vesper-lean-syntax` | toolchain | P4 | 1,496 | 20 | 0 | 0 | map str | Lean printer |
| `vesper-lint` | toolchain | P4 | 1,269 | 10 | 0 | 0 | map str int | lint facts |
| `vesper-span` | toolchain | P4 | 630 | 14 | 0 | 0 | int | tiny, could pilot, but every compiler crate uses it |
| `vesper-diagnostics` | toolchain | P4 | 592 | 14 | 0 | 0 | int | ariadne rendering |
| `vesper-pact-bridge` | toolchain | P4 | 176 | 34 | 0 | 0 |  | glue to the root compiler; goes when the compiler does |
| `vesper-aether` | stays-Rust | - | 9,812 | 125 | 48 | 122 | map str int dyn conc simd os | KVM/UFFD |
| `vesper-chrono` | stays-Rust | - | 7,998 | 79 | 15 | 24 | map str int dyn os | perf/ptrace/iced-x86 |
| `vesper-ignis-vulkan` | stays-Rust | - | 7,925 | 41 | 126 | 16 | map str int f32 dyn conc os | ash/Vulkan |
| `vesper-fiber-abi` | stays-Rust | - | 7,108 | 82 | 111 | 169 | map str int dyn conc simd os | asm stacks, unwinding, Cranelift |
| `vesper-beacon-arrow` | stays-Rust | - | 6,509 | 49 | 0 | 0 | map str int f64 dyn | arrow-rs, tonic, tokio |
| `vesper-pulse` | stays-Rust | - | 6,101 | 61 | 67 | 84 | map str int dyn conc os | io_uring/epoll/kqueue/IOCP drivers; portable scheduler could port later |
| `vesper-migrate` | stays-Rust | - | 5,075 | 45 | 1 | 2 | map str int os | C/Rust lifter on lang-c and syn |
| `vesper-nexus` | stays-Rust | - | 4,600 | 69 | 1 | 0 | map str int dyn conc | wasmtime/WIT host |
| `vesper-ffi` | stays-Rust | - | 4,442 | 48 | 33 | 76 | map str int conc simd os | Bridge runtime and layouts |
| `vesper-loom-cas` | stays-Rust | - | 4,166 | 90 | 3 | 13 | map str int dyn os | BLAKE3 + reflink filesystem store; Merkle naming could port later |
| `vesper-forge-runner` | stays-Rust | - | 3,842 | 43 | 0 | 4 | map str int conc simd os | wasmtime macro execution |
| `vesper-warp-umem` | stays-Rust | - | 3,456 | 27 | 24 | 74 | map int dyn conc os | AF_XDP rings, mmap |
| `vesper-aura-gtk` | stays-Rust | - | 3,431 | 7 | 0 | 0 | map str int dyn conc | GTK 4 |
| `vesper-fusion` | stays-Rust | - | 3,255 | 27 | 0 | 0 | map str int f32 dyn | egglog engine; rule sets could move once egglog has a Vesper binding |
| `vesper-bench-pmu` | stays-Rust | - | 3,220 | 28 | 47 | 68 | map str int dyn conc simd os | perf_event PMU |
| `vesper-vault` | stays-Rust | - | 3,218 | 62 | 12 | 12 | map str int dyn conc simd os | vetted crypto libs (libcrux, RustCrypto) |
| `vesper-loom-sandbox` | stays-Rust | - | 2,951 | 63 | 0 | 0 | map str int dyn os | wasmtime host |
| `vesper-loom-tuf` | stays-Rust | - | 2,617 | 21 | 2 | 5 | map str dyn conc os | tough/TUF crypto |
| `vesper-warp-shard` | stays-Rust | - | 2,486 | 20 | 27 | 6 | int simd os | cache-line tiles + SIMD CID lookup; revisit once source vectors land |
| `vesper-gpu-compute` | stays-Rust | - | 2,385 | 23 | 51 | 0 | map str int f32 conc os | ash/naga |
| `vesper-warp-tls` | stays-Rust | - | 2,382 | 27 | 0 | 0 | map int dyn conc | rustls |
| `vesper-loom-build` | stays-Rust | - | 2,077 | 25 | 1 | 6 | map str conc os | sandbox driver |
| `vesper-simd` | stays-Rust | - | 2,044 | 20 | 50 | 65 | int dyn simd os | Cranelift JIT host |
| `vesper-codegen-verified` | stays-Rust | - | 1,590 | 35 | 0 | 0 | map str os | DDC auditor; keep in Rust on purpose as the diverse path |
| `vesper-warp-crypto` | stays-Rust | - | 1,562 | 21 | 0 | 0 | int | vetted RustCrypto AEADs (no hand-rolled crypto) |
| `vesper-loom-trust` | stays-Rust | - | 854 | 18 | 0 | 0 | map str | Ed25519 quorum (vetted crypto) |
| `vesper-loom-admission` | stays-Rust | - | 413 | 12 | 0 | 3 | os | glue over CAS + trust |

## Missing compiler and runtime features

Each feature gets its own lane, ahead of the ports that need it. The order is the order to build them in.

| # | Feature | Unblocks | Status on main |
|---|---|---|---|
| F1 | **Growable sequences and byte buffers**: `Vec[T]`, `Bytes`, borrowed slices `Slice[T]` with bounds-checked indexing, region-backed and heap-backed | every crate | `TiledSeq` and `RegionBox` only; source `RegionSlice`/`SlotArena` open in #588 |
| F2 | **Strings**: UTF-8 `String`/`Str` with byte access, slicing, comparison, building, and integer/float formatting | all protocol and text crates: Prism, Beacon, Semver, Loom calculus/solver, Serde, Quarry catalog, every toolchain crate | literals only |
| F3 | **Narrow integers** `i8 i16 i32 u16 usize` with checked, wrapping and bit operations, shifts, rotates, byte-order conversion | Warp TCP/BBR/wheel, Grammar, Serde, Tensor (`u16` for bf16/fp16), Quarry pages, eBPF codec | `u8 u32 u64 i64` only |
| F4 | **`f32`** with deterministic semantics, plus `sqrt`, `fma`, `exp`, `ln`, `min`/`max` and bit casts for `f32` and `f64` | Tensor, Kinetic, Resonance, Fusion rule sets, Highlight palettes | `f64` arithmetic and comparison only |
| F5 | **Ordered maps and sets** (`Map[K, V]`, `Set[K]`, deterministic iteration order) built on F1 in the std library, plus a hash map for hot paths | Quarry, Flux, Chorus, Silicon, Loom solver, Aura, Ignis, Crucible, every toolchain crate | none |
| F6 | **Iteration over sequences**: a `for` over F1 slices or guaranteed tail calls between functions (not only self calls) in the native backend, so loops over buffers do not grow the stack | every port, for performance parity | self tail calls only |
| F7 | **Trait objects or dictionary-passing generics**: bounded generic calls the static elaboration rejects today, and `dyn Trait` for pluggable backends | Aura `Backend`, Flux sources and sinks, Chorus transports, Beacon exporters, Crucible strategies, Warp UMEM ring abstraction | static elaboration only |
| F8 | **Export ABI**: compile a Vesper module to an object or static library whose functions have a stable C ABI and a generated Rust binding, linkable by Cargo and Bazel | the differential harness; any Rust crate or the CLI that depends on a ported crate | JIT entry for tests only |
| F9 | **Richer Bridge imports**: real pointers and slices instead of tuples, opaque linear handles, callbacks from Rust into Vesper | every port-later crate with an OS or crypto edge | `.vsys` scalars, wire structs, `CSlice` as tuples; linear extern types open in #611 |
| F10 | **Linkable Pulse runtime** in executables and libraries: fibers, timers and I/O completions from compiled code | Warp, Prism H3, Chorus, Flux, Quarry I/O, Beacon export | open in #585 |
| F11 | **Source SIMD at full width**: `VVec`, masks, gathers, byte shuffles | Serde SIMD JSON, Warp shard CID lookup, Quarry scans, Tensor kernels | `Vector` shapes; `VVec` open in #587 |
| F12 | **Shared-memory concurrency**: atomics and a scoped worker pool for compiled code | Kinetic threaded `RigidWorld`, Quarry latches/SSI, Flux multi-worker executor | none in source |

For the verified backend, the compiler-adjacent crates of wave P4 additionally need closures, recursive enums, strings, F1 and F5 in that backend. Those follow from the Stage 2 plan rather than from this survey.

## Port order

Each wave starts when the features it needs have merged. Inside a wave, crates are independent and can run as parallel lanes. Within a lane the order is: freeze Rust features, write the Vesper port against the existing tests, run them differentially, delete the Rust implementation, keep the tests.

### Wave P0: porting kit and prerequisites

F1, F2, F3, F5, F6 and F8, plus the std library and harness below. Nothing ports before this wave lands. F4 can run in parallel because only Tensor in P1 needs it.

### Wave P1: pilots, then the three crates with specs

1. **`vesper-warp-wheel`** (1.6k lines, 17 tests, no dependencies) and **`vesper-beacon`** (0.9k lines, `no_std`, no dependencies). Pilots to prove the kit end to end: port, run differentially, delete, and have a dependent Rust crate (`vesper-warp`) link the Vesper build through F8.
2. **`vesper-simpulse`** (0.7k lines). Seeded scheduler model that Crucible, Chorus and Flux build on.
3. **`vesper-alloc`** (Strata collections). Port the collections and traces from #847. `ShardSlab` and the `VirtualArena` page reservations stay behind a `.vsys` shim. The Vesper collections then become part of the std library from F1/F5.
4. **`vesper-quarry`**, planner and catalog half, against the #848 golden files. The buffer pool, `O_DIRECT` device and latches wait for wave P3.
5. **`vesper-tensor`**, formats, BPE and sampling against the #846 reference fixtures. Needs F4. `memmap` stays behind an extern.

### Wave P2: pure protocol and logic crates

`vesper-warp-bbr`, `vesper-warp-tcp`, `vesper-prism`, `vesper-grammar`, `vesper-semver`, `vesper-loom-calculus`, `vesper-loom-solver`, `vesper-ignis` (render-graph planner), `vesper-aura` (needs F7 for its `Backend` contract, otherwise pure) and `vesper-crucible` (after Strata and SimPulse).

### Wave P3: larger systems on top of earlier ports

Needs F7, F9, F10 and, per crate, F11 or F12.

- Networking: `vesper-warp` (crypto through `vesper-warp-crypto` and `vesper-warp-tls` behind extern), then `vesper-prism-h3` and `vesper-chorus`.
- Data: the rest of `vesper-quarry`, then `vesper-flux` (compression and TLS stay extern), and `vesper-serde` (scalar codecs first; SIMD JSON after F11).
- Numeric: `vesper-kinetic` and `vesper-resonance` (bit-determinism of F4 is the acceptance bar; their Cranelift and realtime-ring parts stay extern).
- Analysis and models: `vesper-silicon`, `vesper-ebpf`, `vesper-aegis`, `vesper-bench`, `vesper-pact`, `vesper-loom-audit`. Each keeps its OS, ELF-object or certificate-verification edge in Rust.

### Wave P4: toolchain crates with self-hosting Stage 2

`vesper-core`, `vesper-syntax`, `vesper-span`, `vesper-diagnostics`, `vesper-fmt`, `vesper-lint`, `vesper-lsp`, `vesper-doc`, `vesper-highlight` (lexical part), `vesper-lean-syntax`, `vesper-pact-binding`, `vesper-forge`, `vesper-sim-session` and `vesper-pact-bridge`. They move as the compiler moves, and the front end in `packages/vesper-compiler` is where `vesper-syntax` and the checker's share of `vesper-core` go.

### Stays in Rust behind `extern`

`vesper-pulse` (drivers), `vesper-fiber-abi`, `vesper-ffi`, `vesper-warp-umem`, `vesper-warp-shard`, `vesper-warp-crypto`, `vesper-warp-tls`, `vesper-vault`, `vesper-ignis-vulkan`, `vesper-gpu-compute`, `vesper-aura-gtk`, `vesper-aether`, `vesper-chrono`, `vesper-bench-pmu`, `vesper-simd`, `vesper-loom-sandbox`, `vesper-nexus`, `vesper-forge-runner`, `vesper-loom-cas`, `vesper-loom-trust`, `vesper-loom-admission`, `vesper-loom-build`, `vesper-loom-tuf`, `vesper-beacon-arrow`, `vesper-migrate`, `vesper-fusion` and `vesper-codegen-verified`.

Some of these have a portable core worth revisiting later: Pulse's scheduler and frame storage, Warp shard's tile table once F11 lands, Loom CAS's Merkle naming, and Fusion's rule sets if egglog ever gets a Vesper binding. `vesper-codegen-verified` should stay Rust on purpose: it is the DDC auditor, and its value is being a diverse implementation.

## The porting kit

### 1. Vesper std library

A Loom package, `packages/vesper-std`, imported by every port:

- `Option`, `Result` and the combinators the Rust code uses most (`map`, `and_then`, `unwrap_or`, `?` already exists for `Fail[E]`).
- F1 sequences and slices, F5 ordered map and set (seeded by the Strata port), F2 strings and formatting.
- Byte-order helpers, checked and wrapping arithmetic on all F3 widths, and a small `Ord`/`Eq`/`Hash` trait set with derived instances.
- Deterministic by default: no hash-order iteration in public results, so differential tests compare byte for byte.

### 2. `extern` boundary

Bridge `.vsys` files are the only way a port reaches Rust.

- Each stays-Rust crate that a port needs gets one `.vsys` file, owned by that crate, describing the narrow surface it exposes (for example `vesper-vault`'s AEAD seal and open, `vesper-pulse`'s submit and complete). The Rust side is a thin `extern "C"` shim crate next to it.
- Calls carry the `Ffi[Lib]` capability, so the checker shows exactly which Vesper functions can reach Rust, and pure ports stay `eff []`.
- F9 adds slices, opaque linear handles and callbacks, which most of these surfaces need.

### 3. Differential harness

The goal is to reuse every Rust test unchanged.

- For a crate `vesper-x`, the port lives in `packages/x` (Vesper source) and is built with F8 into a static library with generated Rust bindings, in a crate `vesper-x-port`.
- The Rust crate gains a Cargo feature, `vesper-port`. With it on, the crate's public functions forward to the Vesper build. The existing `tests/` and golden fixtures run unchanged against both builds. CI runs both and compares outputs where a test prints or hashes them.
- In-crate unit tests (`#[cfg(test)]` modules in `src/`) often reach private state, for example `vesper-beacon/src/histogram.rs` and Strata's slab and arena tests, so forwarding the public API does not cover them. Before deletion, each one is either translated into a differential case through the public API or ported as a Vesper-side test of the port's own internals. The lane lists which treatment each test got, and an unported unit test blocks deletion.
- For crates with traces or golden files (Strata, Quarry, Tensor), the harness also compares the trace or golden output byte for byte.
- Crucible can drive both builds with the same choice tape to find divergences the tests miss. Its native-coverage JIT entry already calls compiled Vesper from Rust.
- Deletion: once both builds pass the whole suite on all four required CI platforms, the lane removes the Rust implementation, makes the forwarding crate the only build, and keeps the tests.

### 4. Freeze rule

When a crate's port lane opens, its Rust code accepts bug fixes only. Every new feature for that crate goes into the Vesper port. A one-line note in the crate README records the freeze and links the lane's issue.

## Risks

- **Performance.** Every buffer loop is recursion until F6 lands, and the verified backend keeps no values in registers across slot rows. Hot crates (Serde, Quarry scans, Kinetic, Tensor) need benchmarks in the differential harness (`vesper-bench` ABBA runs) before their Rust code is deleted.
- **Float determinism.** Kinetic and Tensor promise bit-identical results. F4 has to define one `f32` semantics on every target, and their fixtures are the acceptance test.
- **Build graph.** F8 links Vesper objects into Cargo and Bazel builds on Linux, macOS and Windows. Windows needs COFF output from the native backend for libraries, not only executables (#360 covers executables).
- **Two implementations during a lane.** The whole-crate rule limits this to one lane at a time per crate. The freeze rule keeps the Rust side from moving underneath the port.
- **Grep counts.** The numbers in the inventory are a ranking aid. Each lane should re-measure its crate before it starts.

## Appendix: reproducing the counts

From the repository root (GNU grep):

```sh
for c in crates/*/; do
  n=$(basename "$c"); s="$c/src"; [ -d "$s" ] || continue
  loc=$(find "$s" -name '*.rs' | xargs cat | wc -l)
  tests=$(grep -rh '#\[test\]' "$c" | wc -l)
  unsafe=$(grep -rhow unsafe "$s" | wc -l)
  ffi=$(grep -rhoE 'libc::|windows_sys::|extern "C"|asm!|core::arch|std::arch' "$s" | wc -l)
  conc=$(grep -rhoE 'std::thread|Arc<|Mutex<|Atomic[A-Z]' "$s" | wc -l)
  maps=$(grep -rhoE 'HashMap|HashSet|BTreeMap|BTreeSet|VecDeque|BinaryHeap' "$s" | wc -l)
  f32=$(grep -rhow f32 "$s" | wc -l)
  narrow=$(grep -rhowE 'i8|i16|i32|u16|usize|isize|u128|i128' "$s" | wc -l)
  echo "$n loc=$loc tests=$tests unsafe=$unsafe ffi=$ffi conc=$conc maps=$maps f32=$f32 narrow=$narrow"
done
```
