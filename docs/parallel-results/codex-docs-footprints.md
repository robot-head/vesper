# Wave eight lane 17: Codex may-use footprints, mode-crossing tables, capability badges and a package-wide index (#466)

Base: `1094e64` (origin/main, a descendant of the required `ea1ed29`). Branch:
`claude/project-thread-iargwt`. Claim issue: #466. Before starting, open issues
and PRs were checked; nothing overlapped (open work: Codex's Lean4Lean PRs,
PR #165 and the other wave-seven/eight lanes). Lane 16 (Lens fixtures) may add
a narrow preflight hunk to `crates/vesper-doc`; this lane did not touch
`source_preflight`.

## What was built

### 1. May-use footprints and inferred effect rows (§40.1 1b)

`vesper_doc::footprints` walks every documented function body (the checked
declarations from the `World`) and records, per parameter, the FieldTree paths
the body may touch and how: assignment targets are `write` (compound
assignment too), `inout` arguments `write`, `in` arguments `read`, `sink`
arguments `consume` (of a `sink` parameter) or a copy-out `read`, method
receivers the strongest access the parameter's own pass allows (the receiver
pass is not known there), and every other occurrence a `read` (or `consume`
for a whole `sink` parameter). Index expressions truncate the path. Names
bound by `let`, `match` arms, closure parameters and handler clauses shadow
the parameter. It is a sound over-approximation over the surface syntax of
uses *through the parameter's name*; it is not an alias analysis.

Each parameter reports `uses` and `unused_declared`: declared projection paths
no use reaches (`inout account.{balance, pending}` that only writes `balance`
reports `pending`). The heatmap rows gain `may_use` cells next to the declared
ones, and a declared-but-unreached cell renders dashed and lowercase.

The same walk infers the effects the body may perform: `Effect::op` calls, the
declared rows of called documented functions, `?` and checked arithmetic
(`+ - * / % << >>`, unary `-`, compound assignment) as `Fail`, minus effects
handled by an enclosing `handle`. Calls to a `let`-bound closure literal or a
handler clause's continuation are counted where their bodies are written. A
method call, a call through a parameter or any other local (a function value
such as `let cb = record; cb()`) or an unknown callee makes the row incomplete; only a complete row
reports `declared_unperformed` (e.g. `eff [Journal, Metrics]` that never
ticks `Metrics`). The declared row remains the checked contract.

### 2. Mode-crossing tables (§40.1 1b)

`vesper_doc::modes` renders, for every generic ADT that is documented or that
a documented signature or field mentions (builtins such as `Option`), the
`[K-Adt]` mode of the type for each assignment of modes to its parameters:
the full product for up to 4 parameters (81 rows), else the three diagonal
rows (`complete: false`). Modes come from the checker's own `World::mode`
(declared mode joined with field modes, modalities applied); rows that exceed
a declared bound (`[T: mod affine]` at `linear`) are `rejected` with the
reason, never given a mode. Row parameters are listed as unsupported.

### 3. Capability badges (§40.1 item 2)

`vesper_doc::capabilities` derives badges from each function's checked row:
Pure (`eff []`), Zero-Alloc (no `Alloc`), Uncancellable Linear I/O (a
`linear effect` in the row), Abortable Checkpoint (`Fail` or `Cancel`, naming
the linear parameters passed `in`/`inout` that stay live in the caller),
Constant-Time Verified (`ConstTime`), Hard-Realtime Safe (`Realtime`) and
Deterministic Float (`DetFloat`). Each badge carries its evidence text; with
the may-use analysis it also says whether the inferred body row performs the
effect. Open rows get no badge (`undetermined`). Capability effects are the
checker's source rules (§23.2, §32.2, §36.2), not machine-code proofs, and no
WCSD stack bound is computed, which the Hard-Realtime evidence says. A package
summary intersects the badges of every function and lists effects outside
the `Loom.toml` `capability_ceiling`.

### 4. Package-wide search index, impls, Pact models and generic instantiation (§40.1 items 4, 5)

`vesper doc PACKAGE_DIR (--json | --bundle DIR)` documents every `.ves` file
of the package (the same discovery as path-less `vesper lint`), reads the
manifest's name and ceiling with `vesper_loom_calculus::parse_manifest`, and
builds ONE `vesper-codex-search` **v2** index (`hoogle::build_package_index`):

