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
Use the same selected actual tool for hashing and spawn. Preserve launcher
argv0 during initial discovery: resolve a bare lean/lake name through PATH
without following its final symlink, and invoke that path with its original
lean/lake basename for --version and lean --print-prefix. Never canonicalize an
elan/rustup-style multi-call shim to its underlying executable and then execute
it under the wrong name. Do not reinterpret a bare name as cwd-relative.
After these probes, canonicalize the reported sysroot directory and select its
bin/lean and bin/lake paths (with the actual platform executable suffix).
Probe those actual tools too: Lean must report4.24.0 and compiler commit
797c613eb9b6d4ec95db23e3e00af9ac6657f24b, and --print-prefix must resolve to that
same canonical sysroot. Lake's version must agree with the selected4.24 tool.
Use these actual sysroot program paths for builds/replays, preserving their
argv0 as well; canonical file resolution is for identity checks/hashing, not a
reason to execute a differently named alias target. Reject missing/wrong tools,
altered files or incompatible setup.
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
    pub compiler_runtime_files: usize,
    pub compiler_runtime_bytes: u64,
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
Rust chooses a fresh per-run token for association; it is an echo
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
`Vesper.ArithmeticOracle` to be present, eligible and covered. A missing or
uncovered mandatory root fails even if the generic count matches a prior run.

The coordinator's subsequent native-loader probe confirms all four named
arithmetic roots are eligible definitions. Require their actual eligible
coverage, with no skipped-root alternative. Require the33 model roots as well;
when ArithmeticOracle imports RewriteWitness, check the33 imported model roots
and its four target roots (37 total). RewriteWitness itself requires33.

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
| selected compiler-runtime library entries |128|256|
| selected compiler-runtime library bytes, streamed |2 GiB|4 GiB|

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

## Exact schema refinement for the coordinator's freeze

This section supplies the exact proposed wire/report/receipt seam. It supersedes
the tentative message/root/receipt details above, while the security and scope
gates remain mandatory. Only final candidate patch hashes and the kernel
writer's finite control inventory remain data to populate before release.
The kernel component contract is `262b5e9`; keep its existing exclusive paths.
Use separate `patches/replay.patch` and setup/driver files for subsequent lanes,
rather than writing the kernel-owned patches or controls.

The actual native-loader probe observed the following; these are feasibility
measurements, **not expected successful counts**:

| Target | All / eligible | Unsafe-only / partial-only / both | Ctor / recursor | Target-owned eligible | Modules |
|---|---:|---:|---:|---:|---:|
| RewriteWitness |87495 /86647|85 /763 /0|1178 /767|179|800|
| ArithmeticOracle |89237 /88337|86 /814 /0|1252 /815|368|820|

The maximum observed JSON-encoded name was294 bytes /20 components; aggregate
JSON-encoded names were6,536,611 /6,649,344 bytes. Maximum observed inductive/quotient
group size was4. Existing recommended name/count/storage limits cover these
observations. Old CLI additions equal eligible minus constructors/recursors,
but repeated idempotent quotient additions must disappear in the new driver;
the corrected actual addition count need not equal the old count.

### Exact tool arguments and common wire values

The patched executable receives **exactly** four arguments:

```text
--vesper42 MODE TARGET TOKEN
```

MODE is canonical decimal0=inventory,1=fresh replay,2=controls,3=profile.
TARGET is0=RewriteWitness,1=ArithmeticOracle; modes2/3 require TARGET=2
(no target). Unknown/extra/duplicate/malformed arguments exit nonzero. This
exclusive branch never invokes upstream flag partition/prefix discovery or
parallel tasks. stdin must be closed by Rust; no request language is accepted.

TOKEN is32 lowercase hexadecimal ASCII bytes. Rust may derive it from
domain-separated BLAKE3(clock,PID,checked process-local counter), taking16 bytes.
No cryptographic unpredictability/authentication claim is made and no new
dependency is needed. The private pipe/process is the session boundary.

