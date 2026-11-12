# Local CAS command integration

Added `vesper cas put-blob`, `put-tree`, `verify` and `materialize` over the existing
CAS library. Ingestion requires an explicit source/interface/artifact domain;
verification and materialization require an existing initialized store.
Tree ingestion rejects a store nested within its input. Object verification
reports its object-only scope; materialization verifies the transitive tree
into a fresh destination using verified reflinks/copies. Reports establish
content identity and keep cache eligibility false.

Five CLI tests passed, covering domain separation/deduplication, binary data,
independent materialization and destination preservation, corrupt/missing child
rejection with staging cleanup, invalid arguments without store initialization,
symlink rejection and self-ingestion. Strict workspace Clippy passed. The root
CLI and its tests cross-compiled for Windows GNU with pure BLAKE3; no Windows
runtime execution is claimed. Usage and boundaries are in `docs/cas-workflows.md`.
