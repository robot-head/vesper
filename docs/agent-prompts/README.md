# Retained implementation prompts

The [current wave](wave-nine/README.md) and linked earlier assignments are kept
for open issues, unmerged branches, and their implementation dependencies.
Completed prompt archives have been removed. Confirm claim status and ownership
on the [live issue board](https://github.com/robot-head/vesper/issues?q=is%3Aissue+is%3Aopen)
and [pull request board](https://github.com/robot-head/vesper/pulls) before acting.

The [manuscript](../manuscript/README.md) is the sole specification. Prompt text
defines task scope and acceptance work, with earlier section identifiers mapped
in the [section map](../section-map.md). Read the manuscript for language and
platform requirements.

The [port wave P1 plan](port-p1/README.md) moves the port-first crates to
Vesper source; its lanes start after the owner approves the plan.

The [port parity wave](port-parity/README.md) puts the merged
[Stage 1 performance wave](stage1-perf/README.md) speedups into the ports and
measures each port against its Rust build again. The
[port gaps wave](port-gaps/README.md) closes the compiler gaps those ports
measured. The [port speed wave](port-speed/README.md) closes the gaps its
re-score left.

The [port wave P3a plan](port-p3/README.md) ports the next six crates
(Pact, Loom audit, Bench, eBPF, Silicon, Aegis) and adds the `dyn Trait`
and atomics compiler lanes that the later P3b ports need.

The [port wave P3b plan](port-p3b/README.md) ports Warp, Prism H3, Chorus,
Flux, Kinetic and Resonance, and adds compiler lanes for effectful and
callable trait objects, boxes owned across loops, and opaque Bridge handles.

The [P3b gaps wave](p3b-gaps/README.md) closes the compiler gaps the P3a and
P3b ports measured: `f32` vectors, bounds checks, `inout` slice inlining,
fixed arrays, `continue`, two-phase arguments, the export boundary,
std sort and five correctness fixes.

The [self-hosting wave SH0](self-host-sh0/README.md) lays the groundwork for a
Vesper compiler: a hybrid build that links Vesper objects into the compiler,
std I/O and compiler data structures, Cranelift through a C-ABI shim, the
toolchain pilots and a real self-hosted front end. The crate-by-crate plan and
the later waves are in
[self-hosting-plan.md](../maintenance/self-hosting-plan.md).

The [Rust-exit wave SH6](self-host-sh6/README.md) drives the repository to
no Rust except the irreducible minimum: Stage 2 and a committed wasm seed,
every ported library flipped at the 1.5x bar, the OS and C-API crates on
`.vsys`, one crypto shim, and the Rust tests moved to Vesper. The program and
the census are in [rust-exit-plan.md](../maintenance/rust-exit-plan.md) and
[rust-census.md](../maintenance/rust-census.md).

Earlier assignments retained as dependencies:

- [06-source-simulation](next-wave/06-source-simulation.md)
- [02-verified-direct-codegen](wave-eight/02-verified-direct-codegen.md)
- [22-quarry-concurrency-indexes](wave-eight/22-quarry-concurrency-indexes.md)
- [04-ebpf](wave-five/04-ebpf.md)
- [11-quarry-btree-mvcc](wave-seven/11-quarry-btree-mvcc.md)
- [17-ebpf-library-rest](wave-seven/17-ebpf-library-rest.md)
- [26-ebpf-backend](wave-seven/26-ebpf-backend.md)
- [10-quarry-durable](wave-six/10-quarry-durable.md)
- [18-ebpf-depth](wave-six/18-ebpf-depth.md)
- [04-crash-wal-network-simulation](wave-three/04-crash-wal-network-simulation.md)
