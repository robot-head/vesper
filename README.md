# Vesper

Vesper is a systems programming language with modal ownership, lexical effects,
and disjoint place projections. This repository contains its Rust compiler,
runtime libraries, developer tools, and Lean 4 models.

The [specification in `docs/manuscript`](docs/manuscript/README.md) is the sole
source of truth for the language and platform design. Read the
[published specification](https://vesper-lang.org/vesper-specification.pdf), or
edit the native Typst sources there. The PDF is generated from those sources.
The implementation covers a subset of the specification; command help, source,
and tests describe the behavior available in this checkout.

## Build and run

CI uses the Rust toolchain pinned in [MODULE.bazel](MODULE.bazel). Install
[Bazelisk](https://github.com/bazelbuild/bazelisk) as `bazel`, then run:

```sh
bazel build //:bin/vesper
bazel-bin/bin/vesper --help
bazel-bin/bin/vesper check examples/transaction.ves
bazel-bin/bin/vesper run examples/fibonacci.ves
```

Cargo is also supported with a compatible Rust toolchain and a system C linker:

```sh
cargo build --locked -p vesper
cargo run --locked -p vesper -- --help
cargo run --locked -p vesper -- check examples/transaction.ves
```

See [building with Bazel](docs/bazel.md) for toolchain setup, Windows support,
and individual test targets. Lean checks use the version in
[lean-toolchain](lean-toolchain); CI setup is described in
[platform validation](docs/ci-platforms.md).

The fixed-model external checker is [Logos](tools/logos42/README.md), pinned
alongside Lean 4.35.0. Its acceptance suite binds fresh model artifacts to the
checker and its actual rejection controls.

## Validate changes

```sh
bazel test //...
```

For local Cargo checks:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
```

See [platform validation](docs/ci-platforms.md) for Lean configuration and
hardware-dependent checks.

## Repository guide

| Path | Contents |
| --- | --- |
| [docs/manuscript](docs/manuscript/README.md) | Authoritative specification and publication checks |
| [docs](docs/README.md) | Build, package, and audit workflows |
| [src](src) | Compiler, evaluator, native backend, Lean extraction, and CLI |
| [crates](crates/README.md) | Runtime and tool libraries |
| [lean](lean) | Mechanized models and proofs |
| [tests](tests) | Integration tests and fixtures |
| [examples](examples) | Vesper source examples |
| [bootstrap](bootstrap) | Bootstrap corpus and seed lock |
| [tools](tools) | Build and proof-audit tooling |
| [website](website/README.md) | Public site and generated specification PDF |

Update language requirements in the manuscript. Keep other documentation focused
on using and maintaining the implementation, with links to the specification
instead of copied requirements. Keep prompts and design notes for open issues
and unmerged branches; retire completed work context when it is no longer needed.

## License

Licensed under either of:

* **Apache License, Version 2.0** (`LICENSE-APACHE` or <http://www.apache.org/licenses/LICENSE-2.0>)
* **MIT License** (`LICENSE-MIT` or <http://opensource.org/licenses/MIT>)

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in Vesper by you shall be dual-licensed as above, without any additional terms or conditions.