Every stdout message is exactly one four-byte unsigned LE length plus UTF-8
JSON array of that length. Only the array forms below are legal. All numeric
controls/counters are exact nonnegative JSON integers fitting u64, with IDs
additionally fitting u32; reject floats, exponents, signs, booleans-as-integers
and out-of-range values. B below is JSON integer0/1. JSON strings support normal
JSON escaping; accept only valid UTF-8. Pre-scan nesting<=8 before typed parsing.
No arbitrary objects, additional elements or trailing payload bytes. Preserve
short-read/short-write handling and enforce frame caps before allocation/write.

Name is an array of1..128 components: `[0,UTF8_STRING]` or
`[1,"CANONICAL_U64_DECIMAL"]`. The numeric string has1..20 ASCII digits,
no leading zero except `"0"`, and checked u64 parsing. A string component is
nonempty, <=1024 UTF-8 bytes; the whole Name's canonical typed binary encoding
(defined below) is<=4096 bytes. The tool must also bound the actual JSON frame
before writing. NAME_BYTES uses that binary encoding, avoiding dependence on
differences between Lean and Rust JSON string escaping.
Anonymous names are not constant/module names in this fixed profile. Use
component identity, not pretty-printed dotted strings. The inventory has
unique Names sorted by pinned `Lean.Name.cmp`; Rust checks identity and ordinal
consistency without inventing another name comparator. Bounds apply before
recursive name conversion; the fixed helper uses a bounded component walk.

Form is0=axiom,1=definition,2=theorem,3=opaque,4=inductive,5=constructor,
6=recursor,7=quotient primitive. Skip is0=eligible,1=unsafe-only,
2=partial-only,3=both. Form and skip come from actual ConstantInfo, not a name
prefix or source grep. Group IDs are sorted unique ordinals, maximum64 members.

### Complete message array forms

```text
hello       [1,0,TOKEN,MODE,TARGET,"4.24.0","vesper.lean4lean42/1"]
inventory   [1,1,MODULE_COUNT,FORMS_8,ELIGIBILITY_4,TARGET_ELIGIBILITY_4,NAME_BYTES]
module      [1,2,MODULE_ID,Name,OLEAN_PATH]
constant    [1,3,ID,Name,MODULE_ID,Form,Skip,OWNER_IDS]
inventory_end [1,4]
coverage    [1,5,EVENT_SEQ,ACTION,IDS]
skipped     [1,6,ID,Skip]
controls    [1,7,CONTROL_COUNT]
control     [1,8,INDEX,ID,EXPECTED_ACCEPT,ACTUAL_ACCEPT,EXPECTED_REASON,ACTUAL_REASON,MATCHED]
done        [1,9,TOKEN,MODE,TARGET,ADDITIONS,COVERED,CTOR_COMPARES,REC_COMPARES,
             UNSAFE_ONLY,PARTIAL_ONLY,BOTH,TARGET_COVERED,REMAINING,PENDING,
             CTOR_PENDING,REC_PENDING,CONTROLS,MATCHED_CONTROLS]
error       [1,10,CATEGORY,DETAIL]
```

FORMS_8 and ELIGIBILITY_4 are exact arrays of8/4 u64 counters; sum(FORMS_8)
equals sum(ELIGIBILITY_4). TARGET_ELIGIBILITY_4 uses the same mutually exclusive
skip order for actual target-owned constants. NAME_BYTES sums canonical typed
Name bytes for constant names (not module names). Rust additionally charges all
module/path/name storage against inventory_bytes. MODULE_COUNT>0; all constant
origins must name an actual inventory module. OLEAN_PATH is the resolved base
artifact path from this tool's controlled search path, nonempty<=4096 UTF-8
bytes, checked/canonicalized by Rust against its allowed roots.

After hello, modes0/1 emit inventory, exactly MODULE_COUNT module frames with
contiguous IDs0..M-1, then exactly sum(FORMS_8) constant frames with contiguous
IDs0..N-1, then inventory_end. Recompute every header count, target ownership
and name byte total from these actual records. Module Names and constant Names
must each be unique. Mode0 then emits done with all work fields zero; its three
skip fields repeat the inventory totals. Mode1 emits each noneligible skipped
frame once in increasing ID order, then coverage events and done. All skipped
records must match the inventory's exact nonzero Skip. Native inventory and
replay each emit their own complete inventory; Rust compares decoded records
and canonical transcript hashes before accepting any replay event. This needs
no Lean implementation of BLAKE3 and does not trust a supplied digest.

