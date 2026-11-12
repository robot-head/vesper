# Checked-source lint slice

This root-free crate analyzes **trusted typed compiler facts** for §39. Its facade
adapter, `src/lint_commands.rs`, parses and checks actual Vesper source, lowers
Place IR, and extracts resolved calls, generic substitutions, checked primitive
failures, and selected place accesses. The leaf depends on `vesper-core` and
Serde, with no dependency on `vesper` or another new tooling crate. DTOs are not
proof certificates or an input format for untrusted plugins.

After coordinator registration:

```sh
vesper lint FILE [--json] [--fix] [--semver-safe]
cargo run --bin vesper -- lint crates/vesper-lint/examples/lint_demo.ves --json
```

`lint_demo.ves` produces two unused-effect diagnostics and one diagnostic-only
projection suggestion. Use a disposable copy to try `--fix --semver-safe`, then lint again to see that
the unused-effect findings disappeared. The projection remains for caller review.
After this fix the entry has no effects and `vesper run` returns 42; the original
entry deliberately declares Audit and cannot be executed without a handler.

## Implemented diagnostics

- `L0102 unused_effect_capability`: full `Effect { label, name, args }` identities
  are compared structurally. Direct operations use checked inferred arguments;
  `Result` propagation (`?`) retains the operand's actual `Fail[E]`;
  direct calls recursively analyze the actual callee body and apply its checked
  generic substitution at each call site. External declaration-only callees
  conservatively require their declared closed effects. A used instance of
  `Mark[u64]` does not retain `Mark[u32]`. Explicit capability labels are retained.
  Identical unnamed declarations are conservatively retained together if used;
  each still has its own source index for diagnostics/edits when wholly unused.
- `L0101 overbroad_inout_projection`: whole-root inout parameters of resolved
  structs are compared to actual IR reads/writes and callee parameter selectors.
  Findings retain mutable/read field paths. Root reads, noncopy moves, unknown
  places, unsupported nested scopes, and already-projected signatures prevent
  narrowing suggestions. Local names are taken from IR's unique renamed roots,
  so shadowed locals cannot masquerade as accesses to a parameter. Reading a
  sibling field and writing another are reported separately.

Projection changes are **diagnostic-only**: no broken function arity, duplicate
parameter name, or unreviewed caller rewrite is synthesized. The analyzer never
infers arena escape freedom from a return type/name set. Mode relaxation, arena
demotion, FIP, Cancel/secret hygiene, egglog simplification, severity/plugin packs,
Loom configuration, LSP scheduling, and formal proof generation are unimplemented.
No general zero-false-positive or completeness claim applies.

Handlers, closures, indirect calls, unresolved instances/signatures, open or
polymorphic effect rows, and recursive/deep direct call graphs cause a
conservative skip that propagates through callers. They are not treated as pure.
No edit batch is published if there is unsupported/skipped evidence anywhere in
the analyzed file. Supported read-only diagnostics may still be displayed.
This deliberately favors fewer fixes over guesses about lexical capability scope.

## Transactional edits

`prepare_source(source, fix, semver_safe)` returns a report and optional candidate
without filesystem I/O. It only offers removal of mapped entries from explicit
closed source effect rows. Span mapping uses the actual lossless lexer and
checked function header, including nested type arguments. Edits remove individual
nontrivia tokens and the necessary commas; all whitespace and comment bytes stay
in place. Formatting the result is a separate operation.

`apply_edits` sorts and rejects overlapping/stale ranges, invalid UTF-8 boundaries,
empty edits, and oversized output. After constructing the complete candidate,
the adapter reparses, fully checks types/effects/ownership/PCG, and reruns lints.
Any failed candidate check, new lint, new unsupported evidence, or surviving
offered unused effect rejects the transaction. Reports describe the input
findings and include `changed`/`applied_edits`; rerun lint to inspect the new file.

