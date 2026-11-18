# Port wave P3a lane 4: vesper-loom-audit

Claim #1039. Baseline `main` at `3c075a4`. Rust stays the default: the
compiler and every crate inside its closure link the seed copy
`vesper-loom-audit-seed`; nothing is switched to the port and no Rust code is
deleted (owner rule).

## What was ported

The Vesper package `crates/vesper-loom-audit/vesper` (`@vesper/loom-audit`,
about 2,000 lines in seven modules) imports `@vesper/loom-calculus` and runs that
package's decoder, SemVer order and solution validator on the calculus state
it holds.

| Rust | Vesper module | Notes |
|---|---|---|
| `license.rs` (tokenizer, WITH > AND > OR parser, `allowlist`, `assess`) | `license.ves` | Same messages and budgets. The value is computed while parsing, which is equivalent because any failure discards it. Grants are 80 codes (license * 4 + exception). |
| `validate_catalog`, `validate_policy`, `Limits::validate`, `preflight` | `check.ves` | Duplicate native claims are found by a stable sort, so the error order matches the Rust loop. |
| `dependency_paths` | `paths.ves` | BFS per root over the validated graph, with stamped parents. Paths live in one word array plus records, and native ties are broken by `(len, path)` order. |
| `audit` findings, outcome | `report.ves` | Advisories are indexed by identity instead of being scanned per package. Work is still spent in bulk exactly as the Rust loop spends it, so budget errors are unchanged. |
| `AffectedRange::contains` | `report.ves` (`range_holds`) | Precedence (`cmp_precedence`) is in `util.ves`. |
| input words | `input.ves` | See `src/port.rs` for the encoding. |

## What stays in Rust and why

| File | Reason | What unblocks it |
|---|---|---|
| `src/input.rs` (bounded JSON parse, `check_size`) | JSON text parsing; the serde scalar port does not cover generic DTO decoding with duplicate-key rejection | a Vesper JSON reader (#965 is the related TOML/JSON follow-up) |
| `digest` in `src/lib.rs` | BLAKE3 over `serde_json` serialization of the DTOs. Vetted crypto stays Rust, and Vesper never sees the JSON. So the digest is computed in the wrapper and no `.vsys` extern is needed. | a Vesper JSON writer, then a BLAKE3 extern |
| `src/port.rs` | Encoder and decoder | — |
| Seed copy (`seed/`, the Rust bodies under `cfg(not(vesper_port))`) | Seed-copy rule | Stage 2 |

`InvalidSolution` diagnostics come from rerunning `vesper_loom_calculus::validate_solution` (the port-backed calculus) on the error path, because the calculus decoder is crate-private. `@vesper/semver` is not imported: the calculus already carries the SemVer order the audit needs.

No `unsafe` code was added outside the generated binding.

## Seed pattern

- `crates/vesper-loom-audit` is the port-backed package. `build.rs` always builds the port, as in loom-calculus and loom-solver. It has no dependents outside the compiler closure.
- `crates/vesper-loom-audit/seed` is `vesper-loom-audit-seed`, library `vesper_loom_audit`. The root `Cargo.toml` depends on it by rename (its own commit).
- `crates/vesper-loom-calculus/BUILD.bazel`: `:port_package` is now visible to `//crates/vesper-loom-audit`.

## Tests

- `tests/audit.rs` (16 tests, unchanged) passes on both packages. The crate has no in-crate unit tests.
- New `tests/differential.rs`:
  - A corpus of 400 random solutions, catalogs, policies and limits. It covers invalid solutions, bad grants, overlapping ranges, tight budgets and shuffled inputs. Each case runs `audit`, `parse_catalog` and `parse_policy`.
  - A `contains` matrix.
  - FNV digests of the `Debug` text must equal the Rust build's (`0x95e3c99598862434`, `0xafca9eef982680ae`).
  - The corpus must yield more than 100 reports (it yields 123, with 377 findings).
  - A leak check: `port_live_allocations` is unchanged afterwards.
- Divergence checks (`vesper-port-divergent`, `DIVERGENT` in `build.rs`, mirrored in `BUILD.bazel`, asserted by `port/test-port.sh`):

| Module | Edit | Caught by |
|---|---|---|
| input | path depth budget + 1 | `graph_catalog_path_and_work_budgets_fail_closed` |
| license | AND evaluates like OR | `spdx_boolean_precedence_and_explicit_exception_grants` |
| check | overlapping intervals pass | `malformed_or_ambiguous_catalogs_never_produce_clean_results` |
| paths | a longer native path replaces a shorter one | `native_shared_dependency_chooses_shortest_path_per_root` |
| report | severity threshold ignored | `severity_threshold_and_mixed_unknown_violations_are_explicit` |
| util | precedence ignores prereleases | `semver_intervals_include_lower_exclude_fixed_and_ignore_build_metadata` |

`empty_root_scope_cannot_certify_an_empty_or_unreachable_graph_clean` still passes under the divergent build.

## Performance

`examples/audit_abba.rs`:

- One `audit` of a layered graph: package i depends on i+1, i+3 and i+7, and every fourth package needs a native package.
- The catalog has four advisories per package and two native claims per native package.
- The policy has three grants.
- Release builds, cloud Linux x86_64, 9 ABBA epochs of 10 passes.
- Instructions are callgrind `Ir` per pass, computed as (3 passes − 1 pass) / 2.

| Packages | Wall clock B/A (median) | Instructions Rust | Instructions port | Instr B/A |
|--:|--:|--:|--:|--:|
| 8 | 1.73 | 1.94 M | 2.98 M | 1.54 |
| 64 | 1.52 | 18.2 M | 24.3 M | 1.34 |
| 512 | 0.87 | 237 M | 208 M | 0.88 |

All three pass the 2x gate. The port is slower on small inputs and faster at 512 packages. The speedup comes from representation, not compiler parity: advisories are indexed by identity, where Rust scans all advisories for every package.

Profile at 64 packages (port arm):

- The Vesper code is 27% of the instructions.
- The two builds share about 50%: BLAKE3 over the JSON of the inputs and the `check_size` serializations.
- The boundary encoding is most of the rest: `Enc::version` (`Version::to_string`) at 9%, string interning at 9%, and the calculus's per-selection canonical JSON.

So the gap is the export boundary, not the Vesper code:

- **Gap 1, text crosses as an interned string table.** Every version is printed for the calculus table even when no Vesper code reads the text. Repro: callgrind `Enc::version` in `audit_abba window 5`.
- **Gap 2, the calculus solution encoding needs canonical selection JSON.** It is the sort tie-break, and the audit never uses it. A calculus encoding flag that omits it would remove about 5% here. That is a calculus-side change.

Nothing in the port needed a compiler workaround beyond the known ones: no `continue`, alias errors on `f(inout s, s.field)` (locals instead), and string literals cannot be returned as `Str` (a literal match per table entry).

## Platforms

- Run locally on Linux x86_64 with Cargo: both packages, the divergent build, `cargo +1.100.0 clippy --all-targets -D warnings`, `vesper fmt --check` and the closure guard.
- CI covers Bazel, macOS and Windows.

## Std candidates

- `sat_add` and `sat_mul` (through `mul_wide`)
- A stable index merge sort with a comparator kind (the calculus has one with fixed kinds)
- Stamped marks
