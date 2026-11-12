# Forge hygiene and emitted-code authority — lane report

Lane 7 of the [next wave](../agent-prompts/next-wave/07-forge-hygiene-authority.md) (design §16.4; contract audit rows "Macro capability provenance" and "Authority versus discharged rows").
Owned paths: `crates/vesper-forge/**` and this file. Branch `claude/project-thread-1z4nx8`, base `origin/main` at `c2ba0b1` (contains the required `0e6caf9` baseline). Not merged.

No root manifest, lockfile, CLI, CI, shared AST/parser/checker, sandbox, CAS, build runner or Pact file was touched.

## What was built

`crates/vesper-forge` is a new standalone crate (temporary `[workspace]` table and nested `Cargo.lock`, seeded from the root lockfile so versions match). Dependencies are `serde 1`, `serde_json 1` and, behind the default `sandbox-transport` feature, the existing `vesper-loom-sandbox` crate (read-only; only its `transport` module is used).

| Module | Content |
| --- | --- |
| `identity` | `SessionId`, `ScopeId`, `ExpansionId`, `SpliceHandle`, `BinderId`, `Symbol`, `ScopeSet`, `IdentityLimits`. Every identity is session-qualified and minted from a checked counter (`IDENTITY_SCHEMA_VERSION = 1`). |
| `host` | `HygieneHost`: owns the registry, module (definition-site) scopes, global environments, symbol table, expansion records and splice arenas. `begin_expansion` / `finish_quotation` / `finish_generated` / `abandon`, `lex_source`, `add_scope`, `declare_global`, `validate_token`. |
| `token` | `HostToken` (host-assigned kind, symbol, scopes, `Origin`, unvalidated `claimed_def`, display-only `ReportedSpan`), `GeneratedToken`, `Quotation`/`QuoteItem::{Token, Splice}`, `QuoteLimits`. |
| `registry` | `PackageId`, `DefId`, `DefInfo` (printed path, kind, visibility), `PackageInfo` (dependencies), `DefinitionRegistry` with `visible_from` and `validate_claim` (`REGISTRY_SCHEMA_VERSION = 1`). |
| `syntax` | Parser for the lexical fragment below. |
| `resolve` | `HygieneHost::resolve`: scope-set resolver producing `ResolvedFragment` (binders, references, emitted operations with their origins, and the expansion chain records). |
| `authority` | `Authority`/`EffectId`/`AuthorityArg`/`AuthorityGrant`, `AuthorityGraph` (nodes, edges, handler-construction rules), `MacroPolicy`, `CallerCeiling`, `PolicySet`, `check_authority`, `Violation`, `AuthorityAudit` (`AUTHORITY_SCHEMA_VERSION = 1`). |
| `transport` | Flat sandbox import: `HygieneHost::finish_flat_outcome`, `HygieneHost::invocation_for`, `generated_from_transport`. |
| `error` | `ForgeError` with stable `FORGE-H/R/Q/P/B/M/S` codes. |

The runnable example is `cargo run --example forge_gate`.

### 1. Identities and quotation

* Scopes, expansions, splice nonces, binders and symbols come from `u32` counters with configurable limits. Hitting a limit returns `IdentityExhausted` and keeps returning it. Counters never wrap, and an identity is never reissued.
* Deserialized identities are only claims. `validate_token` rejects scopes from another session (`ForeignScope`), scopes this host never minted (`ForgedScope`), unknown symbols, unknown packages, and tokens from expansions that never finished successfully.
* `begin_expansion` checks that the definition is a registered `Macro` that the invoking package can name. It stores each caller argument in a host arena and returns one `SpliceHandle` per argument. Each expansion gets a fresh introduction scope.
* `finish_quotation` gives each generated token `{macro definition-site scope, introduction scope}` and `Origin::Expansion(id)`. It replaces each antiquote with the host's stored tokens, which keep their own origin and scopes. A handle from another session is `Foreign`. A handle for a different expansion of this session is `Stale`. A nonce or expansion this host never minted is `Forged`. An expansion closes on its first finish attempt, successful or not, so a later call gets `ExpansionClosed`.
* Quotations are bounded by `QuoteLimits`: items, output tokens, text bytes, splices and nesting depth. Delimiters must balance.

### 2. Lexical fragment and resolver

The fragment ("Forge lexical fragment v1", see `syntax.rs`):

