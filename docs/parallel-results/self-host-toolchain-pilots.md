# Self-hosting SH0 lane 6: toolchain pilot ports

Claim: #1128. Plan: `docs/agent-prompts/self-host-sh0/06-toolchain-pilots.md`.

This lane ports three small compiler-linked toolchain crates to Vesper as
pilots for self-hosting. Part 1 (this PR) ports `vesper-span` and
`vesper-lean-syntax`. Part 2 ports `vesper-diagnostics` (ariadne-identical
terminal output and the same LSP JSON) and is still to do.

Rust stays the default for every dependent. Nothing Rust is deleted.

## What changed

Both crates use the seed pattern from Beacon and semver:

- `crates/<c>/seed/` is a package `<c>-seed` with the same lib name, built
  from the same `src/lib.rs` without a build script, so it is plain Rust.
- The compiler and every compiler-linked dependent use the seed. The root
  `Cargo.toml`, `vesper-lsp` and `vesper-diagnostics` use `vesper-span-seed`.
  The root and `vesper-fmt` use `vesper-lean-syntax-seed`.
- The original package always builds the Vesper port through
  `vesper_port_harness::Port::build` (cfg `vesper_port`). Its public API is
  unchanged. Under the port, the work is done by `vesper/src/lib.ves` through
  export ABI v2, and `src/port.rs` holds the flat encodings.
- `tests/differential.rs` checks the port against the seed (renamed dev-deps
  `span-seed` and `lean-seed`) on generated inputs. Every result, error
  included, must print the same.
- `tests/port_memory.rs` checks that the port frees everything it allocates.
  It is a separate binary so other tests cannot race the allocation counter.
- `port/test-port.sh` builds three deliberately divergent ports per crate
  (feature `vesper-port-divergent`, chosen by `VESPER_SPAN_DIVERGE` or
  `VESPER_LEAN_DIVERGE`) and requires a named test to fail on each one. Both
  scripts are appended to Linux port shard 7 in `scripts/test-port-harness.sh`.

### vesper-span (596 lines of Vesper)

The port covers the line table (`LineIndex::new` with its budgets), `offset`,
`position`, `range`, `span`, the `SourceEdits` checks and `apply`, and the
`Diagnostics` checks. Copying the diagnostic data into owned values stays in
Rust.

Divergent builds:

| Area | Change | Test that catches it |
| --- | --- | --- |
| index | 3-byte chars counted as 2 UTF-16 units | `utf8_utf16_and_strict_interiors` |
| edits | equal empty-span inserts not treated as collisions | `collisions_and_invalid_edits_leave_source_unchanged` |
| diagnostics | missing primary label accepted | `diagnostic_invalid_labels_fixes_and_aggregate_budgets` |

### vesper-lean-syntax (1,700 lines of Vesper)

The port covers the term and module printer, the identifier, module-name and
decimal checks, and the `free_references` and `references` walks. Rust
encodes the tree into words plus a text pool (`src/port.rs`, every node tag is
documented there). Vesper prints into a `String` and returns it, or returns the
names as word offsets into the pool.

Error order matches Rust: for an application whose function fails, the
arguments are still checked, and an argument's error wins, as in Rust's
evaluation order.

Divergent builds:

| Area | Change | Test that catches it |
| --- | --- | --- |
| printer | `\n` escaped as `\t` in string literals | `ordinary_lists_include_empty_nested_and_escaped_literal_elements` |
| checks | `_root_` accepted as an identifier | `identifiers_are_injective_and_cannot_break_quotations` |
| walks | binders no longer drop names out of scope | `free_references_respect_binders` |

Both crates' existing test suites pass unchanged on both builds, and the Lean
text is byte-identical in the differential test.

### Outside the exclusive paths

