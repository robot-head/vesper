# Lean4Lean 4.24 external replay: contract recommendation (#297)

This is a proposed implementation contract for review, not source authorization
or an execution report. Only this new document is owned by this planning lane.
It was prepared from the repository's tool/proof plans, the coordinator's
`/tmp/vesper-lean4lean42-port` and `port6.diff`, and Lean 4.24's import APIs.
This lane performed no installs, builds, replay, Cargo commands or runtime
source changes. Child issues and a reviewed frozen contract must precede writers.

## Deliverable and candidate-release gate

Implement a small developer binary, `lean4lean42`, which prepares and externally
rechecks **exactly** the bundled `RewriteWitness` and `ArithmeticOracle` modules.
Both are mandatory; there is no arbitrary module, prefix, single-declaration,
trusted-import or user-artifact option in this first profile. This is useful
external rechecking of the actual §42 models, not a general publish gate or a
claim that Lean4Lean's implementation is completely verified.

The coordinator reports actual fresh prototype checks of RewriteWitness
(84,702 declaration additions, 173.966 s) and ArithmeticOracle (86,270 additions,
172.626 s), plus three bad-value-FVar rejections, three closed controls, and
valid/invalid universe controls. Attribute these to the coordinator, not this
lane. They establish feasibility of the prototype only. Counts and times are
observations, never golden acceptance counts or deadlines that excuse a prefix.

**Do not release port6 unchanged.** Freeze the final candidate only after the
proof reviewer supplies an explicit applicability/disposition table for at
least these later corrections and the coordinator runs discriminating controls:

| Change | Required disposition |
|---|---|
| value FVar/MVar checks, all three declaration-value entrypoints | Keep the three pre-inference guards; reject the stale-cache exploit for definitions, theorems and opaques, with accepted closed controls. |
| d7e70a5 nested-parametric-argument validation | Port the applicable validation, or demonstrate the actual older path already enforces it using the exploit and a valid nested positive. |
| 997e298 normalized-Prop/projection restrictions | Port/review the applicable invariant; discriminate the universe-dependent false-proof route from valid projections. |
| 5518bf8 constructor FVar/MVar, nested auxiliary scan, equal mutual level parameters and duplicate mutual names | Review each constituent check, not just the commit title; applicable exploit controls must reach the actual checker. |
| d7a52 primitive reflection and bc9a736 level ordering | Retain the reviewed corrections and both accepting and rejecting primitive/universe controls. |
| quotient ordering, postponed constructors/recursors, loader lifetime | Preserve checked initialization and exact comparison, using the fresh callback's lifetime; do not reintroduce the import-only free-region path. |

The release manifest lists full immutable commit IDs, exact patch bytes and
ordered patch hashes, each review disposition, and the actual control results.
An unresolved applicable correction means `UnsupportedCandidate`, never a
successful reduced-scope result. The current prototype's identity is not the
future approved candidate identity.

## Narrow ownership and paths

Recommend three writer lanes after publication:

1. **Pinned tool/setup and strict Lean entrypoint:** new
   `tools/lean4lean42/{pins.json,port.patch,setup.py,README.md}` and any fixed
   controls included by that patch. The reviewed patch adds a strict machine
   mode to the pinned upstream `Main.lean`, instruments its actual checking
   sites, and includes the applicable kernel corrections. Setup is explicit;
   ordinary replay never fetches packages or installs a toolchain.
2. **Rust driver:** new `src/bin/lean4lean42.rs` and
   `src/bin/lean4lean42/{mod.rs,process.rs}`. The thin binary parses options;
   the module owns setup-receipt validation, binding, inventory/protocol and
   reports; process.rs owns one bounded child/session implementation. No
   import of private arithmetic-oracle process internals.
3. **Independent acceptance:** new `tests/lean4lean42.rs`, with genuine candidate
   controls and actual process/coverage failures. Any fixed transport fault
   peer must be named and claimed before writing it; it cannot supply positive
   external-check evidence.