- functions under `file::name` (e.g. `src/ledger.ves::post`);
- methods of source `impl` and Pact `model` declarations from the elaborated
  generated functions, as `file::Entry as Score::score` (`impl_method`) and
  `file::model ByCount::score` (`model_method`), with trait and model names;
  a generic trait keeps its arguments (`X as Convert[i64]::convert` and
  `X as Convert[bool]::convert` stay distinct);
- each entry links to its file's page (`files/src/ledger.ves.html#item-post`),
  percent-encoded so file names with `#`, `%` or spaces still link.

Impl and model methods also join the per-file and package capability
summaries under the same paths.

Generic functions are no longer excluded: a type parameter becomes
`IndexType::Var` and unifies with the query type, consistently across the
signature (`keep[T](sink T) -> T` matches `u64 -> u64`, not `u64 -> bool`);
a partial pattern unifies with a later one (`Option[_] -> Option[u64]` binds
`T = Option[u64]`).
The parameter's mode in the index is its bound, so variance checks hold for
every admitted instantiation (`tag[T: mod copy]` refuses an `affine`
argument). Wildcards never bind. A hit lists its `instantiation` and is never
exact. Row binders, open rows, effect rows mentioning a type parameter and
function-typed signatures stay unsupported. Name queries match the path, so a
trait or model name finds its methods. The page script (`search.js`) was
ported in step, and a Node parity test pins it to the Rust matcher on the
package index (and on the single-file index, as before).

The bundle holds `index.html` (files, package capabilities, search link), one
page per file under `files/`, `search.html` and `search-index.json`. Any file
that does not check fails the package with its name.

### 5. One-step `.olean` publishing (§40.1 item 4)

`vesper doc ... --proof-audit REPORT.json --olean-store DIR|default
--olean-root LEAN_BUILD_DIR` publishes each accepted, kernel-replayed audit's
inventoried `.olean` files into the Loom store (`certs::store_certificates`,
the existing API) and links them in the same run, so `store-olean` is no
longer a separate step. Bytes that differ from the audited size/BLAKE3 are an
error; `--olean-root` without `--olean-store` is an error. The
`vesper proof-audit` command itself was not changed (not this lane's path):
its JSON output is the input here.

### 6. Per-package doctest modules (§40.1 item 3)

`vesper test --doc [DIR]` now checks each file's examples against that file
exactly as before, then concatenates the per-file merged modules into ONE
package module, checks it, and compiles the runnable examples of every file
into ONE shared `NativeBatch` JIT module (with the existing per-example
evaluator fallback). This requires the files' top-level names to be disjoint
(files are separate modules with no imports); when the package module does
not check, exceeds a budget, or an entry is defined twice, every file runs in
its own module and `package_fallback` gives the reason. The JSON adds
`package_module`, `package_fallback`, `batch`, `coverage_probes`,
`jit_examples`, `evaluator_examples`; text output adds a
`package module: N entry point(s) in one shared JIT module` line. The
single-file path (`vesper test --doc FILE`) is unchanged.

## Tests

- `crates/vesper-doc` unit tests: may-use writes/reads/compound assignment,
  shadowing by `let`/`match`/closures, call passes and unknown receivers
  (incomplete rows), handled effects, heatmap cell attachment; generic
  instantiation (consistency, bounds, wildcards never bind), package index
  paths, impl signatures and duplicate refusal; mode-table products, diagonal
  fallback and bound rejection; capability badges, open rows and package
  intersection/ceiling.
- `tests/codex_footprints_cli.rs` (real binary):
  - goldens `crates/vesper-doc/tests/fixtures/account-footprints.json` (may-use,
    mode crossings, capabilities, heatmaps) and `account-footprints.html` (the
    rendered sections) from `crates/vesper-doc/examples/footprints/account.ves`,
    determinism, and targeted assertions;
  - the multi-file package `crates/vesper-doc/examples/codex-package` (three
    files, a manifest ceiling, a trait with an impl and a model, two generic
    functions): goldens `codex-package-search-index.json` and
    `codex-package-capabilities.json`, 12 search queries with exact ranked
    paths (instantiation, impl/model methods, bounds), typed package errors;
  - the package bundle: files, links, no network references and Node parity of
    the page script against the package index for 17 queries;
  - one-step `.olean`: store created and linked in one run, idempotent, later
    lookups, bundle copy, tamper refusal, `--olean-root` without a store;
  - package doctests: two files' JIT examples in one module (entry_count 2,
    module_count 1) beside an evaluator fallback; a name collision falls back
    to per-file modules that still pass.
