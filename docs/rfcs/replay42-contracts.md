# Independent Lean replay of AST rewrite witnesses (§42): frozen contract

Public parent claim #239 and child claims #240–#242 precede implementation.
The coordinator reviewed this contract, with independent model/API and kernel
acceptance reviews. The preceding Rust foundation #238 has its own CI/merge
status. Source writers begin only after this frozen contract is pushed and
linked from the claims; interface amendments likewise precede dependent edits.

## Ownership and exclusions

| Claim/lane | Exclusive future writes |
|---|---|
| #239 coordinator | this contract, narrow `src/optimize.rs` witness_lean registration, integration report, narrow `BUILD.bazel` timeout entry, narrow model-CI script addition |
| #240 static model | new `lean/RewriteWitness.lean`, model report |
| #242 structured exporter | new `src/optimize/witness_lean.rs`, new `tests/replay42_export.rs`, export report |
| #241 independent kernel/audit acceptance | new `tests/replay42_kernel.rs`, acceptance report |

Reports are `docs/rfcs/replay42-{model,export,acceptance,integration}-report.md`.
No Cargo/dependency, core/numeric/AST library, CLI/main/lib, native, IR/ir_opt,
formatter, allocator, existing fixture or unrelated workflow edits. The sole
script exception is coordinator-owned `scripts/ci/check-lean-models.sh`, adding
only RewriteWitness to its existing explicit model list. Exporter
does not modify the existing Rust checker/producer APIs. The coordinator alone
adds `pub mod witness_lean` under optimize after coherent source exists. No push/PR/
merge is delegated. Compile/test leases are assigned explicitly; no author may
claim an execution performed by another lane or an unexecuted check.

Root refreshed open claims and actual external PR149/151/163/165 file lists;
there is no BUILD overlap. The coordinator's public claim comment precedes its
narrow `test/replay42_kernel` timeout entry. Start with Bazel `long` (900s), all
platforms enabled. Actual measurements may justify a published narrow change to
`eternal` (3600s); no macOS exclusion is inherited from old audit tests. Bazel's
enforcement is not a promise that every worst-case Lean workload finishes in
that time. Root resources already glob the new Lean file; Cargo and the Bazel
metadata watcher discover the new test. The coordinator's narrowly reserved
model-CI script addition is separate from the acceptance lane's direct static
model compilation; script coverage is claimed only after actual execution.

## Trust boundary

Export starts from actual source/candidate Expr values, the actual checked
TypeMap and an actual RewriteCertificate. Rust endpoint/provenance verification
is mandatory before a public export builds a Lean declaration. A second public
helper obtains a genuine certificate from the actual existing Runner/Extractor
path, preserving its cumulative full-gate budget. Neither API changes default
optimization behavior or invokes a process.

Generated Lean proves equality of two mathematical semantic functions in the
bundled model. It does not prove that Rust source admission, the certificate
checker, AST-to-model translation, backends, allocator or whole compiler refine
that model. Binding/association digests are inspection data, not a kernel proof
of the Rust binding or an approval token. Lean compilation and explicit axiom
audit remain required after preparation/rendering; no KernelAccepted or cache
approval field exists.

The bundled static model imports only Std and is checked with Lean 4.24.0. It
contains no sorry, project axiom, native_decide, trust oracle or replacement
semantics selected by a caller. Acceptance compiles the exact bundled model
bytes as module RewriteWitness in a private regular-file root; it does not
silently trust a different same-named module found through ambient LEAN_PATH.

## Public Rust API

All names below are under `vesper::optimize::witness_lean`. Declarations specify exact
Rust signatures/types; implementation bodies follow only after publication.

