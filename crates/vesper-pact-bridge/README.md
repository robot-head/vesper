# vesper-pact-bridge

Checked-source-to-Pact adapter with a coherence-gated evidence session.

* `CheckedPackage::check` runs the real Vesper parser and checker and rejects any diagnostic.
* Definitions get package-qualified `DefPath` identities and dense, sorted Pact id tables with reverse maps.
* Constructor `ModeScheme`s are derived from `World::mode` and verified against it on every well-formed argument-mode combination. Cases the scheme cannot represent are `Unsupported`.
* Traits, impls, models and queries enter through the versioned `BindingsDto`/`QueryBindingDto`, because Vesper source has no syntax for them yet. Every type they mention must resolve to a checked source declaration, a builtin or a parameter.
* `SessionBuilder::build` runs `Program::new` and `check_coherence`. Only the resulting opaque `CoherentSession` answers queries.
* All five solver outcomes are preserved. `Proven` becomes `Evidence` only after the derivation re-checks. `consume` re-checks it again and rejects evidence from another declaration `Revision`.

```sh
cargo fmt -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --example bridge
```

The `[workspace]` table and nested `Cargo.lock` are temporary until the coordinator registers the crate. See [the lane report](../../docs/parallel-results/pact-compiler-bridge.md) for decisions and limits.