The coordinator owns the final contract, integration report and any separately
claimed narrow BUILD/model-CI additions. No Cargo/lock/dependency, main/lib,
compiler/native, existing proof audit, oracle, model or metatheory edits. Cargo
auto-discovers the standalone binary. Preserve old APIs and semantics.

## Reproducible explicit setup

The starting point is upstream `4faf51403bc4cb35cf6666d91fc9c938ae46d3aa`,
with Lean `leanprover/lean4:v4.24.0`, compiler commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, and Batteries
`8da40b72fece29b7d3fe3d768bac4c8910ce9bee`. Full final patch identities remain
pending the release gate above. Both lakefile and lake-manifest must agree.

`python3 tools/lean4lean42/setup.py --directory NEW_DIRECTORY --lean PATH
--lake PATH --git PATH` is an explicit build operation. The destination must
not exist. Use argument vectors, no shell interpolation. Obtain immutable
commits, verify their trees and full identities, apply the exact reviewed patch
without fuzz, and build only the named runtime checker target initially.
Never run a floating update, default proof-target build, or downloaded opaque
checker as a replacement. A separate explicit proof-audit operation may build
Theory/Verify, recording their assumptions and holes without relabelling them
as completely verified.

Setup works in a fresh private staging directory, sanitizes Lean/Lake package
and search-path inputs, and does not reuse preexisting `.lake/build` output.
It records an allowlisted source/dependency inventory, patch identity, exact
compiler/sysroot, build commands and exits, and resulting executable hash in a
versioned receipt. Publish the receipt/directory only after successful build
and genuine required controls; failure retains no success receipt. A receipt
copied from another candidate or with unknown/extra fields is rejected.

This receipt and source/executable hashes associate an observed local build;
they are not cryptographic proof that machine code derives from source, nor
protection against an adversary controlling the host and receipt. State that
limit rather than claiming an executable hash proves its own provenance.

Before every run, verify the receipt's pinned profile against repository-owned
pins, relevant source/patch/dependency identities, and actual executable bytes.
Use the same resolved executable for hashing and spawn. Resolve a bare compiler
name by PATH first, then canonicalize that found path; do not reinterpret it as
cwd-relative. Reject missing/wrong tools, altered files or incompatible setup.
Record actual platform-specific executable hashes, not a fabricated universal
binary hash. Runtime checking is offline and cannot repair setup silently.

## Fixed CLI and Rust seam

Suggested CLI:

```text
lean4lean42 --tool-root DIR [--lean PATH] [--artifacts DIR]
```

No skip-missing, arbitrary checker path, arbitrary module, imported-only mode,
prefix discovery, comparison kernel, partial success, or unverified fallback.
Reject unknown/duplicate options. Success writes one bounded JSON report and
exits0. Any other outcome exits nonzero with a bounded structured failure report.

The internal module can expose the following small seam to integration tests;
this is not a new `vesper` library API. Derive Debug/Clone and existing serde
Serialize/Deserialize where meaningful; reports use `snake_case` enums and
explicit schema1. Do not turn report deserialization into execution approval.

```rust
pub enum Target { RewriteWitness, ArithmeticOracle }
pub enum Status { Complete, Failed, Unsupported }
pub enum Scope { EligibleFreshClosure }
pub struct Limits {
    pub setup_timeout_ms: u64,
    pub compile_timeout_ms: u64,
    pub inventory_timeout_ms: u64,
    pub replay_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub frame_bytes: usize,
    pub stdout_bytes: u64,
    pub stderr_bytes: usize,
    pub declarations: usize,
    pub name_bytes: usize,
    pub inventory_bytes: usize,
    pub artifact_bytes: u64,
    pub artifact_files: usize,
    pub report_bytes: usize,
}
pub struct Config {
    pub tool_root: PathBuf,
    pub lean: PathBuf,
    pub artifacts: Option<PathBuf>,
    pub limits: Limits,
}
pub enum ErrorKind {
    InvalidConfig, UnsupportedCandidate, ToolIdentity, Allocation, Io,
    Compile, Protocol, Coverage, ExternalRejection, Controls, Timeout, Crash,
    OutputLimit, Artifact,
}
pub struct Error { pub kind: ErrorKind, pub detail: String }
pub fn run(config: &Config) -> Result<Report, Failure>;
```

