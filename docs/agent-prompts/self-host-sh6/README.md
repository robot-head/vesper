# Rust-exit wave SH6: Stage 2, then every crate flipped

> Plan written 2026-11-21 against `main` at `af7dd900` (claims #1396-#1414;
> folds in SH5 lane 20, #1343). The [manuscript](../../manuscript/README.md)
> is the sole specification. The program, the flip bar and the irreducible
> minimum are in [rust-exit-plan.md](../../maintenance/rust-exit-plan.md);
> the crate-by-crate numbers are in the generated
> [census](../../maintenance/rust-census.md). Confirm live ownership on the
> issue board before acting.

**The owner's goal (2026-11-21): "Drive this until absolutely no rust code is
left except the bare minimum that is impossible to get rid of."** The census
counts 519k non-test lines of Rust (199k in the compiler and its own crates,
198k in libraries that already have a Vesper port, 85k in crates the port
survey had left in Rust) and 355k lines of Rust tests.

**The flip bar changed (owner, 2026-11-21):** a module's Rust is deleted once
its Vesper port has full function and is within 1.5x of Rust's speed;
optimization comes later. Lanes flip and delete on their own once the flip
gate passes. This replaces the 2026-11-17 rule (parity in both function and
speed, Rust kept until then).

**Bootstrap (owner, 2026-11-21):** once the Rust compiler is deleted, the
repository builds from a committed wasm seed, Zig-style (lane 2).

Claims: lane n is issue 1395+n.

## Lanes

### Program and the compiler

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 1 | [Scoreboard, census and the flip gate](01-scoreboard-flip-gate.md) | #1396 | now (folds SH5 lane 20, #1343; closes #1292 and #1343) | `scripts/rust-census.py`, `docs/maintenance/rust-census.md`, the scoreboard sections of `docs/maintenance/rust-exit-plan.md` and `docs/maintenance/self-hosting-plan.md`, new `scripts/ci/flip_gate.py` and `tests/flip_gate*.rs`, `docs/parallel-results/self-host-sh4-scoreboard.md`, `docs/parallel-results/self-host-sh5-scoreboard.md` |
| 2 | [Stage 2, the bootstrap seed and the Rust compiler's retirement](02-stage2-retirement.md) | #1397 | seed design now; the self-rebuild after SH5 lane 12's driver (#1367) merges | new `bootstrap/stage2*`, new `bootstrap/seed/**`, new `.github/workflows/bootstrap-stage2.yml`, the default-build hunks of `crates/vesper-hybrid/**` (serialize with every lane that adds a hybrid module), `tests/stage2*.rs`, the retirement section of `docs/maintenance/rust-exit-plan.md` |
| 3 | [Shrink the compiler's closure](03-compiler-closure.md) | #1398 | now | root `Cargo.toml` dependency hunks (serialize merges), the moved command and verifier modules and their dispatch rows in `src/main.rs`, `crates/vesper-tools/**`, `tests/compiler_closure*.rs`, report |

The compiler's own modules stay with the SH2-SH5 lanes that own them (see
the census's compiler table: every module has an owner). SH6 adds what those
lanes need to finish: the flip gate, Stage 2, and the retirement.

### Libraries that already have a port: finish, reach 1.5x, flip

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 4 | [Warp family](04-warp.md) | #1399 | now | `crates/vesper-warp`, `crates/vesper-warp-tcp`, `crates/vesper-warp-bbr`, `crates/vesper-warp-wheel`, `crates/vesper-warp-shard`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 5 | [Prism and Prism-H3](05-prism.md) | #1400 | now | `crates/vesper-prism`, `crates/vesper-prism-h3`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 6 | [Quarry and Flux](06-quarry-flux.md) | #1401 | now | `crates/vesper-quarry`, `crates/vesper-flux`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 7 | [Ignis, Grammar and Bench](07-ignis-grammar-bench.md) | #1402 | now | `crates/vesper-ignis`, `crates/vesper-grammar`, `crates/vesper-bench`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 8 | [Aegis](08-aegis.md) | #1403 | now | `crates/vesper-aegis`, `src/attestation_commands.rs` hunks that call it, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 9 | [Kinetic and Resonance](09-kinetic-resonance.md) | #1404 | now | `crates/vesper-kinetic`, `crates/vesper-resonance`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 10 | [Silicon and eBPF](10-silicon-ebpf.md) | #1405 | now | `crates/vesper-silicon`, `crates/vesper-ebpf`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 11 | [Serde, Tensor and Alloc](11-serde-tensor-alloc.md) | #1406 | now | `crates/vesper-serde`, `crates/vesper-tensor`, `crates/vesper-alloc`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |
| 12 | [Small libraries and Chorus](12-small-libs.md) | #1407 | now | `crates/vesper-crucible`, `crates/vesper-semver`, `crates/vesper-loom-calculus`, `crates/vesper-loom-audit`, `crates/vesper-beacon`, `crates/vesper-chorus`, the Rust left in `crates/vesper-pact/src`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report |

### Crates the survey left in Rust: no Rust between Vesper and C

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 13 | [Runtime glue on libc and syscalls](13-runtime-glue.md) | #1408 | now (part 1 is an audit) | `crates/vesper-std-host`, `crates/vesper-pulse`, `crates/vesper-ffi`, `crates/vesper-fiber-abi`, `crates/vesper-simd`, `crates/vesper-std-driver`, `packages/vesper-std/{fs,process,env}/**` `.vsys` hunks, report |
| 14 | [Tracing, PMU, AF_XDP and KVM on syscalls](14-os-tools.md) | #1409 | now (part 1 is an audit) | `crates/vesper-chrono`, `crates/vesper-bench-pmu`, `crates/vesper-warp-umem`, `crates/vesper-aether`, report |
| 15 | [Graphics and GUI on C APIs](15-graphics.md) | #1410 | now (part 1 is an audit) | `crates/vesper-aura-gtk`, `crates/vesper-ignis-vulkan`, `crates/vesper-gpu-compute`, the rasterizer modules of `crates/vesper-highlight`, report |
| 16 | [Wasm hosts and the Loom store](16-wasm-hosts-loom.md) | #1411 | now (part 1 is an audit) | `crates/vesper-loom-sandbox`, `crates/vesper-nexus`, `crates/vesper-forge-runner`, `crates/vesper-loom-cas`, `crates/vesper-loom-admission`, `crates/vesper-loom-build`, `crates/vesper-loom-tuf`, report |
| 17 | [One crypto and TLS shim](17-crypto-shim.md) | #1412 | now | new `crates/vesper-crypto-sys/**`, new `packages/vesper-crypto/**`, `crates/vesper-warp-crypto`, `crates/vesper-vault`, `crates/vesper-warp-tls`, `crates/vesper-loom-trust`, `tests/crypto_shim*.rs`, report |
| 18 | [Replace the Rust-only libraries](18-rust-libs.md) | #1413 | now | `crates/vesper-fusion`, `crates/vesper-beacon-arrow`, `crates/vesper-migrate`, `src/migrate_commands.rs` hunks, report |

### Tests

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 19 | [Rust tests to Vesper tests](19-tests-to-vesper.md) | #1414 | now (recipe); conversions follow each flip | new `tests/vesper/**` (or the location the recipe picks), new `scripts/convert-rust-tests*`, the test-runner hunks that run them, `docs/maintenance/porting-a-crate.md` (the test section), report |

**Start order.** Lanes 1, 3-12, 17 and 19 start now from `main`. Lanes 13-16
and 18 start now with their part 1 audit. Lane 2 starts the seed now and the
self-rebuild when SH5 lane 12's driver (#1367) merges. Nothing here stacks
on an open PR.

**Recommended merge order.**
- Lane 1's flip gate first; flips before it merges paste the hand-run recipe.
- Lane 17's shim before lanes 8 and 4 drop their crypto crates; lane 13's
  direct libc imports before lanes 6, 10 and 11 drop their OS boundaries.
- Lane 3 before lanes 9-12 delete (rather than seed) compiler-linked Rust.
- Every PR that touches root `Cargo.toml` or `crates/vesper-hybrid`: merge
  `main` right before merging, one at a time.
- Lane 2's retirement PR last, once every compiler row passes the gate.

## Rules for every lane

Every rule in [SH2's "Rules for every lane"](../self-host-sh2/README.md#rules-for-every-lane)
and the SH3, SH4 and SH5 additions apply, with these changes.

- **Flip at the bar, without asking.** Full function (0 differences, every
  test) and within 1.5x of Rust (upper end of the ABBA 95% interval): make
  the Vesper build the only build and delete the Rust in the same lane. Paste
  the flip gate's verdict into the PR.
- **Compiler-linked crates use the seed pattern** (#1102) until lane 3 moves
  them out of the compiler's closure or lane 2 retires the Rust compiler.
- **Tests move, never vanish.** A flip converts the crate's Rust tests (lane
  19's recipe) or keeps them as a differential against the last Rust commit.
- **No Rust between Vesper and C.** Where a crate wraps a C API or system
  calls, Vesper imports them through `.vsys`; Rust stays only as a shim over a
  vetted Rust-only library (Cranelift, crypto), listed in the plan's
  irreducible table.
- **No CI on branches that cannot merge** (owner, standing): cancel the runs
  right after such a push. Push rarely.

## Not in this wave

- The remaining parts of the SH2-SH5 lanes (their own claims).
- A Vesper-native backend (SH7): Cranelift stays behind its shim.
- The DDC auditor and the `src/bin/*42` oracles, which stay Rust on purpose
  (owner, 2026-11-21: "keep oracles in rust").
