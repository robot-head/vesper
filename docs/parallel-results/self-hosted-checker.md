# Self-hosted Stage 1 checker (§42.3, §4–§6)

Issue #581. Base `eb97e086f5486abea4db38acf930477b52661a72`, a descendant
of the required `37f7b8b`. Branch `codex/issue-581-self-hosted-checker`.
Synced with main `d76067cb885d74d561dccc8007861052327025c9` after review.
No new dependencies, shared compiler edits, or unsafe code. The isolated root
BUILD patch declares the complete corpus and the checker test's measured
runtime; implementation files stay within this lane.

## Delivered slice

The resolver, kind checker, expression checker, ownership environments, joins,
and diagnostics execute as Vesper source. The Rust test harness supplies only
source bytes through `source_byte`/`source_len`, invokes the Vesper entry point,
and compares results against the Rust seed. It supplies no AST, resolved names,
types, modes, ownership facts, or expected diagnostic to the Vesper program.

Assembly order is the existing package's five parser files, followed by
`ast_core.ves`, `resolve_core.ves`, `check_modes.ves`, `check_adt.ves`,
`check_expr.ves`, `check_blocks.ves`, and `check_driver.ves`.
`tests/self_host_check.rs` demonstrates the complete assembly. Loom still does
not assemble these compilation units; this lane does not edit its manifest.

The public entry is `check_main() -> i64`. Success is zero. Errors originate as
`Fail[CheckError]`, whose variants are `Semantic(code, start, end)`,
`Unsupported(rule, start, end)`, and `Limit(at)`. At the host boundary they are
encoded as `-1 - (code * 2^32 + start * 2^16 + end)`:

- Semantic codes are the seed's decimal diagnostic suffix (`302` = `E0302`).
- `9001`–`9009` identify unsupported rules, documented in `ast_core.ves`.
- `9701`–`9708` identify the existing parser's unsupported constructs.
- `9801`, `9802`, `9804` preserve lexer/parser rejection categories.
- `9900` is a resource limit or internal checked-arithmetic failure.

Supported:

- Lexical scopes, shadowing and restoration, exact bytewise identifier matching
  (including long identifiers), forward function/type references, separate
  function/type namespaces, and the seed's alphabetical declaration-check order.
- Monomorphic nominal structs/enums, unit/tuple/named variants, constructors,
  destructuring, exhaustive constructor matches, and `i64`, `bool`, `Unit`.
- Monomorphic function types and values, explicit function modes, passing-mode
  checks, contravariant parameter/covariant result compatibility, effect-row
  inclusion and function-mode strengthening.
- `copy ⊑ affine ⊑ linear`, nominal floors and transitive component joins;
  `k_i ⊑ m_i` for every annotated field. A visited declaration set bounds
  traversal of shared ADT graphs; active cycles are explicitly unsupported.
  After that premise is validated,
  `k_i ⊓ m_i = k_i`, so field kind joins retain the component kind.
- `in`, `inout`, `sink` on whole bindings; mutation of copy places; persistent
  argument loans including accesses inside later arguments; shared/shared
  aliasing; rejection of borrowed ownership transfers and temporary linear
  borrows.
- Linear exhaustion at lexical/function/arm exits, shadowing checks,
  use-after-move, linear expression disposal, and ownership agreement at
  `if`/`match` joins (affine moves are unioned).
- Unlabelled finite `Fail[T]` rows, typed-instance matching, missing-effect and
  mismatched-instance diagnostics, and the abort frame condition for live
  linear owners and active `inout` parameters.
- Small integer/bool/unit literals, comparisons, calls, blocks and copy-place
  assignment. Integer literal magnitudes are bounded to `2^40` in this adapter.

## Parser adapter and explicit boundaries

At implementation time #580 remained open with no merged tree API. The current
parser on main produces a digest, not an addressable tree. The checker therefore
uses a token adapter in its own new files, sharing the existing self-hosted
lexer and significant-token cursor. Semantic success and semantic diagnostics
are validated by that existing self-hosted parser. Unsupported inputs return
immediately rather than constructing a purported checked program. The adapter
can be replaced when lane 3's tree interface merges; no unmerged code is used.

This is a bounded core checker, not a complete Stage 1 compiler or a checker for
all syntax accepted by the parser. Unsupported syntax/rules include generics,
traits/models/impls, non-unit tuples, other scalar types, strings, arithmetic,
closures, lexical handlers/state/continuations, `?`, projections/partial moves,
regions, capability labels, open rows, and recursive modal kinds. Constructor
and pattern field order must follow the declaration; reordering is explicitly
unsupported. Literal/wildcard match patterns and refutable nested payload patterns are outside
the constructor-match slice. Ambiguous unqualified constructors return
`Unsupported(8)`; qualified constructors remain supported. Function joins need
identical parameter shapes and a common type already represented by one of the
source signatures. Incomparable rows/results return `Unsupported(2)`. Passing
function values whose signatures contain effects (including nested arrows), and
binding those parameter types under an enclosing nonempty Fail row, return
`Unsupported(7)` until lexical capability identities are represented. There is no elaboration or code-generation output.

