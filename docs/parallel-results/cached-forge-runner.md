# Cached Forge macro runner — lane report

Wave three, lane 3 ([prompt](../agent-prompts/wave-three/03-cached-forge-runner.md); contracts in
[wave-three-contracts.md](../rfcs/wave-three-contracts.md), "macro runner" paragraph).
Branch `claude/project-thread-yyji21` (the session's assigned branch; the prompt suggests
`wave3/cached-forge-runner`). Base: `origin/main` at `1ffd0aec27dfcc24c5564585ae9068255490bf61`, a
descendant of the required `02b3a1d`.

Edited paths: only the new `crates/vesper-forge-runner/**` and this file. The existing Forge,
loom-build, sandbox, CAS, trust and compiler crates, the sandbox guest workspace, root manifests
and lockfile, CLI, CI and global docs are untouched. The crate builds as a temporary standalone
workspace with its own `Cargo.lock`, seeded from the root lock. Every shared package keeps its
root version; the lock adds only this crate.

## What was built

`vesper-forge-runner` executes real `ProcMacro`/`DeriveMacro` components and caches only the raw
execution. It keeps two layers strictly apart.

### Execution layer (`MacroRunner`)

`MacroRunner` runs a component under `vesper-loom-sandbox`'s `SandboxHost`. Each run gets:

* read-only declared inputs, snapshotted into the CAS twice (the two snapshots must agree) and
  materialized fresh for every run;
* a fresh, empty `/out` in a uniquely named scratch directory;
* fixed clocks and seeded randomness (`Determinism`);
* explicit budgets (`ResourceLimits`): fuel, memory, tables, instances, stdio, a wall-clock
  backstop and transport limits.

Build scripts cannot be expressed: `MacroKind` has only `ProcMacro` and `DeriveMacro`. Loom-build
was not changed to accept macros.

**Execution key.** The key is a CAS `DerivationInputs` (`src/key.rs`). It binds:

* the exact component bytes (an Artifact blob);
* the canonical invocation JSON: step kind, entry, args, and the input tokens' text, kinds and
  display spans;
* each mount's guest path and snapshot tree;
* every `ResourceLimits`, `TransportLimits` and `Determinism` field. The struct destructuring has
  no `..`, so a new sandbox field fails to compile until it is keyed;
* the measured `HostIdentity`.

**Host identity** (`src/identity.rs`) is measured at run time from executable bytes, not taken
from labels:

* BLAKE3 of the running executable, read through `/proc/self/exe`. Wasmtime, Cranelift, WASI,
  the sandbox policy, the Forge importer and this crate are linked statically, so this digest
  covers them.
* On Linux, BLAKE3 over the path and bytes of every file-backed executable mapping in
  `/proc/self/maps`: the loader, libc and any other shared object. A deleted mapping makes the
  identity unknown.
* The `rustc -vV` and target triple from `build.rs`, the runtime-detected CPU features, the WIT
  package and digest, the protocol version and the record schema.

If any required field is unknown, the cache is refused (`IncompleteIdentity`) and only
`CachePolicy::Uncached` runs.

**Raw record** (`src/record.rs`). This is the only thing written to the cache. It is a bounded
binary encoding of the untrusted wire output: tokens with the guest's spans, scope marks and
claims; notes and warnings; stdout and stderr; and fuel and peak memory. It embeds the key it was
produced under. It never contains host scopes, expansion ids, splice handles, resolutions, audits
or `AcceptedExpansion`. A test checks that the stored bytes contain no session, expansion, graph
or audit data.

**Bounded validation, at three points:**

1. *Lifting.* Wasmtime's canonical-ABI lifting of the packed result makes host allocations
   bounded by guest linear memory. The runner caps `max_memory_bytes` through `RunnerLimits`; the
   initial allocation is not zero.
2. *Unpacking.* The sandbox then checks counts, slice bounds and the text/scope totals against
   the request's transport limits *before* allocating any per-token string. The runner caps
   those limits too, and requires that a worst-case record at the caps fits `max_record_bytes`.
3. *Cache reads.* The object is rehashed by streaming (`verify_object`, which retains nothing).
   Its size is checked before it is read. Header counts are checked against the request's
   limits, and the exact total length before any per-token allocation. Per-entry sums must match
   the header totals, and UTF-8, tags, nesting and the absence of error diagnostics are checked.
   The record must re-encode byte-identically, which makes the encoding canonical, and its
   embedded key must equal the lookup key.

**Corrupt and stale entries.** The default `CorruptionPolicy::Fail` returns `CorruptCache`. A
corrupt object or record file under `Rerun` is quarantined by the store when the true bytes are
re-stored. A well-formed record that names another execution's output, a junk blob or an
oversized object is never replaced: `Fail` rejects it, and `Rerun` reports `DivergentExecution`.
Records are published no-replace, and the existing `DerivationConflict` maps to
`DivergentExecution`.