OWNER_IDS are empty for forms0..3; the complete mutual-inductive ID group for
form4 (including itself); the single owning-inductive ID for form5; the complete
owning mutual-inductive group for form6; and the four quotient IDs for form7.
All referenced IDs must exist and have the expected forms. Groups are derived
from actual inductive/constructor/recursor metadata, not inferred from names.
Every form4 member has identical group IDs; a constructor's owner belongs to
the group checked before comparison. Every form7 record has the same four IDs,
whose exact Names must be Quot, Quot.mk, Quot.lift, Quot.ind.

ACTION0=successful scalar addition (forms0..3, IDS length1);
ACTION1=successful inductive-group addition (all form4, IDS exactly its group);
ACTION2=first successful quotient initialization (all four form7 IDs);
ACTION3=successful postponed constructor comparison (form5, IDS length1);
ACTION4=successful postponed recursor comparison (form6, IDS length1).
Emit only after the actual checking/comparison succeeds. EVENT_SEQ starts0
and increments by1 without gaps. ACTION0/1/2 increment ADDITIONS by1 regardless
of group size; every ID in an event increments COVERED by1. ACTION3/4 increment
their comparison counter by1. IDs must be eligible, not previously covered;
the owning inductive group must already have its ACTION1 event for ACTION3/4.
Scalar declarations are reconstructed and checked normally; never count a
successful environment lookup as an addition.

For ACTION2 explicitly schedule Eq prerequisites, require quotient state not
initialized, check all four names are absent in the fresh environment, and
remove all four from remaining/pending so no later name triggers an idempotent
addition. Call the actual checked quotient initializer once. Confirm the four
generated ConstantInfos agree with the imported quotient metadata before
emitting the four-ID event. A second quotient event or a claimed success from
an already-initialized no-op is an error. Do not change the public kernel API's
separately tested idempotent behavior.

Before done in replay, COVERED=eligible, TARGET_COVERED=target eligible>0,
CTOR_COMPARES and REC_COMPARES equal the eligible form5/form6 counts; all37/33
required roots are covered, and REMAINING/PENDING/CTOR_PENDING/REC_PENDING are0.
All skip counters equal inventory totals, every skipped frame was seen once,
and CONTROLS/MATCHED_CONTROLS are0. Rust recomputes each counter from observed
events and rejects any discrepancy. This applies to all eligible constants,
not only required roots or target-owned declarations.

Mode2 emits hello, controls, exactly CONTROL_COUNT control frames, then done.
All non-control work/skip/pending fields are0; CONTROLS=MATCHED_CONTROLS must
equal the pinned required case count. Mode3 emits hello then done with all
counters0. No inventory/coverage/control message is allowed in another phase.
Every successful mode then closes stdout and exits0; extra bytes, duplicate
done, missing EOF or nonzero exit fail. No shutdown handshake is required.

On failure, at most one error frame is emitted, then nonzero exit and EOF;
never done. CATEGORY is exactly `checker`, `coverage`, `limit`, `import`,
`controls`, `io` or `internal`. DETAIL<=4096 UTF-8 bytes. Only a real
Lean4Lean.addDecl/checker error is classified checker; loader and protocol
failures cannot discharge a logical rejection control. A child crashing without
an error is still failure. Ordinary timing/progress text goes only to bounded
stderr, never stdout.

### Exact controls packaging and expected data

Kernel-owned `controls/VesperControls.lean` is copied into
`Lean4Lean/VesperControls.lean`, using the existing library target. No new
package or Cargo dependency. Runtime namespace/API:

```lean
namespace Vesper.Lean4Lean42Controls
structure ControlResult where
  id : String
  expectedAccept : Bool
  actualAccept : Bool
  expectedReason : String
  actualReason : String
  matched : Bool
runControls (baseline : Lean.Kernel.Environment) : IO (Array ControlResult)
```