```text
expr    := 'let' IDENT '=' expr ';' expr
         | 'handle' path 'with' path block
         | postfix
postfix := primary ( '(' [ expr { ',' expr } ] ')' )*
primary := path | INT | STRING | '(' expr ')' | block
block   := '{' [ expr { ';' expr } [';'] ] '}'
path    := IDENT { '::' IDENT }
```

Resolution runs on the fully expanded token sequence, after all expansions:

* Each `let` mints a fresh binding scope. That scope goes on its binder and on everything in its body, but not on its initializer.
* A binding is a candidate for a reference when the symbols match and `scopes(binding) ⊆ scopes(reference) ∪ enclosing binding scopes`. Local and global bindings are both candidates. Globals live in a package's top-level environment under its module scope (`declare_global`).
* The candidate with the largest scope set wins. If the maximal candidates disagree, resolution fails with `Ambiguous`. No candidate gives `Unbound`. Nothing falls back to text.
* All segments of a path must agree on origin, scopes and claim (`MixedPath`). `handle` requires an `Effect` and a `Handler` definition.

Effects of this design, each covered by a test:

* A binder the macro introduces cannot capture a caller identifier.
* Caller locals cannot hijack a macro's unqualified globals, which resolve in the macro's definition-site environment.
* Macro text cannot reach globals that exist only in the caller's environment.
* Nested expansions and shadowing resolve consistently.

### 3. Global identity

A pre-resolved `DefId` on a token is a claim. `validate_claim` checks it from the point of view of the package that emitted the token: the source package, or the macro package for generated tokens. The definition must exist. It must be in the emitting package itself, or be public in a declared dependency. Its registry path must equal the printed path.

So guest output cannot invent a definition in its own package, cannot reach non-dependencies or private items, and cannot show one name while calling another. Pre-resolved binders are rejected. Identity is `DefId`; printed names and spans are display data.

### 4. Emission provenance

`Origin` records who emitted a token, independent of which package defines its target. Only arena substitution through a valid splice handle preserves caller origin.

`finish_flat_outcome` imports a `vesper-loom-sandbox::transport::Outcome`:

* It checks `TransportLimits` and fails on any guest `Error` diagnostic.
* It discards guest scope arrays and keeps spans only as `ReportedSpan`.
* It turns `pre_resolved` values into claims to validate.
* It attributes every token to the macro.

`invocation_for` exports caller tokens to the guest with empty scope arrays and no `DefId`s, so host identities never reach the guest.

### 5–6. Authority gate

`AuthorityGraph` is trusted, versioned compiler input. Each node holds:

* `direct` authorities;
* an informational `residual_row`;
* edges: `Call(DefId)`, `Handler { effect, handler }` and `Unresolved { note }`.

Handler-construction rules give the authority needed to install a handler for an effect. Authorities are package-qualified (`EffectId { package, name }`) with typed arguments compared exactly, so `Ffi[LibA] ≠ Ffi[LibB]`. A grant is `Exact` or `AnyArguments(effect)`.

For each emitted operation (`Use { def }` for any global use, `Handler { effect, handler }` for `handle`), the gate:

* Computes transitive requirements by breadth-first reachability, so cycles terminate. Each requirement keeps a witness path.
* Fails closed on a missing node, an `Unresolved` edge or a missing handler rule.
* Never subtracts locally handled effects. A definition with an empty residual row still needs its direct and callee authority.
* Attributes the operation to every expansion that contributed one of its tokens. Each requirement must be permitted by the `emits` policy of that macro and of every enclosing expansion, and by the root caller's ceiling. This applies whichever package defines the target: macro-own, injected dependency, effect operation or handler.
* Fails on missing macro policies or ceilings.
* Records operations whose tokens all have source origin under `source_operations`. They are the caller's own code and are not exempted or certified here; the compiler's ordinary checks of that package apply.

Accepted output returns `AcceptedExpansion { fragment, audit }`. The `AuthorityAudit` records the schema version, session, graph version, per-expansion macro-policy and ceiling versions, and each operation with its expansions and requirements. Its JSON is deterministic, and `from_json` rejects other schema versions and unknown fields.

Rejections list every violation, sorted and deduplicated, with stable `FORGE-A00x` messages and witness paths.

## Validation

Run in `crates/vesper-forge` (Rust 1.97.0):

```text
cargo fmt --check                                                        ok
cargo clippy --all-targets --locked -- -D warnings                       ok
cargo clippy --all-targets --no-default-features --locked -- -D warnings ok
cargo test --locked                                                      32 passed (13 authority, 15 hygiene, 4 transport)
cargo run --example forge_gate                                           rejects emits=[]; accepts emits=[Net] with audit
```