```rust
use crate::ast::{Expr, TypeMap};
use crate::numeric::IntegerKind;
use crate::optimize::witness::{
    ProofId, RewriteCertificate, TermId, WitnessError, WitnessLimits,
};
use vesper_lean_syntax::Module;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportLimits {
    pub max_declarations: usize,
    pub max_ast_nodes: usize,
    pub max_ast_depth: usize,
    pub max_name_bytes: usize,
    pub max_source_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayScope { MathModelEq }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayManifest {
    pub format_version: u16,
    pub model_version: u16,
    pub schema_version: u16,
    pub arithmetic_version: u16,
    pub module_name: &'static str,
    pub theorem: &'static str,
    pub scope: ReplayScope,
    pub kind: IntegerKind,
    pub input: TermId,
    pub output: TermId,
    pub conclusion: ProofId,
    pub term_count: usize,
    pub proof_count: usize,
    pub declaration_count: usize,
    pub ast_node_count: usize,
    pub ast_depth: usize,
    pub name_bytes: usize,
    pub source_bytes_upper_bound: usize,
    pub model_blake3: [u8; 32],
    pub certificate_blake3: [u8; 32],
    pub binding_blake3: [u8; 32],
}

// Fields private: generated AST + preflight/manifest information. Not Clone/Copy.
pub struct PreparedLean { /* ... */ }
impl PreparedLean {
    pub fn module(&self) -> &Module;
    pub fn manifest(&self) -> &ReplayManifest;
    pub fn into_module(self) -> Module;
    pub fn render(self) -> Result<RenderedLean, ExportError>;
}

pub struct RenderedLean {
    pub source: String,
    pub source_blake3: [u8; 32],
    pub manifest: ReplayManifest,
}

pub struct PreparedReplay {
    pub candidate: Expr,
    pub certificate: RewriteCertificate,
    pub lean: PreparedLean,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportError {
    InvalidLimits,
    Witness(WitnessError),
    Limit,
    Allocation,
    LeanSyntax,
}

pub fn export_rewrite(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    certificate: &RewriteCertificate,
    witness_limits: WitnessLimits,
    export_limits: ExportLimits,
) -> Result<PreparedLean, ExportError>;

pub fn prepare_rewrite(
    source: &Expr,
    types: &TypeMap,
    witness_limits: WitnessLimits,
    export_limits: ExportLimits,
) -> Result<Option<PreparedReplay>, ExportError>;

pub fn model_source() -> &'static str;
pub fn model_audit_roots() -> &'static [&'static str];
```

ExportLimits implements Default and has associated constant HARD. ExportError
implements Display/Error, exposing WitnessError as the source of Witness.
Static syntax errors map to LeanSyntax without recursively formatting caller
data. Existing error enums remain unchanged. The module accessor permits
independent structural inspection and a caller-owned clone for adversarial AST
mutation; that explicit caller clone is outside exporter-request allocation
accounting. into_module consumes the prepared wrapper and transfers the AST.
Neither access path confers semantic approval on a subsequently modified AST.

format_version/model_version are 1, and schema/arithmetic versions retain the
actual supported certificate versions (currently 1). The fixed module_name is
`ReplayGenerated`, intended for one artifact per generated module/file. The
fixed final theorem is `Vesper.ReplayWitness.replay`; scope is MathModelEq.
There is no caller-supplied namespace, module import, theorem name or tactic.
Names/digests/counts are read-only observations of this export, not trusted
values a consumer can substitute for checking the actual artifact.

model_source returns the exact static file bytes through include_str; this is
the hand-authored fixed model, not generated or reparsed Lean source. The model
import is fixed RewriteWitness. Public export does not access filesystem or
spawn Lean. RenderedLean.source_blake3 hashes its actual returned UTF-8 bytes;
manifest.model_blake3 hashes the bundled model bytes. Recomputing these hashes
does not itself constitute a kernel or axiom-policy result.

## Exact static Lean declarations

Namespace is `Vesper.RewriteWitness`. The model owner provides the following
types/functions with this parameter order and no extra typeclass assumptions:

