# Formatter document proof model contracts

Claims #116–120 were published before writers, after checking the complete open
board and actual external PR101 filenames. External Warp/TCP changes remain
reserved. Baseline is published PR115 headffd856e7f1e599517d5a740068472d763494ee96;
that draft depends on PR110, whose full Rust1.100 workspace gate awaits the
external owner's already-implemented Warp lint fix. The live design is unchanged
at41sections/AppendixA, modified2026-11-12T09:18:30.204Z; §39.4 was reread.

## Ownership and scope

| Issue | Branch | Exclusive writes |
| --- | --- | --- |
| #116 | proof41/model | new lean/StylusAndLintSafety.lean, model report |
| #117 | proof41/projection | new fmt/src/proof_model.rs, doc.rs narrow actual-arena projection API, tests/proof_model.rs, projection report |
| #118 | proof41/emitter | new fmt/src/proof_lean.rs, tests/proof_lean.rs, emitter report |
| #119 | proof41/acceptance | new tests/format_proof_cli.rs, check/test-format-proof scripts, format-proof.yml, fixtures/format_proof, acceptance report |
| #120 | proof41/integration | new fmt/src/proof_source.rs, fmt/lib.rs capture/export hooks, new src/verify_commands.rs + main routing, proof_commands.rs narrow reusable kernel/execution helpers, Lean syntax Term::List and DefinitionKind::NoncomputableDef/printer/tests, formatter/Lean-syntax README proof sections, manifests/lock/path filters, contract/integration report |

Separate worktrees from this contract, lightweight private caches only, one job,
debug0/incremental0; target/parallel-next is coordinator-only. Initially3.6GiB free:
no new toolchain or Wasmtime build. Existing renderer/layout, ordinary formatting,
recovery APIs, LSP, checker/exhaustion/AST/IR/ABI stay compatible. All generated Lean
uses vesper-lean-syntax AST; no raw/source-string node or formatted Lean code.

The deliverable is a kernel-checked document-algebra theorem instantiated with
the actual first-pass formatter Doc projection, plus existing runtime source
token/comment conservation checks. It is not a proof that the Rust renderer
refines Lean, a lexer proof, semantic program proof, certified autofix or
performance/RSS bound. Model group selection may abstract the Rust lookahead,
provided comment conservation is proved independent of that selection and the
abstraction is stated explicitly. No merely counted-comment toy witness.

## Fixed Lean model API — coordinate before changing

Module `StylusAndLintSafety`, namespace `Vesper.StylusSafety`.

```
inductive Doc where
  | nil | text (value : String) | comment (value : String)
  | hardLine | softLineOrSpace | softLineOrEmpty
  | nest (indent : Nat) (inner : Doc)
  | append (left right : Doc)
  | group (inner : Doc)
inductive Mode where | flat | broken
```

Actual kernel replay requires the non-nested binary append representation above:
the existing auditor does not replay Lean's generated nested Doc.rec_1 recursor.
Expose the same public callable `Doc.concat : List Doc -> Doc` as an explicitly
noncomputable total foldr append nil. The Rust ModelDoc::Concat and emitter's
Doc.concat ListAST call stay unchanged; only the Lean internal representation
normalizes lists into ordered binary concatenation. All logical definitions that
would emit unsafe codegen auxiliaries use explicit noncomputable def. There is no
auditor-policy relaxation, extra axiom, partial definition or public API change.

Provide tagged rendered pieces, `commentsOf`, and a total `render` whose public
signature is `render (width col : Nat) (mode : Mode) (doc : Doc)` (piece result may
be an internal record with a public commentsOf projection). The model owner may
use internal helpers/choice abstractions as needed, but must coordinate exact
signature with emitter before consumers. Required theorem:

```
comments_render_invariant (doc : Doc) (width col : Nat) :
  commentsOf (render width col .flat doc) =
  commentsOf (render width col .broken doc)
```

Also prove `projection_narrowing_preserves_disjointness` with the design's
`leavesNew leavesOld callerOther : Nat -> Bool`, subset and disjoint premises.
That lemma remains generic: it does not certify an existing compiler autofix.
No axioms/sorry/partial/native_decide. Actual Lean4.24.0 and printed axiom checks
must pass; root additionally kernel-replays emitted theorem artifacts under an
empty axiom policy. Include real edge-case examples/counterexamples.

## Bounded actual document projection

Public `ProofLimits` fields and defaults/HARD:

| Field | Default | HARD |
| --- | --- | --- |
| max_nodes |4096|16384|
| max_text_bytes |128KiB|256KiB|
| max_depth |64|128|
| max_metadata_bytes |1MiB|4MiB|
| max_lean_bytes |1MiB|2MiB|
| max_work |64Mi|256Mi|

`ProofLimits::validate()` rejects zero/above-HARD values. Public `ProofError`:
`InvalidLimits`, `Resource(&'static str)`, `InvalidDocument`, `Allocation`,
`Formatter(Error)`, `Conservation`, `LeanSyntax(String)`; implement Display/Error.
No source contents are needed in errors. Resource limits are explicit refusals.

`Document::proof_model(root: DocId, limits: ProofLimits) -> Result<DocumentModel, ProofError>`
reads the actual private node arena and validates current-owner IDs. No separately
rebuilt tree or caller-provided metadata is accepted. Preflight expanded
occurrences (not just unique DAG nodes), text bytes, depth, metadata and work
before cloning/reserving; shared repeated children must not bypass limits.
Then construct a private owned tree. Destructor depth remains bounded.