As a mutation check, I removed the scope-subset test for local bindings. Four capture and hijack tests then failed. The check was restored.

Acceptance coverage:

* **Hygiene:** capture, hijack (lexical and pre-resolved), caller-only globals, shadowing, nested expansions, ambiguity.
* **Splice and scope faults:** forged, stale and foreign splice handles; closed expansions; forged and foreign scope marks; unfinished-expansion origins; scope, expansion and binder exhaustion.
* **Global claims:** unknown, own-package-invented, non-dependency, private and printed-name-mismatch `DefId` claims; pre-resolved binders.
* **Authority:**
  * privileged calls injected from the macro's own package and from another dependency, through a cycle;
  * direct effect operations;
  * typed `Ffi`;
  * locally handled effects and macro-constructed handlers;
  * unresolved and unknown graph nodes;
  * missing policies;
  * an empty `emits` policy in a Net-holding caller rejects, while `emits ∩ ceiling` accepts and a ceiling-only denial rejects;
  * nested policy intersection;
  * spliced caller code keeps caller origin.
* **Transport:** the guest receives no host scopes; forged caller scopes and spans can neither capture nor exempt; guest error diagnostics and unbalanced output fail the expansion; transported claims are validated.
* **Determinism and schemas:** deterministic audit JSON and diagnostics; schema-version and unknown-field rejection for the registry, graph, policies and audit; duplicate graph nodes are rejected.

## Integration steps (coordinator)

1. Remove `[workspace]` and the nested `Cargo.lock` from `crates/vesper-forge/Cargo.toml`. Add the crate to root `members` and `default-members`, and regenerate the root lockfile. No new third-party crates are introduced.
2. Optionally move `serde`/`serde_json` to `{ workspace = true }`. The root's `serde` already enables `derive` and `rc`.
3. Keep `sandbox-transport` as a default feature, or disable it where wasmtime is unwanted. The example declares `required-features = ["sandbox-transport"]`.
4. Per the README, do not connect Forge to Loom macro execution or CAS memoization until this boundary is reviewed. When connecting, the cache key must include the graph version, registry, policy and ceiling versions recorded in `AuthorityAudit`.

## Limits and what the compiler must supply

This is a hygiene/provenance core and an authority gate. It is not a source macro expander, a typed CST arena ABI, a Loom confinement certificate, or an automatic audit of arbitrary source. Accepted authority evidence is conditional on trusted graph and registry input.

* **Fragment only.** Only `let`, calls, blocks, literals, paths and `handle … with …` are resolved. There are no lambdas or parameter lists, patterns, items, modules, imports, types, effect rows or method calls. The real compiler must supply binding forms and scopes for the full grammar, including use-site scopes for definition-context macros (Flatt) and module/import resolution, and must call `resolve` (or an equivalent) on its CST.
* **Expand-then-resolve.** Resolution happens after all expansions. Macros that inspect resolution results, or interleave expansion with binding, are not modeled.
* **Trusted graph.** The `AuthorityGraph` must come from the compiler's checked, resolved HIR or `.vhir` after expansion. It needs complete edges, including trait or dynamic dispatch, closures, function values, FFI declarations and allocator or OS handler construction, each with package-qualified typed authorities. Any edge it cannot resolve must be `Unresolved`. This crate does not derive the graph from source and does not check that the graph is truthful.
* **Uses as operations.** Every global use is treated as an operation, conservatively; data flow is not modeled. Local function values passed through caller bindings are outside the fragment.
* **Splice evidence needs a structured ABI.** The current WIT transport is flat, so all of its output is macro-generated. Splice evidence works only through `finish_quotation`, which models the future typed CST arena ABI. The handle is a host-side table reference, not a cryptographic token, which is sound because substitution uses only host-stored syntax.
* **Policies are inputs.** `MacroPolicy` and `CallerCeiling` are assumed to come from trusted manifests (`Loom.toml`). Loom does not parse or sign them yet. A bounded scratch-region exception and the full set of privileged handler-construction rules are still contract decisions for HIR and Loom.
* **Deterministic, not portable.** Scope and binder indices depend on call order within a session. Persisted identities are valid only for the session that minted them.
* **Not exercised.** No real WASI guest is run in these tests; transport input is constructed directly. Sandbox execution is covered by `vesper-loom-sandbox`'s own suite. No kernel, hardware or Lean claims are made.