* `Kind` has constructors `u8`, `u32`, `u64`, `i64`; `Kind.width` is 8,32,64,64.
* `Value (k : Kind)` is a **structure**, with `bits : BitVec k.width`, deriving
  DecidableEq. It is not an abbreviation: Value u64 and Value i64 remain
  distinct nominal types despite equal widths.
* `Env (k)` abbreviates `Nat → Value k`; `Sem (k)` abbreviates
  `Env k → Option (Value k)`.
* `toInt {k} (v : Value k) : Int` uses unsigned toNat or signed toInt as appropriate.
* `Fits (k : Kind) (n : Int) : Prop` means inclusive native bounds. An explicit
  computable `Decidable (Fits k n)` instance uses no classical choice.
* `literalValue (k) (n : Int) (h : Fits k n) : Value k` encodes exact fitting n.
* `const (k) (n) (h) : Sem k` returns some literalValue; `var (k) (id : Nat)`
  returns some of the environment's value at id. `zero (k)` and `one (k)` are
  const k 0/1 with constructively proved Fits.
* `IntOp` has constructors `add`, `sub`, `mul`.
* `checkedValue {k} (op) (a b : Value k) : Option (Value k)` computes exact Int
  arithmetic and its native range guard. `wrappingValue {k} (op) (a b)` uses
  the corresponding BitVec modular operation and returns Value k.
* `checked {k} (op : IntOp) (a b : Sem k) : Sem k` and `wrapping {k} (op) (a b)`
  strictly evaluate their Option-valued operands. There is one none failure.
* `Total {k} (x : Sem k)` means `∀ env, ∃ v, x env = some v`.

The following are theorem declarations, including all support audit roots:

* `value_fits {k} (v : Value k) : Fits k (toInt v)`.
* `literal_exact (k) (n : Int) (h : Fits k n) : toInt (literalValue k n h) = n`.
* `value_toInt_injective {k} {a b : Value k} (h : toInt a = toInt b) : a = b`.
* `literal_roundtrip {k} (v : Value k) : literalValue k (toInt v) (value_fits v) = v`.
* `total_const (k) (n) (h) : Total (const k n h)`; `total_var (k) (id) : Total (var k id)`.
* `total_of_eq {k} {x y : Sem k} (h : x = y) (hy : Total y) : Total x`.
* `congr_checked {k} {a a' b b' : Sem k} (op : IntOp)
  (ha : a = a') (hb : b = b') : checked op a b = checked op a' b'`.
* `congr_wrapping` has the identical binders/order with wrapping endpoints.

Do not rely on a global congrArg₂ name: it is unavailable in pinned Std4.24.
The model proves its two explicit congruence lemmas constructively.

## Exact 24-rule theorem catalog

Every theorem below starts with implicit `{k : Kind}`. Explicit x/y/z are Sem k.
The endpoints are the existing Rust catalog, with these exact names:

