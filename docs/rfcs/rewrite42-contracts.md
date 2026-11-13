# Bounded AST rewrite witnesses (§42): frozen contract

Public parent claim #230 precedes implementation. The coordinator checked open
claims and actual external PR files for overlapping optimizer paths. The coordinator has reviewed this contract. Source/test work starts only after
this frozen version is pushed and linked from the public claims. Interface
amendments likewise precede dependent edits.

## Ownership and scope

| Claim | Lane | Exclusive implementation writes after publication |
|---|---|---|
|230|Coordinator|`src/optimize.rs` gate/module registration, this contract, integration/review report|
|232|Checker|new `src/optimize/witness.rs`, `tests/rewrite_core.rs`, checker report|
|233|Producer|new `src/optimize/certificate.rs` including its unit tests, producer report|
|234|Independent adversarial|new `src/optimize/adversarial_tests.rs`, `tests/rewrite_witness.rs`, adversarial report|
|235|Independent runtime/CLI|new `tests/rewrite_runtime.rs`, `tests/rewrite_cli.rs`, `scripts/check-rewrite-witness.py`, acceptance report|

Reports are `docs/rfcs/rewrite42-{checker,producer,adversarial,acceptance,integration}-report.md`.
Coordinator exposes `pub mod witness`, keeps `mod certificate` private, and
registers the private adversarial test module after its source exists. External
public-checker tests use `vesper::optimize::witness`; private adversarial tests
can corrupt genuine producer output without introducing a public trust hook.
No Cargo/dependency, core AST/numeric, main/lib, native, IR/ir_opt, formatter,
allocator, BUILD/workflow, existing fixture or unrelated test edits. External
formatter claim231 is acknowledged and does not include these new Rust tests.

Writers initially author and rustfmt only. Actual full compiler builds/tests
are coordinator-only in one stable integration worktree/cache to avoid copying
large compiler dependencies into four caches. Writers wait for coherent shared
source/registration before runtime verification; the coordinator records actual
commands and relays outcomes, and may grant an explicit exclusive build-cache
lease later. No worker may claim an unexecuted test or build. No push/PR/merge is
delegated. Optional Lean models are deferred; any later generated Lean uses the
existing Lean AST seam, never formatted source strings.

The scope is the existing AST optimizer's pure integer islands. The actual egg
Runner/Extractor still proposes candidates; an independent Rust checker gates
every expression replacement, including the six existing direct constant-fold
cases. There is no PlaceIR/effect optimization, SIMD, handler fusion, @fip
optimization, backend validation, self-hosting/DDC or formal refinement claim.
The existing Type::Infer let-annotation preservation logic remains separately
owned by the coordinator; it is not an algebraic expression replacement.

## Public certificate DTO and checker API

The following declarations specify Rust types, not a serialized wire format.
Field visibility intentionally permits malformed certificates in tests; no DTO
constructor grants trust. TermId/ProofId are indices into this certificate only,
never egg e-class IDs, AST spans or semantic authorities.

