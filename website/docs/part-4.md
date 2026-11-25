# 18 Lens and the editor

Type half a method call and the editor already has an incomplete program to check. Lens aims to report the same ownership errors as a batch check while keeping that unfinished call from producing misleading errors elsewhere.

<!-- technical-detail -->

Type half a method call and the editor already has an incomplete program to check. Vesper Lens runs the compiler's ownership, field-projection, and effect analyses directly on unsaved buffers so editor diagnostics match batch builds. When an expression is still unfinished, localized syntax and capability recovery keep that partial edit from flooding neighboring code with false leak errors.

Lens is implemented in `crates/vesper-lsp` and `crates/vesper-span` with CLI entry points in `src/lsp.rs` and `src/check.rs`, shipping `vesper lsp` over bounded stdio JSON-RPC frames and `vesper check [--json]` for batch verification. Its ownership, place-splitting, and guard-stack rules connect to `lean/VesperUnifiedTypeTheory.lean` and `lean/OrderedContexts.lean`. Per `REVIEW-STATUS.md`, the sub-2-millisecond keystroke budget is a design target requiring workload-specific measurement, `GRAM-01` through `GRAM-06` note that the surface grammar is not yet proven unambiguous, and `TYPE-09` plus `REPO-02` leave full source-level `Order::Lifo` integration and combined operational soundness open.

## 18.1 Reuse the checker during editing

The language server maps edits to compiler queries. Completion can use the type and outstanding obligations at the cursor. For a live transaction, a consuming method is a more relevant suggestion than another inspection. Opening a workspace also needs a policy for macros and build-time code.

<!-- technical-detail -->

Instead of maintaining a separate lightweight IDE typechecker that drifts from batch builds, `vesper lsp` shares the front-end and `vesper-mir-pcg` ownership pipeline with `vesper check [--json]`. A reader thread frames stdio requests, a dispatcher answers `$/cancelRequest` tokens immediately so superseded keystrokes stop early, and a session thread evaluates open buffers strictly in arrival order.

Completion queries inspect both the receiver type and the live substructural obligations at the cursor. In `crates/vesper-lsp`, `rank_methods_for_cursor` classifies candidate methods into `CompletionTier::DischargesLiveLinear`, `CompletionTier::ValidFrameCompatible`, and `CompletionTier::BlockedByFrameOrCeiling`, surfacing `sink self` transitions such as `commit` and `abort` ahead of read-only inspections while badging methods whose effects exceed the package `Loom.toml` capability ceiling or violate `[T-Let]` resumption frame conditions while another `Mode::Linear` binding is live. Opening an unfamiliar workspace never runs arbitrary host code: `build.ves` scripts and `#[wasm_macro]` expansions run inside Loom's fuel-metered `wasmtime` (`wasm32-wasip2`) sandbox with zero ambient filesystem or network authority, memoizing `(MacroWasmBlake3, InputCstBlake3)` outputs in `~/.vesper/store/index.redb`.

## 18.2 Recover at a known syntax boundary

When you type `tx.` the member name is still missing. Recovery keeps an incomplete node and resumes parsing at a balanced delimiter or another stable boundary. The checker can suppress ownership conclusions that depend on the missing member while continuing to check neighboring functions.

<!-- technical-detail -->

Substructural checkers face an acute editor hazard when a statement is half-typed: an unconsumed `Mode::Linear` value such as `tx` would ordinarily trigger scope-exit exhaustion errors across the rest of the function. Because Vesper uses square brackets `[...]` for generics and separates `UpperIdent` types and effects from `LowerIdent` bindings and capability labels, unclosed generic argument lists do not collide with `<` and `>` comparison operators during LL(2) recovery.

During lexing and parsing, `vesper-syntax` maintains balanced `{...}`, `[...]`, and `(...)` delimiter trees inside lossless `rowan` concrete syntax trees. Unexpected token runs wrap into localized `SyntaxKind::ErrorNode` subtrees and resynchronize at statement anchors (`;`, `let`, `match`, `handle`, `return`) or item anchors (`fn`, `struct`, `enum`, `effect`, `trait`, `impl`, `model`). Trailing member accesses (`tx.`) or unclosed calls (`tx.commit(`) lower to `HirExpr::Incomplete` carrying the receiver `PlaceProjection`; in `vesper-mir-pcg`, any linear local flowing into an `Incomplete` or `ErrorNode` region transitions to `LocalCapability::PoisonedRecovery`, satisfying `join_at_block` and `check_return_exhaustion` silently while keeping `tx`'s `WriteTxn` type live for dot-completion.

~~~vesper
fn append_in_progress(
    inout broker.{index, metrics}: Broker,
    in cfg: BrokerConfig,
) -> Result[CommitReceipt, IoError] eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(cfg.max_batch_bytes);
    // Mid-edit cursor after `tx.`: lowers to HirExpr::Incomplete(Place(tx)).
    // LocalCapability::PoisonedRecovery suppresses E0303 leak cascades on `tx`
    // while ranking `tx.commit()` and `tx.abort()` in DischargesLiveLinear!
    tx.
}
// Anchor synchronization at `fn` keeps sibling declarations clean and checked:
fn read_limit(in cfg: BrokerConfig) -> u64 eff [] {
    cfg.max_batch_bytes
}
~~~

## 18.3 Stop recomputation at unchanged results

Incremental queries track their inputs. Changing a private body may leave the file’s public signature fingerprint unchanged, so callers can reuse earlier results. Macro outputs and checked interfaces form further boundaries. Slower proof queries can run after editing settles without blocking syntax feedback.

<!-- technical-detail -->

Lens organizes incremental checking around five demand-driven `salsa` query layers separated by two early-cutoff firewalls so edits inside a private function body do not invalidate downstream modules or block syntax feedback on background proof checks.

    Keystroke UTF-8 Rope Delta
       │
       v
    ┌─────────────────────────────────────────────────────────────────────────┐
    │ Layer 1: parse_file_cst        Reparse modified { ... } into GreenNode  │
    └──────────────────┬──────────────────────────────────────────────────────┘
                       │
                       ├═══> [Firewall A: FileSignatureMif([u8; 32])]
                       │     Hashes item/fn headers (bodies stripped).
                       │     Unchanged on body edits -> 0 callers invalidated!
                       v
    ┌─────────────────────────────────────────────────────────────────────────┐
    │ Layer 2: expand_forge_macros   Pure (MacroBlake3, CstBlake3) WASI cache │
    ├─────────────────────────────────────────────────────────────────────────┤
    │ Layers 3 & 4: infer_fn_body_hir + check_fn_pcg  (per-FnId [T-Cross],    │
    │               vesper-pact SLG traits, forward PcgState borrow/linear)   │
    └──────────────────┬──────────────────────────────────────────────────────┘
                       │
                       ├═══> [Firewall B: Checked Fn Interface & Diagnostics]
                       │     Streams squiggles + completions immediately.
                       v
    ┌─────────────────────────────────────────────────────────────────────────┐
    │ Layer 5: verify_fn_lean_bg     Debounced worker thread; mounts cached   │
    │                                OLEAN_CERTS & emits bv_decide CodeLenses │
    └─────────────────────────────────────────────────────────────────────────┘
    ----
    $ vesper lsp
    $ vesper check src/wal_txn.ves --json

## 18.4 Connect a diagnostic to the edits it needs

A move error involves the location where ownership transferred and the later use of the old owner. A multi-span diagnostic shows both. A quick fix edits the concrete syntax tree (CST), which retains comments and spacing. Check the edited function again because changing a permission can affect callers.

<!-- technical-detail -->

A diagnostic can identify both the operation that failed and the earlier binding, move, or loan that makes it invalid. The checker attaches those secondary source spans when it has the relevant evidence. The editor can show the spans together. An edit still needs to be checked in its calling context.

| Code | What the checker reports |
| --- | --- |
| E0301 | Two arguments request overlapping access where at least one needs exclusive access. |
| E0302 | A linear value remains live at scope exit. |
| E0303 | A later access uses a value that has already moved. |
| E0306 | Code attempts to move a field from a linear aggregate before destructuring it. |
| E0401 | A call has an effect the enclosing function neither declares nor handles. |
| E0402 | An abortable effect can leave a linear obligation unfinished. |

