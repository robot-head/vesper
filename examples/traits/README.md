# Source traits and models

`shapes.ves` declares two traits with associated types, canonical impls and two named models, and selects them by receiver calls, `Trait::method` and `Model::method` paths and nested `use model` scopes. Expected result: `4304070304`.

```sh
vesper run examples/traits/shapes.ves
vesper jit examples/traits/shapes.ves
vesper emit examples/traits/shapes.ves --emit object --target aarch64-unknown-linux-gnu -o shapes.o
vesper emit examples/traits/shapes.ves --emit lean
vesper check examples/traits/overlap_rejected.ves   # E0605 (PACT0102 overlap)
```

The accepted grammar and static fragment are described in [the lane report](../../docs/parallel-results/source-traits-models.md).