Bounds are 32,767 source bytes, 512 significant tokens, 16 top-level
declarations, 16 simultaneously recorded bindings (shadow history included),
16 active argument loans, and explicit type/expression/descent limits.
Over-budget units return typed `Limit`; they are never counted as accepted.
Repeated token scans and fixed immutable tuples replace unavailable collections.

The checking phase executes inside `region checker_arena(4096)`. Its Vesper
capability rows contain only `Fail` instances, with all failures caught by the
entry point; there is no ambient I/O. The region does not store a real AST or
symbol arena. The host evaluator allocates, and no counting-allocator test or
Lean refinement proof was performed. No zero-allocation, complete Stage 1,
metatheoretic soundness, or bootstrap/DDC claim follows from these tests.

## Validation

Initial delivery validation at `1df87348ff281dde8671e2563724d6591031b896`:

- `cargo test --locked -p vesper --test self_host_check -- --nocapture`:
  **4 passed**, 0 failed, 176.37 seconds. The 58 acceptance fixtures pin
  semantic successes/failures and exact diagnostic spans. Additional inline
  cases reject unsupported syntax, malformed input, recursive kinds, and
  binding/token/declaration limits. Three programs run on the Cranelift JIT;
  their outcomes also match ordinary full-unit compilation and the evaluator.
- The differential test visited **417 repository sources**: **24 accepted**,
  **48 rejected**, **345 explicitly unsupported or over budget**. All 72
  supported outcomes match the seed, including first error code and exact
  byte span. No disagreement allowlist. Independent repository fixtures have
  positive/negative coverage thresholds in addition to the new lane fixtures.
- `cargo +1.100.0 fmt --all -- --check`: passed.
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  passed. Cargo additionally reports inherited unused-dependency manifest
  warnings (outside the Rust/Clippy warning gate).
- `vesper fmt --check` on the seven new compiler files and fixture directory:
  passed, 65 files unchanged.
- `python3 tools/bazel/sync_build_files.py --check` and `git diff --check`:
  passed; no BUILD change needed.
- `cargo test --workspace --locked`: attempted on the original base and again
  after the main sync. Both stop at the existing
  `genuine_three_model_replay_and_independent_refusal_matrix` in
  `tests/lean4lean42_acceptance.rs`: `VESPER_LEAN4LEAN42_ROOT` must name a
  genuinely prepared candidate. This host has none. The target reports
  15 passed/1 failed; its policy was not bypassed or changed. This is not a
  passing full-workspace result.
- Bazel is not installed on this host; hosted CI remains the integration gate.

Focused runs use Rust 1.99.1, debug builds without debug information or
incremental compilation, `CARGO_BUILD_JOBS=2`, and an isolated target directory.
The evaluator disables optimization and has a 100-million-step fuel ceiling.
The corpus uses four workers on this 5-CPU/18-GB host. The timing above is one
acceptance run, not a performance benchmark. The harness checks the Vesper
compiler once, then combines its unchanged checked AST with separately checked,
disjoint-span byte-provider functions; native tests cross-check that assembly
against checking the whole concatenated unit.

The corpus test walks every available repository `.ves` file in `bootstrap`,
`crates`, `examples`, `packages`, `src`, and `tests/fixtures`; under Bazel it
walks declared runfiles (cross-crate fixture availability follows existing root
BUILD declarations). Every supported outcome must agree with the seed on
accept/reject, first diagnostic code **and exact byte span**. There is no
semantic disagreement allowlist. The test also enforces minimum positive and
negative counts. Unsupported and limit outcomes are counted separately.
Acceptance fixtures pin expected negative codes independently of the oracle.
Native tests compare representative success, failure and branch ownership
programs with the same oracle.

## Lean environment and review follow-up

The previously missing environment is now installed outside the repository:

- Lean/Lake: `/workspace/toolchains/lean-4.24.0-linux`, exact Lean commit
  `797c613eb9b6d4ec95db23e3e00af9ac6657f24b` (release 4.24.0).
- Official release ZIP SHA256:
  `8f2ce2aa6de5ede5847a3e9517a930ede3d6954444f44535155da3250ac7f2fc`.
- Prepared checker: `/workspace/toolchains/lean4lean42-4faf514-v2`, built by
  the unchanged `tools/lean4lean42/setup.py` with the genuine Cargo-built
  `lean4lean42` driver, pinned Lean4Lean `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa`
  and Batteries `8da40b72fece29b7d3fe3d768bac4c8910ce9bee`.
- Setup completed all 55 Lake jobs; all 94 controls matched. The genuine
  schema-2 `vesper.lean4lean42/2` receipt has SHA256
  `4e55bfe71c0f6595b0ae4f75d1820cffbe1a83f445b02125c8ad8ee1ce81ac2d`.
- Reusable shell configuration:
  `source /workspace/.vesper-setup/lean4lean-env.sh`. This also supplies the
  actual `VLB_CARGO_LOCK` required by the existing build-identity code when
  the Cargo target directory is outside the checkout.