**What is never recorded:**

* guest `error` results and successes that carry an `Error` diagnostic;
* traps and exhausted budgets;
* malformed or oversized output;
* any file written to `/out` (`OutputFilesWritten`: macros return tokens only);
* runs whose input was mutated.

`CachePolicy::Verify` re-executes and requires an identical record.

### Approval layer (`ApprovalContext` → `ForgeSession`)

`ApprovalContext` holds every trusted input that resolution and the gate depend on:

* the `DefinitionRegistry`;
* the global environment declarations;
* the `AuthorityGraph`;
* the macro `emits` policies and caller ceilings;
* the **component bindings** (`macro_def` → exact component digest, kind and entry);
* the identity and quotation limits.

`ApprovalContextId` is a BLAKE3 digest of the complete canonical, schema-versioned encoding.
Collections are sorted, and duplicated policies, ceilings or bindings are rejected rather than
silently collapsed. Content changes therefore change the id even when every version label stays
the same. `open_session(SessionId)` builds a private `HygieneHost` from that context, so the
environment cannot drift after the digest.

`ForgeSession::expand(runner, MacroCall, ExecOptions)` runs these steps, **on every miss and every
hit**:

1. **Binding.** The context must bind exactly this component, kind and entry to `macro_def`.
   Otherwise the call is `Rejected(UnboundComponent)` and nothing executes. A caller therefore
   cannot run an arbitrary component under a privileged macro's policy.
2. **Fresh expansion.** `begin_expansion` mints a fresh expansion and introduction scope. The
   input tokens must be valid tokens of *this* session; a forged or foreign input is rejected
   before execution. The guest receives only text, kinds and display spans
   (`invocation_for`).
3. **Execution.** The raw result is a verified hit or a sandboxed run.
4. **Import.** `finish_flat_outcome` makes every token macro-originated, including echoed caller
   tokens. Guest scopes are dropped, spans are kept for display only, and `pre-resolved` ids
   become unvalidated claims.
5. **Resolution.** The output is resolved with the surrounding `before`/`after` tokens against
   the current registry and environments.
6. **Authority.** `check_authority` runs against the current graph, policies and ceilings.

The result is a `MacroOutcome`. It carries the session, the approval-context id, the minted
expansion and an `ExecutionSummary`: hit, miss, repaired, verified or uncached; whether it
executed; the key; the raw-output `ObjectId`; metering; and the guest's notes. The `Verdict` is
either `Accepted(FreshApproval)` (tokens plus `AcceptedExpansion` with its audit) or
`Rejected(UnboundComponent | Hygiene | Authority)`.

Infrastructure failures (sandbox, guest, corrupt cache) are `Err`. In every non-accepted case the
minted expansion is closed: a failed run's expansion cannot be finished later or used as a parent
site. A rejection after a successful run still leaves the raw execution cached, because it is
untrusted and reusable, but no approval is ever stored.

Caller-local antiquotation is unsupported. Flat output has no host-authenticated splice protocol,
so there is no typed or parallel guest ABI. Provenance is never derived from guest scopes, spans
or claims.

### Fixture component

`fixtures/guest` is a new `wasm32-wasip2` macro guest. It is built against the read-only sandbox
WIT by `fixtures/build.sh`. The committed `fixtures/bin/macro.component.wasm` (103 252 bytes) has
SHA-256 `74b3fe4d…678bb`, recorded in `SHA256SUMS` and checked by a test. Two clean rebuilds
produced identical bytes.

The `template` entry reads a template file from a declared input and returns its tokens, using
the Forge lexical-fragment token rules. It supports these directives:

* `$input` (echo the input tokens);
* `$seed` and `$clock` (deterministic WASI random and clock);
* `$claim:P:I` (attach a forged claim to the next identifier).

It also accepts `scopes=` and `span=` options that forge wire metadata. Other entries are hostile
or failing on purpose: `flood`, `alias-bomb`, `bad-slice`, `bad-utf8`, `error-with-tokens`,
`unbalanced`, `error-diagnostic`, `fail`, `panic`, `spin` and `write-out`.

## Public API