```rust
use crate::ast::{Expr, TypeMap};
use crate::numeric::IntegerKind;

pub type TermId = u32;
pub type ProofId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntOp { Add, Sub, Mul }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermNode {
    Const(i128),
    InputVar(u32),
    Checked { op: IntOp, children: [TermId; 2] },
    Wrapping { op: IntOp, children: [TermId; 2] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    pub kind: IntegerKind,
    pub node: TermNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Substitution {
    pub bindings: [TermId; 3], // x, y, z in that order
    pub len: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Refl,
    Rule {
        rule: u16,
        substitution: Substitution,
        guard: Option<ProofId>,
    },
    Congruence { children: [ProofId; 2] },
    Symmetry { proof: ProofId },
    Transitivity { first: ProofId, second: ProofId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Equality {
    pub from: TermId,
    pub to: TermId,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteCertificate {
    pub schema_version: u16,
    pub arithmetic_version: u16,
    pub terms: Vec<Term>,
    pub proofs: Vec<Equality>,
    pub input: TermId,
    pub output: TermId,
    pub conclusion: ProofId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WitnessLimits {
    pub max_terms: usize,
    pub max_proofs: usize,
    pub max_input_nodes: usize,
    pub max_output_nodes: usize,
    pub max_depth: usize,
    pub max_variables: usize,
    pub max_name_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessError {
    InvalidLimits,
    Limit,
    Allocation,
    UnsupportedAst,
    MissingType,
    TypeMismatch,
    ConstantOutOfRange,
    UnknownVariable,
    InvalidIntrinsicOrigin,
    UnsupportedVersion,
    InvalidTermIndex,
    NonTopologicalTerm,
    InvalidProofIndex,
    NonTopologicalProof,
    UnknownRule,
    InvalidSubstitution,
    InvalidRuleInstance,
    UnprovenTotality,
    InvalidEquality,
    InputMismatch,
    OutputMismatch,
}

pub fn verify_rewrite(
    source: &Expr,
    candidate: &Expr,
    types: &TypeMap,
    certificate: &RewriteCertificate,
    limits: WitnessLimits,
) -> Result<(), WitnessError>;
```

WitnessLimits implements Default and has associated constant HARD. WitnessError
implements Display and std::error::Error without payload strings or recursive
diagnostic formatting. Schema/arithmetic versions are both 1; arithmetic version
1 identifies `vesper.arith.checked/1`. Every other version is refused. This
profile adds no serde/parser or persistent certificate compatibility promise.

## Admission and exact AST binding

The caller supplies the actual already-checked AST and its matching TypeMap;
certificates cannot supply authoritative types, variable names or intrinsic
resolution. The source root and every nonliteral source value node must have an
exact closed `Type::Named` with empty arguments, one of u8/u32/u64/i64. Every
admitted node has that same IntegerKind. A literal leaf may inherit this closed
kind when its actual AST i128 value fits and its TypeMap entry is either that
same closed kind or `Type::IntLiteral(_)`. The IntLiteral payload is contextual
metadata, not value authority: it need not equal the actual AST literal value.
All numerical proof constants bind the actual AST i128 value. Missing entries,
Infer, rows, generic/noninteger/mismatched closed kinds fail closed even at
literal leaves. A literal root with only IntLiteral metadata is not admitted;
no default width is invented. `check::integer` alone is insufficient because it
accepts Infer. Literal values are never truncated into range.

