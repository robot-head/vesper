# Port gaps lane 9: the ports after the port gaps wave

Claim #992. Base `72209eb` (origin/main with lanes 1-8 of the
[port gaps wave](../agent-prompts/port-gaps/README.md) and #983/#1001 merged).
Prompt: [09-rescore.md](../agent-prompts/port-gaps/09-rescore.md).

**Result.** The compiler wave alone (main before this lane's port changes)
moved every port's instruction ratio down from the
[port parity scoreboard](port-parity.md): BBR 3.54x to 2.25x, Quarry 1.96x to
1.61x, Prism 1.84x (tests) to 1.37x (benchmark), warp-wheel 3.37x to 1.77x
wall clock. The port rewrites of this lane add Prism 1.37x to **1.22x**
instructions (decode 1.35x to 1.18x wall clock) and Aura 0.90x to **0.86x**.
The others gain 0-1%. Aura still meets the parity bar. No other port does,
so Rust stays the default build of every port but Aura, and no Rust code is
deleted.

## Setup

- Machine: cloud x86-64 container, 4 vCPU Intel Xeon @ 2.30 GHz, Linux 6.18,
  Rust 1.97 `--release`. Every number in this report comes from one session
  on one machine.
- Compiler: release `vesper` built at `72209eb`, passed to every port build
  as `VESPER_PORT_COMPILER` (`optimize = true`, the harness default).
- Arms: "before" is every port at `72209eb` unchanged; "after" is this
  branch. The Rust arm of each port is the same example built without
  `vesper-port`. Aura's Rust arm is `examples/abba.rs` built at `cee8b1a`,
  the last Rust commit, as `bench-abba.sh` does.
- Wall clock: each port's committed ABBA driver, run twice per arm in the
  order before, after, before, after.
  - BBR: `ack_path_abba abba <rust> <port> 16 200000`
  - Quarry: `planner_abba driver <rust> <port>`
  - warp-tcp: `abba drive codec|transfer <rust> <port>`
  - Ignis: `abba compare <rust> <port> 15 100`
  - Prism, Aura: `abba compare <rust> <port> 15 200`
  - warp-wheel: `tests/abba.rs --ignored` (both builds in one process)
- Instructions: callgrind `Ir`, whole process, with the commands of the
  earlier lanes: BBR `measure 20000`, Quarry `window 20`, Prism and Ignis
  `measure 20`, Aura `measure 40`, warp-tcp `worker codec` with 64 requests
  and `worker transfer` with 4.

**Noise.** warp-tcp's and Ignis's port binaries are byte-identical in the two
arms (`md5sum`), so their before/after spread is this machine's noise: the
warp-tcp codec measured 0.93x, 1.23x, 0.89x and 1.17x on one binary, and
every Ignis workload moved by up to 0.3x. Instruction counts are the firmer
result; wall-clock changes below 10% are inside the noise.

## What changed in the ports

| Port | Change | From |
|---|---|---|
| warp-wheel | `shl`/`shr` use `wrapping_shl_u64`/`wrapping_shr_u64`; `low_bit` is `trailing_zeros_u64`. The de Bruijn table (`l_ctz`) and constant are gone, and the binding loses `l_ctz`. | lane 5 (#988) |
| BBR | `// @export abi 3 options`: `on_ack` lends `Option<RttSample>`/`Option<AckedPacket>` as `OptionMirror`s. | lane 8 (#991) |
| Prism | `find_not_in` for field values, `tchar` runs and targets; `extend_ascii_lowercase` for name copies; `eq_ignore_ascii_case` for `eq_lit`/`eq_fold`; `find_either` in `line_break`; the decoder's `head()` views names, values, method and target with `SeqRef::as_slice`. | lanes 4 and 8 |
| Quarry | The lexer's word scan and identifier check use `find_not_in`, and identifiers are lowercased with `extend_ascii_lowercase`. `free_d` is gone: a dropped `D` frees its sequences. `// @export abi 3` moved from `check.ves` into `lib.ves`. | lanes 2, 4, 8 |
| Aura | `insert_at`, `remove_at`, `insert_edge`, `remove_edge` and `move_edge` are one `try_copy_within` each. A commit publishes its staged state with `swap(inout rec.st, inout rec.stage)` instead of a copy. `do_reconciler_prepare` builds one `Prepared` and replaces it on the tree path. The encode and audit wrappers read `Query` items through `as_slice`. `// @export abi 3` moved from `apply.ves` into `lib.ves`. | lanes 2, 3, 8 |
| Ignis, warp-tcp | None: the lane reports name no workaround the new compiler lets them undo. Ignis and BBR join their files into one root module, so their directive was never misplaced. | |

The port changes are 23 files, +228/-265 lines. All port tests pass
(`cargo test --release -p <port> --features vesper-port`, and
`-p vesper-aura`), including the leak tests (`port_memory`, Aura's
`port_frees_every_allocation_it_makes`) and the divergence builds
(`--features vesper-port-divergent` still applies every deliberate edit).

## Instructions

| Port | Rust Ir | Port Ir, before | Port Ir, after | Ratio before | Ratio after |
|---|---:|---:|---:|---:|---:|
| warp-wheel | | | | not measured | not measured |
| BBR | 21.97 M | 49.45 M | 49.26 M | 2.25x | **2.24x** |
| Quarry | 19.36 M | 31.08 M | 30.77 M | 1.61x | **1.59x** |
| Prism | 53.31 M | 73.07 M | 65.01 M | 1.37x | **1.22x** |
| Ignis | 116.17 M | 165.60 M | 165.60 M | 1.43x | 1.43x |
| warp-tcp codec | 33.94 M | 24.30 M | 24.30 M | 0.72x | 0.72x |
| warp-tcp transfer | 8.74 M | 12.82 M | 12.82 M | 1.47x | 1.47x |
| Aura | 362.86 M | 327.26 M | 313.35 M | 0.90x | **0.86x** |

## Wall clock

Medians of port/Rust, with the 95% interval of the median in parentheses
(Quarry's driver prints the median regression only). Two runs per arm.

| Port, workload | Before, run 1 | Before, run 2 | After, run 1 | After, run 2 |
|---|---|---|---|---|
| warp-wheel | 1.77x (1.76-1.79) | 1.77x (1.72-1.80) | 1.73x (1.69-1.75) | 1.77x (1.75-1.79) |
| BBR | (2.28-2.86) | (2.60-3.13) | (2.32-2.87) | (2.62-2.74) |
| Quarry | 1.82x | 1.88x | 1.84x | 1.75x |
| warp-tcp codec | 0.93x (0.88-0.98) | 0.89x (0.78-1.01) | 1.23x (1.04-1.27) | 1.17x (1.11-1.24) |
| warp-tcp transfer | 1.55x (1.30-1.58) | 2.02x (1.75-2.51) | 1.45x (1.34-1.72) | 1.99x (1.86-2.34) |
| Ignis build 64 / 256 | 2.45x / 2.36x | 2.60x / 2.55x | 2.09x / 2.68x | 2.46x / 2.38x |
| Ignis plan 64 / 256 | 1.38x / 0.67x | 1.39x / 0.69x | 1.36x / 0.71x | 1.37x / 0.67x |
| Ignis lower 64 / 256 | 3.22x / 2.82x | 3.01x / 2.55x | 3.24x / 2.70x | 3.13x / 2.69x |
| Prism decode | 1.33x (1.22-1.40) | 1.37x (1.31-1.44) | **1.18x** (1.06-1.24) | **1.18x** (1.05-1.46) |
| Prism route | 2.61x (2.48-2.73) | 2.54x (2.48-2.66) | 2.61x (2.16-2.75) | 2.68x (2.47-3.06) |
| Prism encode | 2.03x (1.81-2.13) | 1.99x (1.92-2.14) | **1.79x** (1.44-2.17) | **1.79x** (1.62-2.13) |
| Prism connection | 1.55x (1.39-1.61) | 1.54x (1.47-1.61) | **1.33x** (1.13-1.61) | **1.38x** (1.20-1.44) |
| Aura one-text | 0.32x (0.29-0.34) | 0.36x (0.30-0.39) | 0.30x (0.27-0.33) | 0.28x (0.27-0.33) |
| Aura reorder | 0.37x (0.35-0.43) | 0.44x (0.38-0.50) | 0.40x (0.36-0.44) | 0.37x (0.34-0.45) |
| Aura encode-audit | 0.54x (0.52-0.60) | 0.57x (0.54-0.67) | 0.54x (0.47-0.60) | 0.51x (0.50-0.58) |

warp-tcp and Ignis rows compare identical binaries (see Noise).

## Where the remaining time goes

Callgrind of the "after" ports, whole process (allocator = glibc
`malloc.c`):

| Port | Port Ir | Allocator Ir | Rust Ir | Allocator Ir |
|---|---:|---:|---:|---:|
| Quarry `window 20` | 30.79 M | 13.32 M | 19.47 M | 11.28 M |
| Prism `measure 20` | 65.00 M | 10.88 M | 53.31 M | 7.99 M |
| Ignis `profile lower 64 2000` | 376.88 M | 109.02 M | 145.03 M | 53.88 M |
| BBR `measure 20000` | 49.26 M | 0 | 21.97 M | 0 |

- The allocator explains little of the gap except in Ignis `lower`, which
  allocates twice the bytes the Rust build does (the sequences of each
  lowered barrier list, then the Rust wrapper's `Vec` of
  `NativeBackendBarrier`). In Quarry and Prism the allocator adds 2-3 M
  instructions against gaps of 11-12 M.
- The rest is generated code. BBR's four hottest Vesper functions
  (`vesper_function_126`, `_56`, `ack_step`, `_106`) are 48% of its
  instructions. Prism's largest single function (`vesper_function_10`,
  31%) is the decoder's head scan. Ignis spends 31% in `lower_plan` itself.
  The lane reports give the code-level reasons (below).

## Parity

The bar ([port parity wave](../agent-prompts/port-parity/README.md)): every
workload's 95% interval at or below 1.00x, with every port test, leak test
and divergence check green.

- **Aura** meets it again on every workload (0.28x-0.54x wall clock, 0.86x
  instructions). It is already the only build (#979).
- **warp-tcp codec** is at 0.72x instructions, but its wall clock straddles
  1.0x on this machine, and the transfer workload is 1.47x. Not met.
- **Ignis plan 256** is at 0.67-0.71x, from a better algorithm than the Rust
  build's (the parity report does not count it). The other five Ignis
  workloads are 1.36x-3.24x. Not met.
- warp-wheel 1.77x, BBR 2.24x (instructions), Quarry 1.59x, Prism 1.22x:
  not met.

Owner's decision needed: none. No port newly meets the bar.

## Remaining compiler gaps

Each has a repro in the report named.

| Gap | Ports | Report |
|---|---|---|
| Empty sequences allocate a header (Rust's `Vec::new()` does not) | Quarry, Prism, Ignis (allocator above) | port-gaps-seq-runtime |
| `Write` destinations (`q.f = T { .. }`) built in a temporary and copied | BBR, Ignis | port-gaps-codegen-redundancy |
| Values reaching their consumer through a block parameter (inlined `return`, `if` between constructors) get no home | BBR, Ignis | port-gaps-codegen-redundancy |
| `in` aggregate parameters spilled to a slot at entry (`sum3`) | warp-wheel, BBR | port-gaps-codegen-redundancy |
| A loop that calls hoists at most one sequence header | Prism, Aura, Ignis | port-gaps-codegen-redundancy |
| Inactive payload words of a variant are zeroed (needs a no-read rule) | Ignis `check`, Quarry | port-gaps-codegen-redundancy |
| Inliner charges callees before simplification; no partial inlining (`mul_div_sat`, `TexEntry` cmp, `WheelCore::unlink`) | BBR, Ignis, warp-wheel | port-gaps-inliner-reach |
| Generic copies that apply operators stay calls; recursive functions stay calls | Ignis, Quarry | port-gaps-inliner-reach |
| Short scans (under 16 bytes) run the scalar loop; `find_not_in` constants rematerialized per step | Prism, Quarry | port-gaps-byte-classes |
| No in-place ASCII lowercase of a slice of the receiver (`E0301`) | Quarry catalog path | port-gaps-byte-classes |
| `x << s` under `if s < 64` still needs a handler (no range facts) | warp-wheel, BBR | port-gaps-bit-builtins |
| Drops: `Box` payloads, partial moves, closure captures, `Wild` parts, temporaries across blocks | Aura, Quarry (correctness, not time) | port-gaps-drops |
| `Option`/`Result` struct fields convert at the boundary; `Fail` records decode through `Abi::get` | BBR glue | port-gaps-export-boundary |
| Wider vectors (AVX2) | Prism, warp-tcp | deferred: Cranelift 0.125 has no 256-bit types |

## Tests

- `cargo test --release -p vesper-warp-wheel -p vesper-warp-bbr -p vesper-prism
  -p vesper-warp-tcp -p vesper-ignis -p vesper-quarry --features vesper-port`
  (one crate at a time) and `cargo test --release -p vesper-aura`: all pass.
- `cargo build --release --features vesper-port-divergent` for each of the
  seven ports: every deliberate edit still applies.
- `cargo +1.100.0 clippy` on the changed crates is clean.
- The full suite, macOS, Windows and Bazel run in CI.