Keep `Failure { error, partial }` small with `Box<Report>` if necessary. Error
details are at most4096 UTF-8 bytes, safely cut at character boundaries. The
report contains actual tools/patch/control evidence, exact target source and
artifact hashes, eligibility/coverage counts, stream hashes and measured times.
`Complete` requires **both targets** and every release/control/binding/coverage
condition. Unsupported is explicit, never a successful exit or skipped test.

## Exact source, artifact and import binding

Copy the exact bundled model bytes to regular files in a fresh private root;
hash the bytes actually written. Compile RewriteWitness first, then
ArithmeticOracle with the actual pinned compiler and controlled LEAN_PATH.
Use `lean -o`; never execute ArithmeticOracle's server while preparing it.
Record native Lean compilation separately from later external replay.

Use the exact validated sysroot, an empty known cwd and explicit allowlisted
search roots: the private model artifacts plus the candidate/tool and pinned
compiler/dependency libraries actually needed. Clear ambient LEAN_PATH and
LEAN_SYSROOT overrides before setting the intended values. Never scan all
installed modules or inherit an unrelated project's artifact path. The strict
entrypoint imports exactly the enum-selected module with `importAll := true`
and begins replay in `Lean4Lean.Environment.empty`; it never calls the
upstream module-prefix discovery, compare or trusted-import paths.

Inventory the actual resolved import closure (module name, canonical path,
artifact parts and digest), including relevant `.olean` sidecars for this pinned
loader. Hash regular files incrementally using a <=64 KiB buffer and explicit
file/count/aggregate caps; never load the installed tool or all artifacts into
one Vec. Preserve the source→compiler→artifact→external-run association in the
report. Missing or shadowed dependencies, unexpected roots, symlinks replacing
private model files, or changing identities before/after use fail closed.
The ordinary compiler/loader's internal allocations remain outside the logical
buffer bounds; watchdogs do not establish whole-process RSS limits.

## Strict machine protocol and actual coverage

Do not parse the upstream human `checked N declarations` line. Add fixed private
machine modes to the reviewed pinned Main entrypoint, selected only by fixed
Rust argument vectors: profile/version, inventory, controls and fresh replay.
No user Lean source or theorem text appears in these arguments or requests.
All Lean implementation/helper text is fixed reviewed source; this batch needs
no dynamically generated Lean. Any future generated Lean uses the existing
structured syntax AST and exactly one serialization.

Use a four-byte little-endian length plus bounded JSON payload. Read exact
header/payload loops, reject a length before allocation, require schema1 and an
exact session/target echo, and reject unknown message forms. The profile/version
handshake identifies pinned Lean, protocol, reviewed patch profile and mode.
Rust chooses an unpredictable per-run token for association; it is an echo
binding against accidental stale/mixed traffic, not authentication of a hostile
host. A single request is outstanding; the strict tool processes one exact
module per process. Run the two target processes sequentially, never tasks per
prefix or import. A single combined two-target fresh environment would be a
different scope and is not substituted.

First run the native-loader inventory mode: import the exact target, enumerate
the actual closure and classify every constant by origin module, declaration
form and unsafe/partial eligibility. Require its terminal record and clean
EOF/exit0. Bound loaded-constant counts before building a list/index, then check
aggregate name bytes while constructing bounded records. Compiler/loader work
before that inspection is covered by the process deadline, not claimed to be
preallocated Rust data.

