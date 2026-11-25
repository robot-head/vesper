# CLIF passes port (#1829)

Ports the two Cranelift-IR passes, `src/native_vectorize.rs` and
`src/native_cold_outline.rs`, to Vesper packages. These are the first
CLIF-level ports, so the lane also adds a shared CLIF reader.

## Design

- `tests/common/clif_words.rs` encodes a Cranelift `ir::Function` as LE u64
  words (values, layout, every instruction with its own opcode and type codes,
  effects bits, condition, immediate, offset, mem flags, args, results and
  branch destinations). Codes are append only.
- `packages/vesper-clif` reads those words into tables. Every change goes
  through an edit function that updates the tables and also writes an edit
  script op (E_BLOCK, E_PARAM, E_PLACE, E_RETARGET, E_CLONE, E_INS, E_OUTLINE,
  E_SKIP). Later decisions therefore see the changed function, as in Rust.
- `clif_words::apply` replays the script with the same cursor and builder
  calls in the same order, so entity numbers match; it checks each new id.
- `packages/vesper-native-vectorize`: call-for-call port of plan, emit,
  plan_widen and emit_widen. Entry `vectorize_words` returns the loop count and
  the script.
- `packages/vesper-native-cold-outline`: all decisions in Vesper (CFG with
  Cranelift's predecessor order, dominators with the unreachable-block quirk,
  the loop forest in Cranelift's order, root classification, closed regions,
  weighing, slots, live-ins, rematerialized values, RPO block order). Rust's
  `apply_extraction` keeps the helper building (the Cloner's entity copying)
  and the stub; the Vesper model applies the same stub to its own tables for
  later rounds.
- `src/native_vectorize.rs` gains `install(hook)` / `run_rust`; nothing
  installs the hook by default. Both passes `capture()` their inputs for the
  differentials.

## Results

| Pass | Differential | Speed (release) |
| --- | --- | --- |
| vectorize | kernels 0/25 functions (22 loops); corpus 0/461 functions in 356 worlds | whole build with the Vesper pass 0.89-1.04x (about 1.0x); pass alone about 0.5x; hook end to end 2.5x (encode boundary, not gated, as in earlier flips) |
| cold outline | kernels 0/14 functions (4 regions); corpus 0/434 functions in 357 worlds (3 regions) | end to end 1.12x (1.10-1.18) |

Mutation checks: vectorizer gap 7 -> 6 gives 17 diffs; MAX_REGIONS 0 gives 4
diffs, so the differentials see changes.

Rust lines deleted: 0. Compiler modules stay in Rust until Stage 2 retirement
(same as bounds_hoist and ir_opt). The cold outliner's Cloner stays as the
applier.

## Gotchas

- Call the package `entry()` before installing a hook: the package compile
  runs the vectorizer, and re-entering the entry OnceLock deadlocks.
- Kernels for the cold-outline differential are read at runtime from
  `tests/native_cold_outline.rs` (Bazel data entry), not `include_str!`.
- The #1426 codegen-size lane edits these Rust files often; mirror its changes
  in the port.
