# Local CAS commands

The local store provides domain-separated BLAKE3 content identity and verified
materialization. Choose an explicit store outside the input directory:

```sh
vesper cas put-blob output.o --domain artifact --store .vesper-cas --json
vesper cas put-tree package-src --domain source --store .vesper-cas --json
vesper cas verify '<object-id>' --store .vesper-cas --json
vesper cas materialize '<tree-id>' workspace --store .vesper-cas --copy --json
```

`put-blob` ingests a regular file by streaming it through the store's hasher;
`put-tree` ingests a canonical directory tree. Both require an explicit
`source`, `interface` or `artifact` domain. Identical bytes in different domains
have different identities. JSON reports `object_bytes`, the size of the named
object's encoding, and `newly_stored`, which is false for verified duplicates.
A tree's encoding size is distinct from its transitive payload size. Ingestion
initializes the configured store if necessary. Keep source directories stable
during ingestion; this is not an atomic filesystem or Git snapshot operation.

`verify` requires an existing initialized store and rehashes only the named
object. Tree verification also checks the canonical tree encoding. Its JSON
scope is `object_only`: this does not verify every referenced child. Missing or
corrupt objects fail without a success report.

`materialize` verifies the entire referenced tree while producing a fresh
destination. Existing files/directories are never replaced. The default tries
reflinks, verifies their results and falls back to copying. `--copy` requests
verified copies only. Materialized files are independent from CAS objects;
editing a workspace cannot mutate the store through a hard link. Errors remove
the unpublished staging directory. The report gives files, directories, payload
bytes and actual copy/reflink counts.

Symlinks and special input files are rejected by the library. The store assumes
a caller-controlled private root and protected ancestors; it is not an isolation
boundary against its owner rewriting files. Commands perform no network access
or build execution. Content identity does not establish registry/package
ownership, provenance, builder trust or cache eligibility. Machine reports keep
`cache_eligible` false; the separate attestation and registry policies must be
applied before any trusted-cache decision.