|ID|Theorem|Explicit arguments|Equality|
|---|---|---|---|
|1|r01_add_zero|x|checked .add x (zero k) = x|
|2|r02_zero_add|x|checked .add (zero k) x = x|
|3|r03_sub_zero|x|checked .sub x (zero k) = x|
|4|r04_mul_one|x|checked .mul x (one k) = x|
|5|r05_one_mul|x|checked .mul (one k) x = x|
|6|r06_mul_zero|x, hx:Total x|checked .mul x (zero k) = zero k|
|7|r07_zero_mul|x, hx:Total x|checked .mul (zero k) x = zero k|
|8|r08_self_sub|x, hx:Total x|checked .sub x x = zero k|
|9|r09_wadd_zero|x|wrapping .add x (zero k) = x|
|10|r10_wsub_zero|x|wrapping .sub x (zero k) = x|
|11|r11_wmul_one|x|wrapping .mul x (one k) = x|
|12|r12_wadd_comm|x,y|wrapping .add x y = wrapping .add y x|
|13|r13_wmul_comm|x,y|wrapping .mul x y = wrapping .mul y x|
|14|r14_wadd_assoc|x,y,z|wrapping .add (wrapping .add x y) z = wrapping .add x (wrapping .add y z)|
|15|r15_wmul_assoc|x,y,z|wrapping .mul (wrapping .mul x y) z = wrapping .mul x (wrapping .mul y z)|
|16|r16_wadd_cancel|x,y,hy:Total y|wrapping .sub (wrapping .add x y) y = x|
|17|r17_wmul_zero|x,hx:Total x|wrapping .mul x (zero k) = zero k|
|18|r18_wself_sub|x,hx:Total x|wrapping .sub x x = zero k|
|19|r19_checked_add_constants|a,b,r,ha,hb,hr,h|checked .add (const k a ha) (const k b hb) = const k r hr|
|20|r20_checked_sub_constants|a,b,r,ha,hb,hr,h|checked .sub (const k a ha) (const k b hb) = const k r hr|
|21|r21_checked_mul_constants|a,b,r,ha,hb,hr,h|checked .mul (const k a ha) (const k b hb) = const k r hr|
|22|r22_wrapping_add_constants|a,b,r,ha,hb,hr,h|wrapping .add (const k a ha) (const k b hb) = const k r hr|
|23|r23_wrapping_sub_constants|a,b,r,ha,hb,hr,h|wrapping .sub (const k a ha) (const k b hb) = const k r hr|
|24|r24_wrapping_mul_constants|a,b,r,ha,hb,hr,h|wrapping .mul (const k a ha) (const k b hb) = const k r hr|

For 19–24, a/b/r are Int, followed by ha:Fits k a, hb:Fits k b, hr:Fits k r.
The final explicit h is a **closed value-level equality**, not function equality:
for checked rules it is `checkedValue op (literalValue k a ha)
(literalValue k b hb) = some (literalValue k r hr)`; for wrapping rules it is
`wrappingValue op (literalValue k a ha) (literalValue k b hb) = literalValue k r hr`.
The op is fixed by the lemma name, not another explicit argument. Export uses
fixed ByDecide nodes for ha/hb/hr/h at the declared closed kind and actual
constants. It never assumes h because Rust computed a matching value. Full
U64 products and signed MIN corners are therefore independently checked.

Partial operands are intentional: wrapping commutation/association lemmas cover
arbitrary Option-valued Sem, not only Total operands. Erasing/canceling rules
retain their exact Total premise. Unknown/future rule IDs have no export case.

## Structured declaration lowering

Import only the fixed model. Generated namespace is Vesper.ReplayWitness. Emit
all terms, then all proof records, then final theorem in existing topological
order, including unused records already validated by Rust. Each term definition
`n0`, `n1`, … has result `Sem` of **its own Term.kind**, not a substituted root
kind; bodies refer to earlier named definitions. Checked/wrapping applications
carry that same explicit kind argument. U64/I64 must never collapse because
their BitVec widths coincide. A mixed-kind mutated declaration must fail Lean
typing instead of being silently coerced by exporter code.

Const definitions apply const with an actual signed Int literal and fixed
ByDecide Fits proof. InputVar definitions apply var with their numeric source
ID; source names never enter generated Lean identifiers. Negative integers use
Unary Neg over a bounded natural magnitude with explicit Int typing; do not
truncate/cast an unsigned decimal through i64. Zero/one model aliases remain
definitionally compatible with exported fitting Const definitions.

Every proof record gets a named theorem `p0`, `p1`, … with proposition equality
of its two named semantic definitions. Apply the catalog lemma to the exact
substitution definitions. Refl uses Rfl; congruence uses the model's matching
two-child lemma and earlier p references; symmetry/transitivity use fixed
Eq.symm/Eq.trans references. For guarded rules, inline total_const/total_var
for an actual leaf, or total_of_eq with the earlier guard theorem (symmetrized
when required) and inline leaf totality. There are no per-leaf Total theorem
declarations. All proof/DAG references stay references: never expand a prior
term/proof tree, flatten an egg explanation or turn DAG sharing into a let/CSE
evaluation claim.