The fixed mode imports only the compiled umbrella and its actual dependencies
with importAll, then passes env.toKernelEnv while inside the import callback.
This is the explicit control fixture baseline, not an assertion that controls
replay their imported closure freshly. Kernel-owned Check.lean calls the same
runtime suite. No separate canned Boolean simulation or run_meta-only suite.

Pinned known-fixes.json has schema1 and a top-level controls array. Each entry
has exact fields id, source, expected_accept, expected_reason, kind; source is
`tools/lean4lean42/controls/VesperControls.lean`, kind is `raw_declaration`,
`checker_expr` or `condition`. Exact case IDs/reason data come from the kernel
writer's reviewed manifest. Require1..256 cases, unique IDs<=128 printable
ASCII bytes, reasons<=128 printable ASCII bytes, and source fixed as above.
Every entry is required; there is no runtime skip/unexecuted control outcome.
INDEX is the pinned manifest array index. Tool expected fields must equal that
entry; Rust recomputes MATCHED from actual accept/reason and requires1. A tool's
expectedAccept or matched=true alone is not authority. Fixture errors must not
be confused with the intended negative reason. Keep per-kind control evidence
so a condition check is not described as a raw declaration rejection.

The actual kernel manifest's exact top-level allowlist is schema, role,
upstream_base, lean_toolchain, batteries_commit, controls, fixes, source_sha256,
executions, unexecuted. Validate schema/role/pins/controls structurally. Other
documentary values may be opaque but must remain within a4 MiB total document,
nesting<=32, strings<=64 KiB and at most65536 aggregate JSON values; hash the
complete exact bytes against repository pins. Reject an unknown top-level key.
Expectation entries have exactly the five fields above; the runtime
ControlResult has six. Documentary executions/fixes text is review evidence,
never a runtime success receipt and never a substitute for executing controls.

### BLAKE3 setup sealing without a Python dependency

Refine setup CLI to require `--driver PATH` as well as the exact compiler/Lake/
Git paths. This is an already-built genuine Rust binary; setup does not build
Vesper or select an arbitrary positive replacement checker. The Python script
uses only its standard library and subprocesses. After applying verified
immutable source/dependency patches and building the named executable, it
invokes exactly:

```text
DRIVER --seal-setup STAGING_DIRECTORY
```

This private mode is exclusive with ordinary run options. STAGING_DIRECTORY
contains bounded setup-inputs.json (<=16 KiB) with schema1 and exact fields
lean, lake, git, build_argv, build_exit, build_stdout, build_stderr. Tool paths
are absolute resolved launcher paths preserving their lean/lake argv0 for the
initial probes (not final-symlink-canonicalized shim paths). The sealer discovers
and binds actual bin/lean and bin/lake under the same canonical4.24 sysroot as
described above. build_argv is an array of<=32 strings, total
<=8192 bytes, with the repository-pinned build recipe; build_exit must be0.
Log paths are relative regular files under staging, <=64 KiB retained each;
output-overflow flags, if needed, must be part of a separately reviewed recipe
amendment rather than an ignored extra JSON field. Setup itself enforces the
build deadline/output cap and records the observed result. Sealing does not
claim logs cryptographically prove the earlier command ran.

The Rust sealer validates strict schema, repository pins/known-fixes/control
sources, exact patched source/dependency inventories and compiler profile,
hashes the actual checker/compiler/build inputs with bounded reads, and runs
the real profile and all required controls using production protocol/deadlines.
It writes setup-receipt.json with create_new only after those gates pass, flushes
it, and exits0 with that same bounded receipt JSON. Failure writes no receipt.
Only then may setup atomically rename its sibling staging directory to the
requested absent destination. Ordinary run neither fetches nor repairs setup.

