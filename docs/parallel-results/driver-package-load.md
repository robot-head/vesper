# The driver's package load in Vesper, slice 1: assembly

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1986 (part of hd_step group 1, #1896, and Stage 2 retirement,
#1397). Built on #1984's step table (`LoadPackage` = 70).

## Summary

- The Vesper driver (`crates/vesper-hybrid/modules/driver`) now assembles
  the package it loads. `LoadPackage` reads the package graph with the Rust
  loader (`packages::read_for_target`: manifests, the dependency closure,
  `src/` walks, `[package] vsys` and `objects`) **without parsing its
  modules**, and hands the graph to the driver. The driver's
  `assembled_words` (`src/assemble.ves`) parses, resolves and renames every
  module with `@vesper/check-names-packages` (#1178), and the new host call
  `hd_assembled` reads the renamed modules, the qualified-name map and the
  assembler's diagnostics back into the session's `packages::Assembled`
  (`src/swap/assemble.rs`).
- What stays in Rust for now: the loader's I/O and manifest checks
  (slice 2), sugar expansion (slice 3, after `@vesper/sugar` #1953), and
  every graph the Vesper assembler does not cover. A graph with manifest
  errors, an oversized source, or a module with a parse diagnostic is loaded
  again by `packages::load_for_target`, which reports those exactly as
  before. That includes every package that uses sugar, since sugar only
  appears in modules the core parser rejects. `--include-tests` loads stay
  in Rust too, since the assembler parses with the ordinary parser.
- **Differential: 0 differences** on all 193 Loom packages of the corpus
  (`examples/`, `packages/`, `crates/`, `tests/fixtures/`, the loader's
  accept and reject fixtures included): 188 are assembled in Vesper, 5 go
  back to Rust. `vesper check` (and `check --json` on the package fixtures)
  prints byte-for-byte what the default compiler prints, and at
  `VESPER_HYBRID_STATS=time` the hybrid compares every assembled package
  with `packages::load_for_target`'s (`module ==`, qualified names,
  diagnostics): `0 differ`.
- **Speed: 1.33x (95% CI 1.27..1.40)**, under the 1.50x bar. This is an ABBA
  over 6 rounds of the whole package load (Rust read plus Vesper assembly
  and read-back, against Rust read plus Rust parse and assembly), on every
  6th corpus package the driver assembles (32 packages), release,
  `RUSTFLAGS="-C target-cpu=native"`. A first version that kept the Rust
  loader's parse measured 2.0x, because it parsed every module twice. The
  load is a small part of `vesper check`, whose wall clock does not move.

## What changed

| Area | Files |
| --- | --- |
| Loader split (behaviour-neutral for every other caller) | `src/packages.rs` (`read_for_target`, `Loading`), `src/packages/sugar.rs` (`ModuleSugar::is_plain`) |
| Assembler outputs for the driver | `packages/vesper-check-names-packages/src/{lib,state,load}.ves`: diagnostics as rows, parse diagnostic count |
| The AST writer, shared | `crates/vesper-hybrid/modules/parse/src/words.ves` (`put_ast` moved out of `lib.ves`, unchanged) |
| Driver | `crates/vesper-hybrid/modules/driver/src/{assemble,lib}.ves`, `Loom.toml`, `port/driver.rs` |
| Host and read-back | `crates/vesper-hybrid/src/driver.rs` (`LoadPackage`, `hd_assembled`), `src/swap/assemble.rs`, `src/parse_words.rs` (`read_module`), `src/hybrid.rs` (re-export) |
| Test | `crates/vesper-hybrid/tests/driver_packages.rs` |
| Bazel | `BUILD.bazel` (`check_names_packages_package`, `std_collections_package`), `crates/vesper-hybrid/BUILD.bazel` |

The swap is `assemble` (an open row, so it runs in `VESPER_HYBRID_SWAPS=all`
and not in `gated`). `VESPER_HYBRID_ASSEMBLE=0` turns it off, which is the
ABBA's "off" side. `VESPER_HYBRID_STATS=1` prints `assemble: package load
<ms> ms`, the time the timing compares.

## Reproduce

```text
cargo build --release --bin vesper
VESPER_PORT_COMPILER=$PWD/target/release/vesper \
VESPER_DEFAULT_COMPILER=$PWD/target/release/vesper \
  cargo test --release -p vesper-hybrid --test driver_packages -- --nocapture
DRIVER_PACKAGES_SPEED=1 DRIVER_PACKAGES_SPEED_STRIDE=6 DRIVER_PACKAGES_ROUNDS=6 \
RUSTFLAGS="-C target-cpu=native" VESPER_PORT_COMPILER=$PWD/target/release/vesper \
  cargo test --release -p vesper-hybrid --test driver_packages package_load_speed \
  -- --ignored --nocapture
```

Bazel runs the differential on every 4th package
(`VESPER_DRIVER_PACKAGES_STRIDE=4`).

## What is left

1. **Slice 2: the loader's I/O and manifest checks in Vesper**
   (`Loader::package`): `Loom.toml` through `@vesper-std/toml`, the
   dependency closure and its E0114/E0115 errors, the `src/` walks (OS and
   WASI source directories), `[package] vsys` and `objects`. The driver then
   encodes the assembler's input itself, and the graph never crosses.
2. **Slice 3: sugar expansion** (`Loader::expand_sugars`) once
   `@vesper/sugar` lands, and the parse-diagnostic fallback goes away.
3. `--include-tests` loads, once the assembler can parse test-only
   declarations.
