# Resource snapshot artifact binding

This implements the artifact portion of #395/#396 under
`lean4lean42-resource-contracts.md`. It freshly compiles the twelve fixed
ResourceModel42 sources, invokes the unchanged kernel snapshot producer, binds
its complete occurrence/omission/duplicate/alternate-proof report, and requires
its complete canonical-plus-alias output set in the actual native inventory
before external replay. Source model, projector and strict-audit policies are
unchanged by this slice.

`resource::verify_bundle` checks the coordinator's fourteen-file BLAKE3 bundle
against the actual embedded twelve sources, snapshot tool and historical
manifest. The historical manifest remains SHA256 metadata with its original
bytes. Its SHA256 fields are never treated as BLAKE3 hashes. Pins, sealing,
source preflight and ordinary offline execution all require the new resource
bundle association and profile/schema 2 identities.

`PreparedResource` has a private constructor and owns its private workspace.
Its read-only association includes the actual source-bundle digest, historical
manifest digest, tool/request/producer-report/projection file digests, original
object-manifest digest and producer counters. The full producer report is a
bound sidecar, rather than an enlarged summary report. The descriptor checks
module paths, unique per-module occurrence indices, counter equations,
canonical/omitted/alias partitions, duplicate module associations, same-module
auxiliary owners and every alternate-proof occurrence association. It never
uses a caller-supplied accepted flag or a report deserializer as authority.

The actual typed inventory is forward-rendered only for owned names under the
frozen identifier/primes/numeric/`_@` restriction. Duplicate rendered images or
unsupported components fail. There is no inversion or splitting of producer
name strings. The entire owned set must equal the retained canonical names
plus all aliases. The 284 theorem catalog is recovered from the fixed embedded
source commands, including source-module origins; those names and all aliases
must actually be theorems. The four required APIs must be definitions with the
fixed original source origins. All owned declarations must be eligible. The
protocol independently requires complete imported-closure replay and all-owned
coverage.

The private tree has thirty files on the fixed producer path: twelve sources,
twelve original legacy objects, projector, request, report, projection, bundle
manifest and historical manifest. Unknown files, unexpected directories,
package sidecars, symlinks/reparse points, changed source bytes and changed
file sets are refused. All file hashing is streamed and finite. Producer JSON
has a sixteen-MiB capture ceiling, bounded depth and a conservative parsing/map
reservation against the configured inventory budget, including the simultaneously
retained captured stdout/stderr capacities. The live descriptor is also charged
against every subsequent model session alongside transport and the protocol
parser's borrowed native inventory. Its checked allowance includes struct and
Vec capacities, all retained string/path/receipt buffers, and 1 KiB per BTree
entry plus one root node for each collection. Counts are independently
checked; the observed 3,792 output declarations are not an acceptance golden.

Production calls descriptor revalidation before and after native inventory and
external replay, including the sealed source/compiler/runtime/build envelope
and exact receipt snapshot. `artifact::prepare_resource` and
`artifact::resource_inventory` expose those same production operations for
independent genuine-file and genuine-inventory mutation tests without another
full external replay. The preparation helper performs fresh compilation and
projection; the inventory helper obtains an actual mode-0 inventory and does
not claim external success.

Compiler output remains private. When evidence retention is requested, the
bounded actual tree is staged privately and compared to its expected complete
file manifest before publication. Retained files and the bounded report are
preflighted before retained writes. A replay refusal can retain actual prepared
evidence without inventing success. Public report association paths are
relative to the retained directory with the `resource/` prefix; handle-local
association paths are relative to `PreparedResource::root()`.

## Validation

The source checkpoint `b36eb46`, with the published protocol and source-bundle
dependencies, passed actual `cargo check --locked --bin lean4lean42` in 90
seconds with no warnings. The command used one job,
`CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next`, development/test debug
information disabled and incremental compilation disabled. Log:
`/tmp/lean4lean42-resource-artifact-check.log`.

Follow-up `f29eacd` preserves private evidence on refusal and adds a read-only
owned-name accessor. It passed rustfmt and `git diff --check`; its combined
compiler, strict Clippy and independent acceptance checks, including the later
shared descriptor/capture reservation refinement, remain coordinator
work at this checkpoint. No Lean compiler, kernel snapshot, external replay or
new genuine setup was executed by this source lane. Coordinator pilot evidence
is documented separately and is not substituted for execution of this code.

This stage is external logical declaration replay. Both axiom audit fields
remain `not_run`; it does not rerun the separate #323 axiom policy audit,
complete Lean4Lean implementation proofs, establish native equivalence or
provide the resource-program differential campaign.
