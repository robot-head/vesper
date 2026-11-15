# Pinned Logos external checker

`logos42` freshly compiles and externally rechecks the complete eligible import
closures of `RewriteWitness`, `ArithmeticOracle`, and the ResourceModel42 kernel
snapshot. The backend is the Lean-theory semantic engine of
[Logos](https://github.com/Brahmastra-Labs/logicaffeine), pinned to the current
[upstream revision](https://github.com/Brahmastra-Labs/logicaffeine/commit/8ce95bd31f92a75670cc6e003c48a664b795e433)
`8ce95bd31f92a75670cc6e003c48a664b795e433`.

Lean 4.35.0 is required. The exporter is pinned separately to lean4export
`df81913eee5519d5a88ae5b0fd701b5298cc29ab`. The small primitive port regenerates
the reference terms for Lean 4.35's structure-based `Decidable` and uses its
decision projection. Primitive validation stays enabled. Builds use normal
bounds-checked indexing; the Arena's `unchecked-access` feature is disabled.

## Setup and acceptance

Use Python 3.11+, Git, a Rust toolchain supporting the pinned packages, and the
already installed Lean/Lake toolchain. Setup uses Python's standard library.
The destination must not exist.

```sh
cargo build --locked --bin logos42
python3 tools/logos42/setup.py \
  --directory /absolute/path/to/NEW_LOGOS_ROOT \
  --driver "$PWD/target/debug/logos42" \
  --lean "$(elan which lean)" --lake "$(elan which lake)" \
  --cargo "$(rustup which cargo)" --git "$(command -v git)"

VESPER_LOGOS42_ROOT=/absolute/path/to/NEW_LOGOS_ROOT \
  cargo test --locked -p vesper --test logos42_acceptance -- --test-threads=1
```

The `lean4lean42` binary and the old setup entry points are compatibility
aliases for this backend. They accept only the `vesper.logos42/2` profile.
Earlier Logos and Lean4Lean receipts are refused and never relabelled.

## Checking and binding

The Lean adapter inventories the exact imported artifacts and exports every
eligible declaration, including internal names and projected proof aliases.
It checks the exporter's visited-name set against that inventory. Logos checks
the entire export without a trusted prefix, reconstructs inductives, and checks
the claimed constructor and recursor metadata. The Rust entry point then
compares the exact typed names and declaration kinds with Logos' settled
environment. Coverage is reported only after both checks succeed.

Unsafe and partial declarations remain explicit skips. Every required model
root must be covered. The separate axiom-policy and implementation-proof audit
fields remain `not_run`; external replay is not an axiom audit or a proof of
the checker, exporter, compiler, native binary, or host.

The driver binds the Logos binary separately from the Lean adapter, compiler
and runtime libraries, complete generated build outputs, model artifacts,
resource-source bundle, source inventory, patches, and control results. Cargo
versions and registry checksums are frozen by `Cargo.lock`. Setup validates the
source inventory with the genuine Rust driver before executing build inputs,
fetches locked registry inputs, builds offline, runs the actual controls, seals
the receipt, and atomically publishes it without replacing an existing path.
The Cargo builder identity and both observed build commands are recorded too.

The source walk includes all regular files and rejects symlinks and reparse
points. Only `.git`, the exact generated `.lake/build` subtree, and the four
fixed root setup/log/receipt filenames are excluded. Generated files are bound
separately. Source inventories have finite file/byte/depth limits; exports are
limited to 2 GiB and their eligible-name manifests to 64 MiB. The parent driver
retains its framed-output, memory-accounting, deadline and artifact quotas.
Direct-child supervision is not an OS sandbox or descendant-tree containment.

## Regression controls and maintenance

There are 42 fixed raw-export controls: 19 prescribed Arena cases from
`c77e7951d2ac3b53f4af23b09ae9722bc16f7f47`, nine Vesper cases, and 14 quotient
integrity cases from the pinned Logos revision. They include 36 expected
rejections and six expected acceptances. These bypass Lean
elaboration and run through the same Logos engine as the model closures.
Controls cover loose variables, universe substitution, malformed recursors,
nested declarations, duplicate names, ill-typed terms, false theorem bodies,
and forged quotient members alongside valid quotient packages.
The full acceptance suite additionally tests omitted coverage, forged control
results, altered source/build/kernel bytes, legacy profiles, receipt changes,
artifact substitutions and resource-source associations.

The existing `Lean4LeanLevel42` proof packets remain proofs about their archived
helper sources. They are not implementation proofs of Logos.

Maintainers can refresh the embedded associations from an assembled candidate
with `python3 tools/logos42/update-pins.py CANDIDATE` (requires Python `blake3`).
This produces source expectations, not a success receipt. A clean setup and the
actual acceptance suite must still pass after a source or pin change.
