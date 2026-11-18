# Port wave P1 lane 15: vesper-semver

Claim [#912](https://github.com/robot-head/vesper/issues/912). Base: `main` at
`69c79b9`. Prompt: [15-semver.md](../agent-prompts/port-p1/15-semver.md).

**Outcome.** The crate's logic is ported to Vesper (`@vesper/semver`, about
2,400 lines in `crates/vesper-semver/vesper/`). All 48 tests pass on both builds,
and four new differential and leak tests pass too. The crate follows Beacon's
seed-copy rule: the compiler links the frozen Rust copy, and the Rust code is
not deleted. The port is about 4 to 5 times slower than Rust on interface
comparisons and about 7 times slower on a bare release check, so the Rust
seed stays the default for every dependent (the owner's parity rule).

## API inventory (measured before porting)

| Item | Kind | Treatment |
|---|---|---|
| `compare(&Interface, &Interface) -> Report` | function | Ported (`check.ves`) |
| `Projection::is_subset_of` | method | Ported (`check.ves` `subset_q`) |
| `Report::is_compatible` | method | Kept in Rust: one `matches!` over the shared type |
| `check_release(..) -> Result<ReleaseReport, ReleaseVersionError>` | function | Ported (`version.ves` `release_q`) |
| `ReleaseBump: Display`, `ReleaseVersionError: Display + Error` | impls | Kept in Rust (formatting of shared types) |
| `Mode`, `Resumption`, `Pass`, `Primitive`, `Type`, `Generic`, `Projection`, `Parameter`, `Capability`, `GradeRelation`, `Effects`, `Function`, `Fields`, `Declaration`, `ExternalType`, `Interface`, `Classification`, `IssueKind`, `Issue`, `ReborrowAdapter`, `Report`, `ReleaseBump`, `ReleaseIssueCode`, `ReleaseIssue`, `ReleaseReport`, `ReleaseVersionSide`, `ReleaseVersionError` | shared types | Kept in the crate root with their serde derives, so both builds and every dependent share them |
| `SCHEMA_VERSION`, `SEMANTICS_VERSION`, `RELEASE_POLICY_VERSION` | constants | Kept; `check.ves` repeats the first two |
| `unique_map` (duplicate-key deserializer) | private | Kept in Rust (serde) |
| `IMPLEMENTATION`, `port_live_allocations` | new | Which build this is, and the leak counter (0 in Rust) |

Private Rust helpers ported one to one: `Checker::{validate, validate_function,
validate_type, generic_bounds, function, subtype, declaration}`,
`u64_counterexample`, `i64_counterexample`, `resumption`, `type_mode`.

From the `semver` crate (1.0.28) the release policy uses `Version::parse` (all
its error kinds and messages), `Version::cmp_precedence`, `Version: Display`
and `Prerelease::is_empty`. `version.ves` reimplements exactly that subset,
following SemVer 2.0.0 §9 to §11 and the crate's parser. Version requirements
(`VersionReq`) are not used by the crate, so they are not ported.

## How the port is wired

- `crates/vesper-semver` is the port-backed package. Its `build.rs` always links
  the port and sets `cfg(vesper_port)`. `compare`, `is_subset_of` and
  `check_release` forward to `src/port.rs`. The Rust bodies stay under
  `#[cfg(not(vesper_port))]`.
- `crates/vesper-semver/seed` is package `vesper-semver-seed` with library
  `vesper_semver`, the frozen Rust build of `../src`. The root `vesper`
  package depends on it by rename, which is a one-line change in root
  `Cargo.toml`. `cargo tree -p vesper -e normal,build` shows only the seed, and
  the closure guard passes. Nothing outside the compiler closure depends on the
  crate, so for now only the crate's own tests exercise the port build.
- `src/port.rs` encodes both interfaces as one flat `u64` stream over a text
  pool, in the layout documented at the top of `check.ves`. It interns strings
  in sorted order, so comparing ids compares strings. It also hash-conses
  nodes per class, so where the Rust code compares values with `==`, the port
  compares offsets. The port writes every issue path, rule, message and
  witness itself. The wrapper only slices its text pool.
- Types nested more than 256 levels are cut by the encoder, so input of any
  depth cannot overflow its stack. Every checker walk stops at 128 levels, and
  a cut node carries the index of its subtree among the distinct cut subtrees,
  found with a non-recursive equality. Because of that, offsets still compare
  equal exactly when the Rust `==` would.
  `tests/interface_differential.rs` checks this against the seed at depths
  200, 300 and 5,000. It also checks that a 200,000-level type on a 2 MiB
  stack gets the nesting report.
- `check_release` computes the comparison first and then calls the port with
  the classification. The comparison has no side effects, so this order cannot
  be observed.

## What stays in Rust and why

| File | What | Why | Unblocked by |
|---|---|---|---|
| `src/lib.rs`, `src/release.rs` | Shared types, serde derives, `unique_map`, `Display` impls, `Report::is_compatible` | The lane prompt keeps serde on the shared Rust types; the rest formats or inspects those types | - |
| `src/port.rs` | Encoder and decoder | The export ABI has no tree-shaped or string-keyed inputs (records with strings in sequences are refused) | Richer ABI (lane 1 follow-ups) |
| `seed/` (`../src` without the port) | The whole Rust implementation | Seed-copy rule: the compiler links the crate | Self-hosting Stage 2 |

The port has no unsafe code. The generated `port/binding.rs` holds the ABI's
usual `unsafe` FFI calls.

## Unit tests

`src/tests.rs` held 39 `#[cfg(test)]` tests. They are translated to
`tests/interface.rs` and run against both builds. Two of them called the
private helpers `u64_counterexample` and `i64_counterexample` directly. Those
assertions were rewritten as equivalent checks through `compare`: an invalid
range has no counterexample, so its incompatibility keeps the generic witness,
and a contained range raises no incompatibility. No test was deleted.

## Tests and divergence checks

| Test | Port build | Seed build |
|---|---|---|
| `tests/interface.rs` (39, translated) | pass | pass |
| `tests/release.rs` (9) | pass | pass |
| `tests/version_differential.rs`: parse errors, messages and `Display` against `semver` over 525 generated and fixed inputs (more than 100 of them malformed), and precedence against `cmp_precedence` on 10,000 generated pairs (each ordering at least 500 times) | pass | pass |
| `tests/interface_differential.rs`: full `Report` JSON of port and seed on 4,000 generated interface pairs (half well-formed; every classification at least 100 times) and on deeply nested types, and `is_subset_of` on 5,000 projection pairs | pass | port only |
| `tests/port_memory.rs`: 1,000 cycles of comparisons and release checks (accepted, rejected, malformed) leave no live port allocation | pass | pass (count 0) |

Divergence: the `vesper-port-divergent` feature (Cargo) and the
`semver_vesper_divergent_{check,version}` targets (Bazel) apply one edit per
ported module. `port/test-port.sh`, run from `scripts/test-port-harness.sh` on
macOS and Windows, asserts the following:

- The `check.ves` edit (return mode check reversed) fails
  `function_and_produced_value_modes_are_covariant`, and
  `identical_interface_is_patch_and_round_trips` still passes.
- The `version.ves` edit (equal precedence accepted) fails
  `component_resets_are_allowed_only_when_precedence_increases`, and
  `invalid_and_overflowing_versions_return_typed_errors` still passes.

## Performance

Setup: `tests/port_bench.rs`, ignored by default, run with
`cargo test --release -p vesper-semver --test port_bench -- --ignored --nocapture`.
It runs on one cloud container (4 vCPU Intel Xeon @ 2.10 GHz, shared, so
expect noise), with the port built by the in-tree compiler at its default
optimization. Each case runs 21 ABBA rounds (port, seed, seed, port) on the
same inputs, and the table gives the median per-call time and the median
ratio.

| Case | Port | Rust seed | Ratio (min to max) |
|---|---|---|---|
| `compare`, 8 exports, minor change | 32.6 µs | 6.3 µs | 5.17x (4.81 to 6.14) |
| `compare`, 8 exports, identical | 30.9 µs | 7.0 µs | 4.43x (3.33 to 5.57) |
| `check_release`, 8 exports | 34.7 µs | 6.7 µs | 5.19x (4.98 to 5.55) |
| `compare`, 64 exports, minor change | 231 µs | 52.2 µs | 4.33x (3.16 to 5.15) |
| `compare`, 64 exports, identical | 222 µs | 56.5 µs | 3.93x (3.58 to 3.97) |
| `check_release`, 64 exports | 226 µs | 52.3 µs | 4.31x (4.23 to 4.51) |
| `check_release`, empty interfaces (version parsing and policy) | 3.0 µs | 0.44 µs | 6.84x (6.07 to 7.07) |

The first working version was 12 to 16 times slower. Three changes brought it
to the numbers above:

1. A single-pass, stack-based encoder that borrows strings and uses an
   open-addressing hash-cons table. Encoding 64 exports went from 312 µs to
   43 µs.
2. Paths are built lazily as nodes and written as text only for reported
   issues.
3. The inputs are copied once into local `ChunkSeq`s, because the inliner
   never inlines a function with a slice parameter. Before this, every word
   read was two calls (`at` and `sw`), about 7.8 million calls per 600
   comparisons under callgrind.

Of the remaining time on 64 exports, about 45 µs is the encoder and the rest is
the Vesper checker (validation of both sides plus comparison).

Rust is not deleted, and it stays the default for every dependent, by the
owner's parity rule.

## Compiler gaps found (repros in this port)

- **Slice parameters block inlining** (`src/ir_inline.rs`: "no type in it
  mentions ... a slice parameter"). Small accessors over an `in Slice[T]` stay
  calls. Every port that reads its input through a slice pays for this. The
  workaround here is copying into a local `ChunkSeq` (`compare_q`,
  `release_q`).
- **A local non-`copy` struct with a `ChunkSeq` field leaks.** Wrapping the
  copied input in `struct W { w: ChunkSeq[u64] }` as a local made
  `tests/port_memory.rs` fail. A plain local `ChunkSeq` is freed.
- **`ChunkSeq::set` raises `Fail`**, so in-place digit reversal (as Quarry's
  `put_u64` does) needs a handler. `util.ves` writes digits by subtracting
  powers of ten instead.
- **No tree or string-record export ABI.** Inputs with recursive structure and
  string keys need a hand-written flat encoding, about 45 µs of the 64-export
  cost.

## Divergences from Rust behavior

None observed on the corpora above. One known limit: in "unexpected character"
messages, a non-ASCII character is quoted as-is. Rust's `char` `Debug` would
escape the non-printable or grapheme-extending ones (other than the C1
controls, which the port does escape). The corpora use only printable
non-ASCII characters (`é`, `λ`, `😀`) plus U+0085.

## Platforms run

- Linux x86_64 (cloud container): both builds, the differentials, the leak
  test, the divergence script, and `cargo +1.100.0 clippy --all-targets -D warnings`.
- macOS, Windows and Bazel on Linux: through CI on the PR.

## Files

- `crates/vesper-semver/**`: `vesper/` (port), `port/binding.rs` (generated),
  `port/test-port.sh`, `seed/`, `build.rs`, `src/port.rs`, and the tests.
- Root patch (its own commit): `Cargo.toml` registers `crates/vesper-semver/seed`
  in `members` and `default-members`, switches the root dependency to the seed,
  and regenerates `Cargo.lock`.
- One line each in `scripts/test-port-harness.sh` and
  `tests/fixtures/self_host/driver.rs`, where the port's `vesper/` joins the
  `use` skip list (#880).
