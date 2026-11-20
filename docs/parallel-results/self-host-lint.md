# Self-hosting SH1 lane 13: the lint engine in Vesper

Claim: #1156. Plan: `docs/agent-prompts/self-host-sh1/13-lint.md`.
Base: origin/main `62b4e34` (after lane 9's AST port, #1168).

This lane ports `vesper-lint`'s engine to Vesper: both lint rules, the
`Loom.lint.toml` policy reader (with its own TOML reader) and the
validation and application of fix edits. The port is
`crates/vesper-lint/vesper` (package `@vesper/lint`: `lib.ves` with the
rules, the edits and the exports, `policy.ves` with the policy reader, and
`util.ves` with the shared helpers; 3,800 lines), built on `@vesper/ast`'s type, effect and generic nodes,
`@vesper-std/intern` and `@vesper-std/sort`.

Rust stays the default for every build and every dependent. Nothing Rust is
deleted.

## What changed

The crate uses the seed pattern from Beacon, semver, fmt and the toolchain
pilots (`docs/parallel-results/self-host-toolchain-pilots.md`):

- `crates/vesper-lint/seed/` is a package `vesper-lint-seed` with the same
  library name, built from the same `src/lib.rs` with no build script, so
  it is plain Rust. The root package and `vesper-hybrid` use the seed. That
  is the root patch, in its own commit. The root `BUILD.bazel` gains
  `ast_package`, `std_arena_package` and `std_intern_package` for the port's
  Bazel build. `vesper-hybrid/Cargo.toml` follows lane 11's precedent for
  fmt: it is outside this lane's paths, but it is in the compiler closure,
  and the closure guard (`crates/vesper-port-harness/tests/closure_guard.rs`)
  passes.
- `crates/vesper-lint` builds the Rust code by default. Under the Cargo
  feature `vesper-port`, `build.rs` links the Vesper object and sets
  `cfg(vesper_port)`. `analyze`, `apply_edits` and `Policy::parse` then go
  to the port (`src/port.rs`); the Rust code is kept as `analyze_rust` and
  `apply_edits_rust`.
- `src/port.rs` encodes the checked facts as a word stream plus one text
  blob (functions in name order, parameters, actions, places, types and
  effect rows), and decodes the port's result words back into the same
  `LintReport`, `Finding`, `Skipped`, `LintError` and `PolicyError` values.
- `tests/differential.rs` (cfg `vesper_port`) checks the port against the
  seed (dev-dependency `lint-seed`). Every result, errors included, must
  serialize the same:
  - `repository_sources_lint_identically`: the checked facts of every
    repository `.ves` file that checks as one module (328 sources, 132
    findings, 344 skipped functions), each also with every function marked
    owned, at call depths 1 to 3, and swept across the work limit.
  - `fuzzed_facts_lint_identically`: 4,000 generated programs (env
    `VESPER_LINT_FUZZ`) with generics, effect rows, labels, projections,
    opaque actions and call cycles.
  - `fact_limits_fail_identically` and `policy_limit_sweeps_fail_identically`.
  - `hand_written_documents_parse_identically`,
    `repository_manifests_parse_identically` (every repository `.toml`) and
    `fuzzed_documents_parse_identically` (20,000 mutated documents, env
    `VESPER_LINT_TOML_FUZZ`, between 10% and 90% accepted).
  - `fuzzed_edits_apply_identically`.
  - `port_frees_its_allocations`, which reads one process-wide counter and
    so runs alone.
- The crate's own tests (`tests/policy.rs`, `tests/typed.rs`) pass against
  the Rust build, the port build and the seed.
- `port/test-port.sh` runs those, then builds four divergent ports (feature
  `vesper-port-divergent`, `VESPER_LINT_DIVERGE`, edits in `build.rs` and
  `BUILD.bazel`) and requires a named test to fail on each while an
  unrelated one passes:

  | Area | Edit | Caught by |
  |---|---|---|
  | effects | a labelled request matches any labelled effect | `fuzzed_facts_lint_identically`, `repository_sources_lint_identically`, `structural_instances_not_just_names_and_labels` |
  | projections | reads under a written path count only on an exact match | `fuzzed_facts_lint_identically` |
  | toml | a header may reopen an implicit dotted table | `fuzzed_documents_parse_identically`, `hand_written_documents_parse_identically` |
  | edits | an oversized result reports a stale edit | `fuzzed_edits_apply_identically` |

  The script is appended to Linux port shard 7 in
  `scripts/test-port-harness.sh`.
- Bazel builds the port object and the four divergent objects
  (`crates/vesper-lint/BUILD.bazel`, `crates/vesper-lint/seed/BUILD.bazel`).
  The differential tests run in Cargo (shard 7), as for the other
  feature-gated ports.
- `tests/fixtures/self_host/driver.rs` skips `crates/vesper-lint/vesper`,
  which imports with `use` (#880), like the fmt and highlight ports.

### The rules

- **L0102 (unused effect):** each function's declared row against the
  effects its body and its callees need, with generic substitution at call
  sites, instance matching on structure, names and labels, and cycles
  failing closed.
- **L0101 (redundant parameter mode):** the parameter's place actions,
  write and read projections, and what callees do with it, to the same
  call depth.
- Both charge the same work units and refuse at the same limit, and the
  skipped-function reasons are the same, including the text of an opaque
  action (code 16 + k names action k of the whole program).

### The TOML reader

#965 has no Vesper TOML reader in flight, so the port has the smallest
reader the tests need, in `lib.ves`. It is a whole-document validator with
the semantics of the `toml` 0.8.23 crate that the Rust code uses
(`toml_edit` 0.22.27 underneath):

- the full grammar, including datetimes, numbers and their backtracking,
  multi-line strings and escapes;
- table rules: dotted keys, implicit and explicit tables, reopening,
  arrays of tables, and sealed inline tables and arrays;
- the float overflow check (only `+inf` past `2^1024 - 2^970` is refused);
- the policy walk in sorted key order, as `toml::Value` tables are a
  `BTreeMap`.

It returns only what `Policy` needs (the seven severities, the configured
mask and the usage counts), not a value tree. A document whose table starts
with serde's private datetime key, `$__toml_private_datetime`, goes back to
the Rust code (code 38).

### What stays Rust

- `Policy::from_value` and `Policy::from_rules`: they take Rust values
  (`toml::Value`, `&[Rule]`), not text.
- The semver report. It lives in `src/tooling/lint.rs` in the compiler,
  outside this lane's paths (`docs/parallel-results/self-host-tool-split.md`).
  It should move through the semver port when that file moves.
- Fact extraction from checked source. That is the compiler's job
  (`checked_source`), and the port starts from the facts.
- Rendering. The engine returns structured findings, and the compiler
  renders them.

## Stage 1 guard

This lane does not touch `src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`. The bootstrap-stage1 workflow ran on the
branch: [run 44743437868](https://github.com/robot-head/vesper/actions/runs/44743437868).

## Numbers

Release builds, one binary linking both (`examples/abba.rs`), from the
repository root. Lower is better; the ratio is Vesper over Rust. ABBA is
`vesper-bench`'s 30-epoch run.

| Workload | Rust | Vesper | Vesper/Rust |
|---|---:|---:|---:|
| `corpus`: `analyze` over 328 checked sources, wall (median) | 7.455 ms | 13.756 ms | 1.79x (min 1.41, max 2.03) |
| `policy`: `Policy::parse` over every repository `.toml`, wall (median) | 4.203 ms | 2.931 ms | 0.69x (min 0.61, max 0.82) |
| `corpus`, callgrind Ir of one pass | 57.8M | 108.4M | 1.88x |
| `corpus`, of which encoding the facts (`encode` arm) | | 11.0M | 10% of the Vesper arm |
| `policy`, callgrind Ir of one pass | 32.4M | 27.5M | 0.85x |

Callgrind counts are the inclusive Ir of each arm's function in
`examples/abba.rs` (`once <workload> <arm> 1`), which leaves out the setup
(checking the corpus).

The corpus ratio includes the boundary: encoding the facts into words
(the `encode` arm) and decoding the result into Rust values. The policy
port is faster than Rust because it validates the document in one pass
without building a `toml::Value` tree, then walks only the lint tables.

Rust stays the default: the corpus number does not meet parity.

## Compiler gaps

1. **`effect` cannot be an identifier.** It is a keyword everywhere, so
   lint code that talks about effects must rename its variables.

   ```vesper
   fn f(x: u64) -> u64 {
       let effect = x;   // E0002 expected pattern
       effect
   }
   ```

2. **Division by a variable raises `Fail`.** There is no way to state that
   the divisor is nonzero, so `fits`-style overflow checks are rewritten
   with literal divisors and wrapping operations.

   ```vesper
   fn f(x: u64, r: u64) -> u64 {
       x / r             // E0401 unhandled effect 'Fail'
   }
   ```

3. **A string's bytes cannot be sliced inline.** The view has to be bound
   first.

   ```vesper
   fn f(in s: Str) -> u64 {
       s.as_bytes()[0..1].len()   // E1521; `let b = s.as_bytes(); b[0..1].len()` checks
   }
   ```

4. **No generic `wrapping_neg`.** Only the suffixed builtins exist
   (`wrapping_neg_i64`), and `x.wrapping_neg()` is E0202. The same holds for
   casts: `wrapping_i64` instead of a cast.

5. **`quote` as an identifier switches `vesper fmt` to the macro profile**,
   whose token cap is 8,192, so a large file that names a variable `quote`
   cannot be formatted (TokenLimit). The policy reader calls it `delim`.

6. **No arrays of `Str`.** A name table becomes an `if` chain over
   `text_is`.

7. **Shift amounts must be `u32`**, so every `wrapping_shl_u64` needs
   `wrapping_u32` around a `u64` count.

8. **Facts cross the boundary as words.** The export ABI has no way to lend
   a Rust tree (`CheckedProgram` with its `BTreeMap`s) to Vesper, so
   `src/port.rs` flattens it on every call. That costs 10% of the Vesper
   arm's instructions on the corpus.

## What is left

- Parity on `analyze`: the boundary encoding and the effect-instance
  matching loops are the cost; a lent-tree ABI (gap 8) would remove the
  first.
- The semver report through the semver port, once `src/tooling/lint.rs`
  moves.
- `Policy::from_value` and `from_rules` in Vesper, which need a value-tree
  ABI.
- A shared Vesper TOML reader for #965: this one validates a whole document
  but only reports the lint tables.
- The datetime-marker fallback (code 38).