`DocumentModel` has private fields/no Deserialize or caller constructor; public
getters `node_count()->usize`, `text_bytes()->usize`, `depth()->usize`,
`comments()->&[String]` in actual document order. Internal `doc()->&ModelDoc` with
crate-private enum:
`Nil, Text(String), Comment(String), HardLine, SoftLineOrSpace, SoftLineOrEmpty,
Nest(usize,Box<ModelDoc>), Concat(Vec<ModelDoc>), Group(Box<ModelDoc>)`.
Preserve every payload/occurrence/order. Account for the separately retained
comment payload list before copies. No IDs/nonces from this tree become semantic
or source authority. A public arbitrary Document projection proves its algebra
only; root's private source artifact supplies source association.

## Source capture and AST emitter

Root adds `format_for_verification(source:&str, format:Limits, proof:ProofLimits)
-> Result<FormatVerification,ProofError>`. Private FormatVerification fields and
getters `output()->&str`, `changed()->bool`, `source_hash()->[u8;32]`,
`output_hash()->[u8;32]`, `model()->&DocumentModel`, and
`emit_lean(limits:ProofLimits)->Result<String,ProofError>`.

The normal formatter first pass captures its actual Document/root after rendering
and before dropping the arena. Public format_source and crate-private
format_with_work signatures stay unchanged. Only the first pass captures; the
normal whole-token conservation/reparse/idempotence validation must succeed
before returning FormatVerification. Compare actual source lexer comment payloads
with the projected model comments; derive both hashes from actual bytes. No
recovery/unsupported source proof or original-source CheckedSource is created.

The source proof operation prepays one projection allowance before any projection
clones while retaining the original finite formatting budget. AST emission has
one further finite allowance. CLI defaults128Mi formatting+64Mi projection+64Mi
emission, total256Mi logical envelope; unused failed stages never reset. Public
source-proof profile validates finite caps (input128KiB/output256KiB/tokens8192/
Docnodes100000/depth24/formatwork128Mi) and the checked sum of proof stages.
Ordinary formatter defaults clamp to this stricter source-proof profile, retaining
any tighter caller limits; zero format limits and invalid ProofLimits reject.

Emitter crate-private API:
`proof_lean::emit(model:&DocumentModel, source:[u8;32], output:[u8;32], limits:ProofLimits)
-> Result<String,ProofError>`.
Root only exposes it through private FormatVerification. Fixed generated namespace
`Vesper.StylusGenerated`, definition `document : Vesper.StylusSafety.Doc`, fixed
source/output hash string metadata, theorem `comments_preserved` with Nat binders
width/col, proving commentsOf(render flat document)=commentsOf(render broken document)
by direct application of `Vesper.StylusSafety.comments_render_invariant`.

Build Module/Term/Declaration only. Root adds `Term::List(Vec<Term>)` to the
central AST/printer (ordinary list syntax) and typed
`DefinitionKind::NoncomputableDef` for the generated total logical document
definition, which calls the noncomputable concat normalization. Existing ordinary
definition kinds stay unchanged; no raw modifier text or partial definition.
There are no dynamic imports or theorem names.
Preflight AST nodes/depth/payload copying/escaped rendered-size and conservative
recursive printer work before construction/rendering; cap actual final bytes.
Do not emit source code via format!/concatenated templates. Tests must elaborate
real generated modules and demonstrate malicious text remains a literal.

## Explicit CLI and kernel acceptance

Frozen command:
`vesper verify SOURCE --format [--json | --emit-lean] [--lean PATH] [--timeout SECONDS]`.
Only --format is supported in this slice; no default program-verification claim.
Input regular UTF8 <=128KiB/no final symlink, bounded arguments. --json and
--emit-lean conflict; --emit-lean prints exactly generated Lean only after all
checks succeed. There is no public output-file mutation; source stays untouched.

Root compiles bundled known model plus generated witness in a private directory,
using actual Lean4.24.0 with bounded memory/heartbeats/captured output and timeout.
Do not accept a mocked executable. Clear user Lean path/plugin settings for
compilation; selected toolchain/parser remain the trusted boundary. Reuse existing
proof-audit kernel replay and empty-axiom policy via narrow crate-private helpers;
preserve existing CLI behavior and tests. No cached approval. No success report
or emitted witness on any format/projection/emission/toolchain/kernel failure.

Success JSON schema `vesper.format-proof/1`, status `kernel_checked`, scope
`formatter_document_model`, source_blake3/output_blake3, input_bytes/output_bytes,
document_nodes/comment_tokens, actual lean_version and actual nested audit report.
Keep failures bounded with status rejected, stage/message, never accepted fields.
Plain output states model scope. Actual kernel replay must include generated
`Vesper.StylusGenerated.comments_preserved` and generic projection lemma, with
empty forbidden axioms. Default format/check/LSP/Lean paths do not run verification.

Native3OS acceptance uses real pinned Lean and disposable files, not fake Lean
stubs, validates emitted witnesses independently, malformed/opaque/resource/file/
wrong-toolchain failures, source immutability, source/output hash association and
hostile escaped payloads. No partial certificate or semantic proof promotion.
