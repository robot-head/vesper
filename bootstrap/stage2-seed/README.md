# The Stage 2 seed

`stage2.wasm` is `@vesper/stage2` (packages/vesper-stage2) as a WASI 0.2
component, built by Stage 2 itself. It is the compiler the Stage 2 gate
(crates/vesper-stage2-gate, #2671) starts from, so the gate needs no Rust
compiler, as the owner's 2026-11-21 decision ("bootstraps from a committed
wasm seed") asks. The frozen C runner in [../seed](../seed) is not involved.

The gate builds a chain from it:

1. The seed builds stage A from today's sources.
2. If A equals the seed, the seed is current and rebuilds itself.
3. Otherwise A builds B and B builds C, and B must equal C. Then every
   compiler package built by A and by B must be byte-identical.

A stale seed is not an error: it only has to build today's sources. Refresh
it when the gate reports it stale and the extra two builds start to matter,
or when today's Stage 2 uses a feature the seed cannot compile:

```sh
aspect bootstrap stage2 -- refresh-seed
```

The task runs the same chain under the pinned Wasmtime CLI and replaces
`stage2.wasm` with B. Commit the refreshed seed in its own pull request.
