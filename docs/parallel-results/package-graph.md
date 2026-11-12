# Offline package-graph inspection

Coordinator work while the seven next-wave lanes are active. The implementation adds `src/package_graph.rs`, CLI routing/help, and `tests/package_graph_cli.rs`; compiler and active library APIs are unchanged.

`vesper graph [Loom.lock]` validates the entire solution snapshot before displaying its reachable Vesper/native graph. Text, schema-versioned JSON and Graphviz DOT include versions, active features, declared ceilings, conservative required capabilities, dependency-edge constraints and native requirements. Unused snapshot selections are reported separately.

`--package @org/name` selects a cycle-safe dependency closure. `--invert` selects dependents and reverses DOT arrows. Original edge ownership and the full graph's computed capability requirements remain intact in filtered views. JSON/text/DOT output is deterministic under selection reordering. Vesper/native DOT node IDs occupy distinct namespaces, and labels are quoted safely.

Validation:

- Seven new graph CLI tests plus 20 existing CLI/locked-resolution/package tests passed.
- The binary's DOT string-escaping unit test passed (28 tests total).
- Workspace formatting and strict all-target Clippy passed.
- Real Graphviz rendered DOT to SVG, and the documented focused reverse-graph invocation succeeded. Coverage includes shared dependencies, cycles, native requirements, unreachable selections, invalid metadata outside a filtered view, output determinism and read-only/error behavior.

This command inspects internally consistent snapshot metadata. It does not check a current catalog/manifest, enforce resolver trust/optimization policy, authenticate sources, audit compiler authority or execute builds. Use `resolve --locked` for current-input consistency first. Report schema 1 is an inspection format, not a MIF or authenticated SBOM. The compiler/Lean/backend suite was not repeated for this CLI-only addition.

See [package workflows](../package-workflows.md) for command and error-output details.
