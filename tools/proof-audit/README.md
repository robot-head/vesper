# Trusted Lean artifact audit helper

`Audit.lean` is the trusted Lean-side implementation for the local `proof-audit`
driver. It does not compile package source files. The driver elaborates this
helper with only the pinned toolchain's library on `LEAN_PATH`, then passes one
JSON request to its `main` function:

```json
{
  "schema_version": 1,
  "root": "/absolute/private/artifact-snapshot",
  "toolchain_prefix": "/absolute/pinned/lean-prefix",
  "module": "Package.Model",
  "theorems": ["Package.Model.property"]
}
```

For direct development testing:

```sh
LEAN_PATH="$prefix/lib/lean" "$lean" --run tools/proof-audit/Audit.lean request.json
```

The helper sets an exact runtime search path, with the toolchain first. It loads
target artifacts without plugins or environment-extension initialization,
constructs an independent environment containing only toolchain imports, and
sends every new artifact declaration through `Lean.Environment.replay`. It
rejects unsafe/partial declarations, attempted toolchain declaration shadowing,
missing or non-theorem requests, declarations omitted by replay, and changed
replayed constant information. Requested names must belong to the artifact
declaration set, not merely to its trusted standard library imports.

One successful line begins with `VESPER_PROOF_AUDIT_V1:` and contains JSON with
`kernel_replayed`, theorem names/types and actual transitive axiom lists, the
number of checked constants, and resolved imported module paths. Axiom policy
and file/toolchain fingerprinting belong to the Rust driver. A successful helper
result does not mean every axiom is permitted by that policy.

The Rust driver must also reject any Lean `PANIC` output, even if Lean exits
successfully. During investigation, broad replay of standard-library differences
produced a panic followed by a successful return; process exit status alone is
insufficient. The separate trusted base avoids replaying toolchain constants,
but does not make ignoring future panics safe.

This is a bounded local artifact check, not an OS sandbox, authenticated package
certificate, or proof of Vesper compiler preservation. The driver must pin the
executable and toolchain, snapshot and bound artifact inputs, reject symlinks and
unexpected paths, bound process resources/output, and bind the result to the
actual module/toolchain bytes. Plain `#print axioms` or `--trust=0` import alone
does not validate untrusted serialized theorem bodies.