The final theorem `replay` states equality of certificate.input/output named
definitions, with body referencing certificate.conclusion. Term/proof symbols
use bounded decimal indices and static ASCII prefixes. Model/standard names
are explicit fixed rooted Names; declaration names use validated Ident/Name.
Every generated declaration/expression uses vesper-lean-syntax nodes. No raw
source, caller tactic, generated theorem reparsing or formatted Lean fragment
is permitted. The existing library supplies all required syntax; no extension
is currently authorized or needed.

## Resource contract and single serialization

All ExportLimits fields are positive and at most HARD; invalid limits fail
before working allocation. Default and HARD values are:

|Field|Default|HARD|
|---|---:|---:|
|max_declarations|16384|65536|
|max_ast_nodes|262144|1048576|
|max_ast_depth|32|64|
|max_name_bytes|1MiB|4MiB|
|max_source_bytes|4MiB|16MiB|
|max_metadata_bytes|64MiB|256MiB|
|max_work|8000000|32000000|

The export budget is separate from WitnessLimits and is explicit in both APIs.
It remains cumulative across export planning, hashes, AST allocation/building
and consuming render; PreparedLean retains the move-only budget state needed
by render. prepare_rewrite never resets or bypasses the producer's witness
budget: call actual propose_rewrite, then full verify_with_budget on its
returned token, then the exporter shared private lowering path. Standalone
export_rewrite calls full verify_rewrite first under its WitnessLimits. The
shared private lowering path is not a public unchecked certificate entrypoint.
Barrier/Unchanged returns None; a producer/gate error does not become a partial
fold export. Default optimizer does not call either replay API.

Preflight a closed lowering plan before cloning/allocating Lean AST names,
boxes, vectors, declarations or numeric strings. Count every node, binder,
argument, name component/byte, declaration including the namespace wrapper,
and generated decimal byte. Use checked sums/products and bounded decimal digit
counts before allocation. Topological dependency depth does not become syntax
tree depth because references remain names. The exact declaration count is
terms + proofs + final + one namespace wrapper. Import metadata is charged too.
Different resource limits may conservatively refuse a witness even if its Rust
checker accepts it; no unconditional same-budget export guarantee is made.

Charge represented certificate capacities, generated AST allocations, the
rendered buffer, and conservative serializer temporary/copy envelopes as
logical metadata/work. Fallibly reserve vectors/strings before growth; charge
full requested replacement capacities cumulatively. Hashing actual binding
names/model/certificate/source bytes consumes work. Existing caller AST/TypeMap
storage is not copied wholesale. Bounds concern represented/requested data,
not allocator overreservation, kernel memory, process RSS, physical OOM or OS
termination. Returning candidate/certificate from the source helper moves
producer-owned data; it does not clone the original source or certificate.

Compute a structural upper bound for the existing printer's **closed emitted
subset** before serialization, including UTF-8 quotation delimiters, qualified
name separators/root prefixes, whitespace/newlines, negative literals and
fixed proof syntax. Account for its bounded-depth formatting temporaries and
copies. Do not render to discover a length. PreparedLean::render consumes the
wrapper, invokes Module::render exactly once, checks actual length against its
preflight bound/limit, hashes returned bytes and returns RenderedLean. Err
returns no string/module/file prefix to a caller; no external file is written.
No additional caller buffer has transactional mutation semantics to maintain.
into_module transfers the AST instead; subsequent caller printing/cloning is
the caller's explicit operation and is not silently counted as exporter work.

The private implementation may expose bounded test-only/parent-visible lowering
helpers for structural adversarial tests, but public functions always perform
the actual Rust gate. Such helpers must retain index/topology/resource checks
and preserve every declared Kind; they cannot be used by the normal optimizer
as an unchecked path. No public skip-verification/trust flag is introduced.

## Association digest schema

