# CycloneDX snapshot inventory

Coordinator-owned `src/sbom_commands.rs` exposes `run(Vec<String>)` for `vesper sbom [Loom.lock]`. It validates the complete snapshot before emitting CycloneDX 1.6 JSON and performs no package execution, network access or file writes. The parent coordinator wires CLI routing and end-to-end tests.

CLI integration is complete. Five new end-to-end tests and 20 graph/package/binary unit regressions passed, with clean formatting and strict workspace Clippy. The integrated CLI's output also passed the official schema and format checks described below. The prior full 17-package suite passed 742 tests; this CLI-only addition was checked with the targeted suite.

Reachable Vesper and native components have distinct deterministic generic PURLs/bom-refs and sorted direct dependencies, including leaf/native entries. Cycles remain graph edges rather than recursive tree expansion. Namespace encoding preserves `@std/lib` versus reserved `std/lib`, native identities and SemVer build metadata. Original root identities and valid unused selections remain metadata properties. Features, declared/required capabilities, FFI policy, dependency ceilings and native constraints are namespaced properties; free-form license text is preserved as a property without asserting SPDX validity.

No UUID, timestamp, fabricated source/hash or synthetic project identity is emitted. This inventories declared metadata only, not authenticated source contents, actual native installations, ABI/CVE findings, compiler-body authority or an authenticated SBOM.

Validation:

- Isolated harness compilation, encoding regression and strict all-target Clippy passed with the current calculus/serde dependencies. No repository manifest or production dependency changes were needed.
- Actual emitted JSON passed the official CycloneDX 1.6 Draft-07 schema with format checking, using Python `jsonschema` and locally loaded references.
- The validation fixture includes a dependency cycle, two same-leaf namespace identities, build metadata, `libstdc++`, free-form quoted/newline license text and unused package/native selections. Every dependency ref resolved, namespace/domain references stayed distinct, input reordering produced identical output, and no-root/invalid-schema inputs returned errors without partial output.

The official `https://cyclonedx.org/schema/bom-1.6.schema.json` endpoint returned HTTP 403 on this host. Equivalent official repository schemas were downloaded to `/tmp/vesper-sbom-schema/` from `https://raw.githubusercontent.com/CycloneDX/specification/master/schema/`; no large schema files or validation-only dependencies were vendored.

| Official schema file | SHA-256 used for validation |
| --- | --- |
| `bom-1.6.schema.json` | `18f57f7482593bad9f21b4feed09084640cbeff419d62ad5090c5ceccca5b37d` |
| `spdx.schema.json` | `4b345e2329f209f34e960ae2a8e7cb46a166907e6a45e94978565925dc47b359` |
| `jsf-0.82.schema.json` | `8bae002c25e723db7ee1f26afde680ae1a2b1a8f6b4b0fd65dc3becb090aae` |

The schema declares `$id = http://cyclonedx.org/schema/bom-1.6.schema.json`. Validation used `jsonschema.Draft7Validator` with that base URI, the two references in a local resolver store and `Draft7Validator.FORMAT_CHECKER`. The only warning was Python's deprecation notice for `RefResolver`.
