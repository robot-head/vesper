# Self-hosting wave SH5: finishing the backends, then the driver and tools

> Plan written 2026-11-21 against `main` at `a656954` plus the open SH4 pull
> requests (the SH5-plan part of #1292, SH4 lane 18). The
> [manuscript](../../manuscript/README.md) is the sole specification. The
> crate-by-crate inventory and later waves are in
> [self-hosting-plan.md](../../maintenance/self-hosting-plan.md). Confirm live
> ownership on the issue board before acting.

**The owner asked to stack SH5 on SH4** (2026-11-21, "then stack sh5 on top
of sh4"). SH4's lanes are still open: lane 1's swap point is #1309, and the
family lanes 6-17 are #1294-#1306, #1308, #1311, #1315 and #1319, each with
its own next parts still to come. This plan does not take those parts. It
takes what the SH4 lanes reported they **cannot** finish inside their own
packages (new infrastructure, compiler gaps, build time), plus the
candidates SH4 named, plus the planned SH5 work: the driver and the tools.

SH4 lane 18 (#1292) was to write this plan after a single-build re-measure of
the merged SH4 swaps. The plan part is here. The re-measure moves to lane 20,
which closes #1292 with the SH4 scoreboard.

Every port keeps Rust as the default build under the standing owner rule.

## What SH4 left (read from each lane's PR, 2026-11-21)

| SH4 lane | PR | Left inside the lane (not SH5's) | Blocked on (SH5 lane) |
|---|---|---|---|
| 1 swap point | #1309 | review of the family hunks | entity numbers as bare `u64` (2) |
| 2 swap inputs | #1307 | per-function program cache | one shared world, text answers, 1.4 µs per call (10) |
| 4 callbacks | #1317 | | `Option` results in memory, header re-reads, impure-call homes (7) |
| 5 optimizer | #1312 | | whole build 1.72x not 2x: `World::mode` (8); debug-compiler optimizer time (9) |
| 6 scalar | #1302 | part 3 (hybrid install) | `Read`/`Bind`/`Write` and the raising arms need the walk's maps (1); no 128-bit layout (6) |
| 7 calls | #1306 | entries, export-set boundary | result stores, `inout` spills, Fail record (1) |
| 8 aggregates | #1301 | addressing, patterns, equality, fip | 128-bit layout (6) |
| 9 seq | #1295 | part 2b ops, part 3 | `.vsys` import (3); `handle`/`copy` names (4); 128-bit (6) |
| 10 control | #1298 | hybrid install | closure and clause bodies, `perform`, `fail_exit`, `dyn`, session parks need lent walk state or re-entry (1) |
| 11 numeric | #1294, #1315 | floats, regions, atomics, `parallel_for` | `get_name`, `fvpromote_low`/`fvdemote` (2); `.vsys` import (3) |
| 12 export | #1299 | `plan_with`, glue | method call on a call result (4); `Str` `as_bytes().len()` (6) |
| 13 FFI | #1296 | the `Rewriter` (part 2), hybrid install | `isolated(mpk)` helpers need function bytes, `sext`/`uext` (2); `while k < n {` (4) |
| 14 freestanding | #1305 | entry compile, Windows objects, hook swap | float check and Silicon's analysis read `ir::Function` (2) |
| 15 wasm | #1311 | handlers, host frames, evidence | component wrapping (11); parser gaps (4); `Err(e)?`, E0404 (6) |
| 16 eBPF | #1308 | stack bound, hybrid swap | optimizer 460 s debug (9); `ChunkSeq::last`, `a && if` (4, 6) |
| 17 checks | #1319 | coverage, batch, traces | optimizer 27 min debug (9); shifts, division, `!` (5); stack overflow (6); reading Cranelift tables (2) |

## Lanes

Claims: lanes 1-20 are #1324-#1343 (lane n is issue 1323+n).

### Backend infrastructure

| Lane | Prompt | Claim | What | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|
| 1 | [Lent walk state and re-entry into Rust](01-walk-state.md) | #1324 | `vars`, homes, registers, split locals and edge infos lent to a hook; Rust emits a sub-expression on request | #1309 | new `src/codegen_swap/walk.rs` and its constants, the walk-state hunks of `src/native.rs`, new `@vesper/codegen-base` files, `self_host_walk_state*` tests, report |
| 2 | [Cranelift shim extras](02-shim-extras.md) | #1325 | `sext`/`uext`, function bytes, declaration lookup, `fvpromote_low`/`fvdemote`, typed handles, function read-back | #1309 | new calls in `crates/vesper-cranelift-sys/**` and `packages/vesper-cranelift/**`, `sh5_*` shim tests, report |

### Compiler gaps (S22)

| Lane | Prompt | Claim | Gap | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|
| 3 | [`.vsys` in packages, extern types in `use`](03-vsys-packages.md) | #1326 | packages cannot import a dependency's `.vsys`; E0111 on extern types | main | `.vsys` hunks of `src/packages*` and `src/package_graph.rs`, the E0111 hunk, `tests/vsys_package_imports*.rs`, report |
| 4 | [Parser gaps](04-parser-gaps.md) | #1327 | `while k < n {`, `&& if`, `&& match`, `handle` `;`, calls on call results, `copy`/`affine`, keyword cascades | #1264 | those hunks of the Rust parser and lexer with their front v2 and AST mirrors, `tests/sh5_parser_gaps*.rs`, report |
| 5 | [Integer totality](05-integer-totality.md) | #1328 | guarded shifts, guarded division, integer `!` | main | shift/division fact hunks, integer `!`, `tests/integer_totality*.rs`, report |
| 6 | [Checker, JIT and wide-scalar gaps](06-check-jit-gaps.md) | #1329 | `Err(e)?` in the JIT, E0404, `ChunkSeq::last`, E1531, debug lowering stack depth, 128-bit check-data and check-layout | main | one commit per fix, the 128-bit hunks of check-data and check-layout, `tests/sh5_check_gaps*.rs`, report |

### Build time and native code

| Lane | Prompt | Claim | What | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|
| 7 | [Option results, header hoisting, impure-call homes](07-native-after-callbacks.md) | #1330 | #1317's three gaps (#1271's 0.5% bar) | #1317 | enum scalar replacement in `src/native_aggregates.rs`, the home-rule hunk of `register_locals`, the closure hunk of `src/bounds_hoist.rs`, tests, report |
| 8 | [`World::mode` speed](08-world-mode-speed.md) | #1331 | the 2x build target | #1312 | `World::mode` hunks of `src/check*` and `src/ir_check.rs`, tests, report |
| 9 | [Optimizer build time](09-optimizer-build-time.md) | #1332 | 27 min / 460 s in debug compilers; wasm and `emit --emit ir` callers | #1312 | `src/ir_opt_policy.rs`, the wasm and emit call sites, tests, report |
| 10 | [Shared world and binary answers](10-swap-crossing.md) | #1333 | each hybrid library decodes its own world; text answers | #1307 | the hybrid's cross-library and answer-encoding hunks, `swap_crossing*` tests, report |
| 11 | [WIT component wrapping](11-wit-component.md) | #1334 | wit-component and wit-parser behind the wasm port (owner: measure first, then port) | #1311 | new `packages/vesper-wit-component/**`, the hybrid wasm wrapping hunk, tests, report |

### Driver and tools (the planned SH5)

| Lane | Prompt | Claim | Rust replaced | Stacks on | Exclusive boundary |
|---|---|---|---|---|---|
| 12 | [The driver](12-driver.md) | #1335 | `src/main.rs`, `src/lib_commands.rs`, `src/package_commands.rs`, the build path | main | new `packages/vesper-driver/**`, hybrid module `driver`, `tests/self_host_driver*.rs`, report |
| 13 | [The other CLI commands](13-cli-commands.md) | #1336 | `src/*_commands.rs` command layers | lane 12 | new `packages/vesper-driver-commands/**`, hybrid dispatch rows, tests, report |
| 14 | [Test runner, doctests, Anvil](14-test-runner.md) | #1337 | `src/test_runner.rs`, `src/doctest*.rs`, `crates/vesper-test` | main | new `packages/vesper-test-runner/**`, hybrid module `test_runner`, tests, report |
| 15 | [`vesper-doc`](15-vesper-doc.md) | #1338 | `crates/vesper-doc`, `src/tooling/doc*`, `src/doc_commands.rs` | main | new `packages/vesper-doc/**`, hybrid module `doc`, tests, report |
| 16 | [LSP transport and memo tables](16-lsp-core.md) | #1339 | `crates/vesper-lsp`, `src/database` (salsa, S8) | main | new `packages/vesper-lsp/**` and `packages/vesper-memo/**`, hybrid module `lsp_core`, tests, report |
| 17 | [LSP features](17-lsp-features.md) | #1340 | `src/lsp/**`, `src/tooling/{frontend,recovery}` | lane 16 | new `packages/vesper-lsp-features/**`, hybrid module `lsp_features`, tests, report |
| 18 | [Lean extraction](18-lean-extraction.md) | #1341 | `src/lean*.rs`, `src/verify_commands.rs`, `src/proof_commands.rs` | main | new `packages/vesper-lean-extract/**`, hybrid module `lean_extract`, tests, report |
| 19 | [`vesper-sim-session`](19-sim-session.md) | #1342 | `crates/vesper-sim-session` | main | new `packages/vesper-sim-session/**` (or the crate's `port/**`), tests, report |

### Planning

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 20 | [SH4 and SH5 scoreboard, SH6 plan](20-scoreboard-sh6-plan.md) | #1343 | last: lane 12 and SH4 lanes 6-17 have each merged a first swap | `docs/maintenance/self-hosting-plan.md` (SH4/SH5 results, SH6 sections), new `docs/agent-prompts/self-host-sh6/**`, the SH4 and SH5 scoreboards |

**Start order.** No code lane starts before the CI-trim PR #1322 (#1321)
merges. Then: lanes 3, 5, 6, 12, 14, 15, 16, 18 and 19 from `main`; lanes 1,
2, 4, 7, 8, 9, 10 and 11 stacked on their SH4/SH3 PR; lanes 13 and 17 after
the first PR of the lane they stack on; lane 20 last.

**Recommended merge order.**
- A stacked lane merges only after the PR it stacks on (and that PR's own
  bases) has merged. Merge `main` into the branch right before landing.
- Lanes 1 and 2 merge before the SH4 lanes adopt them (6, 7 and 10 adopt
  lane 1; 11, 13, 14 and 17 adopt lane 2). The SH4 lanes adopt in their own
  packages; SH5 lanes never edit SH4 packages.
- Lanes 3-6 merge in any order; each lists which SH4 workaround it frees.
- Lanes 8 and 9 both stack on #1312 and touch different files; merge `main`
  between them.
- Lanes 12-19 each add a module to `crates/vesper-hybrid`: merge `main` right
  before merging each, and serialize merges that touch root `Cargo.toml`.
- Lane 20 last.

## Rules for every lane

Every rule in [SH2's "Rules for every lane"](../self-host-sh2/README.md#rules-for-every-lane),
every [SH3 addition](../self-host-sh3/README.md#rules-for-every-lane) and every
[SH4 addition](../self-host-sh4/README.md#rules-for-every-lane) applies, with
these additions.

- **No CI on branches that cannot merge (owner, 2026-11-21, standing).**
  While the PR a lane stacks on is open, cancel every GitHub Actions run a
  push starts, right after the push (`gh api -X POST
  repos/robot-head/vesper/actions/runs/<id>/cancel` for each run of the
  branch). Push rarely. Test locally (targeted tests only) while stacked.
  Let CI and the bootstrap-stage1 workflow run once the base has merged and
  `main` is merged in, right before landing. Lanes from `main` run CI
  normally, but still push rarely: Actions is saturated and each push
  cancels the branch's previous run.
- **SH4 lanes keep their parts.** Each open SH4 lane still owns its
  package's remaining parts. An SH5 lane that unblocks one (lanes 1-6)
  delivers the infrastructure or fix, a test, and a note in its report of
  what the SH4 lane can now do. It does not edit the SH4 package.
- **Driver and tool lanes use the seed pattern.** The Rust command or crate
  stays the default; the Vesper package is installed in the hybrid build
  only, with a differential of exit codes, output bytes and files written.

## Not in this wave

- The remaining parts of SH4 lanes 1-17 (their own claims, #1275-#1291).
- SH3 lane 14's single-build re-measure of SH2 and SH3 (#1253).
- Stage 2 (SH6) and a Vesper-native backend (SH7).
- Deleting any Rust module or seed copy: an owner call per module at parity.