```text
MacroEngine::new / executions
MacroRunner::open(engine, store, scratch) / with_caps / with_identity_unchecked
            / prepare(&MacroRequest, Vec<wire Token>) -> PreparedExecution / execute(&, ExecOptions) -> RawExecution
MacroRequest { component, kind: MacroKind, entry, args, inputs: Vec<InputSpec>, limits, determinism }
RunnerLimits { transport, max_memory_bytes, max_stdio_bytes, max_record_bytes }
CachePolicy { ReadWrite, Verify, Uncached }   CorruptionPolicy { Fail, Rerun }   CacheStatus
HostIdentity::current / missing / is_complete
RawRecord::{encode, decode(&, RecordLimits)}  RECORD_SCHEMA_VERSION = 1  KEY_SCHEMA_VERSION = 1
ApprovalContext { registry, globals, graph, macro_policies, ceilings, bindings, identity_limits, quote_limits }
            ::canonical_bytes / id / open_session(SessionId) -> ForgeSession   APPROVAL_CONTEXT_SCHEMA_VERSION = 1
ForgeSession::expand(&MacroRunner, MacroCall, ExecOptions) -> Result<MacroOutcome, RunnerError>
            / lex_source / module_scope / host / session / approval_context
MacroOutcome { session, approval_context, expansion, execution: Option<ExecutionSummary>, verdict }
Verdict::{Accepted(Box<FreshApproval>), Rejected(Rejection)}
```

`cargo run --example cached_macro` shows a miss accepted with `emits = [Net]`. A hit then follows
under the same raw output (`executed: false`) with `emits` emptied under the same version label
1; it gets a different approval-context id and is rejected with `FORGE-A001 … denied by emits`.
Guest executions: 1.

## Validation

All runs used Rust 1.97.0 on x86_64 Linux, from `crates/vesper-forge-runner`, with a dedicated
`CARGO_TARGET_DIR` and `CARGO_PROFILE_{DEV,TEST}_DEBUG=0`, `CARGO_INCREMENTAL=0`:

```text
cargo fmt --check                                   ok
cargo clippy --all-targets --locked -- -D warnings  ok
cargo test --locked                                 32 passed: 5 unit (record), 10 approval, 11 cache, 6 hostile
cargo run --example cached_macro                    miss accepted, hit rejected after same-label policy change
fixtures/build.sh (twice)                           identical 74b3fe4d… component
```

Every integration test executes the real component under Wasmtime. None constructs transport
output by hand.

**Registered-mode check (uncommitted, then reverted).** With the coordinator patch below applied
at the root, `cargo test -p vesper-forge-runner` passed 31/31. Strict
`cargo clippy -p vesper-forge-runner --all-targets --locked` and `cargo fmt --all -- --check` also
passed. The root lock gained only the new package entry.

**Mutation checks** (each change restored afterwards):

| Mutation | Test that failed |
| --- | --- |
| Skipped the record-key check | `stale_or_crossed_records_are_rejected_and_never_replaced` |
| Accepted any binding for a macro | `unbound_or_mismatched_components_are_rejected_before_execution` |
| Digested only the graph's label | `same_label_content_changes_cannot_retain_old_authorization` |

**Acceptance coverage:**

* **Miss and hit.** A real miss followed by hits in new sessions and a fresh engine gives
  equivalent tokens and audits: same graph label, policy stamps, operations and requirements, and
  no re-execution. Every token, scope and expansion id belongs to the new session. Old scopes are
  `ForeignScope` in the new session, and two hits in one session get distinct expansions and
  introduction scopes.
* **Pure output** is accepted with an empty `emits` and ceiling.
* **Privileged output.** Own-package privileged calls, dependency effect operations, a locally
  handled effect (empty residual row) and macro handler construction each need `emits` AND the
  ceiling, on the miss and on every hit. An earlier acceptance is not reused when the context
  rejects again.
* **Execution invalidation.** Changing the component bytes, input tokens, args, kind, input
  contents, mount path, seed, clock, fuel, a transport limit, the wall-clock backstop or the host
  executable identity changes the key. Identical contents in another host directory keep it.
  Seed and clock reach the guest, and `Uncached` runs are byte-identical.
* **Approval-context changes under the same labels** (`graph-v1`, policy v3, ceiling v11): a
  graph edge, a registry visibility plus environment change, a macro policy and a ceiling each
  change the context id. Each still reuses the raw execution as a hit, and each loses the old
  approval.
* **Forgery.** Forged caller scopes and spans can neither capture nor exempt, on the miss or the
  hit; spans stay display-only. Forged claims (wrong printed name, private definition,
  non-dependency, unknown definition) are rejected on both. Foreign input tokens are rejected
  before execution, and echoed caller tokens are macro-originated.
* **Malformed or oversized output:**
  * token flood over the limit;
  * alias bomb, by count and by text bytes;
  * out-of-bounds slice and invalid UTF-8;
  * tokens inside an error result and unbalanced delimiters;
  * requests above the caps, zero limits, bad, nested or `/out` mounts, and inputs overlapping
    the store;
  * a record bound above the cap;
  * an input token stream over the limit.

  All are rejected, with nothing published.
* **Stale and corrupt cache:**
  * a tampered raw object and a tampered record file are each rejected, then quarantined and
    repaired;
  * a record crossed to another execution's output, a junk blob and an oversized object are
    each rejected and never replaced;
  * a stored record is rejected under a stricter request's limits;
  * single-byte flips, truncation and extension of a record are rejected.