These digests identify data; they do not authenticate it or prove semantics.
Use the existing BLAKE3 dependency, with no formatted AST/Lean serialization
as a hashing intermediate. Scalars use fixed little-endian widths; byte strings
carry u64 byte lengths. Hash incrementally under the export work budget.

certificate_blake3 hashes domain bytes `vesper.replay.certificate/1` plus zero,
then schema/arithmetic u16, term/proof u64 lengths, input/output/conclusion u32,
then every term/proof in order. Each term starts with its kind tag, then its
node tag and payload. Kind tags u8/u32/u64/i64 are 0/1/2/3; IntOp
add/sub/mul are 0/1/2. Term tags Const/InputVar/Checked/Wrapping are 0/1/2/3,
followed by i128 value, u32 variable, or op u8 and two u32 children respectively.
Proofs carry from/to u32, then Reason tags Refl/Rule/Congruence/Symmetry/
Transitivity 0/1/2/3/4. Rule carries rule u16, substitution len u8 and all three
binding u32 values, then guard tag0 or tag1+u32. Other reasons carry their fixed
proof-ID fields in declaration order. Vec capacity is resource accounting,
not semantic digest input.

binding_blake3 hashes domain bytes `vesper.replay.binding/1` plus zero, then
schema/arithmetic u16 versions, then two bounded preorder AST occurrence
transcripts: role tag0 source, then role tag1 candidate. Each role tag precedes
its u64 Expr occurrence count. Each admitted value occurrence
carries a tag (Const0, Name1, CheckedAdd2/Sub3/Mul4, WrappingAdd5/Sub6/Mul7),
span start/end as checked u64, and the actual TypeMap lookup encoding:
closed-kind tags1..4 (u8/u32/u64/i64 respectively) or tag5+i128 for contextual
IntLiteral metadata. Const then
carries its actual i128 value; Name carries actual UTF-8 bytes with u64 length.
Binary children follow left then right. Wrapping nodes additionally carry
their original callee Name bytes, span start/end u64 and lookup tag0 (absent),
then their two arguments in order. The callee counts as an Expr occurrence in
the leading count; its fields are encoded in that wrapping record rather than
as a value node. Validated empty generic types/Pass::In/nonexplicit conventions
are fixed by the node tag and need no ambiguous textual encoding. Missing/
unsupported value metadata cannot reach this stage. Never use an IntLiteral
metadata payload as the actual literal value.

Manifest versions/counts/root IDs state what these digests describe. A changed
candidate/certificate/model/source artifact requires fresh association; a
consumer cannot turn unchanged manifest bytes into approval of modified data.
The independently hashed RenderedLean source and actual compile/audit object
inventory remain separate observations. Kernel replay has no expected-Rust-AST
argument and therefore cannot itself establish the binding digest's meaning.
Only these actual looked-up metadata entries are encoded: unrelated TypeMap
entries, map iteration order, Debug output and platform-native integer layouts
are never hash inputs. Precharge the bounded transcript/model/certificate/source
byte counts and traversal work before each hashing pass. Source and candidate
roles and their exact span associations are retained explicitly.

## Exact model audit roots and independent acceptance

model_audit_roots returns exactly the 24 rXX names from the catalog, followed by
these nine, each fully prefixed with `Vesper.RewriteWitness.`:
value_fits, literal_exact, value_toInt_injective, literal_roundtrip, total_const,
total_var, total_of_eq, congr_checked, congr_wrapping. There are 33 model theorem
roots. Adding final `Vesper.ReplayWitness.replay` gives 34, below the existing
auditor's 128-name cap. Audit all catalog/support roots, not only the subset
reachable from one successful example.

Use pinned Lean4.24.0. Existing proof-audit CLI is sufficient; no CLI/main/auditor
edit is authorized. Tests use the existing Cargo binary path discovery and may
configure VESPER_LEAN, or use lean on PATH when it is unset. A missing compiler
or any version other than Lean4.24.0 is a hard test failure, including absent
default tooling; there is no skip path. Actual completion requires coordinator
execution with the pinned compiler available.

