# Bounded Loom lint severity policy — issue #76

Branch `protocol41/lint-policy`, base `9eeaecf`. Implements actual TOML policy
for the checked-source lint command, following §39.2 and
`docs/rfcs/protocol41-contracts.md`. The original analyzer, compiler, source
preflight, interface compatibility checks, and staging ownership guard remain
unchanged.

## Public API and registration

`vesper_lint::policy` exports `Policy`, `PolicyLimits`, `PolicyUsage`,
`Severity`, `LintName`, `Rule`, `RuleStatus`, `PolicyError`, and `ErrorKind`.
`Policy::parse(&str, limits)` parses actual TOML after preflight;
`Policy::from_value(&toml::Value, limits)` validates the entire borrowed tree
before extraction; `Policy::from_rules(&[Rule], limits)` checks direct typed
rules, duplicates, names, and unavailable denies. All preserve borrowed inputs
on failure. `severity_for(&FindingKind)` uses typed finding variants, never a
caller-provided diagnostic name. `rules()`, `unavailable()`, and `usage()` expose
effective levels and bounded validation accounting.

The root adapter retains `prepare_source(source, fix, semver_safe)` and adds
`prepare_source_with_policy(source, fix, semver_safe, &Policy, provenance)`.
Neither helper writes files. CLI:

```text
vesper lint FILE [--json] [--fix] [--semver-safe] [--config PATH]
```

The only dependency addition is leaf `toml = "=0.8.23"`, already present in the
workspace lockfile. Coordinator-owned lock registration: add `"toml"` to the
existing `vesper-lint` dependency list. No new workspace or CLI route is needed.
The command-specific help includes `--config`; global help is coordinator-owned.

## Policy behavior

`[lints.vesper]` accepts exact documented names with exact lowercase
`"allow"`, `"warn"`, or `"deny"` strings:

| Name | Analysis support | Default |
| --- | --- | --- |
| `unused_effect_capability` | Checked resolved effects | warn |
| `overbroad_inout_projection` | Supported field projections, diagnostic only | warn |
| `demote_alloc_to_region` | unavailable | unconfigured |
| `overly_restrictive_mode` | unavailable | unconfigured |
| `missed_fip_opportunity` | unavailable | unconfigured |
| `early_cancel_checkpoint` | unavailable | unconfigured |
| `unredacted_pii_boundary` | unavailable | unconfigured |

Unrelated manifest sections coexist and receive the same whole-document bounds.
Unknown policy names/sections, malformed levels/shapes, and duplicate decoded
TOML keys fail. A future builtin configured warn/allow appears explicitly in
`policy.unavailable` with `available: false`; it produces no invented finding.
An unavailable deny fails before source editing. Empty `[lints.plugins]` is
accepted; every nonempty plugin request fails without loading anything.

Only explicit `--config` loads policy. There is no adjacent/default Loom.toml
discovery. No config preserves both existing warning defaults. Configs must be
bounded regular UTF-8 files, with final symlinks rejected; the reader also checks
opened metadata and reads at most 65,537 bytes. Successful reports identify the
absolute path label, exact raw byte count, and BLAKE3 hash of the bytes parsed.
Paths are bounded to 4,096 encoded bytes. The path label does not claim directory
symlink resolution or a stable inode identity. An invalid report identifies the
requested config path when available.

Severity filtering happens before selecting source edits. Allowed findings are
suppressed and cannot cause a fix. Warnings retain successful exit behavior.
Remaining denied findings produce a nonzero exit and exactly one JSON report
with `status: "denied"`; stderr carries the ordinary command error. A validated
fix that clears all denies succeeds. When a projection deny remains after a
different fix, the command still publishes the fully checked candidate and
reports `changed: true` and nonzero exit. Invalid config, failed candidate check,
or failed publication preserves the original source.

Existing `diagnostics` describe the input findings, including ones fixed during
the command. `outcome.remaining_denied` and `remaining_warnings` describe the
validated candidate (or input when unchanged); `suppressed` counts allowed
findings in that same analysis. The candidate still passes full parse/check,
lint-delta validation, and the actual exported supported-interface gate when
`--semver-safe` is requested. Projection fixes remain unsupported.

