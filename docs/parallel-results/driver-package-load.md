# The driver's package load in Vesper

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1986 (part of hd_step group 1, #1896, and Stage 2 retirement,
#1397). Built on #1984's step table (`LoadPackage` = 70). Slice 1 (the
assembly) is #2033; slice 2 (the loader's I/O) is #2188; slice 2b (the
manifests and version matches) follows it.

## Slice 2b: manifests and version matches

- The driver decodes its manifests and matches dependency versions itself:
  the host steps `Manifest` and `VersionMatches` (72-73) and
  `packages::driver_host::DriverManifest` are gone.
- **Manifests**: `@vesper/loom-calculus` gets `loader::parse_manifest`,
  `vesper_loom_calculus::parse_manifest` in Vesper. It decodes the TOML
  with the port's `manifest_from_toml` (`toml::from_str::<Manifest>`, with
  its errors), reads the manifest's JSON back, encodes it as the
  validator's word stream (what `src/port.rs`'s `Enc::manifest` does in
  Rust: the SemVer versions and requirements through the port's
  `semver_text`, a `sys_deps` entry split at its first white space) and runs
  `validate_manifest`'s checks. It returns what the loader reads: the name,
  the version, the `vsys` and `objects` paths and the dependencies (name,
  requirement, optional, path).
- **Version matches**: `loader::version_matches`, `VersionReq::matches` over
  the port's SemVer text parser and its matcher (`semver.ves`).
- **`.vsys` type names stay a host step** (`VsysNames`, 74):
  `@vesper/vsys` does not yet check everything `vesper_ffi::sys::parse`
  does (its source and token limits are larger than `Limits::default()`'s,
  and it skips the linear handle finalizer and drop import signature
  checks), so it would accept libraries the loader rejects.
- A manifest error still hands the graph back to the Rust loader, which
  reports it.
- **Differential: 0 differences** on all 211 Loom packages of the corpus:
  202 read in Vesper, 9 handed back (the manifest-error fixtures and the
  sources that name `include_str`), the same split as slice 2.
- **Speed: 0.82x (95% CI 0.79..0.84)** of the Rust load, the same ABBA
  as slice 2 (6 rounds, 32 packages, release, `RUSTFLAGS="-C
  target-cpu=native"`); slice 2 measured 0.78x with the Rust decode.

## Slice 2: the loader's I/O

- The driver now reads the package graph itself
  (`crates/vesper-hybrid/modules/driver/src/load.ves`): `Loader::package`
  in Vesper over `@vesper-std/fs`. It canonicalizes each package directory
  (cycles and packages loaded twice), reads every `Loom.toml` of the
  dependency closure, picks the source directory (`src-wasip2` for the WASI
  component build, `src-<os>` when the target's OS has one, else `src`),
  walks it in the loader's order (each directory sorted by name, hidden
  names skipped, symbolic links followed, the depth and file bounds),
  checks the module names and the single root module, reads the sources,
  and reads and checks the `[package] vsys` and `objects` files (per-target
  paths, the static `.vsys` library objects need). It then encodes the
  assembler's input itself, so the graph no longer crosses from Rust, and
  hands the session only what `packages::Assembled` keeps besides the
  assembly: the `.vsys` libraries and the objects (the `Graph` step). The
  sources, root package and root module come back out of the assembler's
  input.
- What still runs over the Rust parsers, as host steps (71-75,
  `crates/vesper-hybrid/src/driver/load.rs`): the manifest decode
  (`vesper_loom_calculus::parse_manifest`), a dependency's version match
  and a `.vsys` library's type names and link mode (`vesper_ffi::sys`). A
  graph with a manifest error or an I/O error, or one this reader does not
  model (an absolute dependency path, a UNC path, a file name that is not
  UTF-8, a source over the parser's byte budget, a source that names
  `include_str`, #2119, until the driver expands it), goes back to the Rust
  loader: the driver then runs `LoadPackage` as before, which reports it
  exactly.
- **Differential: 0 differences** on all 211 Loom packages of the corpus:
  206 read in Vesper, 5 handed back (the manifest-error fixtures). At
  `VESPER_HYBRID_STATS=time` the comparison with `packages::load_for_target`
  now covers the whole `Assembled` for a graph the driver read: the source
  files (display, text, offset), the root package and module, the root
  package's files, the `.vsys` libraries and the objects.
- **Speed: 0.78x (95% CI 0.76..0.80)** of the Rust load. ABBA over 6 rounds
  of the whole package load (the Vesper read, assembly and read-back,
  against `VESPER_HYBRID_ASSEMBLE=0`, the Rust loader's whole load) on every
  6th corpus package the driver reads and assembles (32 packages), release,
  `RUSTFLAGS="-C target-cpu=native"`. The read alone
  (`DRIVER_PACKAGES_SPEED_OFF=VESPER_HYBRID_LOAD`: the Rust read with the
  Vesper assembly as the other side) measured 1.05x (95% CI 0.86..1.24, 3
  rounds over 15 packages).
- The swap is `load` (an open row, in `src/swap/assemble.rs`);
  `VESPER_HYBRID_LOAD=0` turns the read off and keeps slice 1.
  `VESPER_HYBRID_STATS=1` prints `load read <n> package graphs in Vesper
  (<m> handed back to Rust)`.
- Slice 2b moved the manifest decode and the version match to Vesper
  (above).

## Slice 1: assembly

- The Vesper driver (`crates/vesper-hybrid/modules/driver`) now assembles
  the package it loads. `LoadPackage` reads the package graph with the Rust
  loader (`packages::read_for_target`: manifests, the dependency closure,
  `src/` walks, `[package] vsys` and `objects`) **without parsing its
  modules**, and hands the graph to the driver. The driver's
  `assembled_words` (`src/assemble.ves`) parses, resolves and renames every
  module with `@vesper/check-names-packages` (#1178), and the new host call
  `hd_assembled` reads the renamed modules, the qualified-name map and the
  assembler's diagnostics back into the session's `packages::Assembled`
  (`src/swap/assemble.rs`). The modules cross as the parse swap's `ast` words
(`put_ast_words` of `@vesper/ast`, read by `parse_words::read_ast_section`).
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
| Driver | `crates/vesper-hybrid/modules/driver/src/{assemble,lib}.ves`, `Loom.toml`, `port/driver.rs` |
| Host and read-back | `crates/vesper-hybrid/src/driver.rs` (`LoadPackage`, `hd_assembled`), `src/swap/assemble.rs`, `src/hybrid.rs` (re-export) |
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

1. Manifest errors rendered in Vesper (E0114/E0115 and the decode errors),
   `include_str` expansion, and the `.vsys` type names once `@vesper/vsys`
   checks what `vesper_ffi::sys` does, so that no graph goes back to Rust.
2. **Slice 3: sugar expansion** (`Loader::expand_sugars`) once
   `@vesper/sugar` lands, and the parse-diagnostic fallback goes away.
3. `--include-tests` loads, once the assembler can parse test-only
   declarations.