- `scripts/test-port-harness.sh`: two lines for shard 7.
- `tests/fixtures/self_host/driver.rs`: the self-host driver skips the two
  new `vesper/` directories, because they use `p[a..b]` string intervals that
  the Stage 1 Vesper compiler package does not parse yet (#887).

## Stage 1 guard

Nothing under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` is touched
(`git diff --name-only origin/main` lists only the files above). The
`bootstrap-stage1` workflow is run on the branch; its result is on the PR.

## Numbers

Linux x86_64 cloud container, release builds. ABBA is 30 epochs with the
median per operation (`examples/abba.rs`, run by `scripts/bench.sh`).
Instructions are from callgrind, as the difference between runs of 6 and 1
batches, so setup is not counted (`CALLGRIND=1 scripts/bench.sh`).

| Workload | ABBA Vesper/Rust (min-max) | Rust Ir | Vesper Ir | Ir ratio |
| --- | --- | --- | --- | --- |
| span lookup (offset/position over a mixed-width file) | 1.48 (1.36-1.63) | 34.76M | 54.26M | 1.56 |
| span index (build the line table) | 0.28 (0.24-0.32) | 10.31M | 2.67M | 0.26 |
| span edits (check and apply 64 insertions) | 3.14 (2.74-4.49) | 0.92M | 3.52M | 3.82 |
| lean term (print a depth-15 term) | 0.35 (0.31-0.39) | 1,401M | 416M | 0.30 |
| lean module (print 400 definitions) | 0.39 (0.35-0.48) | 997M | 330M | 0.33 |
| lean free (free references of a large term) | 1.96 (1.85-2.13) | 96.0M | 289.0M | 3.01 |

The index build is faster because the Vesper line scan uses `find_either`,
which is a SIMD scan, while Rust walks bytes. The printer is faster because
Rust formats through `fmt::Write` and many small `write!` calls, while the port
pushes into one pre-sized `String`.

The edits workload checks and applies 64 insertions spread over a 64 KiB file
(about 17 µs per operation in Rust), so the export call and the encoding are a
large part of its cost. An earlier version of the benchmark picked only 6
edits because its offsets were not sorted; the numbers in gap 4 come from
that smaller batch.

The port encodes edits and diagnostics only as far as the budgets allow: the
limits, source size and counts are checked first, and the encoding stops
where the check is sure to fail, so a rejected batch with a huge expected
text is not copied.

Not at parity: span lookup, span edits and lean free. So Rust stays the
default (the user's parity rule), and the seeds stay what the compiler uses.

## Compiler gaps

Each gap has a minimal shape and where the port works around it.

1. **`Fail::raise` does not drop a live local struct's `String` field.**

   ```vesper
   struct P { out: String, err: u64 }
   pub fn f(n: u64) -> String eff [Fail[u64]] {
       let mut p = P { out: String::with_capacity(64), err: 0 };
       p.out.push_str("partial");
       if n == 0 { Fail::raise(1); }   // p.out leaks
       let P { out, err } = p;
       out
   }
   ```

   Workaround: `p.out = String::new();` before each raise
   (`render_term`, `render_module` in lean-syntax).

2. **Returning one field of a local struct leaks the other fields.**

   ```vesper
   struct R { found: ChunkSeq[u64], scope: ChunkSeq[u64] }
   pub fn g() -> ChunkSeq[u64] {
       let r = R { found: ChunkSeq::new(), scope: ChunkSeq::new() };
       r.found                         // r.scope leaks
   }
   ```

   Workaround: destructure, `let R { found, scope } = r; found`
   (`free_references`, `references`).

3. **An early `return` inside an `if` with a live local struct leaks it.**
   The shape is gap 2 with `if c { return 0; }` after a scratch `P` is made.
   Workaround: move the scratch printer into a helper that destructures it
   (`args_error`).

4. **`String::push_str` copies byte by byte.** It does not lower to the
   copy builtin. Together with gap 5, switching `edits_apply` from `push_str`
   into a `String` to `ChunkSeq::extend` cut the 6-edit workload from 5.08M
   to 2.17M Ir. Workaround: `edits_apply` returns `ChunkSeq[u8]`.

5. **The export binding validates returned `String`s again.** A `String`
   result is checked with `from_utf8` in the generated binding, though Vesper
   strings are always UTF-8. So the port pays for validation once more in
   Rust. Returning `ChunkSeq[u8]` moves this into Rust's `from_utf8`, which is
   faster but still a second pass.

6. **`find_not_in` has a high fixed cost on baseline x86-64.** An ASCII fast
   path in `offset` and `position` (skip to the first byte not in `0..128`,
   then walk) raised span lookup from 54M to 73M Ir, because most lines are
   short and the scan has no SSSE3 path on baseline x86-64. It was reverted.

7. **Per-byte `try_get` loops are slow.** `offset` and `position` walk
   `bytes` one byte at a time with bounds-checked reads. The bounds check is
   not removed even though the loop is bounded by the line end, which is
   checked once. In the span lookup profile, over 90% of the port's
   instructions are in `vesper_span_offset` and `vesper_span_position`, which
   are these loops. Lean free compares names in the pool with the same kind of
   loop, so this is likely its main cost too (not profiled).

8. **Tree inputs need a hand-written flat encoding.** The lean-syntax encoder
   is 611 lines of Rust. A tree or record export ABI would remove it and its
   cost, which every port call pays (not measured separately).

## What is left

- Part 2: port `vesper-diagnostics`. It needs the prepare checks in Vesper,
  LSP JSON produced in Vesper and parsed with `serde_json`, ariadne 0.5.1
  terminal output reimplemented byte-for-byte (including the unicode-width
  0.1.14 table), and the 200-label report benchmark.
- Making span lookup, span edits and lean free reach parity, which waits on
  gaps 4, 5 and 7.

## Platforms run

- Linux x86_64 (cloud container): both builds of both crates, the
  differentials, the leak tests, both divergence scripts, and
  `cargo +1.100.0 clippy --all-targets -- -D warnings`.
- macOS, Windows and Bazel on Linux: through CI on the PR.
