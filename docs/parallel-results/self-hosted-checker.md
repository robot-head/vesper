# Self-hosted Stage 1 checker (§42.3, §4–§6)

Issue #581. Base `eb97e086f5486abea4db38acf930477b52661a72`, a descendant
of the required `37f7b8b`. Branch `codex/issue-581-self-hosted-checker`.
No dependencies, shared compiler edits, root patch, unsafe code, or changes to
other lanes' files.

## Delivered slice

The resolver, kind checker, expression checker, ownership environments, joins,
and diagnostics execute as Vesper source. The Rust test harness supplies only
source bytes through `source_byte`/`source_len`, invokes the Vesper entry point,
and compares results against the Rust seed. It supplies no AST, resolved names,
types, modes, ownership facts, or expected diagnostic to the Vesper program.

Assembly order is the existing package's five parser files, followed by
`ast_core.ves`, `resolve_core.ves`, `check_modes.ves`, `check_adt.ves`,
`check_expr.ves`, `check_blocks.ves`, and `check_driver.ves`.
`tests/self_host_check.rs` demonstrates the complete assembly. Loom still does
not assemble these compilation units; this lane does not edit its manifest.

The public entry is `check_main() -> i64`. Success is zero. Errors originate as
`Fail[CheckError]`, whose variants are `Semantic(code, start, end)`,
`Unsupported(rule, start, end)`, and `Limit(at)`. At the host boundary they are
encoded as `-1 - (code * 2^32 + start * 2^16 + end)`:

- Semantic codes are the seed's decimal diagnostic suffix (`302` = `E0302`).
- `9001`–`9009` identify unsupported rules, documented in `ast_core.ves`.
- `9801`, `9802`, `9804` preserve lexer/parser rejection categories.
- `9900` is a resource limit or internal checked-arithmetic failure.

Supported:

- Lexical scopes, shadowing and restoration, exact bytewise identifier matching
  (including long identifiers), forward function/type references, separate
  function/type namespaces, and the seed's alphabetical declaration-check order.
- Monomorphic nominal structs/enums, unit/tuple/named variants, constructors,
  destructuring, exhaustive constructor matches, and `i64`, `bool`, `Unit`.
- Monomorphic function types and values, explicit function modes, passing-mode
  checks, contravariant parameter/covariant result compatibility, effect-row
  inclusion and function-mode strengthening.
- `copy ⊑ affine ⊑ linear`, nominal floors and transitive component joins;
  `k_i ⊑ m_i` for every annotated field. After that premise is validated,
  `k_i ⊓ m_i = k_i`, so field kind joins retain the component kind.
- `in`, `inout`, `sink` on whole bindings; mutation of copy places; persistent
  argument loans including accesses inside later arguments; shared/shared
  aliasing; rejection of borrowed ownership transfers and temporary linear
  borrows.
- Linear exhaustion at lexical/function/arm exits, shadowing checks,
  use-after-move, linear expression disposal, and ownership agreement at
  `if`/`match` joins (affine moves are unioned).
- Unlabelled finite `Fail[T]` rows, typed-instance matching, missing-effect and
  mismatched-instance diagnostics, and the abort frame condition for live
  linear owners and active `inout` parameters.
- Small integer/bool/unit literals, comparisons, calls, blocks and copy-place
  assignment. Integer literal magnitudes are bounded to `2^40` in this adapter.

## Parser adapter and explicit boundaries

At implementation time #580 remained open with no merged tree API. The current
parser on main produces a digest, not an addressable tree. The checker therefore
uses a token adapter in its own new files, sharing the existing self-hosted
lexer and significant-token cursor. Semantic success and semantic diagnostics
are validated by that existing self-hosted parser. Unsupported inputs return
immediately rather than constructing a purported checked program. The adapter
can be replaced when lane 3's tree interface merges; no unmerged code is used.

This is a bounded core checker, not a complete Stage 1 compiler or a checker for
all syntax accepted by the parser. Unsupported syntax/rules include generics,
traits/models/impls, non-unit tuples, other scalar types, strings, arithmetic,
closures, lexical handlers/state/continuations, `?`, projections/partial moves,
regions, capability labels, open rows, and recursive modal kinds. Constructor
and pattern field order must follow the declaration; reordering is explicitly
unsupported. Literal/wildcard match patterns are outside the constructor-match
slice. There is no elaboration or code-generation output.

Bounds are 32,767 source bytes, 512 significant tokens, 16 top-level
declarations, 16 simultaneously recorded bindings (shadow history included),
16 active argument loans, and explicit type/expression/descent limits.
Over-budget units return typed `Limit`; they are never counted as accepted.
Repeated token scans and fixed immutable tuples replace unavailable collections.

The checking phase executes inside `region checker_arena(4096)`. Its Vesper
capability rows contain only `Fail` instances, with all failures caught by the
entry point; there is no ambient I/O. The region does not store a real AST or
symbol arena. The host evaluator allocates, and no counting-allocator test or
Lean refinement proof was performed. No zero-allocation, complete Stage 1,
metatheoretic soundness, or bootstrap/DDC claim follows from these tests.

## Validation

Pending final source-bound runs; exact commands, corpus counts, results and
any inherited workspace failures are recorded here before delivery.

The corpus test walks every available repository `.ves` file in `bootstrap`,
`crates`, `examples`, `packages`, `src`, and `tests/fixtures`; under Bazel it
walks declared runfiles (cross-crate fixture availability follows existing root
BUILD declarations). Every supported outcome must agree with the seed on
accept/reject, first diagnostic code **and exact byte span**. There is no
semantic disagreement allowlist. The test also enforces minimum positive and
negative counts. Unsupported and limit outcomes are counted separately.
Acceptance fixtures pin expected negative codes independently of the oracle.
Native tests compare representative success, failure and branch ownership
programs with the same oracle.

## Missing language features, ranked

1. Region-backed indexed buffers and persistent/arena maps: these replace the
   fixed tuple environment, move bitmap, and repeated token/declaration scans.
2. Byte slices and byte-string comparison: source injection and identifier
   comparisons currently run through generated pure functions.
3. A stable addressable CST/AST interface: this removes this lane's source-token
   adapter and shares parser recovery/span metadata with later phases.
4. Loops or guaranteed tail calls, with source-level work budgets.
5. Recursive algebraic data and useful generic collections/visitors, for a real
   typed HIR and place capability graph.
6. Integer conversions and bit operations, for compact arena indices, modes,
   ownership sets and direct wire diagnostics.
7. Struct update syntax and module constants: immutable state currently needs
   explicit reconstruction and numeric token tags.
8. Multi-file package assembly in Loom, so consumers can invoke the package
   entry without concatenating a source list themselves.

Follow-on work is the unsupported language surface above, integration with
lane 3's merged tree, scalable arenas/PCG, Lean refinement, and bootstrap
integration. These remain separate from the working monomorphic slice.