Admitted source nodes are only integer literals, names, binary +/−/*, and exact
wrapping_add/sub/mul intrinsic calls. An intrinsic source call has a Name callee,
empty generic types, exactly two arguments, Pass::In with explicit==false for
both arguments, and no typed value entry at its actual original callee span.
Only the source callee's own checked resolution evidence establishes this;
absence at a synthetic span does not. The callee Name is not an integer value
node requiring its own integer type entry. Calls to typed local/global values,
shadowed intrinsic names, division, negation, field/index access, handlers,
effects, binding/control flow and every other ExprKind are barriers.

Source-derived variable IDs are assigned deterministically by first left-to-right
occurrence of each exact name within the island. All source occurrences of a
name must have the same admitted kind. No binding construct occurs inside an
island, so this is one lexical context. Every certificate InputVar must refer to
that source inventory; a certificate cannot introduce an unbound symbol or
declare two different names equivalent. Identifier comparisons and bytes are
charged; the certificate carries no name strings of its own.

Bind the declared input term to every actual source occurrence, and the output
term to every actual candidate occurrence, using iterative bounded traversal.
Compare literal value, variable identity, checked/wrapping operator, arity and
kind exactly; ignore nonsemantic expression spans except the origin policy
below. No root hash, e-class membership, textual encoding or candidate-provided
type map substitutes for these comparisons. All certificate terms and proofs,
including unused records, undergo index/type/rule/resource validation.

The source binding records every admitted original intrinsic callee Name+span
by operator. A candidate Wrapping node must copy one of those actual same-op
callee templates, including its original name and span, with unchanged call
argument conventions and empty generics. A candidate cannot introduce a
wrapping operator absent from the source inventory. The safe builder may choose
the first left-to-right template per operator; verification accepts any exact
admitted same-op template. This prevents a mathematical equality from installing
a shadowed AST call and avoids trusting synthetic empty callee spans. Candidate
root span equals source root span. Other generated value-node spans may use the
source root span, with the admitted common kind; they do not establish authority.

The coordinator attempts a maximal admitted pure island before recursively
rewriting its children. The input used for admission/witness verification is
therefore unchanged, with actual source spans/templates. For a non-admitted
barrier, traversal may optimize independently admitted child islands using the
same rule. For an admitted island, producer refusal/unavailable witness/checker
failure leaves that entire island unchanged, without retrying its children as
an unverified alternate path. Traversal and admission perform no speculative
AST mutation or whole-subtree clone before limits are checked.

## Topological terms and equality judgments

The term vector is immutable during checking, nonempty and bounded. Every child
index must be strictly less than its parent index. All nodes have the admitted
common kind; constants fit it; all InputVar references are source-bound. Check
depth bottom-up with checked arithmetic. The DAG representation does not imply
that evaluating a shared child happens only once: semantics expands ordinary
strict AST occurrences. There is no let/CSE/effect-sharing interpretation.

Proofs are immutable, nonempty and bounded. Endpoints must be existing terms;
every cited proof/guard index is strictly earlier than its citing step. Reject
forward references and self-supporting cycles before evaluating their reasons.
The conclusion must have exactly certificate.input and certificate.output as
its ordered endpoints. No trusted union-find/canonicalization/e-class oracle is
used by the checker. Reasons are checked as follows:

* Refl requires shallow typed Term equality, including exact child IDs for
  operators and exact literal/InputVar payload for leaves.
* Rule checks its closed ID, fixed pattern, exact endpoint shapes and explicit
  substitution. Repeated pattern variables require identical TermIds, not merely
  asserted e-class membership. Substitution length must equal rule arity; unused
  binding array elements must be zero; every used binding is an existing term.
* Congruence requires the same checked/wrapping family, operator and kind at both
  endpoints. Its two earlier proofs connect corresponding child IDs in order.
  Leaves cannot use this two-child reason.
* Symmetry reverses the exact endpoints of one earlier proof.
* Transitivity connects first.from to second.to only when first.to==second.from.

Distinct structurally equal terms may be connected with explicit leaf Refl and
Congruence steps, or the producer may intern them. Producer interning is a
space optimization, not trusted semantic equality. Each rule/reason examines a
fixed amount of structure; AST endpoint binding and resource checks are charged
separately. No recursive full-term equality or proof expansion is required.

## Closed rule IDs and arithmetic semantics

All Rule reasons point in the forward direction below. Backward application
requires an earlier valid forward rule equality followed by Symmetry, including
its genuine guard. `C(op,a,b)` denotes checked arithmetic and `W(op,a,b)` denotes
wrapping arithmetic at the common kind. 0/1 are exact in-range typed constants.

| ID | Stable name | Forward equality | Variables | Guarded term |
|---|---|---|---|---|
|1|add-zero|C(+,x,0)=x|x|none|
|2|zero-add|C(+,0,x)=x|x|none|
|3|sub-zero|C(−,x,0)=x|x|none|
|4|mul-one|C(*,x,1)=x|x|none|
|5|one-mul|C(*,1,x)=x|x|none|
|6|mul-zero|C(*,x,0)=0|x|x|
|7|zero-mul|C(*,0,x)=0|x|x|
|8|self-sub|C(−,x,x)=0|x|x|
|9|wadd-zero|W(+,x,0)=x|x|none|
|10|wsub-zero|W(−,x,0)=x|x|none|
|11|wmul-one|W(*,x,1)=x|x|none|
|12|wadd-comm|W(+,x,y)=W(+,y,x)|x,y|none|
|13|wmul-comm|W(*,x,y)=W(*,y,x)|x,y|none|
|14|wadd-assoc|W(+,W(+,x,y),z)=W(+,x,W(+,y,z))|x,y,z|none|
|15|wmul-assoc|W(*,W(*,x,y),z)=W(*,x,W(*,y,z))|x,y,z|none|
|16|wadd-cancel|W(−,W(+,x,y),y)=x|x,y|y|
|17|wmul-zero|W(*,x,0)=0|x|x|
|18|wself-sub|W(−,x,x)=0|x|x|
|19|checked-add-constants|C(+,x,y)=Const(result)|x,y constants|none|
|20|checked-sub-constants|C(−,x,y)=Const(result)|x,y constants|none|
|21|checked-mul-constants|C(*,x,y)=Const(result)|x,y constants|none|
|22|wrapping-add-constants|W(+,x,y)=Const(result)|x,y constants|none|
|23|wrapping-sub-constants|W(−,x,y)=Const(result)|x,y constants|none|
|24|wrapping-mul-constants|W(*,x,y)=Const(result)|x,y constants|none|

Every other u16 rule ID is UnknownRule. Checked folding must compute the exact
result and succeed in range; overflowing constants cannot fold to wrapped
values. Multiplication must use checked i128 multiplication or equivalent safe
wide arithmetic: U64 products can exceed i128::MAX. Wrapping folds compute the
correct modulo 2^bits result, with signed I64 interpretation where appropriate;
no debug/release overflow difference is permitted. No checked commutation,
reassociation, cancellation or bitwise/future rule is silently added.

The six erasing rules (6,7,8,16,17,18) require their actual substituted erased
term to be total. Accept only (a) an exact source-bound InputVar or in-range
Const leaf, or (b) an earlier fully validated equality with that actual erased
TermId at one endpoint and an in-range Const/source-bound InputVar leaf of the
same kind at the other endpoint. Either proof orientation is valid. A supplied
guard must actually establish this fact; an unguarded rule cannot carry an
irrelevant guard. A current/later proof, certificate boolean, e-class leaf or
unchecked analysis fact is insufficient. More general interval/totality
analysis is deferred; unavailable guard proof causes producer fallback.

The semantic domain of an island is deterministic strict evaluation to an
in-range value or the same empty abortive ArithOverflow failure. Names denote
stable in-range copied integer values. Wrapping operators themselves never
raise, but their operands can. The wrapping commutation/association rules remain
sound in this restricted domain: no other effects, failure payloads, handlers
or observable intermediate state occur inside it. They do not license moving
arbitrary effectful AST nodes. Source spans, fuel and cost are not semantic
values. Checker rules are audited Rust semantic cases, not claimed Lean lemmas.

## Bounded resources

All WitnessLimits fields must be positive and no larger than their HARD field.
Invalid configurations fail before working allocation. Checked arithmetic
protects every size/count/byte/work sum and product.

| Resource | Default | HARD |
|---|---:|---:|
|terms|4096|16384|
|proofs|8192|32768|
|input AST occurrences|512|2048|
|output AST occurrences|1024|4096|
|term/source/output depth|64|128|
|source variables|128|512|
|name bytes|16384|65536|
|metadata/requested allocation bytes|4MiB|16MiB|
|logical work|2000000|8000000|

AST occurrence counts include intrinsic callee Expr nodes. Name bytes cover
names actually scanned/copied, including intrinsic names and repeated expanded
output occurrences. The checker/producer charge comparisons, scans, proof-path
walks, term/proof processing, repeated DAG visits, initialized bookkeeping and
requested copies. Work is a conservative logical accounting unit, not CPU
instructions or OS time. Reserve fallibly before growing vectors; charge full
requested replacement capacity cumulatively rather than only net growth.
Count certificate representation and working structures in logical metadata
accounting even when the certificate was already allocated by its caller.

Preflight candidate expanded AST size, depth and expanded name bytes bottom-up
before constructing/cloning boxes, argument vectors, callee templates or strings.
At a shared DAG node the expanded count is the sum of child occurrence counts,
not unique child IDs. Exceeding a limit saturates/refuses without integer wrap.
Materialize candidates iteratively within these bounds; no recursive decode of
an unbounded RecExpr/proof tree or hidden full-DAG flatten is permitted.

Budgets bound logical represented data and requested allocations, not allocator
overreservation/bookkeeping, resident-set size, arbitrary physical OOM/process
abort, external OS work or an existing caller-owned AST's entire allocation.
Admission, fold prefix, Runner/capture, proof construction, candidate building
and actual gate verification share one cumulative WitnessBudget per island;
there is no phase reset. Verification counts the proposal's represented
certificate/candidate along with its scratch, even if this conservatively
charges representation again. The standalone public checker creates its own
fresh budget for its standalone input. There is no whole-process RSS claim.

Private producer ceilings are fixed, not caller-increasable in this profile:
8 iterations, 2048 raw append-only egraph nodes, 128 substitutions per rule per
iteration, and 4096 successful recorded rule applications. A non-saturated
Runner stop (iteration/node/time/other resource stop) refuses the whole attempt.
A capped search reporting its full match cap may conservatively refuse rather
than assume that all matches were seen; never materialize an unbounded match
list to discover its length. These supplement,
not replace, WitnessLimits. Enforce before applying growth-producing work;
Runner's ordinary post-iteration node limit alone is insufficient. Include
recording/substitution storage and proof construction in the same producer
logical budget. Exhaustion returns no replacement and preserves source AST.

## Private producer/checker seam

The checker owns the shared bounded admission/output builder and its budget
helpers; these are private to optimize and available to its sibling producer.
The following names/signatures are frozen to let writers work independently:

```rust
// In witness.rs; all fields stay private to that module.
pub(super) struct WitnessBudget { /* private limits/counters; not Clone/Copy */ }
pub(super) struct BoundSource<'s> { /* borrowed names/callees, owned terms */ }