An explicit deny also fails closed if the analyzer reports conservative skips.
Checked trait implementation methods are lowered to generated functions and
excluded by the unchanged analyzer; `outcome.excluded_generated_bodies` counts
those bodies. Any such exclusion also makes configured denies unavailable.
`outcome.unavailable_denied_rules` identifies these guarantees rather than
pretending zero findings proves complete coverage. This can conservatively
reject a deny even when a skipped body would have no matching finding.

## Bounds and parser boundary

Default limits are also hard ceilings: 65,536 input/decoded logical bytes,
depth 32, 8,192 lexical tokens, 4,096 estimated/actual tree nodes, and 262,144
charged work units. All custom limits must be positive and no larger. Keys are
at most 128 decoded bytes; direct rules are at most seven and charge nodes and
work. Arithmetic uses bounded counts and checked accumulation.

Before invoking the TOML parser, a byte-bounded scanner handles comments,
basic/literal strings, escapes, multiline strings and their three/four/five
closing quote runs. Brackets or periods inside strings/comments are not
structure. It bounds tokens and a conservative node estimate, tracks nested
arrays/inline tables, and accounts for dotted assignment/header paths, including
array-of-table paths. Each key component charges two depth levels to cover
potential array/table nesting. This conservative profile can reject otherwise
valid deep TOML before parsing it. Dotted paths cannot bypass the nesting guard.

The entire resulting value is then checked iteratively, including unrelated
sections. Child counts and work are checked before extending the reference
stack; no unvalidated recursive clone is used. Typed TOML receives the same
iterative validation before extracting policy. Caller-created typed values and
their allocation/destruction remain the caller's responsibility. Usage reports
input bytes separately from decoded logical key/payload bytes, tokens, actual
nodes, and charged work. Work units describe validation operations, not measured
CPU instructions or exact allocator bytes; no constant-time or zero-allocation
claim is made. No formatting options, network access, or plugin execution.

## Verification

Private target `/tmp/tooling41-lint-target`, one build job, debug information off,
incremental compilation off. Passed:

```text
cargo test -p vesper-lint
  6 policy tests + 4 existing soundness tests
cargo test -p vesper --test lint_cli
  29 CLI tests (19 existing + 10 policy tests)
cargo test -p vesper --bin vesper lint_commands::tests
  6 adapter/staging tests
cargo clippy -p vesper-lint --all-targets -- -D warnings
cargo clippy -p vesper --bin vesper --test lint_cli -- -D warnings
cargo fmt -p vesper-lint --check
rustfmt --edition 2024 --check src/lint_commands.rs tests/lint_cli.rs
```

Policy tests exercise duplicate decoded keys, unknown names/shapes, exact levels,
future rules/plugins, comments and multiline quoting, nested arrays/inline
tables/AoTs/dotted paths, byte/token/node/work limits, whole unrelated typed
subtree validation, and borrowed input preservation. Actual CLI tests cover
warn/deny/allow exits, config provenance, allowed fixes suppressed, accepted deny
fix disappearance, changed-source reporting with remaining deny, invalid/deep
configs preserving source, explicit-only discovery, conservative handler and
generated-method deny coverage, and Unix symlink/directory refusal. Existing
direct/transitive/generic/instance/projection/linearity tests remain passing.

The real example copies `examples/lint_demo.ves` and `examples/Loom.lint.toml` to
a temporary directory, then invokes the actual executable:

```text
vesper lint demo.ves --config Loom.toml --fix --semver-safe --json
vesper run demo.ves
```

Observed: lint exit 0, `checked`, `changed: true`, two edits, zero remaining
denies, one remaining projection warning, three explicitly unavailable future
rules, and compatible interface classification `minor` with no issues/adapters.
The resulting source runs to `42`. No source analyzer coverage, allocator/Region
escape analysis, source macros, formatting configuration, or plugin guarantees
are added by this policy layer.
