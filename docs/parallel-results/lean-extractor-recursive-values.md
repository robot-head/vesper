# Effectful recursive function values (#593)

Base: `471d44ac`. Branch: `codex/extractor-effectful-recursive-values`.

Fuel companion generation previously refused every recursive function reference
used as a value. It now supports non-generic recursive functions with unlabeled
effects in monadic contexts. Their source callback type already returns `m A`,
so the companion can preserve that type by partially applying `f$fuel` to the
caller's predecessor fuel. The value can be passed to a higher-order function
or stored in a local binding. Zero-parameter functions additionally receive the
same `Unit` lambda adapter used by ordinary extracted function values.

Each invocation of the captured callback uses that captured fuel value. This
bounds recursive depth along that invocation; it is not a global operation
budget shared across repeated callback invocations. The generated companion
remains an ordinary Lean `def` structurally recursive on fuel. Source execution
and the opaque executable `partial def` remain unchanged.

## Evidence

The `recursive_values` fixture is checked by Lean 4.35.0 and the reference
evaluator. It covers:

- Passing recursive `down` as an effectful callback to `apply` and proving its
  successful-return contract by induction on fuel.
- Computing `down(3)` with four units of fuel, and observing exhaustion with
  zero or three units, through kernel reduction.
- Storing a recursive callback in a local before calling it.
- Adapting zero-argument recursive `spin` as a callback and proving that every
  finite fuel amount exhausts.

The existing pure recursive value and pure closure tests retain their partial
classification and typed contract refusal. The CLI regression confirms that
strict artifact replay still rejects Lean's unsafe `_unsafe_rec` helper for
the original partial definition. The fuel proof script itself compiles; this
whole-artifact policy prevents its admission from the full generated package.
Changing the base-case result from 42 to 41 while retaining the postcondition
and proof is rejected earlier, during compilation. No auditor rule is relaxed.

All 16 extraction tests and the targeted strict-audit boundary regression pass,
as do scoped strict Clippy, workspace/fixture formatting, Bazel manifest
synchronization and diff checks. Commands:

```sh
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test lean_extract
VESPER_LEAN=/tmp/lean-4.35.0-linux/bin/lean cargo +stable test --locked --test verify_extract_cli effectful_recursive_callback
cargo +stable clippy --locked -p vesper --test lean_extract --test verify_extract_cli --no-deps -- -D warnings
cargo +stable fmt --all -- --check
python tools/bazel/sync_build_files.py --check
target/debug/vesper fmt --check tests/fixtures/lean_extract/recursive_values.ves
git diff --check
```

## Limits

Pure callbacks have no monadic result channel for fuel exhaustion, so they
remain unsupported. Generic recursive values, labeled requirements and values
inside pure contexts also retain the refusal. An attempted labeled higher-order
fixture was rejected by the source checker's function-type compatibility check;
this change does not relax that rule or claim labeled callback support.

The contract remains normal-return partial correctness of the fuel model.
The full generated artifact is not admitted by `verify --extract` in this case;
separating total proof declarations from executable partial implementations is
remaining integration work.
Neither the successful reduction example nor the input witness establishes
termination for all source inputs, general partial/fuel equivalence, extraction
correctness or native behavior. Decreases inference, the remaining recursive
value cases and broader effect models remain open under #593.