Exact receipt fields (serde names shown) are schema, profile, upstream_commit,
batteries_commit, lean_release, lean_commit, pins_blake3, known_fixes_blake3,
patches, source_manifest, source_manifest_blake3, compiler_path, sysroot,
compiler_blake3, compiler_runtime_manifest, compiler_runtime_manifest_blake3,
checker_relative_path, checker_blake3, driver_blake3,
build_argv, build_exit, build_stdout_blake3, build_stderr_blake3, controls,
controls_blake3. Patches is the repository-pinned ordered array of
`{path,blake3}`. Source_manifest is an ordered array of
`{path,bytes,blake3}` for the recipe's complete allowlisted source/dependency
files, excluding generated build output/Git metadata; its exact file set is
checked and source changes/additional build inputs fail. Relative paths use `/`,
contain no empty/dot/dotdot/absolute component, and fit4096 UTF-8 bytes.
Compiler/checker hashes are platform-specific observed bytes. Controls uses the
ControlRecord type below. All digests serialize as exact32-byte JSON arrays.
Patch order is toolchain.patch, kernel.patch, replay.patch, each under the
repository-owned patches directory. Apply the fixed controls overlay before
building. Receipt<=4 MiB; source files<=8192 and aggregate source<=256 MiB, streamed.
Recompute rather than trust receipt digests at ordinary run entry and after use.

### Exact report/failure structures

Use the earlier Target/Status/Scope/Limits/Config/Error types. `Target` serializes
`rewrite_witness`/`arithmetic_oracle`; every enum uses snake_case. Derive existing
serde and Debug/Clone, Eq/PartialEq where fields permit. Public Option fields
serialize null when not observed, not fabricated zero-success evidence.

```rust
pub type Digest = [u8; 32];
pub struct FileDigest { pub path: String, pub bytes: u64, pub blake3: Digest }
pub struct LibraryLink { pub path: String, pub target: String }
pub struct CompilerRuntimeFile {
    pub path: String, pub canonical_target: String, pub links: Vec<LibraryLink>,
    pub bytes: u64, pub blake3: Digest,
}
pub enum ControlKind { RawDeclaration, CheckerExpr, Condition }
pub struct ControlRecord {
    pub id: String, pub kind: ControlKind,
    pub expected_accept: bool, pub actual_accept: bool,
    pub expected_reason: String, pub actual_reason: String, pub matched: bool,
}
pub struct InventoryTotals {
    pub modules: u64, pub forms: [u64; 8],
    pub eligibility: [u64; 4], pub target_eligibility: [u64; 4],
    pub encoded_name_bytes: u64,
}
pub struct Coverage {
    pub additions: u64, pub covered: u64,
    pub constructor_compares: u64, pub recursor_compares: u64,
    pub skipped: [u64; 3], pub target_covered: u64,
    pub required_roots: u64, pub required_roots_covered: u64,
    pub remaining: u64, pub pending: u64,
    pub constructor_pending: u64, pub recursor_pending: u64,
}
pub enum TargetStatus { NotRun, Compiled, Inventoried, Complete, Failed }
pub enum AuditStatus { NotRun, Partial }
pub struct TargetReport {
    pub target: Target, pub status: TargetStatus,
    pub source: Option<FileDigest>, pub model_artifacts: Vec<FileDigest>,
    pub import_files: u64, pub import_bytes: u64,
    pub import_manifest_blake3: Option<Digest>,
    pub inventory: Option<InventoryTotals>, pub inventory_blake3: Option<Digest>,
    pub coverage: Option<Coverage>, pub coverage_blake3: Option<Digest>,
    pub native_compile_exit: Option<i32>, pub external_exit: Option<i32>,
    pub compile_ms: u64, pub inventory_ms: u64, pub replay_ms: u64,
    pub stdout_bytes: u64, pub stderr_bytes: u64,
    pub stdout_blake3: Option<Digest>, pub stderr_blake3: Option<Digest>,
}
pub struct ToolInfo {
    pub profile: String, pub upstream_commit: String,
    pub batteries_commit: String, pub lean_release: String, pub lean_commit: String,
    pub checker_blake3: Digest, pub compiler_blake3: Digest,
    pub compiler_runtime_manifest_blake3: Digest,
    pub receipt_blake3: Digest, pub pins_blake3: Digest,
    pub known_fixes_blake3: Digest, pub source_manifest_blake3: Digest,
    pub os: String, pub arch: String,
}
pub struct Report {
    pub schema: u16, pub status: Status, pub scope: Scope,
    pub limits: Limits, pub tools: Option<ToolInfo>,
    pub controls: Vec<ControlRecord>, pub controls_blake3: Option<Digest>,
    pub targets: [TargetReport; 2],
    pub axiom_audit: AuditStatus, pub implementation_proof_audit: AuditStatus,
    pub process_spawns: u64, pub timeouts: u64, pub crashes: u64,
    pub protocol_errors: u64, pub elapsed_ms: u64,
    pub artifacts: Vec<FileDigest>,
}
pub struct Failure { pub error: Error, pub partial: Box<Report> }
pub fn run(config: &Config) -> Result<Report, Failure>;
```

