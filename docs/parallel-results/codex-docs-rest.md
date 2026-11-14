# Wave seven lane 14: Codex FieldTree heatmaps, signature search, `.olean` links and JIT doctests (#337)

Base: `283c811` (origin/main, a descendant of the required `c025b9a`). Branch:
`claude/project-thread-n6yuid`. Claim issue: #337. Before starting, open issues
and PRs were checked; nothing overlapped (other open work: Codex's #322/#358 and
PR #165, plus the other wave-seven lane claims).

## What was built

### 1. FieldTree borrow-footprint heatmaps (§40.1 item 1c)

`vesper_doc::heatmap` computes one heatmap per documented nongeneric named
struct that a documented function borrows or consumes. Leaves are the struct's
FieldTree, expanded through nested nongeneric documented structs (depth 8, 256
leaves); enum, generic, tuple, function and recursive fields are single opaque
leaves with the reason. Each parameter of that type is one row: every leaf under
one of its declared projections is `write` (`inout`), `read` (`in`) or `consume`
(`sink`), and the rest are `free`, i.e. available for concurrent disjoint
borrows across the call (`[T-CallProj]`). Rows also list the free siblings, and
aggregate writer/reader/consumer counts per leaf render as a heat scale.

The footprints are the checker's: pass modes and declared projections, which the
checker enforces (an access outside a projection is `E0304`, overlapping
projections `E0301`, unknown paths `E0202`), and a document is only produced for
checked source. They are the declared upper bound, not a may-use analysis of
the body: a declared leaf the body never touches is still shown. A `sink` row
with a partial projection, or a row that selects no leaf, is an error, never a
silent row. HTML gets a `codex-heatmaps` section; `--json` gets `heatmaps`.

### 2. Type-directed signature search: static index plus search page (§40.1 item 5)

`vesper_doc::hoogle` builds a deterministic `vesper-codex-search` v1 index of
every documented function: structural parameter and result types, pass modes,
modes, declared projections, canonical effect texts and the item's proof badge.
Generic, open-row, function-typed and unresolved signatures are listed under
`unsupported` with the reason instead of being matched approximately.

Text queries (`vesper doc FILE --json --search TEXT`, and the page's input box):

```text
query  := params "->" ret [ "eff" "[" effect,* "]" ]  |  word
param  := [in|inout|sink] [copy|affine|linear] type [ ".{" path,* "}" ]
ret    := [copy|affine|linear] type
type   := "_" | "()" | Name [ "[" type,* "]" ] | "(" type,* ")"
```

Omitted passes, modes and `eff [...]` are unconstrained, `_` matches any type,
`eff []` means pure, a single word is a case-insensitive name search. Matching is
variance-aware as §40.4 states it: a query argument mode must be at most the
parameter's (`copy <= affine <= linear`, contravariant), the function's result
mode at most the query's (covariant), every function effect inside the ceiling,
and the function's declared footprint inside the query projection. Score: 10 for
an exact contract (no wildcard, every pass/mode given and equal, equal
footprints and effects), +5 proved, +2 audited; ties by name. Malformed,
over-long (4 KiB, 512 tokens, 128-byte names, nesting 16) or trailing input is a
typed error.

`vesper doc FILE --bundle DIR` writes a new directory (never an existing one;
a failed write removes it) with `index.html`, `search-index.json` and
`search.html`. The page embeds the index as JSON (`<` escaped), runs a
JavaScript port of the Rust parser and matcher (`crates/vesper-doc/src/search.js`,
included verbatim with `include_str!`), needs no fetch, CDN or network, works from
`file://`, and links hits to `index.html#item-NAME` (item articles now carry
those ids). Its CSP allows only inline style and script. No WASM build: the
optional `codex-search.wasm` was not built; the script is the shipped engine.

### 3. `.olean` certificates from the Loom content-addressed store (§40.1 item 4)

`vesper doc store-olean --proof-audit REPORT.json --root LEAN_BUILD_DIR --store DIR|default`
reads an accepted, kernel-replayed `vesper proof-audit --json` report, takes
each `.olean` it inventories from the build directory (plain relative paths
only), refuses bytes whose size or plain BLAKE3 differ from the audit, stores the
report and the `.olean` as Loom CAS artifact blobs and publishes a derivation:
builder `vesper-codex-olean` v1, input `proof_audit` = the report blob, options
module / artifact inventory hash / path / bytes / BLAKE3, output = the `.olean`
blob. `default` is `~/.vesper/store` (`HOME`, else `USERPROFILE`), which is the
same `vesper_loom_cas::Store` layout as `vesper cas --store`.

`vesper doc ... --proof-audit REPORT.json --olean-store DIR|default` recomputes
that key from the exact report bytes, so a certificate is only linked to the
audit that inventoried it. Lookup never creates a store. A hit is read back
through the CAS (which rehashes the object) and must again match the audited
size and BLAKE3, otherwise generation fails. Each badge with accepted evidence
gets `certificates` (`linked` with the CAS object id, or `missing`); unverified
badges get none. In a bundle the verified bytes are copied to
`certs/<blake3>.olean` and the badge table links them. A link is content
identity with the audited artifact only: it is not provenance, does not change
a badge level, and the badge still describes Lean theorems about a model, not
the Rust or Vesper implementation.

### 4. Doctests in one shared JIT module, with per-example evaluator fallback (§40.1 item 3)

`vesper test --doc` still merges a file's positive non-simulated examples into
one checked module and compiles them into one `NativeBatch` JIT module. Before,
if any example was outside the native subset (finite scalar direct calls, no
aggregates, effects, generics or `inout`), the whole batch failed. Now, when the
full set does not compile as one module, a single-entry probe module is compiled
per example (never executed; counted as `coverage_probes`), the covered examples
compile into ONE shared module and run there, and each uncovered example runs on
the reference evaluator (fuel 1,000,000 per run, `runs_per_example` runs, results
must be identical). Each example reports `engine`: `jit`, `evaluator` (with
`fallback_reason`, the batch compiler's error) or `simpulse_evaluator`; the
report adds `jit_examples` and `evaluator_examples`, and text output ends each
line with `[jit]`, `[evaluator]` or `[simpulse]`. Evaluator failures are
failures, never skips (e.g. an ambient effect with no handler).

## Measured doc-build time

Linux x86_64 cloud host (4 vCPU), wall time per process including process
start, 10 runs each, `scripts`-free Python `subprocess` loop; files are the two
example sources:

| Command | dev build (`target/debug`) | release build |
| --- | --- | --- |
| `vesper doc broker.ves --bundle OUT` | median 14.7 ms (min 13.3, max 58.1) | RELEASE_BUNDLE |
| `vesper doc ledger.ves` (HTML) | median 12.2 ms (min 10.2, max 14.7) | RELEASE_HTML |
| `vesper test --doc ledger.ves` (3 examples: compile_fail, JIT, SimPulse) | median 79.7 ms (min 76.8, max 102.2) | RELEASE_TEST |

These are measurements of these two small files only, not a general
sub-100-millisecond claim.

## Tests

- `crates/vesper-doc/src/hoogle.rs` unit tests: variance (contravariant argument
  modes, covariant result modes), effect ceilings and purity, projection
  containment, wildcards, ranking (exact and proved first), typed errors for 15
  malformed queries and the size/nesting limits, strict index reader, and a
  search page that escapes `</script>` inside the embedded index.
- `src/tooling/doc/certs.rs` unit tests: store then link (idempotent), no link
  for another report, tampered bytes / escaping paths / non-replayed audits
  refused, missing stores not created, and a derivation record pointing at other
  bytes is an error.
- `tests/codex_rest_cli.rs` (real binary):
  - goldens `crates/vesper-doc/tests/fixtures/broker-heatmaps.json` and
    `broker-search-index.json` from `crates/vesper-doc/examples/broker.ves`
    (regenerate with `VESPER_UPDATE_SNAPSHOTS=1`), plus determinism and the HTML
    grid; enum/tuple/unused-type reasons;
  - 13 text queries with expected hits, exact ranking, typed query errors;
  - the bundle: files, no network references, existing directory refused, and
    when `node` is on `PATH`, the page's script run under Node against the
    bundle's index for 18 queries (including invalid ones) must equal the Rust
    matcher hit-for-hit (skipped with a message when Node is absent);
  - `store-olean` + `--olean-store`: missing before storing, linked after, the
    bundle copy and relative link, tamper refusal, no cross-report link, absent
    store not created;
  - doctests on both paths: JIT + evaluator + JIT in one file (one shared module
    holding the two covered entries, 3 probes), all-covered (0 probes), only
    evaluator (no module), and an evaluator failure reported as a failure.
- `crates/vesper-doc/tests/fixtures/ledger-codex.html` was regenerated: it gains
  the heatmap section and the `id` attributes.

## Checks (local, Linux x86_64)

CHECKS

## Root patch and other paths

No root manifest or lockfile change: `src/tooling/doc/certs.rs` uses
`vesper-loom-cas` and `blake3`, which the root package already depends on;
`vesper-doc` gained no dependency. New files outside the listed paths: the test
`tests/codex_rest_cli.rs` (new, so it cannot conflict). `src/main.rs` changes
only help text, in their own commits.

## Decisions

- Heatmaps show checker-enforced declared footprints rather than a new body
  analysis, because the checker already proves accesses stay inside them; a
  may-use refinement would be new analysis outside `src/check.rs` ownership.
- The search page runs JavaScript, not WASM: no new toolchain or dependency, and
  a parity test pins it to the Rust matcher.
- Certificates are bound to the exact proof-audit report bytes through a Loom
  derivation, so no store entry can lend a certificate to a different audit.
- Coverage is decided by compiling probe modules because `NativeBatch` (lane 23)
  has no coverage query; probes run only when the full set fails to compile.

## Remaining gaps

- No may-use footprint analysis, no `tiled(N)` hot-column data, no mode-crossing
  tables (§40.1 1b), and no capability badges (§40.1 item 2).
- Search covers documented functions of one file; no package-wide index, Pact
  model or impl indexing, no generic instantiation, no WASM engine.
- `.olean` links require running `store-olean` after `vesper proof-audit`; the
  proof-audit command itself does not publish to the store (not this lane's
  path). No `Funs.lean`/`Specs.lean` inline expansion, no semver diffs.
- Doctests: one shared module per file, not per package; probe modules cost one
  compile per example when the full set fails; SimPulse examples still run on
  the evaluator. No 64 KiB VirtualArena.
- Rust ownership of the host types is not evidence that the Vesper source typing
  rules are implemented.