- `scripts/ci/check-lean-models.sh`: passed with the installed pinned toolchain.
- `cargo test --workspace --locked --no-fail-fast` runs against the frozen
  initial delivery revision with both `VESPER_LEAN` and
  `VESPER_LEAN4LEAN42_ROOT`. The formerly blocked acceptance target now passes
  **16/16**, including genuine three-model replay, in 836.83 seconds. The
  remaining workspace results are tracked in the PR validation notes.

The review fixes are developed in an isolated checkout while that baseline
suite runs, so its source and fixture inputs remain unchanged. Regressions cover
callee ownership under shared arguments, common function-result types across
match/if branches, modes and innermost selection for function Fail payloads,
reserved builtin declarations, ambiguous constructors, and captured capability
scope. Existing source handles are retained for representable joins; cases
requiring synthesized joins or capability identities are explicit typed
unsupported results, as specified above.

The reviewed checker passes all four focused tests (386.09 seconds while
running alongside the baseline proof suite and the parser differential test).
Its corpus contains **442 sources**: **30 accepted, 63 rejected, 349 explicitly
unsupported/limited**, with exact first-code/span agreement on all 93 supported
results. There are 79 semantic acceptance fixtures and four explicit unsupported
regressions, plus the inline malformed/limit controls. Five evaluator/full-unit/
JIT comparisons pass. Strict Rust 1.100 focused Clippy, workspace Rustfmt,
canonical formatting of all 90 new `.ves` files, BUILD synchronization and
whitespace checks pass.

The existing self-hosted front-end target also passes both tests on a 413-source
snapshot (691.18 seconds under the same concurrent load): all token streams
match, with 341 CST matches, eight typed unsupported inputs, 64 seed-rejected
inputs, and 37 native executions. The one fixture added after enumeration,
`unsupported/function_param_join.ves`, is separately checked through `parse_main`
and the seed's CST: both produce digest `7736908835817788439`. These timings are
loaded local acceptance runs, not benchmark or hosted CI claims.

The complete baseline workspace outcome is tracked in the validation notes of
[PR #624](https://github.com/robot-head/vesper/pull/624). Its tested revision is
`1df87348`; the reviewed Vesper checker has the separate focused and front-end
validation above. Rust compiler/runtime and Lean proof source bytes are unchanged
by the review fixes.

## Main refresh and CI repair

Main commit `9b06602b` made the manuscript the sole specification and retired
the duplicate snapshot, its Python checker and its CI invocation together.
The supplied Native/acceptance log still invoked the deleted checker. Merging
current main brings that coordinated removal into this branch; no replacement
snapshot or conditional skip is introduced.

macOS job `132749653497` also showed this lane's new test hitting Bazel's
default 300-second timeout after its acceptance and limit tests passed. The
isolated BUILD patch gives `test/self_host_check` the existing `long` class
(900 seconds). It also declares the bootstrap and cross-crate corpus data
needed by this target. Pinned Bazel 9.3.1 queries confirm the timeout and show
that its declared corpus contains all **443 tracked `.ves` files**, with no
missing source. The source checker, fuel limits, fixtures and assertions are
unchanged by this CI patch.

Validation after merging main:

- `cargo test --locked -p vesper --test self_host_check -- --nocapture`:
  **4 passed**, zero failures, 282.94 seconds while Clippy ran concurrently.
  Differential counts: **30 accepted, 64 rejected, 349 unsupported/limited**;
  all 94 supported results agree on first diagnostic code and exact span.
- Strict Rust 1.100 full-workspace Clippy and workspace Rustfmt: passed.
- BUILD synchronization, whitespace, CI workflow Python tests (50 run, one
  existing platform skip), all tracked Vesper formatting (432 canonical,
  11 existing exclusions), and doc-link acceptance (49 commands): passed.
- The earlier complete workspace run with genuine Lean4Lean remains
  **5,257 passed, zero failed, eight ignored** at baseline `1df87348`.
  That baseline result is separate from the merged-head checks above.

The same macOS log reports the independent retired-README benchmark-help
regression. Issue #632 and PR #633 own that repair; this lane does not duplicate
their changes. Fresh hosted checks remain the final integration gate.

## Missing language features, ranked

1. Region-backed indexed buffers and persistent/arena maps: these replace the
   fixed tuple environment, move bitmap, and repeated token/declaration scans.
2. Byte slices and byte-string comparison: source injection and identifier
   comparisons currently run through generated pure functions.
3. A stable addressable CST/AST interface: this removes this lane's source-token
   adapter and shares parser recovery/span metadata with later phases.
4. Loops or guaranteed tail calls, with source-level work budgets.
5. Recursive algebraic data and useful generic collections/visitors, for a real
   typed HIR and place capability graph.
6. Integer conversions and bit operations, for compact arena indices, modes,
   ownership sets and direct wire diagnostics.
7. Struct update syntax and module constants: immutable state currently needs
   explicit reconstruction and numeric token tags.
8. Multi-file package assembly in Loom, so consumers can invoke the package
   entry without concatenating a source list themselves.

Follow-on work is the unsupported language surface above, integration with
lane 3's merged tree, scalable arenas/PCG, Lean refinement, and bootstrap
integration. These remain separate from the working monomorphic slice.