Report target order is fixed RewriteWitness then ArithmeticOracle. Model
artifacts<=4 files per target; retained artifact inventory<=16 entries; combined
ToolInfo strings<=4096 bytes. Report file paths are relative evidence labels,
not commands or approval to open arbitrary files. No full closure/source list
is embedded in the final report; use streamed manifests/digests. TargetStatus
advances only after an observed successful phase; failed reports retain actual
partial coverage, without claiming Complete. Axiom/proof audits are NotRun in
this executable's initial profile, since those separate auditors are not run;
the known partial implementation proofs remain documented limitations, not an
invented completed audit result. External Complete does not mean those audits
passed.

Hash typed inventories with domain `vesper.lean4lean42.inventory\0`, schema1
u16LE, target u8, module/constant counts u64LE, then records in ordinal order.
Encode Name as component count u16LE, each component tag u8 and either UTF-8
length u32LE+bytes or numeric u64LE. Module records contain ID u32LE+Name;
constant records contain ID u32LE+Name+moduleID u32LE+form u8+skip u8+owner
count u16LE+owner IDs u32LE. Hash resolved artifact manifests separately under
`vesper.lean4lean42.imports\0` with module/part tags and exact file bytes digests;
this separates deterministic name inventories from host-specific paths.
Artifact part tags are0=base .olean,1=.olean.server,2=.olean.private,3=.ir.
The pinned4.24 loader opportunistically reads server/private parts; Rust hashes
base, server then private when server exists, and any present .ir conservatively
as the resolved artifact envelope. Do not claim every optional .ir was executed.
Reject a private sidecar without its server precursor in this fixed profile.

Coverage hash domain is `vesper.lean4lean42.coverage\0`, schema1 u16LE+target u8,
then each observed skip as tag6 u8+ID u32LE+skip u8, and each coverage event as
tag5 u8+EVENT_SEQ u64LE+ACTION u8+ID count u16LE+IDs u32LE in wire order;
controls domain is `vesper.lean4lean42.controls\0`, schema1 u16LE+count u32LE,
then pinned index u32LE and all ControlRecord fields with UTF-8 lengths u32LE,
boolean u8 and ControlKind u8 in declaration order. Raw per-process stdout and
stderr digests additionally hash their exact bytes incrementally. These hashes
are inspection associations, not proof certificates or executable attestation.

### Selected compiler-runtime library binding

Binding only bin/lean is insufficient for this dynamically linked compiler.
The coordinator's Linux inspection found libInit_shared,
libleanshared_1/libleanshared_2/libleanshared among its loaded dependencies;
the direct lib/lean envelope currently has13 files totaling486,041,402 bytes.
Those are observations, not a fixed successful file count. The candidate
external checker is static; that fact does not eliminate the model compiler's
runtime-library identity obligation.