The inventory assigns deterministic ordinals to exact Lean Names. Encode name
components as tagged string/numeric components, with explicit bounds; do not
use `Name.toString` as an injective name encoding (a component can contain a
dot). Sort by the pinned Lean Name comparator and have both tool modes use that
same definition. Rust checks contiguous IDs, unique canonical encodings,
eligible/skip flags and exact origins, retaining a capped inventory and a
coverage bitset. It need not reimplement Lean's comparator. Use checked u64
counters and canonical length-prefixed transcript hashes, not Rust Debug text.

Fresh replay reconstructs its own inventory and must echo the identical
inventory identity before checking. Instrument the **successful**
`Lean4Lean.addDecl ... true` branch and the actual postponed constructor and
recursor comparison sites, not the pre-check verbose printer. Emit bounded
coverage events containing ordinals and action/group tags after success.
Mutual declarations, generated constructors/recursors and quotient members
need an explicit mapping: a checked group covers its actual constituent names,
while postponed members are covered only after their exact comparisons succeed.
A final environment membership check alone must not turn a skipped name into a
checked name. Keep declaration-addition counts distinct from covered-constant
counts; an inductive block is not one constant.

Rust independently matches these events to the native inventory. Reject unknown
IDs, duplicate coverage, inconsistent action/form/group mappings, an unexpected
skip, or a name claimed before its successful check. Closure completion
requires every eligible constant covered exactly once, with every postponed
comparison completed and no pending/remaining replay work. Unsafe and partial
constants remain explicitly enumerated skipped declarations, not silently
accepted proofs or runtime code. Report skip counts and inventory digests.

Require nonzero **target-owned** eligible coverage for each requested module,
not merely nonzero imports. Require all 33 RewriteWitness audit roots to be
present and eligible, and fixed ArithmeticOracle roots `checkExpr`,
`checkProgram`, `evalSteps`, `processRequest` in namespace
`Vesper.ArithmeticOracle` to have an explicitly reviewed covered/skipped status.
Any partial root's actual compiled definition/helper split must be inventoried
truthfully before choosing the final required list; do not invent coverage for
an unsafe/partial entrypoint. The final frozen contract names exact required
eligible declarations after inspecting the approved artifacts. A missing or
uncovered mandatory root fails even if the generic count matches a prior run.

Terminal success includes expected/covered/unsafe/partial counts, zero
unresolved work and the coverage-stream association. Require it exactly once,
followed by clean EOF and exit0. Zero output, truncated output, a success line
without events, a terminal-before-coverage, crash, timeout or output overflow is
a failure. No expected count is copied from the prototype's84k observations.

## Finite processes and storage

Recommended initial defaults/HARD values, subject to actual measurement before
the contract is frozen; all values positive and validated before spawn:

| Limit | Default | HARD |
|---|---:|---:|
| explicit setup/build deadline |1800 s|3600 s|
| each model compile deadline |300 s|900 s|
| inventory deadline |120 s|300 s|
| each fresh replay deadline |600 s|1800 s|
| shutdown/kill-reap grace |5 s|10 s|
| one frame payload |16 KiB|64 KiB|
| one child's total stdout bytes |32 MiB|64 MiB|
| retained stderr |64 KiB|256 KiB|
| closure constant inventory |262144|524288|
| aggregate encoded name bytes |16 MiB|32 MiB|
| retained inventory metadata |64 MiB|128 MiB|
| individual encoded name |4096 bytes|4096 bytes|
| name components / component bytes |128 /1024|128 /1024|
| resolved artifact files |4096|8192|
| resolved artifact bytes, streamed |2 GiB|4 GiB|
| one generated model artifact |64 MiB|128 MiB|
| final report |256 KiB|1 MiB|