`--semver-safe` requires `--fix`. It exports **both actual checked Worlds** through
`vesper::interface::export` under the same fixed package namespace and invokes
`vesper_semver::compare(old,new)`. Only `Report::is_compatible()` permits publication.
Unsupported contracts (including named capability scopes and current trait/model
interface gaps) cannot pass this gate, even against themselves. This is the
existing conservative source-interface relation, not a `.vhir` ABI/proof certificate.

The CLI accepts one bounded regular UTF-8 file, rejects final symlinks, and writes
nothing until all analysis/validation succeeds. A new private staging file is
created exclusively in the same directory (Unix creation mode 0600), written,
assigned the source's Rust permission bits, and synced. Source bytes are reread
and compared before the final atomic rename. Failure cleans up only a staging
file this process successfully created. Existing staging collisions, stale source
contents, readonly sources, validation errors and write errors preserve the source.
No error is reported after a successful rename. No directory-fsync/durable commit,
hardlink preservation, full Windows ACL cloning, hostile-parent-directory defense,
or atomic compare-and-swap against concurrent editors is claimed; the final
read/rename race requires ordinary cooperative filesystem use.

Warnings return success; parser/checker/unsupported-fix/compatibility/I/O failures
return nonzero. `--json` emits schema-version-1 reports or a structured failure.

## Bounds and API

The CLI caps source at 256 KiB, lexer tokens at 8,192, identifiers at 128 bytes,
delimiter depth at 32,
nontrivia token runs between `;`/`{`/`}` at 256, total `if` tokens at 64, and
a global 256-token recursion-producing operator/postfix/application/arrow budget
that remains effective across brace resets.
These preparse guards cover unary/postfix/result-type and else-if recursion.
After parsing and before the unchanged checker, a nominal structural-expansion
preflight visits source ADT roots and explicit signature/body types, charges
repeated sibling expansions independently (8,192 visits, depth 32), and rejects
recursive nominal contracts. This prevents a compact doubling ADT DAG from
triggering exponential `World::mode` work. Generic-name/nominal-name collisions
can conservatively reject extra inputs. These are conservative tool-scope limits,
not a general resource proof of the existing compiler.

Inference can also clone types exponentially without a nominal DAG (for example,
repeated `let x = (x, x)` or `dup(dup(...))`). Before checking, this slice therefore
refuses results or individual ADT fields containing more than one generic
occurrence in total, ADTs with multiple generic parameters, unresolved result
types, inferred multi-element tuples with nonliteral elements, generic
impl/conformance and associated-type expansion. Result constructors require two
explicit closed type arguments. Generic effects/data with one parameter and
single-use generic result identities remain supported. These restrictions apply
even to read-only linting and may reject otherwise valid source. They keep
checking within this tool's reviewed profile, without asserting that unsupported
source is unsafe. Guard-only tests deliberately never invoke the core checker on
compact expanding inputs.

The leaf API is `analyze(&CheckedProgram, Limits) -> Result<LintReport, LintError>`.
`CheckedProgram` contains `FunctionFacts` with core declarations and typed `Action`
facts (`Perform`, resolved `Call`, selected `Access`, `Opaque`). Default/hard maxima
are 256 functions, 16,384 actions, 1,000,000 traversal work units and call depth 64;
limits may be lowered. Structural type validation additionally caps depth 32 and
512 nodes per type, effect/generic/substitution arity 64, and parameters/struct
fields 128. Source edits cap at 8,192 and 256 KiB. Work counts traversal steps,
not CPU instructions; bounded structural comparisons and validation add overhead.
Inputs/DTOs are owned by their caller; host allocation is used.

Tests exercise actual CLI checking/fixes, generic instance/label selection,
transitive calls, handlers/closures/indirect/open rows, recursive call refusal,
projection siblings/callee selectors/shadowing, checked arithmetic and linear
cleanup, comment retention, supported/unsupported interface gates, malformed and
bounded input, nominal expansion refusal, symlinks/readonly files, exact/stale
edits, concurrent-change detection, and preexisting staging owner preservation.