Try the [overlapping-projection diagnostic](../tour/find-a-conflicting-loan.html) or the [abortable-effect diagnostic](../language.html#results). The older design sketches discuss additional checks and automated edits. The compiler's diagnostic and any attached suggestions describe the behavior for the program being checked.

## 18.5 Translate source spans into editor locations

The span layer connects compiler byte ranges to the editor’s document positions and revisions. A fix computed for an older file version may select the wrong text after an edit. The language server needs to associate diagnostics and edits with the snapshot that produced them.

<!-- technical-detail -->

Compiler diagnostics and CST edits address source text by UTF-8 byte offsets (`FileId` and `TextSpan`), whereas LSP clients exchange line-column positions tied to monotonic document version numbers. Lens binds every `VesperDiagnostic` and `CodeFix` to the exact document snapshot that produced it so stale fixes from an older revision are rejected rather than applied to shifted byte ranges.

    // crates/vesper-span/src/diagnostic.rs & crates/vesper-lsp/src/lib.rs
    pub struct FileId(pub u32);
    pub struct TextSpan { pub file: FileId, pub start: u32, pub end: u32 }
    pub struct TextEdit { pub span: TextSpan, pub replacement: String }
    pub struct CodeFix { pub id: &'static str, pub title: SmolStr, pub edits: Vec<TextEdit> }
    ----
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub enum CompletionTier {
        DischargesLiveLinear = 0,
        ValidFrameCompatible = 1,
        BlockedByFrameOrCeiling = 2,
    }
    pub fn rank_methods_for_cursor(
        receiver_is_live_linear: bool,
        other_live_linears: &[LocalId],
        allowed_pkg_effects: &FxHashSet<SmolStr>,
        methods: &[CandidateMethod],
    ) -> Vec<RankedCompletion> { /* sorts DischargesLiveLinear first, badges blocked effects */ }

# 19 Stylus formatting and ownership-aware linting

Stylus formats code while preserving its tokens and comments. A lint can propose a change to the program itself, such as borrowing just one field of a record. Applying that change calls for a fresh type check.

<!-- technical-detail -->

Vesper Stylus formats source files deterministically over a lossless concrete syntax tree so comments, attributes, and macro tokens stay attached to the constructs they document. Vesper Lint pairs syntax spans with resolved mode, `FieldTree`, and effect-row metadata to diagnose over-broad permissions and unused capabilities, rechecking any proposed source edit before writing it back.

Stylus and Lint live in `crates/vesper-fmt` and `crates/vesper-lint` behind `src/fmt_commands.rs` and `src/lint_commands.rs`, shipping `vesper fmt [PATH...] [--recovery] [--check] [--json]`, `vesper lint [FILE|DIR] [--json] [--fix] [--semver-safe] [--config PATH]`, and `vesper verify --format`. Comment conservation and caller-disjointness models are proved in `lean/StylusAndLintSafety.lean`. Per `REVIEW-STATUS.md` (`ECO-01` and `GRAM-01` through `GRAM-06`), shipped `L0101` projection narrowing is diagnostic-only while `L0102` unused-effect removal supports rechecked `--fix`, dynamic `HeapVec` allocation is preserved unless a fixed-capacity region bound is proved, and arena-escape, mode-relaxation, FIP, secret/Cancel, and WASI plugin-pack passes remain unimplemented in `src/lint_commands.rs`.

## 19.1 Preserve tokens and inspect semantics

A formatter works on a lossless syntax tree so a comment stays with the code it explains. A lint can use type information to notice that a helper requests exclusive access to an entire record but only touches its index. If an autofix narrows that parameter, recheck the function and its callers.

<!-- technical-detail -->

`vesper fmt` lowers lossless `rowan` syntax trees into a Wadler-Lindig document algebra (`FormatDoc`) rendered under a zero-configuration 100-column ribbon (`DEFAULT_LINE_WIDTH = 100`) without resolving imports or executing macros; `--check` exits nonzero when formatting differs, and `--recovery` formats only complete functions while preserving damaged byte spans verbatim. `vesper lint` inspects checked types, `FieldTree` access paths, and `eff [...]` rows across five substructural lint families: `L0101` (`overbroad_inout_projection`), `L0102` (`unused_effect_capability`), `L0204` (`demote_alloc_to_region`), `overly_restrictive_mode`, and `missed_fip_opportunity`, alongside `ScopeSet`-hygienic `egglog` simplifications.

    .ves Source (<= 256 KiB UTF-8)
       │
       v
    ┌──────────────────────────────┐
    │ Lossless rowan CST           │───> [vesper fmt] ──> FormatDoc (Text | Comment | Group | Nest)
    │ (tokens + trivia + comments) │                        │ flat_width() <= 100 - col ?
    └──────────────┬───────────────┘                        ├── yes ──> PrintMode::Flat   (100% comments
                   │                                        └── no  ──> PrintMode::Break   conserved)
                   v
    ┌──────────────────────────────┐
    │ HIR + PCG Semantic Analysis  │───> [vesper lint] ─┬─> L0101 overbroad_inout_projection (diagnostic)
    │ (modes, FieldTree, eff [...])│                    └─> L0102 unused_effect_capability (--fix splice)
    └──────────────────────────────┘                                     │
                                                                         v
                                                  [Reparse + Recheck + --semver-safe Gate] ──> Atomic Write

## 19.2 Set layout and lint policy

Canonical formatting selects line breaks, indentation, and grouping consistently. A package lint policy can assign severities and enable domain-specific rules. For example, a project may treat a public capability increase more seriously than a naming suggestion. External lint packs need an explicit execution boundary.

<!-- technical-detail -->

Stylus exposes zero stylistic knobs so every workspace shares identical 100-column formatting for `FieldTree` projections, square-bracket generics, `eff [...]` rows, and `handle` blocks. Workspace lint severities (`allow`, `warn`, `deny`) are configured declaratively in `Loom.toml` under `[lints.vesper]`, while domain frameworks specify sandboxed `wasm32-wasip2` lint packs under `[lints.plugins]` with explicit `wasmtime` instruction fuel budgets. In shipped `src/lint_commands.rs`, unimplemented `warn` or `allow` rules report as unavailable, whereas requesting an unimplemented `deny` rule or `[lints.plugins]` pack fails closed with a nonzero exit status.

    // CLI invocations and workspace Loom.toml lint configuration
    $ vesper fmt --check
    $ vesper fmt src/wal_txn.ves --recovery
    $ vesper lint --fix --semver-safe
    ----
    [lints.vesper]
    overbroad_inout_projection = "warn"
    unused_effect_capability   = "deny"
    demote_alloc_to_region     = "warn"
    overly_restrictive_mode    = "warn"
    missed_fip_opportunity     = "allow"
    // WASI domain lint plugin policy (fuel-metered in vesper-loom-sandbox):
    [lints.plugins]
    "@vesper-game/ignis-lints" = { version = "^1.0.0", fuel_limit = 5_000_000 }

## 19.3 Narrow a projection or shorten an allocation lifetime

If a helper only writes `broker.index`, a smaller projection can let the caller lend another field. If temporary data never leaves one request, an arena may suit its lifetime. Before applying either change, check aliasing, escape paths, allocation behavior, and the shape of the public signature.

<!-- technical-detail -->

When a helper accepts `inout shard: WorkerShard` exclusively as a whole record, callers cannot concurrently lend `in shard.routing` or `inout shard.replication` across the call under `[T-CallProj]`. Narrowing the parameter footprint to the touched leaves `inout shard.{metrics.requests, config.limit}: WorkerShard` documents the exact frame contract while preserving the two-argument call shape.

~~~vesper
// BEFORE: requests whole-struct `inout shard: WorkerShard`
// Warning[L0101: overbroad_inout_projection]: mutates only `shard.metrics.requests`
//   and reads `shard.config.limit`.
fn filter_ids_unoptimized(
    inout shard: WorkerShard,
    in raw_ids: Slice[u64],
) -> u32 eff [Alloc] {
    let mut kept = HeapVec::with_capacity[u64](256);
    for id in raw_ids {
        if id < shard.config.limit {
            kept.push(id);
        }
    }
    let count = kept.len() as u32;
    shard.metrics.requests += count as u64;
    count
}
~~~

~~~vesper
// AFTER (manual signature refinement guided by L0101; ECO-01 preserves arity and HeapVec):
fn filter_ids_unoptimized(
    inout shard.{metrics.requests, config.limit}: WorkerShard,
    in raw_ids: Slice[u64],
) -> u32 eff [Alloc] {
    let mut kept = HeapVec::with_capacity[u64](256);
    for id in raw_ids {
        if id < shard.config.limit {
            kept.push(id);
        }
    }
    let count = kept.len() as u32;
    shard.metrics.requests += count as u64;
    count
}
~~~

Per `ECO-01` in `REVIEW-STATUS.md` and `src/lint_commands.rs`, two safety boundaries govern this example. First, `L0101` (`overbroad_inout_projection`) is strictly diagnostic-only in `vesper lint` because narrowing a parameter signature alters interface contracts and requires caller review, whereas `L0102` (`unused_effect_capability`) supports `--fix` and commits an effect removal only after reparsing and re-typechecking the edited module (plus checking exported interface compatibility when `--semver-safe` is passed). Second, because `raw_ids` can exceed `256` matching elements, `kept` retains dynamically growing `HeapVec` allocation under `eff [Alloc]` rather than blindly demoting to a fixed-size `Region` arena (`L0204`) without a proven upper capacity bound and compatible out-of-space behavior.

## 19.4 State what an autofix preserves

A formatter property can compare the ordered comment tokens before and after formatting. For a proposed semantic fix, check that the result type-checks and preserves the behavior or compatibility promised by that rule. For an allocation rewrite, one such obligation is that every value remains live until its last use.

<!-- technical-detail -->

`lean/StylusAndLintSafety.lean` (checked via `vesper verify --format` and nightly Logos kernel replay) models Stylus's `Doc` algebra with explicit `Piece.comment` tags and proves that comment payloads and order are conserved under every `GroupChoice` flat-or-broken layout schedule, while `lint_autofix_preserves_caller_disjointness` proves that shrinking a callee's `FieldTree` leaf set preserves disjointness against any concurrent caller loan. These theorems cover the logical `Doc` and leaf-set models rather than Rust suffix lookahead or compiler lowering.

    -- lean/StylusAndLintSafety.lean (checked with 0 custom axioms)
    theorem comments_renderWith_invariant (left right : GroupChoice) (doc : Doc)
        (width col : Nat) (leftMode rightMode : Mode) :
        commentsOf (renderWith left width col leftMode doc) =
        commentsOf (renderWith right width col rightMode doc) :=
      (comments_renderWith left doc width col leftMode).trans
        (comments_renderWith right doc width col rightMode).symm
    ----
    theorem stylus_format_preserves_comments (doc : FormatDoc) (width col : Nat) :
        CommentTokensOf (StylusSpec.render width col .flat doc) =
        CommentTokensOf (StylusSpec.render width col .broken doc) := by
      induction doc generalizing col <;> simp [StylusSpec.render, CommentTokensOf, *]
    ----
    theorem lint_autofix_preserves_caller_disjointness
        (leavesNew leavesOld callerOther : Nat -> Bool)
        (h_subset : forall f, leavesNew f = true -> leavesOld f = true)
        (h_disj   : forall f, ¬(leavesOld f = true /\ callerOther f = true)) :
        forall f, ¬(leavesNew f = true /\ callerOther f = true) := by
      intro f ⟨h_new, h_other⟩
      exact h_disj f ⟨h_subset f h_new, h_other⟩

## 19.5 Select line breaks and recheck edits

A document formatter represents alternatives such as a flat group and an indented group with breaks. It chooses a layout that fits the line width. A lint engine instead proposes a source edit at a span. If the diff changes a parameter’s access mode, inspect the lint edit. Line breaking alone should preserve that mode.

<!-- technical-detail -->

`crates/vesper-fmt` evaluates `FormatDoc::Group` nodes in `O(N)` time: `flat_width` returns `None` whenever a subtree contains a `HardLine` or `//` line comment, and `render_canonical` switches a group from `PrintMode::Break` to `PrintMode::Flat` iff `col + w <= DEFAULT_LINE_WIDTH`. In `crates/vesper-lint`, `analyze_fn_substructural_lints` emits typed `SubstructuralLintFinding` records, and `--fix` atomically replaces the file only after re-running the front-end checker and `--semver-safe` interface checks with zero new diagnostics.

    // crates/vesper-fmt/src/lib.rs & crates/vesper-lint/src/lib.rs
    pub const DEFAULT_LINE_WIDTH: usize = 100;
    pub enum FormatDoc {
        Nil, Text(SmolStr), Comment(SmolStr), HardLine,
        SoftLineOrSpace, SoftLineOrEmpty,
        Nest(usize, Box<FormatDoc>), Concat(Box<FormatDoc>, Box<FormatDoc>), Group(Box<FormatDoc>),
    }
    enum PrintMode { Flat, Break }
    ----
    pub enum SubstructuralLintFinding {
        OverbroadInOutProjection {
            param_name: SmolStr,
            suggested_mut_fields: Vec<FieldIdx>,
            suggested_read_fields: Vec<FieldIdx>,
        },
        UnusedEffectCapability { fn_name: SmolStr, unused_effect: SmolStr },
        DemoteAllocToRegion { fn_name: SmolStr, bounded_bytes_estimate: Option<usize> },
        OverlyRestrictiveGenericMode { type_param: SmolStr, declared_bound: Mode, observed_usage: Mode },
    }

## 19.6 Narrow what a lint proves you do not use

Vesper Lint compares each signature with what the checked body actually does. If a function declares an effect it never performs, the lint can remove it. If an `inout` parameter only touches some fields, or is never written at all, the lint reports the narrower contract so callers keep the other fields free.

<!-- technical-detail -->

Section 19.6 groups the lints into four code families: `L01xx` substructural precision, `L02xx` memory discipline, `L03xx` algebraic canonicalization and `L04xx` package authority. It states four judgements for automatic rewrites: `[Lint-Narrow-Proj]`, `[Lint-Narrow-Eff]`, `[Lint-Demote-Mode]` and `[Lint-CST-Fix]`. Today `vesper lint FILE|DIR [--json] [--fix] [--semver-safe] [--config PATH]` reports `L0101 overbroad_inout_projection` as a diagnostic and removes unused effects (`L0102 unused_effect_capability`) with `--fix`, after a complete reparse and recheck. Policy comes from `[lints.vesper]` in the nearest `Loom.toml`. The port is checked against the Rust seed (`tests/differential.rs`) and by divergent builds of each ported area (`port/test-port.sh`). The remaining catalog entries, SARIF output and the `LintNarrowingSafety` Lean model are proposed.

    // After `vesper lint --fix`: the unused Fail[RuntimeError] is gone.
    // L0101 reports that only {index, metrics} are written; narrowing is manual.
    fn record_batch(inout broker.{index, metrics}: Broker, count: u64) {
        broker.index = wrapping_add(broker.index, count);
        broker.metrics = wrapping_add(broker.metrics, 1);
    }

# 20 Codex documentation and doctests

Codex proposes using the compiler’s ownership, layout, and effect information to document APIs. A caller can look up which fields a function changes or search for a method that consumes a transaction.

<!-- technical-detail -->

Vesper Codex turns checked module interfaces into API documentation with `FieldTree` mutation footprints, linear typestate diagrams, capability badges, and variance-aware signature search. Executable documentation examples are typechecked with full substructural ownership rules and run deterministically through `vesper test --doc`.

Codex is implemented in `crates/vesper-doc` with CLI front ends in `src/doc_commands.rs` and `src/doctest_commands.rs`, shipping `vesper doc FILE|PACKAGE_DIR [--json] [-o OUTPUT | --bundle DIR] [--query QUERY.json] [--search TEXT] [--wire LAYOUTS.json] [--wcsd-stack [--target TRIPLE]] [--proof-audit REPORT.json ...] [--olean-store DIR|default]` and `vesper test --doc [FILE|DIR] [--json]` (there is no `vesper doctest` subcommand). Search variance and contiguous wire-ruler properties are proved in `lean/CodexSearchAndLayout.lean`. Per `REVIEW-STATUS.md` (`COMP-01` through `COMP-05` and `PUB-01`), `@proof(...)` and `.olean` badges report kernel-replayed Lean model theorems from `vesper proof-audit --json` rather than proving native executable equivalence, and `vesper doc` itself performs static generation without invoking JIT code or network fetches.

## 20.1 Search for the operation you can call

A signature search can consider input and output types, parameter modes, and effects. If you own a transaction and need to finish it, a method accepting `sink self` is relevant. Looking only for the word “transaction” would also return methods that merely inspect it.

<!-- technical-detail -->

Keyword search over type names conflates constructors, read-only inspectors, and terminal consumers. Codex builds a package-wide signature index (`search-index.json` and `codex-search.wasm`, queried on the CLI with `vesper doc --json --search`) that matches pass modes (`in`, `inout`, `sink`), modal ranks (`copy <= affine <= linear`), and lexical effect ceilings using variance-aware substitutability: parameter modes match contravariantly (`qParam <= fnParam`, so a function accepting `linear` safely accepts a caller's `copy` or `affine` value under `[T-SubMode]`), return modes match covariantly (`fnRet <= qRet`), and performed effects must stay within the caller's allowed effect set.

Every documented function and package overview carries compiler-derived capability badges from checked `eff [...]` rows and `Loom.toml` ceilings: `Pure` (`eff []`), `Zero-Alloc` (`Alloc` absent), `Uncancellable Linear I/O` (`res(eff) = linear`), `Abortable Checkpoint` (`Fail` or `Cancel` present), `Constant-Time` (`eff [ConstTime]`), `Deterministic Float`, and `Hard-Realtime` (`eff [Realtime]`, which `--wcsd-stack` augments by compiling closed-row functions through Cranelift to decode prologues and attach exact worst-case stack depth in bytes).

    $ vesper doc src/wal_txn.ves --bundle target/doc --wire layouts.json --wcsd-stack
    $ vesper doc . --json --search "sink linear WriteTxn -> CommitReceipt eff [Storage]"
    $ vesper doc src/wal_txn.ves --bundle target/doc --proof-audit report.json --olean-store default
    $ vesper test --doc src/wal_txn.ves --json

## 20.2 Exercise a documentation example

A doctest turns a documented code block into a check or execution in a controlled environment. A simulated clock can give a stable result to a timing example. The example needs an expected result, a defined set of capabilities, and a source location for reporting a failure.

<!-- technical-detail -->

Doc comments (`//!` and `///`) resolve intra-doc references such as `[WriteTxn::commit]` against checked declarations and fail generation on broken or ambiguous targets. Running `vesper test --doc` (rather than a separate `vesper doctest` binary) extracts fenced `vesper` blocks, runs full Stage 3 ownership and effect checking so examples never omit linear cleanup, batches supported finite scalar entries into one shared Cranelift JIT module when top-level names are disjoint (falling back to the reference evaluator outside the native subset and recording `[jit]` or `[eval]` per example), replays seeded `SimPulse` fences twice to confirm journal determinism, and verifies that negative `compile_fail(E0301)` examples fail with the exact expected diagnostic code.

~~~vesper
/// Appends a CRC-validated batch and commits the WAL slot.
/// @proof(KrabkaWal::appendBatch_crc_err_aborts_cleanly)
/// ```vesper
/// Region::scoped(64.kib(), |cap arena| {
///     SimPulse::run_seeded(0x5EED_CAFE, 0, || {
///         let mut broker = Broker::mock_with_region(in arena);
///         let cfg = broker.config;
///         let batch = RecordBatch::from_slice(in arena, b"vesper-codex");
///         let receipt = append_batch(inout broker.{index, metrics}, in cfg, sink batch)?;
///         assert_eq(receipt.wal_slot, 1);
///         Ok(())
///     })
/// })
/// ```
/// Leaving `tx` live across `?` on `Fail[IoError]` is rejected with exact code `E0402`:
/// ```vesper,compile_fail(E0301)
/// fn broken_caller(in batch: RecordBatch) -> CommitReceipt eff [Storage, Fail[IoError]] {
///     let tx: WriteTxn = Storage::begin_txn(4096);
///     validate_crc(in batch)?;
///     tx.commit()
/// }
/// ```
pub fn append_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);
    match validate_crc(in batch) {
        Ok(()) => {
            broker.index.insert_in_place(sink batch);
            broker.metrics.batches_written += 1;
            tx.commit()
        }
        Err(err) => {
            Storage::recycle_batch(sink batch);
            tx.abort();
            Fail::raise(err)
        }
    }
}
~~~

## 20.3 Read a layout or obligation diagram

A wire map gives each field a byte range. A typestate graph shows which operation changes a value’s state or consumes it. A mutation footprint shows which fields an exclusive call may touch. Choose the view that answers the caller’s question, such as “May I reuse this buffer after the call?”

<!-- technical-detail -->

Codex generates visual contracts tailored to systems questions: native struct rulers depict the 8-byte host ABI, `--wire` compiles explicit-endianness Bridge layouts into 16-byte `CodexWireFieldSpan` descriptors with 64-byte cache-line boundaries and zero implicit padding, `FieldTree` heatmaps contrast mutated `inout` leaves against free sibling fields, and SVG typestate state machines trace `CodexLinearEdge` transitions from constructors to terminal `sink` consumers.

    1. Wire-Struct Byte Ruler (CodexWireFieldSpan: 0 padding holes, 64B L1 cache-line ticks)
    Byte:  0                8         12    13        17       19        23 ...       61  |64 (L1#1)
           ┌────────────────┬──────────┬─────┬─────────┬────────┬─────────┬───────────┐
           │ base_offset    │batch_len │magic│ crc     │ attrs  │last_dlt │ timestamps│
           │ i64be (8B)     │i32be (4B)│u8   │u32be(4B)│i16be   │i32be(4B)│ ...       │
           └────────────────┴──────────┴─────┴─────────┴────────┴─────────┴───────────┘
    ----
    2. Typestate Obligation Graph (reverse-BFS checks every state reaches [0: Discharged])
                                   ┌──(inout stage_record)──┐
                                   v                        │
       Storage::begin_txn() ──> [WriteTxn] ─────────────────┘
                                   │
                    ┌──────────────┴──────────────┐
                    │ sink commit()               │ sink abort()
                    v                             v
           [0: Discharged (Ok)]          [0: Discharged (Clean)]
       (Any public linear state with no path to [0] fails with DeadEndLinearTypestate!)

~~~vesper
// Exact 16-byte wire-layout field span descriptor for Codex SVG byte/cache-line rulers:
wire struct CodexWireFieldSpan {
    byte_offset: u32le,
    byte_width: u32le,
    cache_line_idx: u16le,
    endian_tag: u8,
    is_hot_tile_col: u8,
    field_sym_id: u32le,
}
// Linear typestate transition edge (`to_typestate_opt == 0` marks terminal sink discharge):
copy struct CodexLinearEdge {
    from_typestate_sym: u32,
    method_sym: u32,
    pass_mode_tag: u8,
    to_typestate_opt: u32,
    resumption_is_linear: bool,
}
~~~

## 20.4 Define a correct search result

A search result must satisfy the signature constraints the query requested. A complete wire map must account for every byte of the modeled record. These are checkable properties of a representation. To trust the generated diagram, also check that the compiler supplied the field offsets for this version of the record.

<!-- technical-detail -->

`lean/CodexSearchAndLayout.lean` formalizes both variance-aware Hoogle matching over the three-tier `ModeRank` lattice (`copy = 0`, `affine = 1`, `linear = 2`) and contiguous zero-hole coverage for `wire struct` byte rulers, with zero `sorry` or custom axioms.

    -- lean/CodexSearchAndLayout.lean
    theorem codex_hoogle_mode_match_sound
        (qParam fnParam fnRet qRet callerArgRank : Nat)
        (h_caller_has : callerArgRank <= qParam)
        (h_param_contra : qParam <= fnParam)
        (h_ret_cov : fnRet <= qRet) :
        callerArgRank <= fnParam /\ fnRet <= qRet := by
      omega
    ----
    theorem codex_wire_diagram_zero_holes
        (off0 len0 off1 len1 : Nat) (h_contig : off1 = off0 + len0) :
        (off1 - off0) + len1 = len0 + len1 /\ off0 + len0 <= off1 := by
      omega
    ----
    theorem wire_spans_zero_holes (spans : List WireSpan) (totalEnd : Nat)
        (h : checkContiguousSpans 0 spans totalEnd = true) :
        totalWireSize spans = totalEnd := by
      have := contiguous_spans_total spans 0 totalEnd (checkContiguousSpans_sound spans 0 totalEnd h)
      omega

## 20.5 Generate documentation from a checked interface

The proposed pipeline extracts states and transitions, indexes signatures, and plans a shared compilation for runnable examples. A cached page needs a key that includes the interface and relevant examples. A changed method signature should invalidate both its visual contract and any doctest using it.

<!-- technical-detail -->

During `vesper doc` generation, `verify_linear_typestate_reachability` seeds a reverse breadth-first search from terminal `PassMode::Sink` methods (`next_linear_state: None`) and walks backward across state-to-state transitions, rejecting any public linear type trapped in `CodexAuditError::DeadEndLinearTypestate` with no public consumer. Content-addressed `.olean` links from `--olean-store` are rehashed against the `vesper proof-audit --json` digest on read so changed signatures or stale proof artifacts never silently carry forward.

    // crates/vesper-doc/src/lib.rs
    pub enum CodexAuditError {
        DeadEndLinearTypestate { typestate: SmolStr },
    }
    pub fn verify_linear_typestate_reachability(
        states: &[SmolStr],
        edges: &[LinearMethodTransition],
    ) -> Result<(), CodexAuditError> { /* reverse BFS from terminal PassMode::Sink edges */ }
    ----
    pub fn search_codex_signatures<'a>(
        query: &SubstructuralSearchQuery,
        index: &'a [IndexedFnDocEntry],
    ) -> Vec<(u32, &'a IndexedFnDocEntry)> {
        // Enforces entry.ret_mode <= query.ret_max_mode, effect subset,
        // and contravariant q_m <= fn_m per parameter; ranks exact + verified first.
    }

# 21 Chroma highlighting and accessible code

Source coloring can show nesting and keywords even in a fragment. With compiler metadata it can also distinguish a consuming argument, a mutable place, and an effect capability.

<!-- technical-detail -->

Vesper Chroma renders source fragments and complete modules across terminals, static HTML documentation, interactive WebAssembly editors, and standalone vector or raster code cards. By combining lossless syntax spans with optional compiler ownership metadata, Chroma distinguishes consuming transfers, mutable `FieldTree` leaves, and lexical capabilities without relying on color alone.

Chroma is implemented in `crates/vesper-highlight` with CLI commands in `crates/vesper-tools/src/highlight.rs` and `shot.rs`, shipping `vesper highlight [--html|--ansi|--json] [--theme dusk|dawn] [--tier 0|1] [--no-line-numbers] [-o PATH] FILE`, `vesper highlight --css [-o PATH]`, and `vesper shot <file> -o <out.svg|out.png> [--theme dusk|dawn] [--dpi 1-4] [--verify]`. Lossless byte partitioning and luminance contrast gates are modeled in `lean/ChromaHighlightSafety.lean`. Per `REVIEW-STATUS.md` and `website/VALIDATION.md` (line 44), Chroma's primary code-token swatches pass the 7.0:1 gate against Dusk (`#111625`, at least 8.84:1) and tuned Dawn (`#FAF8F5`) card surfaces, whereas Dawn UI amber mode badges on card surfaces measure 4.67:1 (WCAG AA), so no blanket site-wide AAA claim is made.

## 21.1 Choose syntax or semantic tokens

Syntax highlighting can operate on partial text. Semantic highlighting needs resolved information about a binding or call. A static code card should keep the source readable when color is unavailable. Names, labels, and ordinary code syntax must still convey the important distinction.

<!-- technical-detail -->

Chroma separates syntax classification from type-driven enrichment through two explicit tiers. Tier 0 (`--tier 0`) partitions the lossless CST of any snippet—complete or mid-edit, including unexpanded `sugar` clauses—without regex backtracking. Tier 1 (`--tier 1`, the default) consumes read-only `SemanticFacts` from the front-end checker (`World::types`, `World::mode`, and `World::effects`) to classify 18 `VesperTokenClass` variants: marking live `Mode::Linear` bindings with `.vc-lin-live` wavy underlines, separating mutated `FieldLeafMut` leaves from shared `FieldLeafRead` leaves, and distinguishing linear effects from abortable `Fail` or `Cancel` checkpoints; if a snippet fails to parse or check, `vesper highlight` falls back cleanly to Tier 0 with a typed `FallbackReason`.

    .ves Snippet ──> [Lossless rowan CST] ──┬──(incomplete / check fails)──> Tier 0 Syntax + FallbackReason
                                            │
                                            └──(World::{types,mode,effects})─> Tier 1 Semantic Enrichment
                                                                               (18-class VesperTokenClass,
                                                                                .vc-lin-live wavy underline)
                                                                                          │
                     ┌────────────────────────────────────────────────────────────────────┘
                     v
    ┌────────────────────────────────────────────────────────────────────────────┐
    │ ChromaTokenSpanWire (16B) + verify_lossless_spans + OKLCH Contrast Auditor │
    │ Dusk (#111625) & Dawn (#FAF8F5): >= 7.0:1 code tokens, >= 4.5:1 comments   │
    └───────┬───────────────────┬──────────────────────┬─────────────────────────┘
            v                   v                      v
      --ansi (24-bit)    --html (--vc-* CSS,     vesper shot (--verify SimPulse,
                          copy-exact source)      SVG / 4x PNG cached in CAS)

## 21.2 Attach meaning to a source range

A semantic token carries a range and a classification. A website example can associate the source with an expected result or a diagnostic. Keep the underlying text intact so readers can copy a compiling example and assistive tools can read it in order.

<!-- technical-detail -->

Across `vesper highlight --json` (schema `vesper.chroma.spans/1`), Lens semantic tokens, and `vesper shot`, Chroma encodes spans into a 16-byte `ChromaTokenSpanWire` record. Showcase files can start with an `@shot(...)` header and embed non-compiled Twoslash comment pins (`//  ^?` to display a token's inferred type and mode or diagnostic, and `//^!` for callout notes); passing `vesper shot --verify` compiles and runs the example inside the Codex doctest sandbox before rendering and caching the output in `~/.vesper/store` under `BLAKE3(source_partition, theme, config)`.

~~~vesper
// Exact 16-byte token span wire record (schema vesper.chroma.spans/1):
wire struct ChromaTokenSpanWire {
    byte_start: u32le,
    byte_len: u32le,
    token_class: u16le,
    semantic_flags: u16le,
    hover_sym_id: u32le,
}
// Showcase file rendered via: vesper shot examples/wal_txn.ves -o card.svg --dpi 4 --verify
@shot(title = "wal_txn.ves — Disjoint Projections & Linear Exhaustion", theme = "dusk", dpi = 4)
fn append_verified_batch(
    inout broker.{index, metrics}: Broker,
    in config: BrokerConfig,
    sink batch: RecordBatch,
) -> CommitReceipt eff [Storage, Fail[IoError]] {
    let tx: WriteTxn = Storage::begin_txn(config.max_batch_bytes);
    //  ^? WriteTxn : mod linear (rendered with .vc-lin-live wavy underline until discharged)
    match validate_crc(in batch) {
        Ok(()) => {
            broker.index.insert_in_place(sink batch);
            broker.metrics.batches_written += 1;
            tx.commit() //^! Discharges linear tx on Ok branch
        }
        Err(err) => {
            Storage::recycle_batch(sink batch);
            tx.abort(); //^! Mandatory linear cleanup on Err branch (enforced by [T-Let])
            Fail::raise(err)
        }
    }
}
~~~

## 21.3 Render a static example or an editable snippet

A published page can render token spans once during the build. An interactive playground must recompute highlighting when the reader edits the source. Theme variables map the same semantic role to suitable colors on light and dark surfaces. Static examples can remain readable without scripts.

<!-- technical-detail -->

`vesper highlight --html` emits copy-exact HTML styled by `vesper-chroma.css` custom properties (`--vc-*`) so static pages toggle between Vesper Dusk (`#111625`) and Vesper Dawn (`#FAF8F5`) with zero JavaScript, while `.vc-gutter` and `.vc-twoslash-pin` set `user-select: none` so copying a snippet yields pure Vesper source. Interactive playground blocks hydrate the WebAssembly highlighter to recompute token classes and linear obligation spans on live edits.

    $ vesper highlight --html --theme dusk --tier 1 examples/wal_txn.ves -o wal_txn.html
    $ vesper highlight --css -o vesper-chroma.css
    $ vesper shot examples/wal_txn.ves -o card.svg --dpi 4 --verify
    ----
    /* vesper-chroma.css — OKLCH-derived theme custom properties for Dusk & Dawn */
    :root, [data-vesper-theme='dusk'] {
        --vc-bg-card:    #111625;  --vc-fg-primary: #E2E8F5;
        --vc-linear:     #FFB454;  --vc-inout:      #5CCFE6;
        --vc-effect:     #D2A6FF;  --vc-wire-proof: #6EE7B7;
        --vc-keyword:    #FF8F9C;  --vc-comment:    #8B97B3;
    }
    [data-vesper-theme='dawn'] {
        --vc-bg-card:    #FAF8F5;  --vc-fg-primary: #1B2030;
        --vc-linear:     #B85200;  --vc-inout:      #006B80;
        --vc-effect:     #6B2FD6;  --vc-wire-proof: #087443;
        --vc-keyword:    #B4233C;  --vc-comment:    #55607A;
    }
    .vc-lin-live {
        color: var(--vc-linear);
        text-decoration: underline wavy var(--vc-linear) 1.5px;
        text-underline-offset: 4px;
    }
    .vc-gutter, .vc-twoslash-pin { user-select: none; }

## 21.4 Check both source coverage and contrast

Concatenating token ranges in order should reproduce the original text exactly, including whitespace. Contrast checks compare the actual foreground and background pairing. A bright color that works on one theme can disappear against a different surface.

<!-- technical-detail -->

`lean/ChromaHighlightSafety.lean` proves that any span list accepted by `verify_lossless_spans` partitions `[0, src.length)` with zero gaps (`chroma_contiguous_spans_lossless` and N-span `chroma_verify_reconstructs`), and formalizes the integer luminance gate `(max + 50) >= 7 * (min + 50)` for primary code tokens and `2 * (max + 50) >= 9 * (min + 50)` (4.5:1) for comments (`chroma_wcag_aaa_gate_sound` and `verifyGates_sound`). It also records `design_dawn_accents_fail`, showing why initial draft Dawn accents (`4.7:1` to `6.6:1` against `#FAF8F5`) were darkened for shipped code tokens (`dawn_gates`), while `website/VALIDATION.md:44` notes that Dawn UI amber badges remain `4.67:1` (WCAG AA) alongside Dusk code tokens at `>= 8.84:1`.

    -- lean/ChromaHighlightSafety.lean
    theorem chroma_contiguous_spans_lossless
        (s0 e0 s1 e1 srcLen : Nat)
        (h_start : s0 = 0) (h_le0 : s0 <= e0) (h_join : e0 = s1)
        (h_le1 : s1 <= e1) (h_end : e1 = srcLen) :
        (e0 - s0) + (e1 - s1) = srcLen := by
      omega
    ----
    theorem chroma_wcag_aaa_gate_sound
        (lumFg lumBg : Nat)
        (h_gate : (Nat.max lumFg lumBg + 50) >= 7 * (Nat.min lumFg lumBg + 50)) :
        let lMax := Nat.max lumFg lumBg
        let lMin := Nat.min lumFg lumBg
        lMax >= lMin /\ (lMax + 50) >= 7 * (lMin + 50) := by
      constructor
      · omega
      · exact h_gate

## 21.5 Generate a reusable code card

The renderer lays out tokens and annotations into a scene for an SVG or raster image. OKLCH separates lightness, chroma, and hue for palette design. Check the exported size, chosen font, and displayed background so the image preserves the distinctions visible in source.

<!-- technical-detail -->

`crates/vesper-highlight` defines the 18-variant `#[repr(u16)] pub enum VesperTokenClass`, stores OKLCH lightness/chroma/hue coordinates alongside thousandths-of-luminance (`rel_lum_x1000`) on `ThemeSwatch`, gates palettes in `verify_contrast_gates`, validates span contiguity in `verify_lossless_spans`, and lays out `PositionedGlyphRun` vectors in `build_code_card_scene` using bundled JetBrains Mono 2.304 for headless-browser-free vector SVG and `1x` to `4x` PNG export.

    // crates/vesper-highlight/src/lib.rs
    #[repr(u16)]
    pub enum VesperTokenClass {
        KeywordControl = 0,   ModalSpecifier = 1,   PassModeIn = 2,
        PassModeInOut = 3,    PassModeSink = 4,     LinearBinding = 5,
        FieldLeafMut = 6,     FieldLeafRead = 7,    EffectLinear = 8,
        EffectAbortable = 9,  TypeNominal = 10,     WireEndianScalar = 11,
        FunctionIdent = 12,   LiteralScalar = 13,   DocProofAnchor = 14,
        PunctLigature = 15,   CommentTrivia = 16,   Whitespace = 17,
    }
    pub fn verify_lossless_spans(src_len: usize, spans: &[HighlightSpan]) -> Result<(), ChromaError>;
    pub fn build_code_card_scene(
        title: &str, src: &str, spans: &[HighlightSpan], palette: &ChromaPalette, dpi_scale: u32,
    ) -> Result<CodeCardScene, ChromaError>;

# 22 Crucible testing and migration

Crucible records the inputs and scheduling choices that lead to a failure. The test runner can replay that sequence, then shorten it until the remaining choices explain the bug.

<!-- technical-detail -->

Vesper Crucible pairs coverage-guided and concolic property testing with obligation-safe counterexample shrinking and differential C/Rust migration harnesses. Every generated input and simulated scheduling decision flows from a flat `ChoiceTape` so failing traces replay deterministically and shrink to minimal reproducers without violating linear resource cleanup.

Crucible is implemented in `crates/vesper-crucible` (with C/Rust lifting in `crates/vesper-migrate` and CLI tools in `crates/vesper-tools/src/fuzz.rs` and `migrate.rs`), shipping `vesper fuzz <file.ves> --target <fn> [--native] [--native-coverage] [--differential <fn>]`, `vesper verify --refute <file.ves> --contract <fn>`, `vesper test <FILE|DIR> --mutate`, and `vesper migrate --source <path> -o <dir> --artifact <lib>`. Termination, migration header equivalence, and sleep-set DPOR models are proved in `lean/CrucibleShrink.lean`, `lean/CrucibleMigrationSafety.lean`, and `lean/CrucibleDpor.lean`. Per `REVIEW-STATUS.md`, `TYPE-09` confines Bitwuzla solving to finite `QF_UFBV` bit-vector guards while shipped `fuzz.rs` restricts CLI parameter carriers to `i64`, `u64`, `u32`, `u8`, and `bool`, `ECO-02` through `ECO-04` require manual review of foreign pointer aliasing in lifted signatures, and `SYS-04` leaves hardware MPK/PKRU sandbox isolation unverified.

## 22.1 Explore program behavior with properties

A property states what should hold across inputs, such as decoding an encoded record giving the original value. Coverage guides exploration toward unseen branches. Concolic testing pairs a concrete run with symbolic branch conditions to look for another input. One counterexample can refute a universal claim.

<!-- technical-detail -->

`vesper fuzz` drives checked functions from a `ChoiceTape` in the reference evaluator and, with `--native`, cross-checks every execution against the Cranelift JIT. Passing `--native-coverage` compiles the target with Stage 5 Cranelift edge counters (`CoverageFeedbackMap` power-of-2 hit buckets) and branch comparison-operand logging (`vesper::native_coverage`), pairing input-to-state mutation with Bitwuzla `QF_UFBV` bit-vector path negation when a guard stalls. In shipped `fuzz.rs`, CLI fuzz and `--refute` targets accept scalar `i64`, `u64`, `u32`, `u8`, and `bool` parameters; before attempting a Lean proof, `vesper verify --refute` searches for counterexamples to a `bool` contract under an optional `--requires` predicate and writes shrunk failures as formatted `@test` regression functions via `--regressions`, while `vesper test --mutate` measures suite adequacy against source mutants.

    $ vesper fuzz src/codec.ves --target parse_frame --native-coverage --differential shadow_frame --regressions reg.ves
    $ vesper verify --refute src/codec.ves --contract prop_roundtrip --requires valid_len --native-coverage
    $ vesper test src/codec.ves --mutate --max-mutants 128
    $ vesper migrate --source legacy/codec.h -o port/ --artifact target/release/libcodec.a

## 22.2 Shrink a failing choice tape

Record generated values and simulated scheduling choices. After a failure, try shorter tapes or smaller choices and retain a candidate only if it reproduces the failure with valid resource lifetimes. A two-event trace can explain a scheduling bug more clearly than the original thousand events.

<!-- technical-detail -->

Attaching shrinkers to types breaks down when shrinking interleaved `SimPulse` fiber schedules or stateful `Mode::Linear` protocols such as `WriteTxn`. Crucible records both `FuzzSource` data draws and simulated scheduling choices on a single `ChoiceTape` of `u64` words (complemented by sleep-set DPOR schedule exploration). On failure, two-phase shortlex shrinking strictly lowers `ShrinkKey = (tape.len, sum(tape))` while re-running the unmodified handler so every intermediate candidate stays 100 percent linear-obligation-valid.

    ChoiceTape [u64; N]  <═══════ Bitwuzla QF_UFBV Negated Guard / Mutated Seed
           │
           v
    ┌─────────────────────────────────────────────────────────────────────────┐
    │ Handler Execution: FuzzSource draws + SimPulse seeded schedule          │
    │ Cranelift CoverageFeedbackMap (virgin_edges) + Evaluator Cross-Check    │
    └──────┬───────────────────────────────────────────┬──────────────────────┘
           │ novel edge / pass                         │ Failure / Refutation!
           └──> Corpus                                 v
                                       ┌──────────────────────────────────────┐
                                       │ Two-Phase Shortlex Shrinker          │
                                       │ 1. Delete spans  (len' < len)        │
                                       │ 2. Binary-zero   (len'=len, sum'<sum)│
                                       │ Accept iff fails & Mode::Linear ok   │
                                       └───────────────┬──────────────────────┘
                                                       v
                                           Minimal @test Regression Case

~~~vesper
linear effect FuzzSource {
    fn draw_u64_bounded(max_inclusive: u64) -> u64;
    fn draw_bytes[r: Region](in arena: RegionCap[r], max_len: usize) -> RegionSlice[r, u8];
    fn assume_guard(cond: bool) -> Unit;
}
@fuzz_prop(concolic = true, max_runs = 1_000_000)
fn prop_wal_crash_recovery_idempotent[P: IsaProfile]()
    -> Result[Unit, WalError] eff [FuzzSource, Isa[P], Fail[WalError]]
{
    Region::scoped(256.kib(), |cap scratch| {
        let raw_payload = FuzzSource::draw_bytes(in scratch, 4096);
        let crash_after_sqe = FuzzSource::draw_u64_bounded(8);
        let schedule_seed = FuzzSource::draw_u64_bounded(0xFFFF_FFFF_FFFF_FFFF);
        SimPulse::run_seeded(schedule_seed, crash_after_sqe, || {
            let mut store = MemNvmeStorage::with_region(in scratch);
            let tx: WriteTxn = store.begin_txn(4096);
            match store.stage_and_commit_checked[P](sink tx, in raw_payload) {
                Ok(receipt) => store.verify_recovered_slot(receipt.wal_slot, in raw_payload),
                Err(_io_fault) => store.verify_zero_orphaned_pages(),
            }
        })
    })
}
~~~

## 22.3 Compare implementations during a migration

When replacing a C or Rust function, run both versions against equivalent inputs and compare their observable results. Account for aliasing, state updates, and external effects. Comparing a returned integer alone could miss a changed buffer. A proposed lifted signature also needs review against the foreign caller’s actual contract.

<!-- technical-detail -->

`vesper migrate --source <path> -o <dir> --artifact <lib>` lifts preprocessor-free C headers (`.h`), Rust files (`.rs`), or Rust crates (`Cargo.toml` plus `src/lib.rs`) into `<stem>.ves` declarations, `<stem>.migrate.txt` notes, and a Bridge `<stem>.vsys` manifest; omitting `--artifact` suppresses `.vsys` emission because every Bridge manifest must pin a concrete library `BLAKE3` digest. Differential harnesses (`--differential` and `@differential_shadow`) compare both return codes and mutated state across fuzz corpora.

~~~vesper
// 1. Bridge .vsys manifest synthesized by `vesper migrate` (pinned to artifact BLAKE3):
sys library LegacyCodecAbi (rust_crate = "legacy_codec ^0.8.0") {
    fn legacy_parse_batch_header(
        nocapture in raw_bytes: CSlice[u8],
        nocapture inout out_crc: u32,
        nocapture inout out_count: i32,
    ) -> ForeignVal[i32];
}
// 2. Lifted lifetime-free Vesper parser over a 61-byte KafkaBatchWireHeader:
fn vesper_parse_batch_header(
    in raw_bytes: ByteSlice,
    inout state.{last_crc, record_count}: DecoderStats,
) -> Result[i64, WireLayoutError] eff [Fail[WireLayoutError]] {
    let in hdr = raw_bytes.view_wire_prefix[KafkaBatchWireHeader]()?;
    state.last_crc = hdr.crc.to_native();
    state.record_count = hdr.records_count.to_native();
    Ok(hdr.base_offset.to_native())
}
// 3. Differential shadow harness comparing C/Rust outputs and state mutations:
@differential_shadow
fn shadow_verify_legacy_vs_vesper(
    in raw_input: ByteSlice,
) -> Result[Unit, DiffMismatch] eff [Ffi[LegacyCodecAbi], Fail[DiffMismatch]] {
    let mut legacy_crc: u32 = 0;
    let mut legacy_cnt: i32 = 0;
    let legacy_rc = handle Ffi[LegacyCodecAbi] isolated(mpk) in {
        LegacyCodecAbi::legacy_parse_batch_header(
            in raw_input.as_cslice(), inout legacy_crc, inout legacy_cnt,
        ).validate_i32()
    };
    let mut v_stats = DecoderStats { last_crc: 0, record_count: 0 };
    let vesper_res = handle Fail[WireLayoutError] in {
        Ok(vesper_parse_batch_header(in raw_input, inout v_stats.{last_crc, record_count}))
    } with {
        return r => r,
        raise(e) => Err(e),
    };
    Crucible::assert_differential_equiv(legacy_rc, legacy_crc, legacy_cnt, in vesper_res, in v_stats)
}
~~~

Two `REVIEW-STATUS.md` caveats apply directly when migrating foreign code. Under `ECO-02` through `ECO-04`, lifted `nocapture` and disjoint `inout` projections reflect syntactic C/Rust parameter inspection and must be audited manually against hidden foreign pointer aliasing, interior mutability, or shared globals; under `SYS-04`, the `isolated(mpk)` (`WRPKRU` / `POR_EL0`) hardware sandbox shown above remains a proposed design control rather than a verified isolation boundary.

## 22.4 Explain why reduction stops

Ordering tapes by length and then lexicographically gives a shrinking algorithm a measure of progress. A successful reduction must move downward. Agreement of two implementations on a test corpus gives evidence for those cases. A general equivalence theorem also needs a model of all relevant observations.

<!-- technical-detail -->

Three Lean 4 developments model Crucible's guarantees with zero custom axioms. `lean/CrucibleShrink.lean` proves `smaller_wf`, `choice_tape_shrink_terminates`, `delete_span_smaller`, `set_lower_smaller`, and total `shrink` termination under `Prod.Lex (· < ·) (· < ·)` on `(tape.length, tapeSum tape)`. `lean/CrucibleMigrationSafety.lean` proves `choice_tape_shrink_terminates` alongside full round-trip and parser equivalence (`migrated_batch_header_matches_spec`) for the signed big-endian 61-byte `KafkaBatchWireHeader`. `lean/CrucibleDpor.lean` proves `every_trace_class_visited`: sleep-set depth-first exploration under symmetric action independence (`Commutes`) visits at least one representative of every Mazurkiewicz trace class within the fuel bound (without claiming FG05 persistent sets or Rust code refinement).

    -- lean/CrucibleShrink.lean & lean/CrucibleMigrationSafety.lean & lean/CrucibleDpor.lean
    theorem choice_tape_shrink_terminates
        (len1 len2 sum1 sum2 : Nat)
        (h_step : len2 < len1 ∨ (len2 = len1 ∧ sum2 < sum1)) :
        Prod.Lex (· < ·) (· < ·) (len2, sum2) (len1, sum1) := by
      rcases h_step with h_lt | ⟨h_eq, h_sum⟩
      · exact Prod.Lex.left _ _ h_lt
      · subst h_eq; exact Prod.Lex.right _ h_sum
    ----
    theorem migrated_batch_header_matches_spec (hdr : KafkaBatchWireHeader) (st : DecoderStats) :
        (vesperParseBatchHeader (Wire.toBytes hdr) st).map Prod.fst = .ok hdr.baseOffset.toNative := by
      simp [vesperParseBatchHeader]
    ----
    -- CrucibleDpor: sleep-set DFS from empty sleep set visits every maximal Mazurkiewicz trace class <= fuel:
    theorem every_trace_class_visited (hcomm : Commutes step indep) (w : List α) ...

## 22.5 Preserve the smallest useful replay

The shrinker proposes edits to a recorded choice sequence. A projection lifter infers candidate access paths from source operations. A test author needs a reproducible failure, while a migration author needs to know which reads and writes the new signature permits.

<!-- technical-detail -->

In `crates/vesper-crucible`, `ChoiceTape::shrink_failure` enforces the exact `ShrinkKey` descent modeled in `lean/CrucibleShrink.lean`, `CoverageFeedbackMap::record_iteration` updates `virgin_edges` hit buckets, and `lift_rust_param_to_vesper_projection` synthesizes lifetime-free `in`, `inout x.{...}`, or `sink` signatures from extracted read/write field paths.

    // crates/vesper-crucible/src/lib.rs
    pub struct ChoiceTape { pub values: Vec<u64> }
    impl ChoiceTape {
        pub fn shrink_failure(&self, mut reproduces_failure: impl FnMut(&Self) -> bool) -> Self {
            // Phase 1: chunk deletion (halving chunk = len/2 .. 1) strictly lowers tape.len()
            // Phase 2: per-index binary search [0..best[i]) strictly lowers sum(tape)
        }
    }
    pub struct CoverageFeedbackMap { pub virgin_edges: Vec<u8>, pub discovered_edges: usize }
    pub fn lift_rust_param_to_vesper_projection(summary: &LegacyRustParamSummary) -> String;

# 23 Chrono recording and reverse debugging

When a service stops with a missing ticket, its final memory snapshot shows which resources remain. Chrono’s transfer history can show where the missing ticket last had an owner.

<!-- technical-detail -->

When a long-running service stalls or corrupts a field, a final core dump shows what remains in memory but not which earlier transition moved a linear handle or overwrote a projection. Vesper Chrono records nondeterministic outcomes at lexical effect boundaries and indexes both `inout` mutations and `Mode::Linear` obligation hops for backward inspection.

Chrono is implemented in `crates/vesper-chrono` with CLI front ends in `crates/vesper-tools/src/replay.rs` and `top.rs`, shipping `vesper replay <bundle.vchrono> (--key <hex> | --key-file <path>) [--timeline] [--watch <place> [--before <seq>]] [--lineage <id>]` and `vesper top --chrono <bundle.vchrono> (--key <hex> | --key-file <path>)`. Interaction-tree replay, snapshot folds, lineage completeness, and reverse watchpoints are proved in `lean/ChronoReplay.lean`. Per `REVIEW-STATUS.md` and `crates/vesper-tools/src/replay.rs`, the shipped CLI verifies keyed-BLAKE3 `.vchrono` MACs and replays event transcripts, watchpoints, and obligation lineages without restoring full process-image memory snapshots, while the proposed sub-2-percent recording overhead, hardware IPT/ETM branch reconstruction, and external FFI nondeterminism bounds remain workload- and runtime-dependent.

## 23.1 Record what makes a run nondeterministic

An effect-boundary recorder captures observable inputs such as clock readings, incoming completions, and scheduling outcomes. Checkpoints give replay a starting state. Hardware branch traces can help explain execution between events. Sensitive inputs need a redaction policy before recording.

<!-- technical-detail -->

Chrono interposes a lock-free recording handler on the lexical `EvidenceVector` (`r15` on `x86_64`, `x28` on AArch64) across nondeterministic capabilities (`Net`, `Storage`, `Clock`, `RdmaQp`, and `Ffi`) so pure computation between effect boundaries runs without syscall traps. In the full architecture, zero-`Pin` relocatable fiber stacks and contiguous `VirtualArena` slabs allow incremental copy-on-write shard checkpoints (`userfaultfd` write-protect on Linux, Mach `vm_copy` on macOS, and `MEM_RESET` tracking on Windows) at fiber yield points, while optional hardware branch tracing synchronizes Intel Processor Trace (`IPT`) or Arm CoreSight `ETM` rings via `ipt_tsc_anchor` (`rdtscp` / `cntvct_el0`). Before serializing any `.vchrono` bundle, Chrono excludes `Vesper Vault` `SecretSlab` pages (`MADV_DONTDUMP`), scrubs `@pii` fields, and authenticates retained records under a 32-byte keyed-BLAKE3 MAC.

## 23.2 Decode a flight-log entry

A compact wire record identifies an event and the data required to replay it. Preserve event order and identities when stitching records together. A linear resource’s lineage connects creation, transfers, and final consumption across functions or fibers.

<!-- technical-detail -->

Every recorded effect completion begins with a padding-free 32-byte `ChronoEventWireHeader` storing a monotonic `lamport_seq`, `fiber_id`, hardware `ipt_tsc_anchor`, `effect_id`, `op_tag`, and `payload_byte_len`. Alongside effect records, Chrono tracks `ObligationHop` chains (`Mint`, `SinkMove`, `ChannelHandoff`, `UnpackDischarge`) under `linear effect ChronoReplay` so `ReplaySession` itself obeys linear open-to-close cleanup.

~~~vesper
// Exact 32-byte padding-free flight-log frame header:
wire struct ChronoEventWireHeader {
    lamport_seq: u64le,
    fiber_id: u64le,
    ipt_tsc_anchor: u64le,
    effect_id: u16le,
    op_tag: u16le,
    payload_byte_len: u32le,
}
copy struct TraceCursor { epoch_id: u32, lamport_seq: u64 }
copy struct ObligationHop {
    lamport_seq: u64,
    fiber_id: u64,
    source_span_id: u32,
    hop_kind_tag: u8,
}
linear struct ReplaySession { bundle_fd: u32, active_cursor: TraceCursor }
linear effect ChronoReplay {
    fn open_bundle(in path: ByteSlice) -> Result[ReplaySession, ReplayError];
    fn reverse_watch_inout(
        inout session: ReplaySession,
        local_place_hash: u64,
    ) -> Result[TraceCursor, ReplayError];
    fn query_linear_lineage[r: Region](
        in session: ReplaySession,
        in scratch: RegionCap[r],
        obligation_id: u64,
    ) -> RegionSlice[r, ObligationHop];
    fn close_session(sink session: ReplaySession) -> Unit;
}
~~~

## 23.3 Work backward from a bad outcome

Find the failure event, restore a preceding checkpoint, and replay the recorded decisions. A reverse watchpoint seeks the previous write to a particular location. A lineage query asks who owned a particular resource at the time. These questions can produce different answers if a slot was reused.

<!-- technical-detail -->

On a stalled shard or failed invariant, operators inspect open `Mode::Linear` obligations and wait-for cycles with `vesper top --chrono` and query the authenticated `.vchrono` bundle via `vesper replay` or a programmatic `ChronoReplay` triage routine. Note that in shipped `crates/vesper-tools/src/replay.rs`, `vesper replay` folds retained events through an authenticated BLAKE3 transcript chain and answers `--timeline`, `--watch`, and `--lineage` queries directly from the verified log; reconstructing live domain state from checkpoint bytes is an embeddable library `Replayer` trait hook rather than an OS process-snapshot restore.

    $ vesper top --chrono crash.vchrono --key-file bundle.key
    $ vesper replay crash.vchrono --key-file bundle.key --timeline
    $ vesper replay crash.vchrono --key-file bundle.key --watch 0x8fa1_2204 --before 9400
    $ vesper replay crash.vchrono --key-file bundle.key --lineage 42

~~~vesper
fn diagnose_stalled_wal_bundle(
    in bundle_path: ByteSlice,
    stalled_txn_obligation_id: u64,
    corrupted_index_place_hash: u64,
) -> Result[(TraceCursor, u32), ReplayError] eff [ChronoReplay, Fail[ReplayError]] {
    let mut sess: ReplaySession = ChronoReplay::open_bundle(in bundle_path)?;
    // [T-Let] forbids `?` across Fail[ReplayError] while linear `sess` is live:
    match ChronoReplay::reverse_watch_inout(inout sess, corrupted_index_place_hash) {
        Ok(culprit_cursor) => {
            let hop_count = Region::scoped(64.kib(), |cap arena| {
                let hops = ChronoReplay::query_linear_lineage(
                    in sess, in arena, stalled_txn_obligation_id,
                );
                hops.len() as u32
            });
            ChronoReplay::close_session(sink sess);
            Ok((culprit_cursor, hop_count))
        }
        Err(err) => {
            ChronoReplay::close_session(sink sess);
            Fail::raise(err)
        }
    }
}
~~~

## 23.4 Identify the replay assumptions

Deterministic replay requires an initial state and every nondeterministic observation used by the model. A lineage completeness property requires every modeled transfer to emit an event. Unrecorded external input or an incomplete log limits what a replay can establish.

<!-- technical-detail -->

`lean/ChronoReplay.lean` proves four model theorems with zero custom axioms: `chrono_replay_reproduces_live_execution` (replaying a recorded `(op, answer)` `Tape` over an interaction tree `Prog α` reproduces the live output, consumes the exact tape, and re-emits the identical trace), `replay_from_snapshot` and `checkpoint_restore_replays_to_live` (suffix replay from an epoch state equals full-prefix execution, while `restore_rejects_relocation` guards base-address identity), `lineage_complete` (every moved, discharged, or leaked obligation in a valid hop log has a unique preceding `mint`), and `reverse_watch_finds_latest_mutation`. Unrecorded shared-memory data races or FFI mutations outside modeled effects lie outside the replay guarantee.

    -- lean/ChronoReplay.lean
    theorem chrono_replay_reproduces_live_execution {α : Type}
        (env : Nat → Nat → Nat) (p : Prog α) :
        let live := recordRun env 0 p
        replay live.2 p = some (live.1, live.2, []) := by
      have h := replay_recorded_prefix env p 0 []
      simpa using h
    ----
    theorem reverse_watch_finds_latest_mutation
        (writes : List Nat) (queryTs hitTs : Nat)
        (h_mem : hitTs ∈ writes) (h_lt : hitTs < queryTs)
        (h_max : ∀ w ∈ writes, w < queryTs -> w ≤ hitTs) :
        hitTs < queryTs ∧ ∀ w ∈ writes, hitTs < w -> queryTs ≤ w := ...

## 23.5 Index a trace for repeated questions

An index from a location to ordered writes speeds up reverse watchpoints. An index from resource identity to transfers supports ownership queries. Keep the allocation generation with the identity so two resources that occupied the same address can have separate histories.

Consider a trace in which two transactions use the same storage slot.

    Event 10   mint T1 in slot 4
    Event 12   transfer T1 to the log writer
    Event 15   consume T1 at commit
    Event 21   mint T2 in slot 4

A watchpoint on slot 4 can return events from both transactions. A query for T1 ends at event 15. Following the slot into event 21 would incorrectly attribute a second lifetime to T1.

<!-- technical-detail -->

`ChronoTimelineIndex` in `crates/vesper-chrono` indexes writes by `place_hash` (`writes_by_place`) and resource hops by monotonic 64-bit `obligation_id` (`linear_lineages`) so physical slot reuse never conflates distinct linear lifetimes, rejecting any duplicate consumption after `LineageStepKind::UnpackedTerminal` with `ChronoError::DoubleTerminalDischarge`.

Consider a trace in which two transactions reuse storage slot 4 (`place_hash = 0x04`):

    Lamport:  [Snap E0] ──> Seq 10 ──────> Seq 12 ──────> Seq 15 ──> [Snap E1] ──> Seq 21 ──> Seq 25
    Event:                  mint T1        transfer T1    consume T1               mint T2    (query)
    Slot 4:                 write(0x04)    write(0x04)    write(0x04)              write(0x04)   │
    id=101:                 [Minted] ────> [SinkMoved] ─> [UnpackedTerminal]                     │
    id=102:                                                                        [Minted] ─────┤
    Watch:    find_last_writer_before(0x04, 25) <═════════(nearest Snap E1 + hit Seq 21)═════════┘
    Lineage:  obligation_lineage(101) ══> [Seq 10, Seq 12, Seq 15] (stops at 15; never bleeds into T2!)
    ----
    // crates/vesper-chrono/src/lib.rs
    pub enum ChronoError {
        DoubleTerminalDischarge { obligation_id: u64 },
        UnknownObligation { obligation_id: u64 },
    }
    impl ChronoTimelineIndex {
        pub fn find_last_writer_before(
            &self, place_hash: u64, before_seq: u64,
        ) -> Option<(Option<SnapshotEpoch>, InOutWriteEvent)> { /* partition_point O(log N) */ }
        pub fn record_obligation_hop(
            &mut self, obligation_id: u64, hop: ObligationLineageEvent,
        ) -> Result<(), ChronoError> { /* errors with DoubleTerminalDischarge after UnpackedTerminal */ }
    }

A reverse watchpoint `find_last_writer_before(0x04, 25)` inspects `writes_by_place` and sees mutations from both `T1` and `T2`, paired with the nearest preceding `SnapshotEpoch` (`E1`). By contrast, `obligation_lineage(101)` terminates cleanly at event `15` (`UnpackedTerminal`), never attributing slot 4's event `21` lifetime (`obligation_id = 102`) to `T1` and catching any erroneous second discharge of `101` with `DoubleTerminalDischarge`.

# 24 Metronome performance measurements

To compare two versions of a hot loop, decide what work you want to measure, which hardware events represent it, and how much the measurement itself can vary.

<!-- technical-detail -->

Comparing two versions of a hot path requires isolating the measured work from setup noise, accounting for hardware counter wraparound, and separating genuine algorithmic changes from memory-layout accidents or shared host drift. Vesper Metronome combines hardware performance monitoring unit (PMU) counters, interleaved A/B execution, layout randomization, and non-parametric confidence-interval gates.

Metronome is implemented in `crates/vesper-bench` and `crates/vesper-bench-pmu` with CLI execution in `crates/vesper-tools/src/bench.rs` and CI port gating in `scripts/ci/flip_gate.py`, shipping `vesper bench FILE [--compare git:REV] [--pmu] [--pmu-provider group|rdpmc|kpc] [--randomize-layout N] [--deterministic] [--asm] [--expect-vectorized] [--expect-zero-alloc]`. Counter wraparound and paired drift arithmetic are modeled in `lean/MetronomeArithmetic.lean`. Per `REVIEW-STATUS.md` and `crates/vesper-tools/src/bench.rs`, the shipped CLI compiles bounded nongeneric pure scalar kernels (leaving recursive, aggregate, or fallible kernels and the library-only `vesper_bench::causal` profiler to Rust/library harnesses), `--randomize-layout` perturbs child-process heap, stack, and environment offsets rather than relocating JIT machine code, and `--deterministic` counts reference-evaluator steps rather than proving constant hardware execution time.

## 24.1 Pick counters that answer the question

A performance monitoring unit (PMU) counts events such as cycles, instructions, or cache misses. Compare equivalent workloads on a stated machine profile. A lower cycle count on one machine is evidence about that measurement setup. It is a different claim from an allocation-free source contract.

<!-- technical-detail -->

`crates/vesper-bench-pmu` reads hardware counters via Linux `perf_event_open` group syscalls (`--pmu-provider group`), Linux `x86_64` user-space `RDPMC` over mapped perf pages (`--pmu-provider rdpmc`), or macOS Apple Silicon fixed `kpc` counters (`--pmu-provider kpc`), along with optional `--pmu-events` (`ref-cycles`, `l1d-read-misses`, `llc-read-misses`, `branch-mispredicts`, `simd-ops`, `tma-slots`, `tma-backend-bound`). When `--pmu` is requested and hardware counters are unavailable (for instance under restrictive `perf_event_paranoid` or unprivileged `kpc`), `vesper bench` reports an explicit `UnavailableReason` and exits inconclusive without silently falling back to wall-clock timers.

Hardware cycle counts and structural source contracts answer distinct questions. Passing `--expect-zero-alloc` audits per-thread heap allocations across every measured window via `CountingAllocator`, `--expect-vectorized` and `--asm` inspect Cranelift CLIF value types and emitted SIMD instructions directly, and `--deterministic` counts exact reference-evaluator steps with zero variance.

    $ vesper bench benches/hash.ves --compare git:main --pmu --pmu-provider rdpmc --max-regression-bp 100
    $ vesper bench benches/hash.ves --randomize-layout 16 --signed-rank
    $ vesper bench benches/simd_scan.ves --asm --expect-vectorized --expect-zero-alloc
    $ vesper bench benches/scalar_step.ves --deterministic --json

## 24.2 Measure a batch that consumes its inputs

A benchmark context can separate preparing a batch of linear tickets from measuring the operation that consumes them. Recreate equivalent inputs for the next batch. Snapshot the counters before and after measured work. The counter width matters when interpreting their difference.

<!-- technical-detail -->

Benchmarking a consuming operation such as `WriteTxn::commit(sink self)` must not time input allocation or clone overhead, and optimization barriers must not force SIMD registers onto the stack. Metronome records per-window counters into a 64-byte L1-cache-line `PmuSampleWire` struct, provides a register-preserving Stage 5 Cranelift `black_box` value barrier, and exposes `BenchHarness::iter_linear_batched` to mint linear batches inside an untimed `Region` arena before bracketing only the `sink` consumer with PMU fences.

~~~vesper
// Exact 64-byte (1 L1 cache line, 0 implicit padding) PMU & TMA sample wire record:
wire struct PmuSampleWire {
    ref_cycles: u64le,
    instructions_retired: u64le,
    l1d_read_misses: u64le,
    llc_misses: u64le,
    branch_mispredicts: u64le,
    simd_vec_uops_retired: u64le,
    arena_bump_bytes: u64le,
    tma_backend_bound_slots_bp: u32le,
    icache_rand_offset_bytes: u32le,
}
fn black_box[T: mod linear](sink val: T) -> T eff [];
linear effect BenchHarness {
    fn iter_linear_batched[In: mod linear, Out: mod affine, r: eff](
        setup: fn(in RegionCap[bench_reg], usize) -> In eff [bench_reg: Region],
        body: fn(sink In) -> Out eff [r],
    ) -> PmuSampleWire eff [r];
    fn causal_progress_point(point_name: StaticStr) -> Unit;
}
~~~

## 24.3 Express a gate for a real workload

For a SIMD kernel, measure a range of lengths including a tail. For log transactions, decide whether preparation and persistence belong in the measured interval. A benchmark annotation can give the harness a budget to compare with those observations. Keep compiler optimizations from deleting unused results.

<!-- technical-detail -->

In full Vesper benchmark suites, `@bench(...)` annotations attach zero-allocation, SIMD vectorization, L1D miss, and instruction-per-byte budgets directly to workload functions, while shipped CLI `vesper bench` runs direct scalar kernels and `vesper_bench::causal` provides library-level virtual-speedup profiling for multi-thread host pipelines.

~~~vesper
@bench(
    zero_alloc = true,
    expect_vectorized = true,
    max_instructions_per_byte = 2.2,
    stabilizer_pages = 16,
)
fn bench_simd_json_ndjson_ingest[P: IsaProfile](
    in sample_ndjson_64k: AlignedSlice[u8, 64],
) -> u32 eff [BenchHarness, Isa[P], Fail[JsonError]] {
    Region::scoped(256.kib(), |cap scratch| {
        let mut tiles = TiledSeq::with_region[TelemetryPoint](in scratch, 1024);
        let input = black_box(sample_ndjson_64k.as_bytes());
        let parsed = ingest_ndjson_to_tiles[P, scratch](in scratch, in input, inout tiles)?;
        black_box(parsed)
    })
}
@bench(zero_alloc = true, max_l1d_misses_per_op = 4)
fn bench_linear_wal_commit_fastpath() -> PmuSampleWire eff [BenchHarness, Storage] {
    BenchHarness::iter_linear_batched(
        |in arena, idx| Storage::mock_staged_txn_in_region(in arena, idx as u64, 512),
        |sink tx: WriteTxn| {
            handle Fail[IoError] in {
                Ok(black_box(tx.commit()))
            } with {
                return res => res,
                raise(e) => Err(e),
            }
        },
    )
}
~~~

## 24.4 Account for wraparound and shared drift

A hardware counter can wrap between samples. Modular subtraction recovers the delta only under a bound on the elapsed count. Interleaved A/B pairs can cancel some common drift. If the environment changes differently for the two cases, that assumption needs another check.

Take a toy eight-bit counter. A start of 250 and an end of 4 represent ten events if fewer than 256 events occurred. Another complete trip around the counter gives the same two readings. Choose a sampling interval short enough to rule out that extra wrap.

<!-- technical-detail -->

Take a toy 8-bit counter: a start reading of `250` and an end reading of `4` represent `(4 - 250) mod 256 = 10` events provided fewer than `256` events elapsed, whereas a second full wrap produces the same readings. On `x86_64`, user-space `RDPMC` performance counters are 48 bits wide (`mask48 = (1 << 48) - 1`): masked modular subtraction `((endRaw - (startRaw &&& mask48)) &&& mask48)` recovers the exact elapsed count whenever `delta < 2^48` (`rdpmc_48bit_wrap_delta_exact` via `bv_decide`, alongside `width64_modular_delta_exact` in `lean/MetronomeArithmetic.lean`).

Similarly, running interleaved pairs cancels common additive thermal or hypervisor drift `commonDrift` in paired differences `(b + commonDrift) - (a + commonDrift) = b - a` (`paired_interleaving_cancels_common_drift` and `paired_additive_drift_difference`). Crucially, `lean/MetronomeArithmetic.lean` also proves `common_drift_ratio_counterexample`: additive drift does not cancel in a ratio `(b + d) / (a + d)`, which is why Metronome inspects paired differences alongside median ratios.

    -- Manuscript 48-bit x86_64 RDPMC wrap-exactness & lean/MetronomeArithmetic.lean
    theorem rdpmc_48bit_wrap_delta_exact (startRaw delta : BitVec 64)
        (h_delta_fits : delta < (1#64 <<< 48)) :
        let mask48 : BitVec 64 := (1#64 <<< 48) - 1#64
        let endRaw : BitVec 64 := (startRaw + delta) &&& mask48
        ((endRaw - (startRaw &&& mask48)) &&& mask48) = delta := by
      bv_decide
    ----
    theorem paired_interleaving_cancels_common_drift (trueA trueB commonDrift : Int) :
        let measA := trueA + commonDrift
        let measB := trueB + commonDrift
        measB - measA = trueB - trueA := by
      omega
    ----
    -- lean/MetronomeArithmetic.lean: additive drift cancels in differences, NOT ratios:
    theorem paired_additive_drift_difference (a b commonDrift : Int) :
        (b + commonDrift) - (a + commonDrift) = b - a := by omega
    theorem common_drift_ratio_counterexample : (2 + 1) * 1 ≠ 2 * (1 + 1) := by decide

## 24.5 Compare paired measurements

Pair baseline and candidate runs, vary layout where appropriate, and inspect the distribution of differences. A gate should report the affected measure and uncertainty. Compare the generated assembly or use causal profiling to investigate which change slowed the measured work.

<!-- technical-detail -->

`crates/vesper-bench` executes paired `ABBA` (`A, B, B, A`) epochs (`run_abba`) to balance linear drift within each epoch, generates SplitMix64 64-byte cache-line offsets (`stabilizer_icache_offset_bytes`) and child-process stack/heap/environment padding (`--randomize-layout`) to verify that speedups survive layout perturbation, and computes Hodges-Lehmann Walsh-average shift estimates (`hodges_lehmann.rs`), exact binomial order-statistic 95 percent median confidence intervals (`analyze_pairs`), and exact Wilcoxon signed-rank p-values (`signed_rank_exact_paired_samples`).

    Epoch k (SplitMix64 64B Offset + Stack/Heap/Env Padding):
    ┌─────────────────┐   RDPMC   ┌───────────────────────────┐   RDPMC   ┌──────────────────┐
    │ Untimed Region  │ ────────> │ Paired ABBA Quanta        │ ────────> │ PmuSampleWire    │
    │ Linear Setup    │  start48  │ [ A(Rust) -> B -> B -> A ]│   end48   │ (64B, 48b delta) │
    └─────────────────┘           └───────────────────────────┘           └────────┬─────────┘
                                                                                   │
         ┌─────────────────────────────────────────────────────────────────────────┘
         v
    ┌────────────────────────────────────────────┐     ┌─────────────────────────────────────┐
    │ Statistical Engine (crates/vesper-bench)   │     │ CI flip-gate (scripts/ci/flip_gate) │
    │ - Hodges-Lehmann shift & 95% binomial CI   │ ──> │ 1. [[tests]] exit 0                 │
    │ - Exact Wilcoxon signed-rank p-value       │     │ 2. [differential] differences = 0   │
    │ - CountingAllocator 0-alloc & CLIF SIMD    │     │ 3. [[abba]] 95% CI upper <= 1.5x    │
    └────────────────────────────────────────────┘     └─────────────────────────────────────┘

In continuous integration, `scripts/ci/flip_gate.py` and per-crate `flip-gate.toml` manifests (such as `crates/vesper-aegis/flip-gate.toml`) enforce the repository's self-hosting Rust-exit bar before any Rust module is deleted in favor of its Vesper port: every `[[tests]]` suite must exit `0`, `[differential]` fuzzing must report `differences=0`, and every `[[abba]]` workload compiled with `-C target-cpu=native` must keep the upper bound of `vesper-bench`'s 95 percent `ABBA` Vesper/Rust median-ratio interval at or below `1.5x`.

    // CI Rust-to-Vesper port gate (scripts/ci/flip_gate.py & crates/vesper-aegis/flip-gate.toml)
    $ python3 scripts/ci/flip_gate.py vesper-aegis --epochs 30 --threshold 1.5
    ----
    name = "vesper-aegis"
    build = ["cargo build --locked --release -p vesper-aegis --example abba"]
    [[tests]]
    run = "cargo test --locked --release -p vesper-aegis"
    [differential]
    run = "cargo test --locked --release -p vesper-aegis --test port_differential"
    [[abba]]
    workload = "snp"
    run = "crates/vesper-aegis/bench-abba.sh flip-gate snp"
    ----
    // crates/vesper-bench/src/lib.rs: 48-bit RDPMC delta & SplitMix64 64B Stabilizer offset
    pub const PMU_COUNTER_MASK_48: u64 = (1u64 << 48) - 1;
    pub const fn pmu_delta_48bit(start_raw: u64, end_raw: u64) -> u64 {
        end_raw.wrapping_sub(start_raw & PMU_COUNTER_MASK_48) & PMU_COUNTER_MASK_48
    }
    pub fn stabilizer_icache_offset_bytes(seed: u64, epoch: u64) -> u32;
    pub fn evaluate_paired_benchmark_gate(
        spec: &BenchGateSpec, samples: &[PairedEpochSample],
    ) -> Result<i64, BenchGateFailure>;