* **Failed guests and expansions publish nothing:** `fail`, `error-diagnostic`, an unknown entry,
  a trap, fuel exhaustion and `/out` writes. A failed expansion closes, and nesting under an
  accepted one stamps both policies.

## Coordinator patch

Apply after review:

1. In `crates/vesper-forge-runner/Cargo.toml`, delete the `[workspace]` table and its comment,
   and switch to the workspace Serde dependencies:

   ```diff
   -# Temporary standalone workspace until the coordinator registers this crate
   -# (see docs/parallel-results/cached-forge-runner.md). Integration removes this
   -# table and the nested Cargo.lock.
   -[workspace]
   -exclude = ["fixtures/guest"]
   -
   ...
   -serde = { version = "1", features = ["derive"] }
   -serde_json = "1"
   +serde = { workspace = true }
   +serde_json = { workspace = true }
   ```

2. Delete `crates/vesper-forge-runner/Cargo.lock`.
3. In the root `Cargo.toml`, register the crate and exclude its guest workspace:

   ```diff
   @@ members = [
        "crates/vesper-loom-build",
   +    "crates/vesper-forge-runner",
        "crates/vesper-ignis",
   @@ default-members = [
        "crates/vesper-loom-build",
   +    "crates/vesper-forge-runner",
        "crates/vesper-ignis",
   @@
   -exclude = ["crates/vesper-loom-sandbox/fixtures/guest"]
   +exclude = [
   +    "crates/vesper-loom-sandbox/fixtures/guest",
   +    "crates/vesper-forge-runner/fixtures/guest",
   +]
   ```

4. Run `cargo generate-lockfile --offline` or `cargo test -p vesper-forge-runner`. It adds exactly
   one `[[package]] name = "vesper-forge-runner"` entry, with dependencies blake3, serde,
   serde_json, sha2, tempfile, vesper-forge, vesper-loom-cas and vesper-loom-sandbox. It
   introduces no new third-party crates or versions.

No CLI, CI or shared-schema change is required. CI needs no wasm toolchain because the fixture
binary is committed. Rebuilding the fixture needs `rustup target add wasm32-wasip2`.

## Decisions

* **Approval is never cached, not even keyed by context id.** Every hit reruns binding, import,
  resolution and the gate. The context id is reported so callers can tell which trusted content
  an audit is conditional on; it is not a cache key for authorization.
* **Bindings are part of approval.** Execution identity does not include `macro_def`, so two
  macros bound to the same component share one raw execution while their approvals stay
  separate.
* **Raw guest metadata is stored verbatim.** Guest scopes, spans and claims are untrusted wire
  data and are discarded or validated at import, so storing them keeps the record raw.
  Host-minted identities never reach the guest, so the record cannot contain them.
* **Executable bytes instead of source fingerprints.** This is stronger and simpler than
  loom-build's lockfile and source digests, and it does not depend on `CARGO_TARGET_DIR`. The
  cost: every rebuild of the host binary, and each separate test binary, has its own cache
  namespace.
* **Error diagnostics count as guest failure.** As in loom-build, a guest whose result carries an
  `Error` diagnostic is treated as failed and never recorded.

## Limits and remaining gaps

* **Identity coverage.** The host identity covers the executable and, on Linux, the mapped shared
  objects, read by path. A library replaced on disk after it was mapped would be misread; a
  deleted mapping is detected. Kernel behaviour, the WASI implementation's OS interactions and
  the CPU microarchitecture beyond the detected feature list are not bound. On other operating
  systems only `current_exe` bytes are hashed; that is untested here.
* **Validation environment.** Only x86_64 Linux was run. No macOS, Windows or AArch64 execution.
* **Evidence, not proof.** Hashes, records and audits are local, conditional policy evidence.
  They are not package confinement, registry provenance, signatures, Sigstore or a build-graph
  claim. The authority result is only as truthful as the supplied graph and registry; see the
  Forge lane report's limits.
* **Out of scope for this milestone.** No compiler source macro syntax or real expander
  integration: callers build `MacroCall`s from `ForgeSession` tokens. No network access. No
  build-graph scheduling, garbage collection of old records or concurrent-writer stress test
  beyond the CAS's own no-replace guarantees.
* **Unsupported features.** Caller-local antiquotation and a typed CST arena or splice ABI.
  Macro file outputs are rejected, not supported.

## Registration

After PR #29 merged, a follow-up applied the coordinator patch above. It registers the crate in the root `members` and `default-members` and excludes its guest workspace. It also removes the standalone `[workspace]` table and nested `Cargo.lock` and switches to the workspace Serde dependencies. The root lock gains only the `vesper-forge-runner` entry. In the root workspace, `cargo test -p vesper-forge-runner` passes 32/32, and strict Clippy and `cargo fmt --all -- --check` pass.