impl WitnessBudget {
    pub(super) fn new(limits: WitnessLimits) -> Result<Self, WitnessError>;
    pub(super) fn work(&mut self, amount: usize) -> Result<(), WitnessError>;
    pub(super) fn metadata(&mut self, amount: usize) -> Result<(), WitnessError>;
    pub(super) fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize)
        -> Result<(), WitnessError>;
}
impl<'s> BoundSource<'s> {
    pub(super) fn kind(&self) -> IntegerKind;
    pub(super) fn terms(&self) -> &[Term];
    pub(super) fn root(&self) -> TermId;
    pub(super) fn variable_count(&self) -> usize;
    pub(super) fn name(&self, id: u32) -> Option<&'s str>;
}
pub(super) fn bind_source<'s>(
    source: &'s Expr, types: &TypeMap, budget: &mut WitnessBudget,
) -> Result<BoundSource<'s>, WitnessError>;
pub(super) fn build_candidate(
    source: &BoundSource<'_>, terms: &[Term], root: TermId,
    budget: &mut WitnessBudget,
) -> Result<Expr, WitnessError>;
pub(super) fn verify_with_budget(
    source: &Expr, candidate: &Expr, types: &TypeMap,
    certificate: &RewriteCertificate, budget: &mut WitnessBudget,
) -> Result<(), WitnessError>;