Maximum two target runs are sequential. Compile/setup are explicit phases with
their own deadlines; setup does not run during ordinary replay. Retain only
bounded stdout frames/current state and incremental hashes, not a32MiB String
by default. The inventory is the sole explicitly bounded per-closure retained
collection. Event coverage uses ordinals/bitsets, avoiding a second full set of
name strings. Prepay/check capacity arithmetic and use fallible Rust reserves.

Use concurrently drained stdout/stderr and bounded queue/fault signalling.
Output overrun is detected even if bytes are not retained. OS watchdogs kill
and **wait/reap** before joining potentially blocked readers/writers. Avoid
blocking producer sends during cleanup. Collect actual exit/wait/join results;
do not infer reaping from sending kill. A malformed frame or checking failure
cannot be converted to a completed smaller check. This deadline controls the
external process, not interruptibility inside the Rust or Lean allocator.

Artifact output is optional: create a unique subdirectory under a caller-owned
directory, do not overwrite/delete unrelated paths, preflight file/byte caps,
and record hashes of retained source, model artifacts, receipts and bounded
control/coverage transcripts. Exclude the report from its own hash list while
still counting its complete bytes. Never archive the entire installed sysroot
or emit a file per84k declarations by default. A failed artifact write fails the
operation rather than silently dropping requested evidence.

## Axiom policy and honest status

External typing accepts well-formed axiom declarations; it is not an axiom
hygiene verdict. Preserve the existing witness/model axiom policy separately
and explicitly report whether that separate audit was actually executed. No
change to the existing proof auditor is required by this batch. In particular,
ArithmeticOracle imports Lean and may legitimately depend on declarations with
an axiom footprint different from the mathematical catalog. Do not blindly
apply the catalog's `[Quot.sound, propext]` list to every imported IO declaration.

The report keeps distinct: exact source/artifact binding, native compilation,
external eligible fresh-closure replay, release controls, target axiom audit,
and implementation-proof audit. Use `not_run`/`partial` where true. Current
Theory/Verify gaps and explicit assumptions remain visible. Neither the two
successful model checks nor the oracle's100000 comparisons establishes
Lean4Lean's whole implementation verification, Rust translation/refinement,
IO/JIT behavior, DDC/bootstrap, metatheory completion or full §42 completion.

## Meaningful acceptance before a production claim

Default acceptance requires the real pinned tools; missing/wrong tools fail
hard, with no skip-as-pass. Coordinator-executed evidence should include:

1. Fresh reproducible setup with exact reviewed patch/disposition manifest;
   all applicable bad raw-declaration controls and closed positives reach the
   actual Lean4Lean checking path. A native elaboration rejection before an
   artifact exists is not external checker rejection evidence.
2. Exact bundled model compile plus both complete fresh replays, checked
   inventory/events/required roots, honest unsafe/partial omissions and actual
   measurements. Positive tests never replace the tool with a canned peer.
3. Absent target, substituted model/sidecar, wrong compiler/receipt/patch,
   contaminated ambient search paths and unexpected artifact resolution fail
   binding or setup checks rather than becoming generic successful replays.
4. Coverage fixtures: imports-only completion, zero target-owned coverage,
   omitted/duplicate/wrong ordinal, incorrect constructor/recursor accounting,
   premature terminal and a missing mandatory root all fail through the same
   production parser/coverage state machine. Synthetic negative protocol
   fixtures supply no positive mathematical result.
5. Production short-read/truncation/oversize parsing and crash/timeout/stderr
   overflow cleanup, including actual child reaping and bounded retained data.
   Keep fault probes transport-only and separated from genuine replay counts.
6. Separate axiom controls: an intentionally sorry-backed or custom-axiom-backed
   well-typed theorem may be accepted by the external checker but must fail the
   relevant separate policy. Do not claim every changed well-typed statement
   must be rejected by a kernel; binding rejects wrong-associated artifacts.

Freeze actual final tool identities and exact root/event formats only after the
pending applicability review; this recommendation does not authorize writers
to choose a weaker checker or silently discard an unresolved known defect.
