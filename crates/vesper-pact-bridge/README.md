# vesper-pact-bridge

Checked-source-to-Pact adapter with a coherence-gated evidence session.

* `CheckedPackage::check` runs the real Vesper parser and checker and rejects any diagnostic.
* Definitions get package-qualified `DefPath` identities and dense, sorted Pact id tables with reverse maps.
* Constructor `ModeScheme`s are derived from `World::mode` and verified against it on every well-formed argument-mode combination. Cases the scheme cannot represent are `Unsupported`.
* Traits, impls, models and queries enter through the versioned `BindingsDto`/`QueryBindingDto`. Source `trait`/`impl ... for`/`model` declarations are lowered by the compiler and exposed as `CheckedPackage::source_bindings`; tools may still supply bindings directly. Every type they mention must resolve to a checked source declaration, a builtin or a parameter.
* `SessionBuilder::build` runs `Program::new` and `check_coherence`. Only the resulting opaque `CoherentSession` answers queries.
* All five solver outcomes are preserved. `Proven` becomes `Evidence` only after the derivation re-checks. `consume` re-checks it again and rejects evidence from another declaration `Revision`.

```sh
cargo fmt -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --example bridge
```

Everything except `CheckedPackage` and the `SessionBuilder` wrapper now lives in the root-free [`vesper-pact-binding`](../vesper-pact-binding) crate, which the compiler itself uses; the bridge re-exports its API unchanged. See [the original lane report](../../docs/parallel-results/pact-compiler-bridge.md) and [the source traits report](../../docs/parallel-results/source-traits-models.md).