// In certificate.rs.
pub(super) struct RewriteProposal {
    pub(super) candidate: Expr,
    pub(super) certificate: RewriteCertificate,
    pub(super) budget: WitnessBudget,
}
pub(super) enum RewriteOutcome {
    Barrier,
    Unchanged,
    Candidate(RewriteProposal),
}
pub(super) fn propose_rewrite(
    source: &Expr, types: &TypeMap, limits: WitnessLimits,
) -> Result<RewriteOutcome, WitnessError>;
```

BoundSource fields cannot be certificate-provided or publicly forged. Admission
may represent duplicate structural nodes. The output builder independently
validates the supplied term DAG/root/kinds/variables and allowed intrinsic
origins plus expansion limits before materialization; invoking it does not
approve the proof. Public verify_rewrite creates a new WitnessBudget and calls
verify_with_budget. The coordinator's mandatory actual gate instead passes the
producer-returned budget to that same full verifier, preserving cumulative
limits. That verifier rebinds actual AST endpoints and checks every equality
reason from scratch before installation; producer assertions do not bypass it.
Read-only limit accessors/private utility additions that do not change policy
may be agreed between writers and reported; public/API or policy changes require
coordinator amendment publication before dependent edits.

Coordinator retains the existing private parent `Math` variants Num(i64),
Add/Sub/Mul/WAdd/WSub/WMul([egg::Id;2]), and replaces the old arbitrary Symbol
leaf with Input(u32). Parent Graph remains egg::EGraph<Math,()>; parent rules()
returns the existing 18 forward Rewrite values in catalog order. Producer may
use these private parent items; checker imports none of them. Static pattern
names/variables may use egg symbols, but source names never enter its global
symbol interner.

The producer directly encodes Math RecExpr nodes and opaque InputVar symbols;
it does not round-trip source names/operators through formatted s-expressions.
Actual egg explanations stay disabled. Instrument bounded scheduling/appliers
to capture successful current-rule instances as rule ID plus at most three raw
substitution IDs. Egg's append-only original nodes may select concrete terms
only after their child indices are checked backward; canonical classes/find or
union membership never justify an equality. Only independently matched rule
instances and explicit already-justified congruence establish proof edges.

Guarded edges wait for an earlier validated equality to a total leaf when their
actual erased term is not a leaf. Unavailable guard support is skipped, never
assumed. Bounded producer union/proof-path structures may search for a chain,
but emit explicit earlier-only Rule/Congruence/Symmetry/Transitivity records.
Each path/closure round reserves and charges before work; quadratic candidate
pair scans must be prepaid/refused, not performed outside the budget. No
unbounded recursive explanation conversion, proof matrix or hidden host
collection growth is allowed. The final emitted proof must connect the actual
source root to the actual Extractor-selected candidate root, except for the
explicit literal-range fold-only profile below.

Do not invoke egg Explanation::check_proof as the trust gate: egg0.10 accepts
unknown rule reasons and does not revalidate our conditional totality semantics.
Its explanation construction/debug proof checks can recurse/expand before the
adapter budget. No TreeTerm flatten/string APIs are used as a workaround.
The six existing direct folds form a bounded bottom-up prefix over the original
admitted typed term DAG before the Runner. For each operator, connect rewritten
children by Congruence first; if both mapped children are Const, append its
explicit checked/wrapping fold equality, then Transitivity. A checked overflow
keeps the operator rather than folding it to a wrapped value. This constructs
an explicit original-root to folded-root proof even when folding exposes more
constants in an ancestor. All prefix allocations/proofs/work consume the same
attempt budget, and the final original-root to actual extracted-root proof
composes the prefix and Runner witnesses.

The current Math::Num(i64) range remains the graph profile. Check every constant
reachable from the folded root before graph admission. If a reachable constant
is outside i64, a distinct literal-range-ineligible outcome permits a fold-only
proposal with the complete prefix witness; if nothing changed, return RewriteOutcome::Unchanged.
This is not a fallback after search failure. If admission passes and any later
Runner/capture/search/proof/resource operation refuses, discard the entire
prefix/proposal and retain the original island. Constants eliminated by the
prefix need not block graph admission. No temporary folded
AST is installed. Every fold-only proposal still passes the full exact gate.

## Failure behavior and required actual evidence

The gate borrows the actual source while proposing and verifying. It performs
`*expression = proposal.candidate` only after successful full verification with
the proposal's cumulative budget through verify_with_budget.
RewriteOutcome::Unchanged or any Err retains the complete original island,
including spans and existing child nodes. RewriteOutcome::Barrier is returned
only when initial admission finds structurally unsupported syntax or unsupported
source typing/intrinsic resolution; it authorizes ordinary recursive traversal
of independently admitted children. After admission succeeds, no later error
may become Barrier. Limit/allocation/invalid-limit errors are never Barrier,
even during initial admission. This distinction prevents resource refusal from
silently retrying child rewrites. There is no silent
unchecked fallback, post-error partially installed candidate or success flag
without exact input/output comparison. Existing optimizer callers remain valid. Coordinator defines this private
installation seam and uses it from actual traversal (private adversarial tests
may invoke it; it grants no public authority):

```rust
fn install_proposal(
    expression: &mut Expr, types: &TypeMap,
    proposal: certificate::RewriteProposal,
) -> Result<(), witness::WitnessError>;
```

It performs full verify_with_budget before its sole candidate assignment. Any
Err leaves expression unchanged. The CLI enables AST optimization by default;
actual CLI comparisons use the default path versus `--no-opt`. There is no
`--opt` alias in this scope, and `check` returns before optimization. No main.rs
change is authorized.

Checker tests cover each rule/kind, all folds including U64 multiplication and
signed endpoints, valid congruence/symmetry/transitivity, real prior guard proof,
and every malformed version/index/cycle/unknown-rule/guard/type/binding case.
Verify totality cannot be bootstrapped from the rule currently erasing a term.
Budget tests include exact boundaries and shared DAG expansion beyond output
limits, without allocating the expanded output first.

Producer tests execute the actual Runner/Extractor, successful witnessed
simplifications and constant folds, chained congruence/guard support, hard
capture/search/term/proof/work refusal and absent proof fallback. Demonstrate
that e-class equivalence by itself cannot install a candidate. Explain/flatten
APIs must remain unused. Prove execution by actual returned certificates and
the separate checker, not a fabricated hand-built success path alone.

Independent tests corrupt genuine produced certificates one field at a time,
exercise arithmetic overflow/strict failure order, all four kinds, shadowed
intrinsic names, missing/Infer/mixed type maps, source/candidate substitution,
callee origin preservation, both direct folds and egraph extraction, depth/
name/occurrence/resource bounds, and full original-AST preservation on refusal.
Compare actual evaluator/native behavior through existing supported checked
arithmetic tests. No need to modify an unrelated backend or baseline test.

Coordinator executes appropriate actual tests, Rustdocs, strict Clippy,
formatting and diff checks on the combined coherent source. All lanes report
only checks actually executed; root combines independent suites and records
execution and limitations. Do not ignore tests, relax limits, fabricate
stubs, emit raw Lean strings, or claim hosted/Lean/proof/linear-time/performance
success without corresponding evidence. Source/test authorization begins with coordinator publication of this frozen
contract.

## Coordinator publication decisions

Approved: 4MiB/16MiB default/HARD logical metadata accounting; the move-only
cumulative budget passed into the same full checker; public optimize::witness;
original same-op callee inventories; contextual literal metadata with actual AST
value authority; maximal-island ordering; witnessed fold prefixes and the
explicit range-only fold profile. The three-outcome private producer seam
separates an unsupported barrier from unchanged/refused admitted islands.

All five public claims precede their source writers. Parent230 records the
actual external-file overlap checks, and fresh formatter231 ownership remains
separate. No source or build started during draft review. Claims stay reserved
through reviewed protected merge.
