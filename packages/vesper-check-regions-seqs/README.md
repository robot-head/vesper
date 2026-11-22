# @vesper/check-regions-seqs

The checker's region, sequence, string, array, tiled and vector rules in
Vesper (self-hosting wave SH2, lane 9, #1181), on
[`@vesper/check-data`](../vesper-check-data/README.md). Rust stays the
default checker; nothing here replaces Rust code.

The package keeps the census data design: types are `u32` ids in the
hash-consed arena, names are interned symbols, and diagnostics are rows of
one text buffer.

## What is ported

| File | Rust | Vesper |
|---|---|---|
| `src/regions.ves` | `src/regions.rs` type half: `region_row`, `box_type`, `watermark_type`, `row_label`, `box_parts`, `watermark_label`, `is_handle_type`, `normalize`, `validate_type`, `display`, `source_label`, `mismatch` | same names (`box_label` for `box_parts`, `region_display` for `display`) |
| `src/seqs.ves` | `src/seq.rs`: `is_seq_type`, `is_slice_type`, `elem`, `mentions_slice`, `holds_sequence`, `resolve` and the `SeqOp` properties (`fallible`, `mutates`, `receiver_pass`, `byte_scan`, `has_receiver`, `place_primitive`, `with_arity`, `name`, `Debug`), the growing operations of `fip_allocation`, `check_world` (E1520); `src/strings.rs`: `is_string_type`, `is_str_type`, `is_text`, `mentions_str`, `resolve`; `src/arrays.rs`: `element` | an operation is a `u32` code (`SeqOp` 1-41, `Wide` 64 + index, `StrOp` 100 + order); method names are one interned table (`SeqNames`) |
| `src/tiled.ves` | `src/tiled.rs`: `is_seq_type`, `seq_elem`, `seq_adt`, `hot_elem_bytes`, `hot_vector_shape`, `check_world_adts` (E1501, E1502, E1503) | `is_tiled_seq` for `is_seq_type` |
| `src/vector.ves` | `src/vector.rs`: `Elem`, `Shape` (`new`, `bits`, `lanes_per_chunk`, `required_profile`, `Display`), `is_vector_spelling`, `shape_of`, `type_error`, `layout_words`, `Profile::from_name`, `profile_of`, `isa_row_entry` (E1404) | `Shape` is a copy struct; `elem` 0 means none |
| `src/lib.ves` | | the differential entries and the lane's input section (`read_sigs`) |

Not yet ported (the next part of #1181): the checker methods of
`src/{regions,seq,strings,arrays,tiled,vector}/check.rs` and the expression
passes of `regions::check_world`, `strings::check_world`,
`arrays::check_world` and `vector::check_world`. They walk the typed AST,
so they come with an AST input and the hybrid swap.

## Input

Lane 4's encoder writes names, types and the `World` tables. This lane's
world task appends its own section to the task words (not to `World`):
function signatures in name order (name, span, result, and each parameter's
name, pass, type and span), each ADT's span, and each field's temperature
(0 none, 1 hot, 2 cold) in `World::fields` order. `read_sigs` in
`src/lib.ves` decodes it into `Sigs`.

## Tests

`tests/self_host_check_regions_seqs.rs` runs the entries against the Rust
functions over every checked corpus world:

- `type_rules_match_rust`: every corpus type of each world and 24 mutants
  of its first 24 types (bad region arities and labels, source labels,
  `TiledSeq` rows, vector spellings, nested slices, `Isa` rows) through
  every type rule above, and `mismatch` of each normalized type against
  the previous one. E0200, E0792, E0797 and E1404 must fire.
- `world_rules_match_rust`: `seq::check_world` and
  `tiled::check_world_adts` on each world and three mutants that move
  `Slice[T]` into signatures and fields and annotate ADTs with tile widths
  and field temperatures. E1501, E1502, E1503 and E1520 must fire.
- `resolve_matches_rust`: `seq::resolve` of 79 callees on 11 receivers,
  with every property of the operation.
- `unoptimized_jit_agrees`: the JIT with the optimizer off agrees with it on.
- `speed_abba` (ignored, `CHECK_HARNESS_SPEED=1`): ABBA of the type and world tasks, with the decode timed alone.
