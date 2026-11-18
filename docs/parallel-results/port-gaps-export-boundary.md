# Port gaps lane 8: the export boundary

Claim #991, PR #1003. Base `07e7d4e` (origin/main), with main merged again
at `6630e95` before merging. The assignment is in
[08-export-boundary.md](../agent-prompts/port-gaps/08-export-boundary.md).

## What changed

1. **`@export` directives come from the root module.** Packages load their
   files in sorted path order (`src/packages.rs`), so "file 0" is `lib.ves`
   only when no file sorts before it. `src/main.rs` read directives from
   file 0, so `// @export abi 3` in `lib.ves` was ignored without a word
   (Aura gap 1, Quarry gap 4). `Assembled` now records `root_module` (the
   file of `src/lib.ves` or `src/main.ves`) and `root_package_files`, and
   `export_directives_source` keeps the root module's text.
   - `// @export abi N` in another file of the root package still applies,
     with a warning that it belongs in the root module. That is how Aura
     (`apply.ves`), Quarry (`check.ves`), BBR (`types.ves`) and Ignis
     (`d3d12.ves`) keep their bindings until lane 9 moves the line.
   - `instance` and `vsys` directives name items, so outside the root
     module they are now an error instead of being ignored.
   - Directives in dependencies are still ignored. They belong to the
     dependency's own build (review of #921).
2. **`Option` and `Result` lent in place: `// @export abi 3 options`.**
   `vesper-port-abi` gains `OptionMirror<C>` and `ResultMirror<C, D>`. Each
   is the tag word (`None`/`Ok` is 0, `Some`/`Err` is 1) followed by the
   payload cell, so `1 + words` (Option) or `1 + max` (Result) words, as
   in the Vesper layout. A payload cell is the payload's mirror, a full
   word, a `Word` or a `Words`: the cells struct fields already use. These
   are all plain words, so any bits are a value, and the wrappers do no
   conversion. Full-word scalars, `Word` and `Words` now implement
   `Mirror`, which lets mirrors nest (`OptionMirror<OptionMirror<Word<i32>>>`).
   With the option, `in`/`inout` `Option`/`Result` parameters of copy
   payloads are lent as `&`/`&mut` mirrors, and results come back as the
   mirror. The binding asserts each mirror's size and `WORDS`.
   - It is opt-in because it changes the wrapper signatures. Every
     committed v3 binding (BBR, Aura, Quarry, Ignis, Beacon, the pilot's
     gauge) stays byte for byte. The object is identical with and without
     the option (`option_mirrors_change_only_the_binding`).
   - Struct fields of `Option`/`Result` type stay `Words` cells.
3. **`SeqRef::as_slice()`** for the packed scalars (a sealed `Packed`
   trait: `u8 u32 u64 i64 usize isize f64`, the types `with_slice` already
   lends in place). It borrows the Vesper buffer as `&[T]`.
   `SeqRef::to_vec()` is now one copy for those types, not one read per
   element. This is the ABI crate's share of Prism gap 6. Returning field
   ranges instead of copies is a port change.