- Updated goldens: `broker-heatmaps.json`, `broker-search-index.json` (v2
  fields) and `ledger-codex.html` (new sections, limitation texts).
- `tests/codex_rest_cli.rs`: two expected hit lists now include the generic
  `identity`, which search matches by instantiation (its own commit).

## Checks (local, Linux x86_64)

```sh
cargo fmt --all -- --check                                              # clean
CARGO_TARGET_DIR=target/clippy100 cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings  # clean
cargo test -p vesper-doc                                                # pass
cargo test -p vesper --test codex_footprints_cli --test codex_rest_cli --test codex_cli --test doctest_cli \
  --test doc_cli --test doc_links --test doc_links_cli --test doc_navigation --test doc_navigation_cli  # pass
cargo test --workspace --locked --no-fail-fast                          # see below
vesper fmt --check crates/vesper-doc/examples/footprints/account.ves crates/vesper-doc/examples/codex-package/src/*.ves  # unchanged
```

`cargo test --workspace --locked --no-fail-fast`: 591 suites pass, 7 fail, none
in this lane's code. Six need a local `lean` on `PATH` or a prepared Lean4Lean
candidate, which this cloud host lacks (`lean4lean42_acceptance`,
`oracle42_acceptance`, `oracle42_runner`, `proof_audit_private`,
`replay42_kernel`, `resource_model42`: "lean --print-prefix: No such file or
directory"); they run in Linux CI, where Lean is installed. The seventh is a
`vesper-simd` doctest (`JitScanner`) whose link step fails on this host, in a
crate this lane does not touch. The Node parity checks ran locally (Node 22).

## Root patch and other paths

No root manifest or lockfile change: the root package already depends on
`vesper-loom-calculus`, `vesper-loom-cas`, `vesper-ffi` and `blake3`;
`vesper-doc` gained no dependency. Files outside the exclusive list: the new
test `tests/codex_footprints_cli.rs` (new, cannot conflict) and the
two-line expectation change in `tests/codex_rest_cli.rs` (wave-seven lane 14's
test of this same crate). `src/main.rs` is untouched.

## Decisions

- May-use is computed in `vesper-doc` over the checked AST (no new checker
  pass in `src/check.rs`, which has no owner this wave). It over-approximates
  through names and does not chase aliases; the declared footprint stays the
  checker-enforced bound.
- Inferred effect rows are reported, but badges come from the declared row,
  which is what callers may rely on; an incomplete inference never claims an
  unperformed effect.
- Mode tables call the checker's `World::mode` rather than re-deriving
  `[K-Adt]` in the library.
- Generic search binds a type parameter to the first concrete pattern and
  compares later occurrences structurally; wildcards never bind. Instantiated
  hits are never "exact", so concrete contracts rank first.
- A package module is the concatenation of per-file modules, because files
  have no imports: it is used only when it checks, otherwise per-file modules
  with an explicit reason (never a silent merge or rename).
- `.olean` publishing is a `vesper doc` option over the proof-audit output, not
  a change to `vesper proof-audit`, which is outside this lane's paths.

## Remaining gaps

- No alias-aware or interprocedural may-use (a field copied into a local and
  passed on counts as the read that copied it); method receivers are
  conservatively the parameter's own pass.
- No `tiled(N)` hot-column data or L1 cache-line maps for tiled structs.
- No WCSD stack bound for Hard-Realtime; no Loom-level enforcement of the
  ceiling (Codex only reports effects outside it).
- Search: no row-polymorphic instantiation, no trait-bound (dictionary)
  constraints on generic matches, no WASM engine; impl and model methods carry
  no proof badge; no intra-package cross-file links (files have no imports).
- `vesper proof-audit` still does not write to the store itself; no
  `Funs.lean`/`Specs.lean` inline expansion or semver diffs.
- Doctests: the package module needs disjoint top-level names; SimPulse
  examples still run on the evaluator; no 64 KiB VirtualArena.
- Rust ownership of the host types is not evidence that the Vesper source
  typing rules are implemented.