The fixed recipe enumerates only direct entries under canonical SYSROOT/lib/lean
whose filename ends `.a`, `.lib`, `.dylib`, `.dll`, or `.so` optionally followed
by one or more `.DIGITS` suffixes; it also selects direct SYSROOT/bin/*.dll.
Suffix matching is case-sensitive on Unix and ASCII-case-insensitive on Windows.
There is no recursive traversal. Select matching regular files **and aliases**;
do not silently ignore a selected symlink. Both selected directories must be
real directories under the canonical sysroot; a symlinked selected directory is
explicitly unsupported in this initial recipe. Reject a selected non-file,
broken alias, cycle or target outside the canonical sysroot.

The receipt compiler_runtime_manifest is an ordered Vec<CompilerRuntimeFile>,
sorted by original normalized sysroot-relative path's UTF-8 bytes. Each path
and canonical_target is a normalized relative path without dot/dotdot, <=4096
UTF-8 bytes. A regular file has identical path/canonical_target and an empty
links array. An alias records its ordered read_link chain, at most16 steps,
then the canonical regular target. LibraryLink.path is the normalized relative
path at that step; LibraryLink.target preserves the raw link text (<=4096 UTF-8
bytes, possibly relative with dotdot or absolute). Resolve each step and require
it remain within the canonical sysroot; raw link targets are not permission to
read outside it. No alias target is executed under a changed argv0.

Preflight/check file-count and aggregate byte arithmetic before hashing. Defaults
are128 entries /2 GiB, HARD256 /4 GiB; count every selected path and its complete
target byte length, including repeated content reached through multiple aliases.
Use a<=64 KiB read buffer and check actual length/hash, not just stat size. The
envelope is separate from source_manifest and resolved model/import artifacts:
it has independent counters and does not consume their source/artifact counts.
Include its bounded manifest storage/hash work in setup/run logical accounting.

The sealer records compiler_runtime_manifest and
compiler_runtime_manifest_blake3 in the receipt; ordinary run recomputes both
before tool use and after completion, comparing the entire selected set, link
chains, canonical targets, lengths and digests. Addition/removal/substitution or
changed alias resolution fails ToolIdentity even if bin/lean is unchanged.
ToolInfo carries compiler_runtime_manifest_blake3 as the exact observed envelope
association; report the actual profile and platform rather than a universal
binary/library identity.

Hash domain is the bytes `vesper.lean4lean42.compiler_runtime\0`, then schema1
u16LE and entry count u32LE. For each sorted entry: path u32LE byte length+UTF-8,
canonical_target u32LE length+UTF-8, link count u8, each link's path and raw
target using the same u32LE length+UTF-8 encoding, then bytes u64LE and32-byte
file BLAKE3. This typed hash is independent of JSON serialization whitespace.

Clear ambient loader substitution variables for compiler/tool children,
including LD_PRELOAD/LD_LIBRARY_PATH and applicable DYLD insertion/library path
overrides. Use the controlled empty cwd; Windows loader PATH entries are limited
to the selected tool/bin roots and required OS directories. This is a selected
compiler-library envelope under a controlled invocation, not a proof of the OS
loader, all system libraries, entire toolchain derivation or a hostile host's
execution. Private copied model sources/artifacts continue to reject symlinks;
the explicit library-alias rule does not relax their binding policy.

### Streaming and cancellation rule

Use a bounded stdout queue of at most4 complete frames, plus the reader's one
current bounded frame. **Apply backpressure when full; transient queue fullness
is not a protocol error.** Inventory hashing and artifact inspection can
legitimately delay the consumer. Include those bounded queue buffers in logical
metadata accounting. A blocking sender is permitted only with cancellation-safe
cleanup: kill and reap the child, drop the queue receiver to wake blocked sends,
then join stdout/stderr reader threads. Equivalent explicit cancellation is
acceptable; joining while retaining a full receiver is not. Keep stderr draining
concurrently and record overflow without growing retained buffers.

Enforce the absolute process/phase deadline while consumption or import hashing
is in progress. Capture a frame-completion Instant in the reader before enqueue,
and observed exit time separately from later consumer work; a valid frame read
before the deadline is not retroactively late merely because it waited in a
bounded queue. Clean EOF, terminal and actual process exit are still required.
Hashing uses bounded chunks and deadline checks; no promise is made to interrupt
an arbitrarily stalled filesystem syscall. All decoded frames remain associated
with the one isolated child and expected mode/target/token. No queue/fullness or
deadline workaround may drop frames, skip inventory records or accept a prefix.
