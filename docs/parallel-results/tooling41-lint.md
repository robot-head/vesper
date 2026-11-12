# Checked-source lint diagnostics and transactional fixes (#65)

Issue: [#65](https://github.com/robot-head/vesper/issues/65).
Branch: `tooling41/lint`, base `a1a8d938dd8103803219a05c8364cb7a198be8d9`.
Read issue, tooling contracts, refreshed §39 (including the relocated leaf lint
prototype after §40), and actual AST/World/IR/interface/SemVer implementation.
No core schemas, checker, native implementation, parser/lexer, compiler arithmetic,
interface schema, Warp code, or other lane files changed.

## Implemented behavior

New root-free `crates/vesper-lint` depends on existing `vesper-core` and Serde.
Its typed DTOs represent actual checked declarations, resolved calls and generic
substitutions, full effect instances, and selected Place accesses. They are
trusted compiler inputs, not untrusted plugin certificates or caller-provided
performed-name sets. `src/lint_commands.rs` checks real source, lowers Place IR,
and constructs them from the actual checked World.

`L0102 unused_effect_capability` recursively follows resolved direct calls and
substitutes actual generic types into required effects. Full structural type
arguments and labels remain distinct. Declared external/generated signatures
are conservatively required in full; source-owned closed bodies are analyzed.
Primitive checked integer/partial failures retain the actual
`Fail[ArithOverflow]` / `Fail[RuntimeError]` requirements; `?` retains the checked
Result operand's exact `Fail[E]`. Proved-total operations
can lose an unused failure capability. Opaque handlers/closures/indirect calls,
open rows, unresolved type/instance evidence, and recursion/deep calls skip
analysis conservatively and propagate through callers. They never count as pure.

`L0101 overbroad_inout_projection` derives actual reads/writes and effective callee
selectors from checked IR, using uniquely renamed local roots. It retains read
siblings and mutable leaf paths; full-root accesses/noncopy moves/opaque scopes
block narrowing. Already-projected signatures are left alone. These suggestions
are diagnostic-only because the current checker does not support the design's
same-name split parameter rewrite and changing arity would require caller edits.

Only mapped entries of closed explicit effect rows can be removed. The adapter
creates nonoverlapping exact-token edits, retains all comment/trivia bytes,
reparses and fully rechecks the complete candidate, and reruns the analyzer.
Any new diagnostic, unsupported evidence, surviving offered unused effect, or
failed checker rejects the transaction. `--semver-safe` uses actual old/new
`interface::export` results and `vesper_semver::compare`/`is_compatible`; unsupported
interfaces (including named capability scope and current trait/model contracts)
cannot be certified compatible even against themselves. A separate negative test
compares two fully checked supported interfaces with a real u64→u32 signature
change and verifies rejection.

The CLI reads one bounded regular UTF-8 file and rejects final symlinks. It stages
only after complete validation, uses exclusive temporary creation (Unix mode0600),
preserves Rust permission bits, syncs staged bytes, compares current source bytes,
and atomically renames as the last fallible operation. Failure removes only its
own successfully-created staging file. Readonly sources, stale source contents,
and preexisting staging collisions preserve both original/source owners.

## Bounds and conservative compiler preflight

Hard source limits: 256 KiB, 8,192 lexer tokens, 128-byte identifiers, delimiter
depth32, 256 nontrivia tokens between semicolon/brace boundaries, total64 `if`
tokens, a global256 recursion-producing operator/postfix/application/arrow token
budget, and 256 source items. These guards cover unary/postfix/result-type,
else-if and brace-reset binary chains before the unchanged parser/checker.

After parsing and before checking, a nominal structural-mode expansion guard
charges repeated source ADT field occurrences independently: 8,192 expanded type
visits, depth32. It covers nominal roots and explicit signatures/body annotations,
call type arguments, closures/handlers, effects and methods. Recursive nominal
contracts are refused conservatively because the core's recursion key includes
argument-mode vectors, not just nominal names. This was coordinated read-only
with the documentation owner after the compact doubling-ADT hazard was found.
Both compact nongeneric/generic expanding DAG regressions reject before checking;
a small ordinary generic instantiation remains accepted. These are scoped tool
limits, not a general compiler work/termination proof. Generic-name collisions
can conservatively reject extra source shapes.

Independent review also identified inferred tuple/result substitution expansion,
which needs no declared ADT DAG. Prechecker guards now refuse more than one total
generic occurrence in a result/individual ADT field, multi-parameter generic ADTs,
unresolved result types, inferred multi-element tuples with nonliteral elements,
generic impl/conformance/associated-type expansion, and Result constructors
without explicit closed types. These intentionally conservative source-profile
limits apply before all checking, including read-only lint runs. Ordinary generic
effects, small one-parameter data, single-use generic result identities, and
explicitly typed Result construction/propagation remain covered by positive tests.
A guard-only unit regression exercises 25-level tuple/generic growth plus generic
constructor/field cases without invoking the core checker at all.

Leaf limits: 256 functions,16,384 actions,1,000,000 traversal units,call depth64,
type depth32/nodes512,effect/generic/substitution arity64,parameters/fields128;
8,192 source edits/256KiB output. Work units count traversals, not CPU instructions
or all structural-comparison overhead. Host allocation is used.

## Validation and example

Private target `/tmp/tooling41-lint-target`, one build job, zero development/test
debug info and disabled incremental compilation. Temporary root registration and
main routing enabled actual CLI tests and were removed before this commit.

Passed:

```sh
cargo test -p vesper-lint
cargo clippy -p vesper-lint --all-targets -- -D warnings
cargo test -p vesper --test lint_cli
cargo test -p vesper --bin vesper lint_commands::tests
cargo clippy -p vesper --bin vesper --test lint_cli -- -D warnings
cargo fmt -p vesper-lint --check
rustfmt --edition 2024 --check src/lint_commands.rs tests/lint_cli.rs
```

Four leaf tests, nineteen actual CLI integration tests, four adapter unit tests.
Cases cover direct/transitive calls, generic substitutions/instances/labels,
mismatched effect arguments, handler/closure/indirect/open-row/recursive scopes,
projection selectors/read siblings/shadowing/root access, actual checked
arithmetic and linear cleanup, accepted fixes that disappear on a second pass,
actual interface success/unsupported/breaking gates, comments, malformed/bounded
inputs, nominal expansion, symlinks/readonly files, stale and overlapping edits,
concurrent-change detection, and existing staging owner preservation.
The accepted pure-function fix is additionally executed and returns42.

`crates/vesper-lint/examples/lint_demo.ves` yields two L0102 findings plus one
L0101. Tested a disposable copy through `lint --fix --semver-safe`: both unused
effects disappear, the diagnostic-only projection remains, and `vesper run`
executes the fixed entry and returns42. The original example's declared Audit
row intentionally prevents an unhandled entry from running before this fix.

## Coordinator registration / CLI contract

1. Add `"crates/vesper-lint"` to root workspace members (and default-members for
   extracted-crate test parity).
2. Add root dependency `vesper-lint = { path = "crates/vesper-lint" }` and update
   the coordinator-owned root lockfile. No new external dependencies are needed.
3. Add `mod lint_commands;` in `src/main.rs`, plus an early route before generic
   source command parsing:

   ```rust
   if command == "lint" {
       return lint_commands::run(args.collect());
   }
   ```

4. Add `lint FILE [--json] [--fix] [--semver-safe]` to global help. The adapter's
   public `HELP` supplies command-specific help. No root-lib registration needed.
5. Adapter APIs: `run(Vec<String>) -> Result<(),String>` and
   `prepare_source(&str, fix:bool, semver_safe:bool) -> Result<PreparedLint,String>`.
   `PreparedLint` contains a `SourceReport` and optional checked source candidate;
   it performs no filesystem writes itself. Leaf APIs are
   `analyze(&CheckedProgram,Limits)` and `apply_edits(source,edits,max_bytes)`.
6. Warnings return success, errors nonzero. JSON schema1 reports use status
   `checked`, diagnostics with code/function/span/detail/fixable, skipped reasons,
   changed/applied_edits/work and optional actual SemVer report; failures use
   status `invalid` and error text. Reports describe input findings; rerun to
   inspect post-fix diagnostics. `--semver-safe` requires `--fix`.

No arena demotion, generic-mode relaxation, FIP, Cancel/secret lint, plugin packs,
Loom severity policy, LSP/incremental scheduling, egglog rule engine, new grammar,
formal ownership theorem, or broad zero-false-positive claim is implemented.
Filesystem publication assumes cooperative paths: no hostile parent-directory
or final compare/rename race defense, hardlink preservation, full Windows ACL
copy, directory-fsync/durable transaction, or general cross-platform ACL proof.