The independent test lane uses real public prepare_rewrite results, rechecks
candidate/certificate with the independent Rust verifier, inspects structural
AST references/counts and renders once. Compile the bundled static model and
generated module as ordinary files in a private temporary root. Do not pass
a runfiles symlink to the auditor. Actual compilation subprocesses use 300s
limits; outer audit process guards use 360s and kill/reap on timeout. Tests have
a finite declared case matrix; target-wide Bazel timeout is separately enforced
as above. No universal Lean elaboration/kernel-time bound is promised.

Positive cases cover actual checked identities, all six folds, prior-totality
support, signed/U64 endpoints and real original-callee origins. Kernel-check
exporter mapping for all 24 rules/four kinds with bounded fixtures as well as
genuine producer artifacts; hand-built DTO coverage does not replace actual
producer-to-export evidence. Four bounded per-kind aggregate certificates may
cover all catalog rules without starting 96 separate Lean processes; all their
unused proof records remain validated and exported. Test all equality reasons and DAG sharing without
flattening. Pure model tests and actual-runtime Rust binding tests have distinct
claims; no executable Lean/Rust differential oracle is asserted by this chunk.

Default audit allowlist is empty and is expected to reject closures that use
standard axioms. Positive policy explicitly allows exactly sorted
`[Quot.sound, propext]`; every requested root's actual axioms must be a subset,
not necessarily both. No Classical.choice, sorryAx or new project axiom is
allowed. Model proofs avoid known Std helper paths whose closure introduces
Classical.choice, and the actual complete catalog audit decides acceptance.
Known forbidden-shortcut traps include BitVec.toInt_le, BitVec.le_toInt,
BitVec.ofInt_toInt, BitVec.mul_one and BitVec.one_mul in pinned Std. Constructive
range/literal roundtrip proofs and own mul-one identities use unsigned natural
bounds, explicit case splits and toNat injectivity/modular arithmetic instead;
ordinary broad simp must not silently reintroduce those choice dependencies.

Use the existing auditor JSON correctly: success has schema1/status audited,
accepted true, kernel_replayed true, cache_eligible false and module
ReplayGenerated. Require requested theorem names exactly once, the exact
allowlist and no forbidden axioms. Policy rejection is nonzero/status audited,
accepted false, kernel_replayed true with the actual forbidden set. Parse/import/
kernel failure is status error with error.kind audit_failed and has no accepted
field. Argument errors may have no JSON. Do not impose one rejection shape on
all failure categories or invent cache eligibility.

Independent negatives mutate the emitted AST before its one serialization:
change the actual folded output constant 5 to 6 with proof unchanged; change a
declared output kind U64 to I64; corrupt proof references/guard evidence. Check
real Lean rejection, not merely a renamed theorem or changed manifest. A
renamed InputVar may still satisfy a valid identity and is not a sufficient
negative. Separately build a bounded AST theorem using Classical.choice to
test default/restricted policy rejection and explicit-choice fixture acceptance;
no raw axiom declaration or formatted source string is needed. Artifact changes
that remain valid equalities must change independently recomputed association/
inventory hashes; audit acceptance alone does not claim Rust binding.

Exporter tests cover malformed DTO refusal before Lean AST allocation,
canonical spans/provenance retained from the existing checker, every export
limit, exact boundary/depth/name/source bounds, high-sharing term/proof graphs,
all declared-kind preservation, manifest/digest association, and strict fresh-
output failure. Acceptance tests read exported AST via public accessors; no
private fixture backdoor or KernelAccepted state is introduced.

Coordinator records actual Rust, Lean kernel, axiom audit, strict Clippy,
formatting/diff and available platform evidence after implementations are
combined. Read-only /tmp API probes informed this contract but are not a full
model/exporter/acceptance execution claim. Root publication and child claims
precede source/test implementation; actual results are recorded separately.
