# vesper-loom-sandbox

Hermetic Wasmtime host for `wasm32-wasip2` build steps and Forge macros. A step is a component exporting `vesper:sandbox/step@0.1.0` ([wit/sandbox.wit](wit/sandbox.wit)). It runs with fuel and aggregate memory limits, read-only input mounts, one empty writable `/out`, no environment/arguments/sockets, a fixed clock and seeded randomness.

See [docs/parallel-results/forge-wasi.md](../../docs/parallel-results/forge-wasi.md) for the guarantees, evidence, decisions and limitations.

```sh
cargo test                 # uses the committed fixtures in fixtures/bin
./fixtures/build.sh        # rebuild fixtures (needs wasm32-wasip2 and wasm32-wasip1 targets)
```
