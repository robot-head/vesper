# Workspace libraries

The workspace members, dependency versions, and features are defined in
[Cargo.toml](../Cargo.toml). Each crate documents its implemented APIs and
examples in its README and Rust module documentation.

The [manuscript](../docs/manuscript/README.md) is the sole language and platform
specification. Library documentation describes the implementation in this
checkout; it should link to the manuscript for language requirements.

The root `vesper` package contains the compiler, evaluator, native and Lean
backends, and CLI. Shared source types live in `vesper-core`, parsing in
`vesper-syntax`, and runtime scheduling in `vesper-pulse`. The root package
reexports the original module paths for these components.

## Build and test

From the repository root:

```sh
bazel test //crates/vesper-core/...
cargo test --locked -p vesper-core
```

Use the corresponding package name to check another crate. See
[building with Bazel](../docs/bazel.md) for workspace checks, generated build
files, and registering a new crate. Platform and hardware requirements are
documented in [platform validation](../docs/ci-platforms.md).