Docs: `docs/maintenance/porting-a-crate.md` (v3 section and the directives
paragraph), manuscript §16.6 (two sentences: the `options` word, and where
directives are read from), and the `native::export` module docs.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed. The
bootstrap-stage1 workflow ran on this branch at `977f87d` (run
[44256872281](https://github.com/robot-head/vesper/actions/runs/44256872281)).
Both jobs succeeded: "Stage 1 reproducibility and cross-backend ratchet",
which checks the fresh `--reproduce --outcome` digests against
`bootstrap/stage1.lock`, and "Stage 0 two-seed build and Path B". The
merge of main after that brought in no Stage 1 changes from this lane.

## Measurements

Setup: a cloud container with 4 vCPUs (Intel Xeon @ 2.30 GHz), Linux 6.18,
Rust 1.97.0 `--release`. Ports were built by their harness with
`optimize = true`. Instruction counts are from `valgrind --tool=callgrind`
on the whole process. The port rewrites below are local and uncommitted
(lane 9 owns port changes). Their diffs are in the project files under
`port-gaps/`.

### Repros

| Repro | Before | After |
|---|---|---|
| `src/lib.ves` with `// @export abi 3` plus `src/a.ves` (Aura gap 1, Quarry gap 4) | `vesper.native.export-abi/1` | `vesper.native.export-abi/3` |
| Aura and Quarry with the directive moved into `lib.ves` | v2 (directive ignored) | same committed binding as today, no bless |
| `tests/fixtures/native_export/options.ves.in`: `state_observe(inout State, Option[Rtt])` + `rtt_of(in State) -> Option[Rtt]`, one call of each, wrapper plus Vesper code | 116 instructions | 113 instructions |
| `SeqRef<u8>::to_vec`, 1 KiB | 428 instructions | 326 instructions |
| `SeqRef<u8>::to_vec`, 16 or 64 bytes | 218-226 | 212-213 (the allocation dominates) |
| `SeqRef<u8>::as_slice` (new) | n/a | 22 instructions, no allocation |

The options repro went through every wrapper end to end in a scratch crate
that links the fixture's object: Option and Result in and out, `inout
Option`, a nested Option, a converted enum payload, and Results whose two
payloads differ in size.

### Ports

| Port | Setup | Before | After |
|---|---|---|---|
| BBR | `ack_path_abba measure 20000`, instructions (Rust: 21.97 M) | 51.73 M (2.354x) | 51.53 M (2.345x), `abba 3 options` + `on_ack` building the `OptionMirror`s |
| BBR | `ack_path_abba abba <rust> <vesper> 16 200000`, four runs each | 2.61-2.77x, 2.40-3.08x, 2.49-2.77x, 2.68-3.09x | 2.51-2.85x, 2.31-2.93x, 2.74-2.87x, 2.65-2.91x |
| Prism | `abba measure 20`, instructions (Rust: 53.31 M) | 84.26 M (1.581x) | 84.16 M (1.579x) with the new `to_vec` only; 82.72 M (1.552x) with the decoder's `head()` viewing names, values, method and target through `as_slice` |
| Prism | `abba compare ... 15 200`, decode | 1.83x (1.70-2.00), 1.83x (1.77-1.91) | 1.86x (1.71-1.97), 1.88x (1.83-1.95) with `to_vec` only |
| Aura | `abba measure 20`, instructions, port only | 184.12 M | 182.92 M (-0.65%), the encode and audit wrappers and `focusable_order` reading `Query` items and values through `as_slice` |
| Quarry | none | n/a | Quarry already carried the directive in `check.ves`, so the fix changes nothing at run time. The binding is unchanged with the line in `lib.ves`. |

The BBR and Prism wall-clock intervals overlap before and after. These
changes are below the noise of this container. "Before" for the Prism
`to_vec` row is this branch with the old per-element `to_vec` restored.

**BBR's 8.5% glue was mostly not the Option conversion.** Since #983 keeps
scalar locals in registers, the Rust optimizer already inlines the
`Abi::put` of `Option<RttSample>`/`Option<AckedPacket>` to a few stores.
Lending the mirrors saves about 10 instructions per ACK (0.4%). What is left
of the glue is building `repr::AckedPacket`/`PacketDeliveryState` from the
public Rust event types with `from_value` on every packet, and decoding the
`Fail` record into `Result<(), BbrFault>`. Both are inherent to an API that
takes Rust values, unless the public event types become the mirrors.

## What is left

- Lane 9: move `// @export abi 3` into `lib.ves` in Aura, Quarry, BBR and
  Ignis. The warning then goes away and the bindings stay the same, as
  shown above for Aura and Quarry. Ports may adopt `abi 3 options` and
  `as_slice` with the local rewrites in the project files.
- `Option`/`Result` struct fields as `OptionMirror` cells (they stay
  `Words`, converted on access). That would change existing mirror structs,
  so it needs its own opt-in.
- `Fail` records still decode through `Abi::get`, and tuples and copy
  enums other than Option and Result still convert.
- `SeqRef::as_slice` covers only packed scalars. A `SeqRef` of a mirrored
  struct could view `&[repr::T]` if handle field views returned mirrors.
- The options fixture is `options.ves.in`, not `.ves`. The self-hosted
  parser's repository corpus (`tests/self_host_front_end.rs`) walks every
  `.ves` file, and `packages/vesper-compiler` does not parse `pub` items
  yet. The fixture can become `.ves` once it does.
