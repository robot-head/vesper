# Checked source-interface export checkpoint

`vesper::interface::export(&World, package)` converts a successfully checked module into the experimental `vesper_semver::Interface` DTO. The caller must reject parser/checker diagnostics before invoking it. Package names follow Loom's `@org/name` or `std/name` grammar. Definitions use `package::type::Name`, `package::effect::Name`, and `package::fn::name` identities; method names retain the checked `Type::method` suffix. Separate kind components preserve the language's type/effect namespaces.

The current language has no source visibility, so every source declaration, including helper functions, is exported. Compiler-provided Option, Result, RuntimeError and Fail declarations use reserved `std/core` identities and retain their full type/operation contracts. Primitive types map explicitly. Named fields and positional enum payloads remain public, enums remain exhaustive, and effects remain open to user handlers. Generic type and row binders become positional; row binders conservatively admit affine resumption. Functions retain passing modes and projection paths. Unnamed outgoing effects use stable qualified effect identities plus normalized type arguments, sorted independently of source row order. Source spans, parameter/local names, bodies, and inferred `$cap`, `$fncap` or latent identities are excluded.

Nominal kinds publish unconditional ownership requirements computed from checked payloads with copy placeholders, then the DTO conservatively joins argument modes. Return mode metadata uses the same analysis, including phantom arguments, to avoid publishing an inconsistent modal contract. Phantom and conditional ownership may therefore cause conservative incompatibility reports. Field modality overrides currently cannot relax payload ownership (the approved checker restriction), so their effective ownership is represented by payload types and nominal kinds.

Contracts outside the schema emit explicit Unsupported declarations or types: named enum variants (including empty named forms), named capabilities, all effectful arrows and their latent capability scopes, arrows capturing outer type/row binders, value-position row arguments, unresolved types, and effect operations with local generics, bodies or outgoing effects. Captured arrow binders are checked before nominal lookup, including when a global type has the same spelling. These markers make comparison Unknown even against the identical snapshot. Other conservative validation, including projected sink rejection, remains in the SemVer library.

This adapter does not establish canonical `.vhir`, source-interface hashes, executable compatibility witnesses, ABI stability, authority confinement, checked-overflow semantics, or new Lean results. Semantics version 1 describes the current checkpoint, including wrapping integer arithmetic. Equal interfaces must never substitute for implementation, object derivation, layout, or proof dependency cache keys.

Validation: focused integration tests cover body/local/parameter rename invariance, alpha-equivalent type/row binders and effect arguments, row ordering, operation changes, projections/passing, same-nominal ownership changes, builtin and method identities, fixed/phantom ownership metadata, package validation, and fail-closed unsupported contracts after source-span shifts. Commands:

```sh
cargo test --locked --test interface
cargo fmt --all -- --check
cargo clippy --locked --lib --test interface -- -D warnings
```
