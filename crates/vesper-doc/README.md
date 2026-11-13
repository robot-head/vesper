# Checked API documentation

`vesper-doc` is a root-free typed metadata, rendering and offline search library.
It depends on `vesper-core` and the lossless `vesper-syntax` lexer, plus serde,
`vesper-simpulse` (simulated doctest I/O) and `vesper-ffi` (wire layout checking).

Wave-six additions (§40.2, §40.3, proof badges):

- `sim`: the SimPulse doctest host behind `vesper,sim(seed=N,clock,net,disk)`
  fences. One kernel task per run; latency parks it on scheduled completions,
  network jitter is seed-derived, and a second run replays the first journal.
- `visual`: the 16-byte `WireFieldSpan` and `LinearEdge` records of §40.3,
  compiled `vesper-wire-layouts` JSON (`--wire`), native struct rulers and
  static SVG for byte layouts and the linear obligation state machine.
- `badges`: proved / audited / unverified from `vesper proof-audit --json`
  reports and `@proof(Name)` doc anchors only.

`examples/package` is a Loom package used by the README's Getting Started:
`vesper lint` and `vesper test --doc` run over it with no file argument.
The facade's `src/doc_commands.rs` invokes the actual parser/checker, reads the
checked World, resolves result annotations using `world.function_results`, and
passes complete types, modes, effects, selectors and documentation to this leaf.
The leaf does not depend on the `vesper` facade or assert that arbitrary caller
DTOs came from a checker.

After coordinator registration:

```sh
cargo run -p vesper -- doc crates/vesper-doc/examples/api.ves --json
cargo run -p vesper -- doc crates/vesper-doc/examples/api.ves -o /tmp/api.html
cargo run -p vesper -- doc crates/vesper-doc/examples/api.ves --json --query crates/vesper-doc/examples/query.json
```

HTML is the default output. `--json` selects complete typed JSON metadata.
`-o` creates a **new** output file and preserves any existing file. Generation
finishes before opening it. No source is rewritten, no source function or
doctest is run, and no network service or remote asset is used.

Queries use complete core Type JSON, passing modes (`In`, `Inout`, `Sink`), modes
(`Copy`, `Affine`, `Linear`), a return mode ceiling, allowed selector footprints,
and a structural effect ceiling. See `examples/query.json` for a real query
matching the example's `close` function. Empty effect instances with null tail
mean a closed pure ceiling. Every declared effect instance must fit the ceiling
with the same name, type arguments **and label**. Labels are compared
conservatively by their documented lexical spelling; no binder alpha-renaming
or identity inference is attempted. Nested unknown JSON contract fields fail.

Full types and passing modes must match. Parameter modes use `query <= candidate`;
return modes use `candidate <= query ceiling`. Candidate selectors must be within
the allowed footprint: `All` permits all fields, while `Paths` grants listed
subtrees. Nominal arguments, tuple elements and closed pure arrow parameters,
results, passes and capture modes are compared structurally. Unit's two core
spellings are normalized. Effect sets are normalized for order/duplicates.
Exact contracts rank above permitted narrower contracts, with names breaking
ties deterministically. No proof boolean affects ranking.

The search/transition index contains checked source functions and inherent
methods. Effect operations remain scoped inside their owning effect declaration,
with complete operation signatures; they are not flattened into ordinary calls.

Generic functions, open rows, unresolved types, row-valued types and effectful
arrow binder contracts are documented with explicit unsupported reasons and
excluded from search. This implementation does not guess wildcard instantiations,
variance of nominal generic arguments, lexical capability substitutions or
source-level Hoogle query syntax. Ordinary effect instances/labels are retained.

Typestate identities include their complete concrete nominal type. Every edge
is validated against its actual indexed receiver/result. Sink functions with
multiple owned linear inputs or non-nominal linear results have ambiguous
attribution and are omitted explicitly. An inout edge keeps the same nominal
state and cannot be terminal. Unused concrete linear source types remain visible
as states without a terminal path. Reverse reachability means **a terminal path
exists**; cycles may coexist with that path. It is not an all-path ownership or
termination proof.

Layouts are copied from the current native layout implementation: eight-byte
alignment, eight-byte scalar slots, and an eight-byte enum tag. They are not
padding-free wire/tiled layouts. Generic or excessive layout expansion is
reported unavailable. Private generated Pact functions are excluded using
`World.elaboration.generated`; source trait/model metadata remains visible with
its search limitation.

Default source budget is 128 KiB with 16,384 lexer tokens, nesting 48, weighted unfinished expression
segments 256, 128 function tokens and 64 conditional tokens. The facade prechecks
nominal dependency expansion before the existing checker and rejects recursive
user nominal dependencies conservatively. Metadata defaults are 256 items
(including fields/variants/operations), 8192 structural type/effect nodes, type depth 48, and 2 MiB
output; queries have a 32 KiB input limit. Configurable hard ceilings are in
`Limits::validate`. These are input/metadata/output bounds, not a claim that the
existing checker has a general execution-fuel or exact RSS guarantee.

Rendering escapes all source/doc content. Module/item comments come from actual
lexer comment tokens; strings cannot forge comments. Raw source spans/text retain
all doc-comment tokens, including field/variant comments whose current AST lacks
a dedicated span. No attachment to those fields is guessed. No audited artifact-bound
proof evidence is accepted, so the output says unverified. This is static host
documentation and CLI/library search, without WASM, proof certificates, mode
crossing matrices, intra-doc link resolution, semver diffs or doctest execution.

The supported pre-check profile also conservatively refuses nonempty tuple
construction, generic function/method values or calls, generic constructors
(including Option/Result), generic conformances, and unconstrained inferred
closures. These can grow inferred types exponentially even in compact shallow
source. Complete tuple/generic declarations/signatures remain documentable;
closures directly constrained by complete arrow annotations remain supported.
This is an explicit unsupported-source boundary, not a change to the language.
Weighted delimiter frames preserve peak completed-statement cost as well as
unfinished cost, so semicolons cannot hide deep child ASTs before parsing.
