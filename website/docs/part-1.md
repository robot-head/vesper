# 1 Why Vesper tracks obligations

An open transaction needs an owner on the error path as well as the success path. If a call can fail before the transaction commits, Vesper checks who will finish it.

<!-- technical-detail -->

An open write transaction or device buffer needs a clear owner on the failure path as well as the normal path. Vesper combines substructural ownership modes, second-class place projections, lexical effect handlers, and proof extraction so call signatures state who holds each obligation, which fields may change, and how an operation may unwind.

Per `content.typ` (lines 55 and 128) and `REVIEW-STATUS.md`, this specification is an active design draft mixing implemented components (`crates/vesper-core`, `crates/vesper-syntax`, `packages/vesper-compiler`, and the host checker in `src/`) with proposed language extensions and non-repository `illustrative/` listings. Memory safety without garbage collection, data-race freedom, and machine-checked functional correctness are architectural goals; the mechanized Lean 4 models under `lean/` verify isolated fragments and do not establish all three properties together for the complete native implementation.

## 1.1 Four research ideas behind a call

Substructural types count uses of a value. Mutable value semantics lends a caller’s storage to a function for the duration of a call. One-shot effects let a handler decide how a suspended operation finishes. Liquid refinements attach facts such as an index bound to a type. In a buffer write, ownership identifies who must keep the storage live. The loan identifies the writable bytes, an index bound constrains the write, and an effect tells the caller how it can fail.

<!-- technical-detail -->

At a single call site, four type-level mechanisms meet: substructural modalities count dynamic uses, mutable value semantics lends disjoint storage paths for the duration of the call, one-shot lexical effects bound how a handler resumes or aborts a suspended frame, and liquid refinements attach decidable arithmetic predicates and erased `@0` tokens to places. Two compiler and verification streams complete the six-stream architecture by translating mutable and effectful code into prophecy-free Lean 4 definitions and optimizing intermediate graphs through capability-threaded equality saturation: Tree Borrows and OxCaml mode crossing avoid `'a` region variables, Affect forbids multi-shot continuation cloning of linear stack resources, and second-class borrows let extracted Lean 4 functions discharge proofs via `bv_decide`, `omega`, and `grind` without separation-logic framing.

| Stream | Representative Work | Vesper Design Role |
| --- | --- | --- |
| Modal & Substructural Ownership | OxCaml Mode Crossing (ICFP 2026); POPL 2025; IJCAR 2026 | Orthogonal `ModeVec` axes (`copy`/`affine`/`linear`, `portable`/`pinned`, `uncontended`/`contended`, `unordered`/`lifo`) without `'a` lifetimes |
| Mutable Value Semantics & PCGs | Hylo (IWACO 2026); Tree Borrows (PLDI 2025); PCGs 2026 | Second-class `in`/`inout`/`out`/`sink` projections over trie field trees and disjoint slice intervals without GC or ARC |
| One-Shot Lexical Effects | Affect (POPL 2025); Selection Monad; OOPSLA 2025 | Continuations typed as `affine` or `linear` so handlers never duplicate linear stack resources; lowered via evidence registers |
| Decidable Liquid Refinements | Flux (POPL 2025); RefinedRust 2.0; QTT (`@0`) | Refinement predicates `{ v: T \| phi }`, 0-bit `@0` ghost permissions, and strong `~>` updates on exclusive places |
| Prophecy-Free Proof Extraction | Charon + Aeneas & Hax (2026); Lean4Lean; VCVio (2026) | 1-to-1 translation into pure Lean 4 state-tuple functions and `EffM` trees without loan graphs or backward prophecies |
| Effectful Equality Saturation | Effectful E-Graphs / `eggcc` (OOPSLA 2026); `egglog` | State- and capability-threaded Datalog e-graphs fusing tail-resumptive handlers and emitting rewrite witnesses |

## 1.2 When automatic cleanup leaves a choice open

An affine owner can drop a resource when it goes out of scope. That works for releasing memory, but committing and rolling back a transaction have different meanings. Making the transaction linear forces every path to choose an outcome. Projected loans let two helpers work on disjoint fields of the same record.

Async code raises the same ownership question at suspension. A pinned stack or future may still have pointers into its storage. Dropping a pending operation can skip its ordinary cleanup path. The manuscript explores lexical effects to expose these decisions at calls. At build time, a dependency or macro could inherit the compiler’s access to files and network services. The design lets a package limit that access.

<!-- technical-detail -->

Affine destructors and first-class lifetime references leave five structural bottlenecks that motivate Vesper's core design. First, affine (`at-most-once`) ownership treats `mem::forget` as sound and restricts `Drop` to synchronous, infallible scope exit, so the type checker cannot force every path to choose between committing and rolling back a write-ahead log transaction, harvesting an `io_uring` buffer, retiring a QUIC AEAD nonce ticket, or reseeding a confidential-VM snapshot. Second, poll-based `async` state machines color APIs, require `Pin` wrappers to stabilize self-referential borrows across yields, and silently drop partially mutated state when a race cancels a future at a suspension point.

Third, reifying borrows as first-class pointer types (`&'a T` and `&'a mut T`) triggers viral `'a` lifetime cascades across structs, traits, closures, and combinators, while `&mut self` receiver borrows routinely block callers from reading sibling immutable fields. Fourth, ambient build-time and runtime authority gives procedural macros, build scripts, and transitive packages unrestricted access to the host filesystem, network, wall clock, and global allocator during compilation and IDE workspace loading. Fifth, extracting functions that store or return `&'a mut T` pointers into a theorem prover imposes a backward-prophecy verification tax, requiring loan-graph reconstruction and synthesized backward functions to resolve deferred writes when lifetimes end.

## 1.3 Read a contract at a function boundary

Read the parameter modes to find who can read, change, initialize, or consume the arguments. Read the effect row to see which operations need an enclosing handler. A refinement can state what must be true on entry or return. Proof extraction translates supported operations into a mathematical model where such claims can be checked.

<!-- technical-detail -->

Every Vesper function signature states parameter passing modes, return modalities, refinement contracts, and lexical effect rows at the module boundary. The summary below contrasts a conventional lifetime-and-affine baseline against Vesper's intended design guarantees; Appendix A and `REVIEW-STATUS.md` identify which fragments are currently mechanized in `lean/` versus open implementation goals.

    Systems Dimension         Lifetime / Affine Baseline                          Vesper Design Guarantee (Draft Scope)
    ------------------------- --------------------------------------------------- -------------------------------------------------------------------------
    Resource Finalization     Affine `Drop` only; synchronous, infallible, leaky  True `linear` usage axis; exact-once consumption via effectful `sink` functions (Theorem 2 goal)
    Borrowing & Aliasing      First-class `&'a T` / `&'a mut T` infect signatures Second-class `in`/`inout`/`out`/`sink` over `inout x.{a, b.c}` and `out buf[0..k]` with zero lifetimes
    Concurrency & Cancel      Colored `async fn`, `Pin`, cancel-unsafe `select!`  Colorless `eff [...]` on Zero-`Pin` fiber stacks; `[T-Let]` rejects `Fail`/`Cancel` across live `linear` values
    Refinements & Ghost State Runtime bounds panics and separate spec languages   Liquid refinements `{ v: T | phi }`, 0-bit `@0` ghost tokens, and strong `~>` updates on `inout`/`out`
    Syntax & Metaprogramming  `<T>` / `::<T>` turbofish; unsandboxed `proc_macro` Square-bracket `[T]` generics and fuel-metered `wasm32-wasip2` sandboxes governed by `[T-MacroConfinement]`
    Supply Chain & Proofs     Ambient `fs`/`net`/`GlobalAlloc`; prophecy proofs   `Loom.toml` `capability_ceiling`, `mif:blake3` SemVer checks, and 1-to-1 Lean 4 `.olean` replay (`0 sorryAx`)

## 1.4 Follow a transaction through a broker

The caller owns the transaction until a consuming method takes it. Both branches transfer ownership. The success path returns the receipt from §1.3's ownership contract. The rollback path consumes the same transaction and returns zero. The runnable example uses a slot number as the receipt.

~~~vesper
linear struct WriteTxn {
    wal_slot: u64
}

impl WriteTxn {
    fn commit(sink self) -> u64 {
        let WriteTxn {
            wal_slot
        } = self;
        wal_slot
    }
    fn abort(sink self) {
        let WriteTxn {
            wal_slot
        } = self;
    }
}

fn finish(in ready: bool) -> u64 {
    let tx = WriteTxn {
        wal_slot: 42
    };
    if ready {
        tx.commit()
    } else {
        tx.abort();
        0
    }
}

fn main() -> u64 {
    finish(true)
}
~~~

There is a separate loan contract on the broker. A helper that mutates `broker.{index, metrics}` may also read `broker.config`. Two arguments asking for exclusive access to the same projected field produce `E0301`. [Try repairing a conflicting loan](../tour/find-a-conflicting-loan.html).

<!-- technical-detail -->

The canonical `WriteTxn` walkthrough from `content.typ` combines nominal true linearity (`linear struct`), effectful consuming methods (`sink self`), disjoint field-tree borrowing (`inout broker.{index, metrics}`), and compile-time unwind safety (`[T-Let]`). Because `WriteTxn` is `linear`, neither branch of `append_batch` may let `tx` fall out of scope, while the caller retains shared `in` access to `broker.config`.

~~~vesper
linear struct WriteTxn { wal_slot: u64, dirty_pages: BufferPoolSlice }
impl WriteTxn {
    pub fn commit(sink self) -> CommitReceipt eff [Storage, Fail[IoError]] {
        let WriteTxn { wal_slot, dirty_pages } = self;
        Storage::flush_wal(wal_slot, in dirty_pages)?;
        Storage::release_slice(sink dirty_pages);
        CommitReceipt { wal_slot }
    }
    pub fn abort(sink self) -> Unit eff [Storage] {
        let WriteTxn { wal_slot, dirty_pages } = self;
        Storage::discard_wal(wal_slot);
        Storage::release_slice(sink dirty_pages);
    }
}
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

For a compiler-checked example of an abortable effect crossing a live linear binding, see the [E0402 example in the language guide](../language.html#results).

# 2 Reading and parsing Vesper source

Read a function signature from left to right. Square brackets hold type parameters, parentheses hold the arguments, and an effect row after the return type lists operations the caller must handle or pass on. These delimiters also help an editor recognize an incomplete declaration.

<!-- technical-detail -->

Vesper declarations read left to right from square-bracket generic parameters through moded value parameters, return types, and lexical effect rows. Separating uppercase nominal identifiers, lowercase value identifiers, and explicit refinement sigils lets editors, formatters, and syntax highlighters recover declaration and phrase structure before resolving imports.

Source modules use the `.ves` extension and declarative foreign-system bindings use `.vsys` (`REPO-01`). Per `REVIEW-STATUS.md` (`GRAM-01`–`GRAM-06`), the manuscript EBNF is an incomplete design sketch rather than a normative grammar for `crates/vesper-syntax`, `crates/vesper-fmt`, or the self-hosted Stage 1 front end in `packages/vesper-front` and `packages/vesper-compiler`: `f[k](x)` remains ambiguous between generic instantiation and indexing an array of functions, and lexical case alone does not disambiguate lowercase effect-row or clock arguments from index expressions.

## 2.1 Recognize syntax before resolving names

The proposed parser uses short lookahead so an editor can recognize the shape of a file before loading its imports. Uppercase and lowercase names give clues to type and value positions. Square brackets distinguish generic arguments from comparisons. An `@` introduces an index or annotation where the surrounding grammar expects one. The draft still has ambiguous cases.

<!-- technical-detail -->

Six lexical and delimiter conventions aim to keep surface parsing local and import-independent, subject to the unresolved `GRAM-01`–`GRAM-06` ambiguities where `UpperIdent` (`/[A-Z][A-Za-z0-9_]*/`) vs `LowerIdent` (`/[a-z_][A-Za-z0-9_]*/`) cannot by itself separate `PostfixOp` generic calls `[TypeArgs](ArgList?)` from indexed calls `[IndexSel](ArgList?)` on `f[k](x)` or lowercase row/clock binders inside `[...]`.

    Convention                     Surface Form                                        Lookahead Role and Known `GRAM-01`–`GRAM-06` Boundary
    ------------------------------ --------------------------------------------------- -------------------------------------------------------------------------
    Case-partitioned `[T]`         `UpperIdent` vs `LowerIdent`; `Vec[T]`              Eliminates `<T>`/`::<T>` turbofish; `f[k](x)` and lowercase row/clock args in `[...]` remain ambiguous
    `@` refinement index sigils    `Slice[u8, @len]`, `BoundedVec[T, @(n + 1), @cap]`  Separates dependent value indices from `UpperIdent` types in `[...]`
    Subset & `is` refinements      `{ v: u64 | v != 0 }`, `u64 is 1..=4096`            `'{' LowerIdent ':'` in type position avoids nominal, tuple, and fn types
    Four-mode projections & `~>`   `inout broker.{index, metrics}`, `A ~> B`           Separates `in`/`inout`/`out`/`sink` loans and strong updates `~>` from `->`
    Moded & graded effect rows     `linear effect Storage`, `eff [Region @ <= 64 | r]` Binds `affine` vs `linear` continuations and attaches `@` pomonoid grades
    Import-free `PhraseSkeleton`   `ClauseSugarExpr`, `|>`/`|?>`, `.+`, `hex"..."`     Parses clause, dotted-infix, and adjacent-tag sugar slots before imports

## 2.2 Read the grammar and its everyday consequences

In the grammar, alternatives describe choices and repeated elements describe lists. Read a production such as a function declaration from its leading keyword through parameters, result, effects, and body. Keywords depend on their position. For example, `state` can name a local even though a stateful handler uses it as syntax. Braces can start a block or a record. Parenthesizing a record literal in a loop or condition makes the boundary clear.

A `loop` can yield its `break` value. A range `a..b` excludes the upper bound and `a..=b` includes it. A `for` can also visit collection elements. A `return` exits the enclosing function or closure, so an early exit must account for its live owners. In an optional result, `?` can return `None` early. On a `Result` it can perform a failure effect. In `match`, alternatives joined by `|` share an arm and must bind the same names at the same types.

Integer kinds have distinct widths and signs. Choose an explicit widening, narrowing, saturating, or wrapping operation to say what an out-of-range value means. A fixed array `[T; N]` stores its elements inline. A module constant names a compile-time value. A scalar-valued enum groups constants, while a data-carrying variant selects a shape to match.

Floating-point operations use their floating-point results for infinities and NaNs. Explicit conversions select rounding behavior. The deterministic floating-point contract specifies canonical NaNs for selected operations. Bit reinterpretation lets a program inspect a representation directly. The grammar sketch and the compiler cover different sets of programs. For runnable syntax, start with the language guide.

<!-- technical-detail -->

The EBNF excerpt below summarizes core declarations, modalities, sugar slots, expressions, and patterns from `content.typ` §2.2; top-level `test { ... }` blocks access private module items in test builds and are skipped during ordinary builds.

    Module          ::= ( UseDecl | Attr* Item | TestBlock )* ;
    Item            ::= Vis? ( StructDecl | EnumDecl | QuotientDecl | TypeAliasDecl | EffectDecl
                             | TraitDecl | ImplDecl | ModelDecl | MeasureDecl | SugarDecl
                             | MacroDecl | FnDecl | ConstDecl ) | ItemSugar ;
    ModeAxis        ::= 'copy' | 'affine' | 'linear' | 'portable' | 'pinned'
                      | 'uncontended' | 'contended' | 'unordered' | 'lifo' ;
    TypeParam       ::= UpperIdent ( ':' ParamBounds )? | LowerIdent ':' 'eff' ( '=' EffClause )?
                      | LowerIdent ':' 'clock' | '@' LowerIdent ':' PrimType | LowerIdent ':' 'pred' '(' TypeList ')' ;
    StructDecl      ::= GhostMod? StorageSpec? ModeSpec? 'struct' UpperIdent TypeParams? ( '{' FieldDeclList? '}' | ';' ) ;
    FieldDecl       ::= Vis? GhostMod? TempTier? LowerIdent ':' Type ( '@@' ModeSpec )? ;
    EnumDecl        ::= GhostMod? 'open'? ModeSpec? 'enum' UpperIdent TypeParams?
                        ( '{' VariantList? '}' | ':' (Type | '_') '{' MemberList? '}' ) ;
    SugarDecl       ::= 'sugar' LowerIdent ':' SugarSort SugarEmits? '{' SugarRule+ '}' ;
    SugarHole       ::= HolePass? '$' LowerIdent ':' HoleSort
                      | '$' '..' LowerIdent ':' ( 'stmts' | 'scope' | 'args' | 'items' | 'pats' )
                      | '$' '(' SugarAtom+ ')' Punct? ( '*' | '+' ) ;
    ConstDecl       ::= 'const' UpperIdent ':' Type '=' Expr ';' ;
    FnDecl          ::= Vis? 'fn' LowerIdent TypeParams? '(' ParamList? ')'
                        ( '->' GhostMod? Type )? EffClause? WhereClause? EnsuresClause? Block ;
    Param           ::= GhostMod? PassMode? 'self' ( '.' PlaceTree )?
                      | GhostMod? PassMode? LowerIdent ( '.' PlaceTree | '[' IndexSel ']' )? ( ':' Type ( '~>' Type )? )? ;
    Type            ::= '{' LowerIdent ':' BaseType '|' RefinePred '}' | FnType | 'Later' '[' LowerIdent ',' Type ']'
                      | 'dyn' Path | ( PrimType | '(' TypeList? ')' | '[' Type ';' IntLit ']' | Path TypeArgs? ) ( 'is' RefinePat )? ( '@' ModeSpec )? ;
    ClauseSugarExpr ::= GhostMod? LowerIdent PhraseHead* SugarGroup ( SameLine HeadWord PhraseHead* SugarGroup )* ;
    HandleExpr      ::= 'handle' ( LowerIdent ':' )? Path TypeArgs? ( 'state' Pat '=' Expr )?
                        'in' Block 'with' '{' ReturnClause ','? OpClause* '}' ;
    MatchExpr       ::= 'match' HeaderExpr '{' ( Pat ( '|' Pat )* '=>' Expr ','? )* '}' ;
    PostfixOp       ::= '(' ArgList? ')' | '[' TypeArgs ']' '(' ArgList? ')' | '[' IndexSel ']' | '.' LowerIdent | '?' ;
    TaggedDelimExpr ::= TagIdent ( StringLit | '{' SugarToken* '}' ) ;

Contextual keywords (`state`, `linear`, `copy`, `affine`, `with`, `eff`, `effect`, `handle`, `sink`, `inout`, `const`, `loop`, `for`, `break`, `continue`, and `return` outside statement starts) remain valid `LowerIdent` names, whereas `fn`, `let`, `struct`, `enum`, `impl`, `mod`, `in`, `return` at statement start, `match`, `if`, and `else` are reserved. After a closure head, `eff [` always starts an effect row (`{ eff[i] }` or `(eff)[i]` indexes a variable named `eff`); in arguments, `sink` and `inout` pass by mode only before operand-starting tokens (`g(sink x, inout y.a)` vs `g(sink, inout.a, sink(x), sink[i], sink - 1)`, `(sink) | x`, and `sink { (a, b) }`). Open intervals allow `p[..b]`, `p[a..]`, and `p[..]` on place paths `p`.

Core Vesper omits `while` (left to library `sugar`): `loop B` yields the join of `break` values or `Never`, while `for p in a..b` and `a..=b` have type `()` and check the upper bound before incrementing so `for b in 0..=255` over `u8` never overflows even with `continue`. Neither `break` nor `continue` may exit a closure, handled block, or handler clause (`E0213`); `return` leaves the innermost function or closure (skipping handler `return` clauses) and is rejected in handler clauses and `region` blocks (`E0213`). Inside a function or closure returning `Option[U]`, `e?` on `Option[T]` unwraps `Some` or returns `None` (`E0200` elsewhere; `e?` on `Result[T, E]` raises `Fail[E]`). Dropping a live `linear` value at `break`, `continue`, or `return` raises `E0302`, moving an owned value needed by the next loop iteration raises `E0312`, `affine` values drop once on exit, and rebinding or `inout` borrowing a `let` local requires `let mut` (`E0313`). Collection `for p in e` iterates `[T; N]`, `ChunkSeq[T]`, `Slice[T]`, or string-literal arrays `["a", "b"]`, evaluating length and bounds once and freezing the place against writes, `inout` loans, or moves (`E0402`); `copy` elements bind by value, whereas non-`copy` elements of `ChunkSeq` or `Slice` places lend `in` element views requiring a bare name or `_` pattern (`E0213` otherwise). Top-level `match` or-patterns `p1 | p2 => e` require identical bound names and types (`E0214`), forbid nested `Some(1 | 2)` or guards, and are not yet accepted by the self-hosted Stage 1 checker (`packages/vesper-compiler`). Eight import-free rules disambiguate core forms from `sugar` slots:

| Rule | Disambiguation Principle (Tokens + CST Trivia Only; Never Imports) |
| --- | --- |
| 1–2 | Adjacent `TagIdent` touches `"`/`{` (`hex"..."`, `bits{...}`); header mode stops before `{` or `HeadWord` |
| 3–4 | `LowerIdent` before spaced `{`/ident/lit/`let`/`else`/mode enters skeleton; `name(args)` checks token after `)` |
| 5–6 | Continuation clauses (`} else {`, `} and {`) share `}`'s line; `SugarGroup` re-parses in Stage 2 by `HoleSort` |
| 7–8 | No infix word operators (`x matches p` banned); Stage 2 lookup raises `E0142` (unbound) or `E0141` (sort mismatch) |

In `PipeExpr` right-hand calls, bare `_` marks the piped argument (`frame |> parse(_, in cfg)`). The twelve integer `PrimType` kinds (`u8`..`u128`, `i8`..`i128`, 64-bit `usize`/`isize`) disallow implicit casts; `i128`/`u128` use two words (low word first, pointer-passed in `@export abi 3`, excluded from `Atomic[T]` and float casts). Under `vesper.arith.checked/1`, `+`, `-`, `*`, `-x`, and signed `MIN / -1` raise `Fail[ArithOverflow]`, `/ 0` and `% 0` raise `Fail[RuntimeError]`, `a << n`/`a >> n` require `0 <= n < bits(a)`, and `&`, `|`, `^`, `~`, and integer `!` are total; `Fail` is elided whenever constants or flow-sensitive range facts (`if` dominance, `x & m`, `x % m`, bounded shift/divisor intervals, and non-recursive body-only function result ranges) prove safety. Explicit `widen_k`, `narrow_k`, and `wrapping_k` join total reserved intrinsics (`mul_wide`, `leading_zeros_k`, `trailing_zeros_k`, `count_ones_k`, `wrapping_shl_k`, `wrapping_shr_k`, `wrapping_neg_k`, `rotate_left`/`rotate_right`, `to_le_bytes_k`/`from_le_bytes_k`, `to_be_bytes_k`/`from_be_bytes_k`, and `Option`-returning `checked_add_k`/`checked_sub_k`/`checked_mul_k`), with contextual literal inference defaulting to `i64` (`u64`/`i128` if wider; host checker only).

Fixed arrays `[T; N]` (`N < 65536` literal) store `N` inline elements at mode `T`, eliding `Fail[RuntimeError]` on range-proved `a[i]` while requiring a mutable place (`E1540`) and `copy` element (`E0311`) for `a[i] = v`. Module `const NAME: T = e;` forbids calls (`E1541`), type/ctor name collisions (`E1542`), and place writes (`E0300`), and literal constants serve as `match` patterns; typed value enums `enum Name: T { A = 0, B }` group `T` constants (requiring `_` in `match`), while untyped `enum Name: _ { ... }` folds compile-time integer literals (`E1541` otherwise) via `ir_inline::fold_literal_constants`. IEEE-754 `f32`/`f64` (`vesper.float.ieee754/1`) have total arithmetic, `sqrt`, `fma`, `%` (`fmod`), and IEEE 754-2019 `min`/`max`, explicit `widen_f32`/`widen_f64`, `nearest_f32`/`nearest_f64`, `narrow_k` (`Fail[ArithOverflow]`), `saturating_k` (NaN to `0`), and `to_bits_*`/`from_bits_*`, canonical quiet NaNs (`0x7fc00000`, `0x7ff8000000000000`) under `eff [DetFloat]`, and 1-ulp Vesper-source fdlibm ports (`exp_f64`, `ln_f64`, `sin_f64`, `cos_f64` and `f32` wrappers for Cody-Waite `|x| < 2^20 * pi/2`) carrying implicit `DetFloat` and `Realtime`.

~~~vesper
enum OpClass: _ { Alu = 1, Mem = 2 }
const LUT: [u64; 4] = [10, 20, 30, 40];
fn decode_scaled(code: u8, step: u64) -> Option[u64] {
    let table = LUT;
    let mut acc: u64 = 0;
    for i in 0..4 {
        acc = checked_add_u64(acc, table[i] >> (i * 2))?;
    }
    match code {
        OpClass::Alu | OpClass::Mem => checked_mul_u64(acc, step),
        _ => None,
    }
}
~~~

# 3 Ownership, movement, and erased evidence

A value’s usage mode determines whether you may copy it, drop it, or must consume it exactly once. Vesper tracks other restrictions independently, including whether you may move the value to another thread, whether a worker can change it, and whether you must release it in a particular order.

<!-- technical-detail -->

Every Vesper binding pairs a runtime presence bit with a four-axis modal vector that independently governs how many times a value may be used, whether it can cross worker threads, whether concurrent readers can observe interference, and whether it must be released in last-in, first-out stack order. Factoring these properties into a product lattice lets generic containers inherit the exact obligations of their element types without duplicating APIs.

The four-axis `ModeVec` representation and generic `mode_cross` kinding are implemented in `crates/vesper-core`, the host checker (`src/check/`), `crates/vesper-lsp`, and `packages/vesper-compiler`, backed by mechanized proofs in `lean/MetatheoryAppendix.lean`, `lean/MetatheoryFull.lean`, `lean/GhostErasure.lean`, and `lean/OperationalGhostBridge.lean`. Per `REVIEW-STATUS.md` (`TYPE-01`, `TYPE-04`, `TYPE-05`, `TYPE-09`), capsule isolation premises are still supplied as explicit model evidence rather than derived from a heap graph, `lifo` source-event emission is only partially wired, and the production compiler AST has no `@0` ghost-binding syntax yet.

## 3.1 Read the four axes and presence

Usage tracks whether a value can be copied, dropped, or must be consumed. Portability distinguishes transferable and pinned values. Contention records whether another participant can interfere. Exchange order distinguishes freely ordered values from last-in, first-out obligations. A separate presence index distinguishes runtime values from erased proof material.

| Axis | Less restricted | More restricted | Example |
| --- | --- | --- | --- |
| Usage | `copy` | `affine`, `linear` | A transaction must finish exactly once. |
| Portability | `portable` | `pinned` | A pinned handle stays with its worker. |
| Contention | `uncontended` | `contended` | Another participant can invalidate a fact about shared state. |
| Exchange order | `unordered` | `lifo` | Release the last acquired guard first. |

<!-- technical-detail -->

Each binding carries `(rho, m)` with quantitative runtime presence `rho in {0, 1}` (`1` runtime, `0` erased ghost) and `m = (u, p, c, o)` packed into `ModeVec(u8)` (default nominal floor `affine`, scalar floor `copy`). Componentwise join `m1 |_| m2` intersects allowed dynamic behaviors (`U(u1 |_| u2) = U(u1) /\ U(u2)`), and because `{1} subset {0, 1} subset N`, sub-moding is covariant along `<=`: a `copy` or `affine` value (`m1`) satisfies any parameter expecting `linear` (`m1 <= m2`), whereas relaxing `linear` to `affine` is unsound without concrete modal kinding.

    ModeVec(u8) = (u, p, c, o)   [covariant sub-moding m1 <= m2 adds structural restrictions]
    ┌──────────────────────┬───────────────────────┬──────────────────────────┬───────────────────────┐
    │ Usage (u)            │ Portability (p)       │ Contention (c)           │ Order (o)             │
    │   linear      {1}    │   pinned              │   contended              │   lifo   (stack Λ)    │
    │     ▲  (no drop/dup) │     ▲                 │     ▲                    │     ▲  (`IrqGuard`)   │
    │   affine    {0, 1}   │     │ `Capsule` seal  │     │ `[M-ContendStrip]` │     │                 │
    │     ▲  (sync drop)   │   portable (default)  │   uncontended (default)  │   unordered (default) │
    │   copy (⊥)    N      │                       │                          │                       │
    └──────────────────────┴───────────────────────┴──────────────────────────┴───────────────────────┘
    Presence ρ:  @1 runtime (registers/stack)  ──shared obs──▶  @0 ghost (full ModeVec, 0 codegen bits)

## 3.2 Derive a record’s mode from its fields

A record containing a linear ticket must protect the ticket even if its other fields are copyable. A field annotation can impose a stricter floor. The record combines the field requirements with its declared mode. Keep the fields of a linear type module-private if callers must use consuming methods to commit or roll back. Public fields can let a caller discharge the linear obligation by destructuring. Conversely, after consuming the record, a plain integer field can be used at its own intrinsic kind. This recovery of a known type’s permissions is called mode crossing.

<!-- technical-detail -->

Every type `tau` has modal kind `Delta |- tau : mod m0` giving the lowest mode vector justified by its concrete type arguments. Per `TYPE-01`, `[K-Adt]` distinguishes omitted annotations (`m_i = k_i`) from explicit field floors (`f_i: tau_i @@ m_i`), enforces `k_i <= m_i` (`E0105` otherwise, rejecting `Ticket @@ copy` when `Ticket` is `linear`), forbids `lifo` fields inside aggregates (`o(m_i) = unordered`), and joins all effective field floors with `m_decl` (`lean/MetatheoryAppendix.lean`).

    [K-Prim]  scalar(tau)  ==>  Delta |- tau : mod (copy, portable, uncontended, unordered)
    ----
    [K-Adt]   decl(T[A]) = (m_decl, { f_i : tau_i @@ m_i })
              forall i,  Delta |- tau_i[sigma / A] : mod k_i   k_i <= m_i   o(m_i) = unordered
              --------------------------------------------------------------------------------
              Delta |- T[sigma] : mod (m_decl |_| join_i m_i)

~~~vesper
copy enum Option[T] { None, Some(T) }
pub struct Fd { raw: i32 @@ linear }
impl Fd {
    pub fn close(sink self) -> i32 {
        let Fd { raw } = self;
        raw
    }
}
// `Option[u64]` crosses to `copy`; `Option[Fd]` joins to `linear` and must be matched:
fn unwrap_or_close(sink maybe_fd: Option[Fd]) -> i32 {
    match maybe_fd {
        Option::Some(fd) => fd.close(),
        Option::None => -1,
    }
}
~~~

## 3.3 Seal a capsule or release a guard

A `Capsule[T]` packages an isolated owner for transfer. Sealing needs evidence that no outside alias, borrowed capability, or thread-affine handle ties the contents to the sender. Last-in, first-out guards restrict release order. If you acquire A then B, release B before A. Two branches must agree on the guard stack when they rejoin.

<!-- technical-detail -->

Cyclic or region-indexed graphs (`SlotArena`) carry `pinned + uncontended` while mutated on one shard, then seal into `Capsule[T]` to gain `portable` status for zero-copy transfer. Interrupt masks (`linear + lifo struct IrqGuard`) and arena `Watermark` tokens inhabit `lifo`, restricting Exchange to reverse-acquisition release. Per `REVIEW-STATUS.md` (`TYPE-04`, `TYPE-09`), Rust (`crates/vesper-core`) and Lean reject all 24 invalid mode/isolation combinations from explicit evidence fields (not yet derived from a heap graph), and `vesper-core::lifo::LifoState` checks stack traces while full source `E0306` wiring remains open.

    Capsule[T] Sealing Admission (`TYPE-04`)                   LIFO Stack Discipline (`Order::Lifo`, `TYPE-09`)
    ---------------------------------------------------------- -----------------------------------------------------------------
    1. Unique root ownership (`affine` or `linear`, no `copy`) 1. Acquisition pushes fresh guard identity onto stack `Lambda`
    2. Unordered root (`o(m) = unordered`; rejects `lifo`)     2. Release pops only the current top of `Lambda` (`E0306` target)
    3. Self-contained region allocation                        3. Branch joins (`if`/`match`) require identical guard stacks
    4. Zero external aliases into the subgraph                 4. Normal function return requires an empty local stack
    5. Zero free outer lexical effect capabilities             5. Aggregates forbid `lifo` fields (`o(m_i) = unordered`)
    6. Zero thread-affine OS or UI handles                     6. `LifoState` checks traces; full source event wiring is open

## 3.4 Erase a proof without changing the run

In the proposed ghost calculus, a value marked `@0` exists only during checking. The production AST has no ghost-binding representation. A proof can consume a ghost permission to justify an operation. Runtime control flow cannot depend on that permission. The erasure argument checks termination, freedom from runtime effects, and discharge of ghost obligations. The operational model proves erasure for ghost computations that satisfy these conditions and capture no outside resources.

<!-- technical-detail -->

Refinement index parameters (`Slice[u8, @len]`) and zero-sized capability witnesses (`ghost linear struct DmaSlotPerm[@slot: u32]`) are designed for presence `rho = 0` (`@0`), obeying `ModeVec` during checking before erasing to zero runtime bits. Per `REVIEW-STATUS.md` (`TYPE-05`), the production compiler AST currently has no `@0` ghost-binding syntax or source erasure pass; `ghost` declarations in manuscript listings are proposed surface syntax.

In the mechanized models, the upstream ghost-context helper (`TYPE-05`) demotes runtime `@1` bindings to shared read-only observations without altering modes, excludes `out` slots, bars `@1` control flow from inspecting `@0` values, rejects runtime effects, and refuses erasure if `@0 linear` tokens remain live. `lean/GhostErasure.lean` proves bidirectional big-step erasure equivalence and ghost non-interference for closed, terminating ghost resource computations, and `lean/OperationalGhostBridge.lean` connects those proofs to the polymorphic runtime calculus with progress, preservation, no infinite ghost stuttering, and resource exhaustion (captured ghost tokens and dependent heap typing remain open).

# 4 Borrowing parts of a value

You can lend one field of a record to a writer while another operation reads a different field. Vesper tracks the storage each loan covers, down to a field path or a range of array elements.

<!-- technical-detail -->

Second-class place projections let a function read, mutate, initialize, or consume selected struct fields and slice intervals without adding lifetime parameters to types or traits. Because loans cannot be stored in structs or returned after a call finishes, disjointness reduces to checking field-path trees and arithmetic intervals at the call site.

Field-tree loans, borrowed `match` arms, and two-phase argument loans are implemented in the host checker (`src/check/` and `packages/vesper-place-ir`), with separate formal models in `lean/VesperUnifiedTypeTheory.lean`, `lean/OrderedContexts.lean`, `lean/StrongUpdate.lean`, `lean/RefinementPlaces.lean`, `lean/TypedPlaceRefinements.lean`, and `lean/PlacePolymorphismBridge.lean`. Per `REVIEW-STATUS.md` (`TYPE-02`, `TYPE-03`, `TYPE-06`, `TYPE-09`), the production checker does not yet support `~>` strong-update arrows, `out` parameters, parameter refinements, or a full Bitwuzla refinement pass, and the self-hosted Stage 1 checker (`packages/vesper-compiler`) omits two-phase loan reservation.

## 4.1 Read a typing judgment as an inventory

The context to the left of a judgment lists the bindings available at that point. Splitting the context gives each subexpression its permissions. Shared views can be lent to both sides. An exclusive loan or a linear owner has a single destination. Ordered guards have their own stack. At a scope exit the checker asks which entries can be discarded and which still need an action.

<!-- technical-detail -->

Expressions in `lambda_Vesper` check against `Delta`, `Sigma`, `Phi`, and `Gamma` plus a separate ordered stack `Lambda` for `lifo` guards (`lean/OrderedContexts.lean` proves guard-trace equivalence, prefix validity, and conservation). In `eps`, `res(eps)` is the usage-axis meet of active handler modes (`res([]) = linear`, `res([Fail]) = affine`), and `lean/VesperUnifiedTypeTheory.lean` (`TYPE-02`) proves that `shr` (`in`) views of `linear` owners split and discard freely whereas `mut` (`inout`) and `out` permissions never contract.

    Delta ; Sigma ; Phi ; Gamma |- e : tau @ (rho, m) | eps    (plus ordered LIFO guard stack Lambda)
    ----
    Delta : kind bounds `alpha : mod m`, rows `r`, clocks `k`, ghost `@n`, Horn predicates `p`
    Sigma : lexical handler capabilities `l : Cap_q[E]` (`q in {affine, linear}`)
    Phi   : path-sensitive refinement facts from `where`, `if`, and `match` guards
    Gamma : owned `x :^(own) tau`, shared `x :^(shr) tau`, exclusive `x :^(mut) tau`, uninit `x :^(out) tau`
    ----
    discard(Gamma) : every entry is `shr` (including shared views of `linear` owners) or `own` (`unordered`, `<= affine`)
    [Split-Copy]   : `shr` views and `own` (`copy`, `unordered`) duplicate into both `Gamma_1` and `Gamma_2`
    [Split-Sub]    : `mut`, `out`, and `own` `affine`/`linear` (`unordered`) route to `Gamma_1` or `Gamma_2` exclusively

## 4.2 Carry ownership through closures and branches

A closure that captures a linear ticket by ownership inherits an obligation to use it. A borrowed parameter is valid only within its loan. A closure may itself be lent for a call. A sequence must preserve the resources needed after its first expression. Each branch has to leave compatible permissions for the code after the branch.

<!-- technical-detail -->

`[T-Match]` consumes an aggregate and binds each field at its intrinsic kind `k_ij` (`TYPE-01`), so a `u64` inside a `linear struct` becomes `copy` once destructured; protocol handles (`WriteTxn`, `Fd`) must therefore keep their fields and constructors module-private so external callers cannot destructure them directly to bypass `commit` or `close`. `[T-Let]` enforces the Resumption Frame Condition `u(m_z) <= res(eps_1)` on owned bindings in `Gamma_2` (`E0402`) and outer handler states (`E0402`), while allowing stationary `inout tx: WriteTxn` borrows in callees across `?`.

    [T-Let]    rho_1 = 0 => (eps_1 = [] /\ total(e_1))   (forall z in Gamma_2, u(m_z) <= res(eps_1))
               Delta ; Sigma ; Phi ; Gamma_1 |- e_1 : tau_1 @ (rho_1, m_1) | eps_1
               Delta ; Sigma ; Phi ; Gamma_2, x :^(own)_(rho_1, m_1) tau_1 |- e_2 : tau_2 @ (rho_2, m_2) | eps_2
               -----------------------------------------------------------------------------------------------
               Delta ; Sigma ; Phi ; Gamma_1 * Gamma_2 |- let x = e_1 in e_2 : tau_2 @ (rho_2, m_2) | eps_1 ; eps_2
    ----
    [T-Match]  rho <= rho_0   Delta ; Sigma ; Phi ; Gamma_0 |- e_0 : T[sigma] @ (rho_0, m_0) | eps_0
               forall i,  Delta ; Sigma ; (Phi /\ e_0 = C_i(x_i)) ; Gamma, x_ij :^(own)_(rho_0, k_ij) tau_ij |- e_i : tau @ (rho, m) | eps_i
               ----------------------------------------------------------------------------------------------------------------------------
               Delta ; Sigma ; Phi ; Gamma_0 * Gamma |- match e_0 { C_i(x_i) => e_i } : tau @ (rho, m) | eps_0 ; (join_i eps_i)

First-class closures (`[T-Abs]`) capture only owned values, whereas a non-`copy` `in` parameter `f: fn(A) -> R eff [E]` acts as a second-class lent function parameter: callees may invoke `f` repeatedly or re-lend it, never bind, store, return, or move it (`E0305`, `E0310`). Passing a closure literal directly for `f` creates a lent closure whose parameter modes match the expected signature (`E0308`) and whose body borrows enclosing locals `in` (read-only: `E0307`, `E0305`, checked against sibling call loans via `E0301`) for the call duration.

~~~vesper
fn find_matching_slot(in table: SlotTable, in target: Key) -> bool {
    table.with_ref(target.hash, |in entry| entry.id == target.id)
}
~~~

## 4.3 Choose a parameter mode and a footprint

Use field loans to lend selected parts of a record. `in` lends shared access, `inout` lends initialized storage for mutation, `out` lends a destination for initialization, and `sink` transfers ownership. Two writers can receive the left and right halves of an array if their intervals are disjoint. A two-phase loan reserves the receiver while other arguments are evaluated, then activates exclusive access for the call. Matching a borrowed noncopy record can lend its fields without consuming it.

<!-- technical-detail -->

Field projections `x_i.pi_i` and `x_j.pi_j` are disjoint (`p_i # p_j`) whenever their roots differ or `leaves(pi_i) /\ leaves(pi_j) = []`, and `[T-CallSliceProj]` extends disjointness to half-open intervals `[l_a, r_a)` inside `[0, n)`.

    Root `broker: Broker`                     Call Transitions for `f(inout x: A ~> B, out dst: D)`
    ┌──────────────┬───────────────┐          ┌─────────────────────┐   PostCall_ok    ┌────────────────────────────┐
    │ .index (u64) │ .config (u64) │          │ Pre-state (Φ ; Γ)   │ ───────────────▶ │ x : B, Φ[x_old/x], dst : D │
    │ .metrics     │               │          │ x : A, dst : uninit │                  └────────────────────────────┘
    └──────┬───────┴───────┬───────┘          └──────────┬──────────┘   PostCall_abort (A == B, whole x, E0402)
           │ inout         │ in (disjoint leaves)        └───────────────────────────▶ ┌────────────────────────────┐
           ▼               ▼                                                           │ keep-writes x : A (Φ drop) │
    `append(inout broker.{index, metrics}, in broker.config)`                          │ dst : uninit (E0402 if lin)│
                                                                                       └────────────────────────────┘
    ----
    [T-CallProj]       forall i != j, (p_i # p_j \/ mu_i = mu_j = in)   PostCall(Phi, Gamma_args, mu_i p_i) = (Phi' ; Gamma')
                       ------------------------------------------------------------------------------------------------------
                       Delta ; Sigma ; Phi ; Gamma_f * Gamma_args |- f(mu_i p_i) : tau_ret @ (rho_ret, m_ret) => (Phi' ; Gamma') | eps
    [T-CallSliceProj]  forall a != b, Valid_SMT(Phi ==> [l_a, r_a) /\ [l_b, r_b) = [] /\ 0 <= l_a <= r_a <= n)
                       ---------------------------------------------------------------------------------------
                       Delta ; Phi |- DisjointSlices(s : Slice[tau, @n], { [l_1..r_1), ..., [l_k..r_k) })

Under `PostCall_abort` (`res(eps) = affine` or open row), Vesper uses keep-writes rather than copy-in/copy-out rollback: each `inout` parameter must be stationary (`tau_in = tau_out`) and whole at every abortable checkpoint (`E0402`), retaining callee writes while havocking caller leaf facts, and `out` slots forbid writing `linear` sub-values before an abortable checkpoint (`E0402`). The current production checker has no `~>`, `out`, or parameter refinements yet (`E0402` checks moved-out `inout` holes; post-handler joins drop path facts on written places). Host `inout` loans are two-phase (not yet in `packages/vesper-compiler`): reserved during sibling argument evaluation so `s.push(s.len())`, `s.try_set(i, wrapping_add(s.get(i), 1))`, and `f(inout c, g(in c))` check, while `w(inout o, w(inout o, 0))` (`E0402`/`E0410`) and `s.extend(in s[..])` are rejected. Matching a non-`copy` `in`/`inout` place borrows `Box(p)` and variant payloads as `in` (`E0402`, `E0305`, `E0307`; `inout` through `Box` remains open).

~~~vesper
copy struct Broker { index: u64, metrics: u64, config: u64 }
fn append(inout broker.{index, metrics}: Broker, in config: u64) {
    broker.index = wrapping_add(broker.index, config);
    broker.metrics = wrapping_add(broker.metrics, 1);
}
fn push_index(inout xs: ChunkSeq[u64], inout broker: Broker) {
    xs.push(xs.len());
    append(inout broker, in broker.config);
}
~~~

## 4.4 Initialize a destination exactly once

An `out` slot starts without a readable value. A callee must fill it on normal return before the caller reads it. Splitting a destination into slices requires both disjointness and complete coverage. Tail-modulo-cons construction uses the unfinished tail of a result as the destination for the next step, so recursive construction can fill the final structure directly.

For eight output slots, `[0..3]` and `[3..8]` cover the destination exactly. Moving the second start to four leaves slot three uninitialized. Moving it to two gives both writers access to slot two. Checking only the sum of the lengths would miss a gap paired with an overlap.

<!-- technical-detail -->

In the proposed destination calculus, `out p: T` (`OutSlot[T]`) denotes uninitialized caller storage: early `?` unwinds before writing `p` are sound, while `OutInitTree` requires complete leaf or slice coverage before normal return (`UninitializedOutSlotAtReturn`). For 8 slots, `[0..3]` and `[3..8]` partition `[0..8)` without the gap at `3` (`[4..8]`) or overlap at `2` (`[2..8]`) that a length-sum check would miss, and Stage 4 (`vesper-opt-egglog`) targets `out` slots for constant-stack Tail-Modulo-Cons (TMC) loops.

    [T-OutSplitSlice]  Delta ; Sigma ; Phi ; Gamma_1, p_1 :^(out)_(1, linear) Slice[tau, @k] |- e_1 : Unit | eps
                       Delta ; Sigma ; Phi ; Gamma_2, p_2 :^(out)_(1, linear) Slice[tau, @(n - k)] |- e_2 : Unit | eps
                       Valid_SMT(Phi ==> 0 <= k <= n)
                       -----------------------------------------------------------------------------------------------
                       Delta ; Sigma ; Phi ; Gamma_1 * Gamma_2, p :^(out)_(1, linear) Slice[tau, @n]
                         |- split_out(p, k, \p_1 p_2. (e_1 || e_2)) : Unit | eps

## 4.5 Update facts when their values change

A refinement adds a predicate such as “length is within capacity.” A strong update replaces the old facts about the location a program writes. Snapshot its old value first if a postcondition relates the new and old lengths. Other names for the same location need the update too. Across interference, keep only facts that every allowed change preserves. The manuscript’s proofs cover particular scalar stores and typed place operations.

<!-- technical-detail -->

Per `REVIEW-STATUS.md` (`TYPE-09`), Bitwuzla's decidable target is quantifier-free bit-vectors with uninterpreted functions (`QF_UFBV`) under deterministic work limits (timeout/`unknown` rejects; no `< 2 ms` guarantee; no unbounded `QF_LIA` or unrestricted CHC fixpoints; the standalone interval helper evaluates bounded arithmetic only).

    [Sub-Refine]      Valid_SMT((Phi /\ facts(Gamma)) ==> (phi_1[v / x] ==> phi_2[v / x]))
                      --------------------------------------------------------------------
                      Delta ; Phi ; Gamma |- { x : tau | phi_1 } <: { x : tau | phi_2 }
    [T-StrongUpdate]  (x :^(own)_(1, m) tau_old) in Gamma   f : (inout y : tau_in ~> tau_out) ->^(eps) tau_ret   Delta ; Phi |- tau_old <: tau_in
                      ---------------------------------------------------------------------------------------------------------------------------
                      Delta ; Sigma ; Phi ; Gamma |- f(inout x) : tau_ret => (Phi[x_old / x] ; Gamma') | eps
    [M-ContendStrip]  forall s_0, s_1,  (phi(s_0) /\ sameOutside(W, s_0, s_1)) ==> psi(s_1)
                      ---------------------------------------------------------------------
                      contend({ x : tau | phi }) = { x : tau | psi } @ contended

In `[T-StrongUpdate]`, logical snapshot `x_old` substitutes across `Phi` and sibling refinements in `Gamma`: `lean/StrongUpdate.lean` proves exact post-states and alias safety over scalar/integer-field stores, `lean/RefinementPlaces.lean` proves equivalence with lawful lens `patch` composition, and `lean/TypedPlaceRefinements.lean` connects linear-payload updates and physically disjoint swaps to the mutable-place calculus (allocation and pointer fields remain open). In `[M-ContendStrip]`, `StrongUpdate.Heap.Stabilize` and `ForgetMutable` replace syntactic conjunct splitting with universal interference over writable set `W`, proving that `immutable = 1 or mutable > 0` cannot be retained from `immutable = 0, mutable = 5` because interference setting `mutable = 0` falsifies the disjunction.

## 4.6 Stage two DMA transfers

For direct memory access (DMA), split a staging buffer into nonoverlapping ranges, give each producer a destination, and use proof-only permissions to account for the ranges. Reading the whole buffer requires both destinations to be initialized. Keep the buffer live through device completion because the device may still use memory after the submitting function gives up control.

<!-- technical-detail -->

The `stage_parallel_dma_batch` excerpt below combines refined scalars, `@0` ghost tokens, `Stack` grades, `[T-OutSplitSlice]`, and `~>` updates; `out`, `~>`, `ghost`, `@slot`/`@count` indices, and `@ <=` grades remain proposed surface syntax.

~~~vesper
type AlignedLen64 = { n: usize | n > 0 && (n & 63) == 0 && n <= 4096 };
ghost linear struct DmaSlotPerm[@slot: u32] {}
pub fn stage_parallel_dma_batch[P: IsaProfile, @slot: u32, @count: usize](
    out dst_pkt: AlignedByteArray[4096],
    in src_frames: Slice[RawFrame, @count],
    inout stats: ShardStats[@slot, @prev] ~> ShardStats[@slot, @(prev + 1)],
    ghost sink perm: DmaSlotPerm[@slot],
) -> ghost DmaStagedReceipt[@slot]
eff [Scope, Isa[P], Stack @ <= 384] where count <= 32 {
    let mid: usize is 64..=64 = 64;
    Pulse::scope(|inout s| eff [Scope, Isa[P]] {
        let f_hdr = s.spawn(|| eff [] { write_dma_header(out dst_pkt[0..mid], count) });
        let f_body = s.spawn(|| eff [Isa[P]] { pack_frames_simd[P](out dst_pkt[mid..4096], in src_frames) });
        (f_hdr.join(), f_body.join())
    });
    let ghost DmaSlotPerm {} = perm;
    stats.record_staged_packet();
    ghost DmaStagedReceipt {}
}
~~~

# 5 Effects, budgets, and equality

An effect names an operation a function asks its caller to provide. A grade can bound the operation’s resource cost. Refinements and proofs describe which values a caller may supply and what it can expect back.

<!-- technical-detail -->

Lexical effects model I/O, state, failure, and resource consumption as explicit capabilities discharged by enclosing handlers. Typing each delimited continuation as affine or linear reconciles algebraic effect handling with exact-once resource ownership while allowing pomonoid grades, guarded clocks, and quotients to constrain execution and representation visibility.

Handler typing and native handler specialization run in `src/check/` and `src/native_handlers/`, paired with checked grade arithmetic in `crates/vesper-core` and Lean 4 models in `lean/Metatheory/Polymorphism.lean`, `lean/ResourceGrades.lean`, `lean/HandlerGrades.lean`, and `lean/ClockQuotientSafety.lean`. Per `REVIEW-STATUS.md` (`TYPE-07`, `TYPE-08`, `TYPE-09`), source-level grade inference, combining grades with linear handler state, `@0` clock-quantified forcing in the source checker, and binding quotient certificates to implementation bodies remain open, and the combined soundness statements in §5.5 are research goals rather than theorems of a single integrated calculus.

## 5.1 Handle an operation and thread its state

Performing an effect transfers control to its lexical handler. The handler receives a continuation representing the rest of the computation. A linear continuation must be resumed exactly once. An affine continuation may be abandoned. For a counter, pass the updated counter as handler state on each resumption so later operations see the new value.

<!-- technical-detail -->

`[T-Perform]` preserves declared argument/result modes, while `[T-Handle]` (`TYPE-07`, proved for its ungraded operational rule in `lean/Metatheory/Polymorphism.lean`) requires a pure atomic initializer `atom(e_init)`, state survival `u(m_s) <= res(eps)`, non-escaping capability `l notin free_caps(tau_out, eps)`, `copy` ambient clause captures in `Gamma_h`, and one-shot continuation `k` at `q in {affine, linear}`.

    Caller / Handler Frame (`handle l : Counter^linear state s = 20`)
    ┌─────────────────────────────────────────────────────────────────────────┐
    │ Evidence slot `l` ──▶ op clause `bump(delta, cur_s, k)`                 │
    │                         │ computes `next = wrapping_add(cur_s, delta)`  │
    │                         ▼ invokes one-shot `k(next, next)` (`q=linear`) │
    │ Suspended Body Frame  ◀─┘ resumes right after `Counter::bump(1)`        │
    └─────────────────────────────────────────────────────────────────────────┘
    ----
    [T-Perform]  (l : Cap_q[E]) in Sigma   (op : tau_arg @ m_arg -> tau_res @ m_res) in E   Delta ; Sigma ; Phi ; Gamma |- e : tau_arg @ (1, m_arg) | eps
                 ----------------------------------------------------------------------------------------------------------------------------------------
                 Delta ; Sigma ; Phi ; Gamma |- perform l.op(e) : tau_res @ (1, m_res) | eps ; { l : E^q @ g_op }
    [T-Handle]   atom(e_init)   u(m_s) <= res(eps)   l notin free_caps(tau_out, eps)   (forall z in Gamma_h, u(m_z) = copy)
                 e_body : tau_b @ (1, m_b) | { l : E^q } ; eps   op_i(y : k_i, s : m_s, k :_q (sigma_i, tau_s) ->^eps_q tau_out) |- e_i : tau_out | eps
                 --------------------------------------------------------------------------------------------------------------------------------------
                 Delta ; Sigma ; Phi ; Gamma_s * Gamma_body * Gamma_h |- handle l : E^q state s = e_init in e_body with { ... } : tau_out @ (1, m_out) | eps

~~~vesper
linear effect Counter { fn bump(delta: u64) -> u64; }
fn run_counted() -> u64 {
    handle Counter state s = 20 in {
        let first = Counter::bump(1);
        wrapping_add(first, Counter::bump(21))
    } with {
        return (val, final_s) => wrapping_add(val, final_s),
        bump(delta, cur_s, k) => {
            let next = wrapping_add(cur_s, delta);
            k(next, next)
        }
    }
}
~~~

## 5.2 Trace the error edge of a question mark

Consider opening a transaction and then applying `?` to validation. On success execution reaches the commit. On failure the handler can abandon the continuation, leaving nobody to consume the transaction. Validate first, or explicitly consume the transaction on both branches. The resumption frame condition rejects the first version because the abandoned continuation would still own the transaction.

<!-- technical-detail -->

Applying `?` while owning `tx: WriteTxn` triggers a four-step `[T-Let]` rejection (`E0301`), whereas borrowing stationary `inout tx: WriteTxn` inside a helper succeeds under keep-writes (§4.3), leaving the owning caller responsible for catching `Fail` and consuming `tx`.

~~~vesper
fn stage_checked(inout tx: WriteTxn, in batch: RecordBatch) -> Unit eff [Storage, Fail[IoError]] {
    validate_crc(in batch)?;
    tx.record_crc_ok();
}
~~~

| Step | Context-Splitting Derivation for `let tx = begin_txn(); validate_crc(in batch)?; tx.commit()` |
| --- | --- |
| 1 | Minting `tx` introduces `tx :^(own)_(1, linear) WriteTxn` (`q(Storage) = linear`) into caller Gamma |
| 2 | Desugaring `e_crc?; tx.commit()` produces `let _ = e_crc in tx.commit()` with `eps_1 = { Fail^affine }` |
| 3 | Splitting `Gamma_1 * Gamma_2` places owned `tx` into `Gamma_2` because `tx.commit()` consumes it |
| 4 | `[T-Let]` checks `u(m_tx) <= res(eps_1)`, i.e. `linear <= affine` -> FALSE (`Error[E0402]`) |

## 5.3 Compose resource grades

A grade describes how much of a resource an operation needs. An ordered monoid supplies a zero, a way to combine costs, and an order for comparing them with a budget. Two allocations may add their byte counts. Two sequential calls can reuse stack space so their peak is the larger requirement. Nested calls need space for both active frames. An exact timing contract also needs both branches to agree on cost.

For example, if two children need 24 and 40 bytes of stack and finish one after the other, the children’s peak is 40 bytes. If the parent keeps its own 16-byte frame throughout, the combined peak is 56 bytes. A bounded loop can reuse that stack on each iteration even when allocations retained in a region accumulate.

<!-- technical-detail -->

`vesper-core::resource_grade` (`TYPE-09`) models resource budgets as ordered pomonoids: `lean/ResourceGrades.lean` proves finite call-tree stack bounds and exact-time non-interference (SemVer forbids weakening or changing exact `ConstTime` contracts, though physical CPU timing remains separate), while `lean/HandlerGrades.lean` (`evaluator_matches_cps`, `extracted_handler_budget`) and Rust's `handler_additive_bound` prove one-shot handler cost `body_work + op_count * clause_overhead + return_work` (`clause_overhead` excludes the resumed continuation; source grade inference and linear handler state remain open).

    Sequential `e1 ; e2`              Nested Call `parent -> child`     Secret Branch `ct_if`
    ┌──────────────┬────────────┐     ┌───────────────────────────┐     ┌───────────┐
    │ Region, Fuel │ g1 + g2    │     │ Parent frame_self (16 B)  │     │ then: g1  │──┐
    │ Stack (loop) │ max(g1,g2) │     ├───────────────────────────┤     ├───────────┤  ├─▶ requires g1 == g2
    │ ConstTime    │ g1 + g2    │     │ Child max(24 B, 40 B)=40B │     │ else: g2  │──┘   (SemVer invariant)
    └──────────────┴────────────┘     └───────────────────────────┘     └───────────┘
                                      Combined Peak Stack = 56 B (reused across bounded loop trips)

## 5.4 Delay recursion and hide representations

A clock-indexed `Later` delays a recursive observation by one logical step. A clock controls when the observation can be forced and what a recursive definition may capture. A quotient treats multiple representations as one abstract value. For example, two representations of the same key must give the same observable lookup result. Each exported operation needs evidence that it respects the equivalence.

<!-- technical-detail -->

Clock-indexed `Later[k: clock, T]` and substructural `quotient` declarations (`TYPE-08`) separate logical step-indexing and abstract equivalence from runtime scheduling and layout (`lean/ClockQuotientSafety.lean`).

| Construct | Admission Rules (`crates/vesper-core` & `lean/ClockQuotientSafety.lean`) |
| --- | --- |
| `Later[k: clock, T]` | Payload `T` at most `affine` (`linear` rejected); step captures `copy` only with closed `eff []` |
| `@0` clock forcing | Requires `forall k` with fresh `k` absent from ambient context (rejected in source checker until modeled) |
| `quotient Q = repr R ...` | Requires `k_repr <= k_quot` and exact per-export congruence inventory (including `inout` methods) |
| `Quot.lift` vs Rust flag | `publicObserve`/`publicStep` check real Lean proofs; Rust `proved_respects` flag is bookkeeping until bound |

## 5.5 Read the scope of a soundness claim

Progress says a well-typed program in the model is finished or can step. Preservation says a step maintains its typing. Conservation accounts for resource identities across steps. To use one of these results, identify the modeled operations and the assumptions on effects, recursion, and stores. Appendix A identifies which features have been proved together.

<!-- technical-detail -->

Appendix A and `REVIEW-STATUS.md` distinguish proved per-fragment models (`MetatheoryAppendix` `closed_exact_once`, `MetatheoryFull`, `Metatheory/Closures`, `Metatheory/Polymorphism`, `MetatheoryPlaces`, `GhostErasure`, `OperationalGhostBridge`, `PlacePolymorphismBridge`) from the four open combined target statements across the full language and runtime.

| Target Statement | Status in Repository (`REVIEW-STATUS.md`) |
| --- | --- |
| Theorem 1 (Type, Mode & Capability Preserv.) | Open combined goal; progress/preservation proved separately per sub-model |
| Theorem 2 (Total Linear Exhaustion) | Open combined goal; proved for `closed_exact_once` in `MetatheoryAppendix` |
| Goal 3 (Fixed-Address Fiber Stacks) | Open runtime/DMA lifetime goal (`SYS-01`); stacks cannot pool during DMA |
| Theorem 4 (`@0` Ghost-Erasure Bisimulation) | Open full-language goal; big-step fragment proved in `GhostErasure.lean` |

# 6 What happens to an outstanding operation on cancellation?

A service can cancel a read while the device is still writing to its buffer. Continuation fission is a proposal for giving that buffer an owner on either exit, whether the read finishes normally or the service cancels it.

<!-- technical-detail -->

Cancelling a fiber while a disk controller or RDMA network card is still writing into its stack buffer creates a direct conflict between bounded wakeup latency and memory safety. Substructural continuation fission proposes splitting each abortable continuation into a normal resumption branch and a compiler-sliced reaper branch backed by quarantined escrow memory.

Per `REVIEW-STATUS.md` (lines 62–64), Continuation Fission is an unimplemented design proposal inserted as Chapter 6: `escrow`, `reap`, `.resume(...)`, `.reap()`, `EscrowCell`, and `EscrowMem` are absent from `crates/vesper-syntax`, the host checker, `crates/vesper-pulse`, and the Lean extractor, and `illustrative/ContinuationFission.lean` does not exist in `lean/`. Every syntax production, instruction-count target, quarantine bound, and DMA safety claim in this chapter states a proposed requirement rather than an implemented or verified guarantee.

## 6.1 Separate normal completion from cancellation

On the normal path a linear write transaction still needs a terminal operation. At an abort edge, the proposal would let a dedicated cleanup action take responsibility. Some actions finish locally. Others must retain storage until an external completion arrives. Giving a value a cleanup rule would preserve the original obligation on ordinary returns.

<!-- technical-detail -->

Monolithic effect systems (`Affect` POPL 2025, Leijen 2018) assign one mode `q` to `k`, forcing `[T-Let]` to reject `?` and `Pulse::timeout` across live `linear` obligations (`E0402`), while synchronous coroutine `Drop` risks DMA use-after-free and inline `AsyncDrop` stalls callers. Continuation fission proposes typing `k` as a linear additive conjunction (`&`, Girard's `with`) preserving exact-once normal-path linearity, freezing `escrow` memory without `'static` heap copies, detaching async `EscrowCell` sub-frames, and bounding `EscrowMem`.

    Property                         Sync Coroutine `Drop`    Inline `AsyncDrop`     Monolithic `[T-Let]`   Proposed Fission Target
    -------------------------------- ------------------------ ---------------------- ---------------------- -----------------------------------
    Normal-path linearity            None (`mem::forget` ok)  Weak opt-in            Exact-once             Exact-once (`reap` abort-only)
    `?` / `Cancel` across live oblig Permitted (silent drop)  Permitted (inline)     Rejected (`E0402`)     Permitted iff type declares `reap`
    `io_uring` / RDMA stack DMA      Unsafe if DMA outlives   Stalls until DMA ends  Requires heap buffer   Quarantined `EscrowCell` until CQE
    Caller wakeup on timeout         `O(1)` drop (unsafe DMA) Unbounded stall        Cannot cancel          `O(1)` detach target (`4` insns)
    Zombie quarantine memory         N/A                      Retains full stack     N/A                    Static `EscrowMem @ <= B` bound
    Lean 4 verification model        Model-dependent          Opaque poll state      Single-path `EffM`     Dual-branch pure state reducer

## 6.2 Express custody with escrow and reap

`escrow` marks the place temporarily entrusted to an external operation. A ticket records which storage it protects. A `reap` declaration describes how to discharge that type’s obligation during unwind. For a read into a buffer, the ticket protects the bytes even after the caller loses interest in the result.

<!-- technical-detail -->

The proposal adds `'escrow'` to `PassMode`, `ReapDecl` inside `linear struct` declarations (`'reap' '('` separates in two tokens from `FieldDecl`), and `.resume(...)` / `.reap()` eliminators on fissioned continuations.

    PassMode   ::= 'in' | 'inout' | 'out' | 'sink' | 'escrow' ;
    StructDecl ::= GhostMod? StorageSpec? ModeSpec? 'struct' UpperIdent TypeParams? ( '{' FieldDeclList? ReapDecl? '}' | ';' ) ;
    ReapDecl   ::= 'reap' '(' 'sink' 'self' ( ',' ParamList )? ')' EffClause? Block ;
    PostfixOp  ::= ... | '.' 'resume' '(' ArgList? ')' | '.' 'reap' '(' ')' ;

## 6.3 Check both exits at a suspension

A well-formed reaper must account for the fields it consumes and declare its cleanup effects. While an escrow ticket is live, conflicting access to its place stays suspended. At a possible abort, fissioned sequencing computes which owners the cleanup branch must receive. The handler chooses either the resume continuation or the reap continuation. Executing both would give two consumers the same resource.

<!-- technical-detail -->

`[WF-Reap]` restricts `reap` to `linear` structs with infallible `res(eps_reap) = linear` so cleanup cannot abort mid-unwind; `[T-EscrowLoan]` freezes `x.pi` in `escrow(h)` until `[T-EscrowClaim]` or `reap` runs; and `[T-Let-Fission]` partitions `Gamma_2 = Gamma_aff * Gamma_reap` in reverse LIFO order, reducing to `[T-Let]` (`E0301`) whenever a `linear` type omits `reap` (such as `IrqGuard`).

    `submit_pread(fd, off, escrow page)` ──▶ `page : escrow(h)` (CPU read/write/drop frozen while `h` live)
                                                    │
                        `FissionCont` `k` (`linear + pinned + lifo`) at `?` / `Pulse::timeout`
                     ┌──────────────────────────────┴──────────────────────────────┐
                     ▼ `k.resume(v, s)` (`[T-With-Resume]`)                        ▼ `k.reap()` (`[T-With-Reap]`)
    ┌──────────────────────────────────────────┐          ┌────────────────────────────────────────────────────┐
    │ `[T-EscrowClaim]` restores `page : own`  │          │ 1. Sync `eff []` reapers (`tx.reap`) run inline    │
    │ `tx.commit()` discharges linear handle   │          │ 2. Async `EscrowCell` detaches to `reap_ring`      │
    │ `EscrowCell` popped inline; stack kept   │          │ 3. 64 KiB stack recycles; `page` waits for CQE     │
    └──────────────────────────────────────────┘          └────────────────────────────────────────────────────┘
    ----
    [WF-Reap]       u(m_decl(T)) = linear   res(eps_reap) = linear   self :^(own)_(1,linear) T, z :^(mu_z) tau_z |- e_reap : Unit | eps_reap
    [T-Let-Fission] e_1 : tau_1 | eps_1 (res = affine)   Gamma_2, x : tau_1 |- e_2 : tau_2 | eps_2   Gamma_2 = Gamma_aff * Gamma_reap ~> eps_reap
                    -----------------------------------------------------------------------------------------------------------------------------
                    Delta ; Sigma ; Phi ; Gamma_1 * Gamma_2 |- let x = e_1 in e_2 : tau_2 @ (1, m_2) | eps_1 ; (eps_2 |_| eps_reap)
    [T-Handle-With] Binds `k :^(own)_(1, linear+pinned+lifo) FissionCont_l` eliminated once via `k.resume(v, s)` or `k.reap()`

## 6.4 Budget for detached storage

`EscrowMem` counts memory that may remain quarantined. `IoReap` accounts for external cleanup work. A service that admits many concurrent reads needs a bound on the sum of buffers that might remain live after cancellations. If another cancellation would exceed the byte budget, the service must wait for storage to return before admitting more work.

<!-- technical-detail -->

`EscrowMem` and `IoReap` extend §5.3's pomonoids: sequential operations whose tickets are claimed before the next submission take `max(g_1, g_2)`, whereas simultaneously live nested `escrow` loans sum additively so `QF_UFBV` can bound worst-case quarantine slabs.

    Sequential claimed escrows : EscrowMem(claim(prep(escrow b_1)) ; claim(prep(escrow b_2))) = max(sizeof(b_1), sizeof(b_2))
    Simultaneous live escrows  : EscrowMem(let h_1 = prep(escrow b_1) in let h_2 = prep(escrow b_2) in e_abort) = sizeof(b_1) + sizeof(b_2)
    Shard admission target     : max_concurrent_fibers * per_fiber_escrow_mem <= MAX_BYTES

## 6.5 Distinguish rollback from external completion

A write-ahead log transaction may roll back its local state immediately. A submitted kernel read needs the original request’s terminal completion. A remote memory transfer may need confirmation from the network interface before the registration can be released.

<!-- technical-detail -->

The proposed `replicate_verified_page` excerpt contrasts inline WAL rollback (`WriteTxn`, `eff []`), `io_uring` CQE quarantine (`UringReadTicket`, `eff [IoReap @ <= 1]`), and RDMA Type-2 Memory Window invalidation (`RdmaWindowTicket`, `eff [RdmaReap @ <= 1]`) across `?` and `Pulse::timeout`.

~~~vesper
// Proposed Ch 6 syntax (`reap` and `escrow` are not implemented in the compiler):
pub linear struct WriteTxn {
    lsn: u64,
    staged_slots: RegionVec[u32],
    reap(sink self, inout wal: WalJournal) eff [] {
        let WriteTxn { lsn, staged_slots } = self;
        wal.abort_staged_lsn(lsn, sink staged_slots);
    }
}
pub linear struct UringReadTicket[@sqe_id: u64] {
    sqe_id: u64,
    reap(sink self, escrow dst: ByteSlice) eff [IoReap @ <= 1] {
        let UringReadTicket { sqe_id } = self;
        IoUring::prep_async_cancel(sqe_id);
        IoUring::await_final_cqe(sqe_id, escrow dst);
    }
}
pub fn replicate_verified_page(
    inout wal: WalJournal, in hdr_bytes: ByteSlice, fd: RawFd, disk_off: u64,
) -> Result[u64, ReplError]
eff [Io, IoReap @ <= 1, EscrowMem @ <= 4096, Fail[ReplError], Cancel] {
    Pulse::timeout(5.ms(), || {
        let mut disk_page: ByteArray[4096] = uninit;
        let disk_sqe = IoUring::submit_pread(fd, disk_off, escrow disk_page[0..4096]);
        let tx = wal.begin_txn();
        let meta = FrameHeader::decode_non_malleable(in hdr_bytes)?;
        let n_read = IoUring::await_pread(sink disk_sqe, escrow disk_page[0..4096])?;
        tx.stage_verified(inout wal, in meta, in disk_page[0..n_read]);
        Ok(tx.commit(inout wal))
    })
}
~~~

## 6.6 Model a choice of continuations

The proposed extraction gives Lean a normal branch and a cleanup branch that share a resource budget. A proof would need to account for every owner on whichever branch actually executes. The chapter’s Lean listing illustrates the intended theorem. External device behavior and completion still need an operational model.

<!-- technical-detail -->

Proposed Stage 5B extraction lowers `[T-Let-Fission]` to a two-branch pure state reducer; `illustrative/ContinuationFission.lean` (absent from `lean/`) models abstract ticket/place states for Theorem 5 and Theorem 6 without proving compiler lowering or hardware DMA completion.

    [[let x = e_1 in e_2]]_(Gamma_reap)(s_0) :=
      match [[e_1]](s_0) with | .ok (v, s_1) => [[e_2]](v, s_1) | .error (e, s_1) => .error (e, [[reap_(Gamma_reap)]](s_1))
    ----
    -- illustrative/ContinuationFission.lean (illustrative only; absent from `lean/`)
    inductive PlaceState  where | cpuOwned | escrowed (tid : Nat) | quarantined (tid : Nat) | recycled
    inductive TicketState where | active (p sz : Nat) | committed | reaping (p sz : Nat) | reaped
    theorem zero_uaf_during_escrow_and_quarantine ... (hdma : HwDmaMayTouch w p) :
      w.places p != .cpuOwned /\ w.places p != .recycled                           -- Theorem 6 (abstract model)
    theorem fission_choice_exhausts_linear_obligation (c : FissionChoice) :
      dischargeLinearTicket c = .committed \/ dischargeLinearTicket c = .reaped     -- Theorem 5 (abstract model)

## 6.7 Lower the landing pads and preserve compatibility

The proposed compiler walks backward from an abort edge to identify required reapers. An `EscrowCell` separates storage that may outlive the suspended stack. Separate landing pads transfer control to resume or cleanup. Removing cleanup support or raising a public quarantine requirement can break callers that already rely on it. Constant-time detachment and bounded reclamation remain design requirements.

<!-- technical-detail -->

Proposed lowering slices live `linear` bindings backward from abort edges in Stage 3 (`vesper-mir-pcg`, tagging async `escrow` locals `Storage::EscrowSubFrame`), places `EscrowCell` sub-frames on the worker's Domain-1 `ShardSlab` in Stage 4 (`vesper-opt-egglog`) while keeping CPU locals on the 64 KiB `Pulse` fiber stack, and emits 16-byte `(resume_ip, reap_ip)` descriptors in Stage 5A to run inline `eff []` reapers and enqueue async cells onto `PulseWorkerShard::reap_ring`. Under SemVer (`crates/vesper-semver`), adding `reap` or lowering `EscrowMem` is Minor-compatible, whereas removing `reap` or raising `EscrowMem` is Major-breaking.

# 7 Traits, models, macros, and syntax extensions

Pact lets a library define an operation for many types or choose between implementations for one type. Forge lets a library introduce new notation that expands into operations the compiler already checks.

<!-- technical-detail -->

Vesper Pact unifies synchronous, effectful, and substructural interfaces under a single colorless trait system that pairs orphan-checked canonical implementations with lexically scoped named models and stack-lent trait objects. Vesper Forge provides import-free skeletal enforestation for syntactic sugar alongside sandboxed WebAssembly macros that preserve hygiene and effect ceilings.

Trait coherence, builtin derivations, monomorphization, `dyn` trait and function objects, declarative `macro` rules, and `sugar` expansion are implemented across `crates/vesper-pact`, `crates/vesper-forge`, `src/pact_frontend/`, `src/dyn_trait/`, `src/dyn_fn.rs`, and `src/forge_frontend/`. Per `REVIEW-STATUS.md` (`COMP-01`–`COMP-05`, `TYPE-10`), `wasm32-wasip2` macro provenance gates, `@lean_notation` unexpander synthesis, model-brand ABI transport, and the `std/*` sugar catalogs in §§7.6–7.8 remain design proposals rather than shipped standard-library modules.

## 7.1 Extend a program without hiding its contracts

An abstraction must preserve who consumes a value, which places change, and what effects occur. A library fold can abstract over a source’s effect row. A macro introducing a write still has to expose the corresponding permission. The caller can then read the access requirements from the signature or macro invocation.

<!-- technical-detail -->

Six invariants govern Pact and Forge: colorless `trait` declarations abstract over `mod linear`, pass modes, and `eff [r]`; stratified coherence pairs canonical `impl` blocks with lexical `model` overrides (`[T-ModelBrand]`); second-class `in/inout/sink dyn` parameters pass as 16-byte `(data_ptr, vtable_ptr)` pairs with zero heap allocation; `wasm32-wasip2` macros run inside fuel-metered `vesper-loom-sandbox` (`wasmtime`) instances without ambient I/O or clocks; Stage 1 parses `sugar` into `PhraseSkeleton` CST nodes formatted by `Stylus` before import resolution under `[WF-SugarPass]`; and two-way scope-set hygiene plus `[T-MacroConfinement]` prevent capture and capability smuggling.

## 7.2 Abstract over a source and its effects

A source trait can name the item it yields through an associated type. Reading a memory sequence might be pure while reading a socket can suspend. An effect variable stands for the operations needed by each source. A fold also accounts for the effects of its combining function, so the same loop can work with both sources.

<!-- technical-detail -->

Placing `mod linear` on `Source::Item` and `r: eff = []` on `next` lets `fold_source` lower to an inline loop on a pure `TiledSeq` (`r1 = []`, `r2 = []`) and suspend on pooled fiber stacks over `TcpFrameSource` (`r1 = [Net, Cancel]`) while `[T-Let]` checks `u(m_acc) <= res(r1 | r2)`.

~~~vesper
pub trait Source[r: eff = []] {
    type Item: mod linear;
    fn next(inout self) -> Option[Self::Item] eff [r];
}
pub fn fold_source[S, T, Acc, r1: eff, r2: eff](
    inout src: S, sink init: Acc, f: fn(sink Acc, sink T) -> Acc eff [r2],
) -> Acc eff [r1 | r2] where S: Source[r1, Item = T] {
    let mut acc = init;
    loop {
        match src.next() {
            Option::Some(item) => { acc = f(sink acc, sink item); }
            Option::None => break acc,
        }
    }
}
~~~

## 7.3 Pick a canonical implementation or a named model

Coherence gives an implicit trait call a unique implementation. The orphan and overlap rules limit where canonical implementations can be introduced. A named model lets a caller choose explicitly, such as ordering points by horizontal position. A type brand can remember that choice in a container so code cannot accidentally mix incompatible orderings.

<!-- technical-detail -->

Under the Covered-First Orphan Rule in `crates/vesper-pact`, package `P` may declare an anonymous `impl[A] Trait[T_1, ..., T_n] for T_0` iff `Trait` or the first uncovered type constructor in `(T_0, T_1, ..., T_n)` belongs to `P` (`PACT0101` rejects orphan `impl`s, and `PACT0102` rejects overlapping canonical `impl`s with a hint to declare a named `model`), solved via de Bruijn (`?0`, `?1`) SLG tabling. Any package may declare `pub model Name: Trait for Type` and activate it with `use model`, while ordered/hashed containers stamp model identity `M` into their type via `[T-ModelBrand]`.

~~~vesper
pub model BigEndianUuidCodec: WireCodec for Uuid {
    fn encode(in self, inout out: ByteSlice) -> Result[usize, EncodeError] { out.write_u128_be(self.bits) }
    fn decode(in raw: ByteSlice) -> Result[Uuid, DecodeError] { raw.read_u128_be() }
}
fn write_ids(in ids: Slice[Uuid], inout buf: ByteSlice) -> Result[usize, EncodeError] {
    use model BigEndianUuidCodec;
    encode_slice(in ids, inout buf)
}
~~~

    [T-ModelBrand]  Delta ; Sigma_models |- M_1 ===_def M_2  ==>  CacheBTree[K, V, M_1] === CacheBTree[K, V, M_2]
    ----
    Read-Only `dyn Trait` VTable Header (16-byte `(data_ptr, vtable_ptr)` pair; one vtable per `(dyn Trait[args], T)`):
      word 0 : drop_fn(ptr)  (0 when `T` owns nothing)    | word 1 : move_slot (0: Stage 1 moves bitwise by `size`)
      word 2 : size of `T`                                | word 3 : alignment of `T`
      word 4+: method slots in declaration order (`0` for handler-effect methods -> closed whole-program vtable dispatch)

Bounded generics monomorphize per call site (rejecting unbounded polymorphic recursion); builtin `Eq`, `Ord`, and deterministic FNV-style `Hash` hold canonically on integers, `bool`, `()`, and 2..4 `copy` tuples, while structs opt in via an explicit empty `impl Ord for Point {}` or `impl[T] Ord for Pair[T] {}` (lexicographic field order, keeping secret structs unhashable by default). Unboxed `dyn Trait[args]` is admitted only as the whole type of an `in`, `inout`, or `sink` parameter (`E0632` elsewhere) and used only as a receiver or forwarded `dyn` argument (`E0631`); object safety (`E0630`) forbids associated types, generic methods, extra `Self` uses, open rows, and function-valued slot args/results; coercion requires a monomorphic `T` and unique conformance (`E0608`, `E0633`, caller row check `E0401`); `sink dyn` requires `T <= affine` while `in`/`inout dyn` may borrow `linear`; and verified/Wasm/export-ABI backends reject `dyn` (`E0634`).

First-class `Box[dyn Trait[args]]` (`affine`, 1-word pointer to `[vtable_ptr, value @ +8]`) is built via `Box::new(v)` or `Box::new[dyn Trait[args]](v)` for `T <= affine` (`E0205`; no `Box(...)` unpack `E0631`), mutates `s[i]` in place on `inout self` (`E0300` if passed as replaceable `Box`), drops once across loops/exits (`E0312` on cross-iteration move), and is rejected on verified/Wasm targets (`E0634`). Function objects `dyn Fn(A) -> R eff [E]`, `dyn FnMut(..)`, and `dyn FnOnce(..)` expose one `call` method (`in`/`inout`/`sink self`, `E0307` on `Fn` capture writes, `E0207` arity), allocate nothing when lent, bar `linear` captures from `Fn`/`FnMut` (`E0310`) and `Box` (`E0205`), and in Stage 1 require typed capture bindings (`E0102`) without enclosing type parameters (`E0633`).

## 7.4 Keep macro names and authority local

Hygiene protects bindings in two directions. A temporary introduced by a macro must keep its own identity. A reference the macro author intended to resolve at definition time must keep that meaning at the call site. The proposed WebAssembly System Interface (WASI) macro boundary limits access to declared inputs, allowing expansion results to be cached by macro and input content.

<!-- technical-detail -->

CST identifiers carry `(Symbol, Span, ScopeSet, Option[PreResolvedDefId])`: `ScopeMark(k)` stamps enforce `marks(binder) <= marks(reference)` while `quote` paths pre-resolve `DefId`s at macro compile time, WASI sandboxes memoize by `(MacroWasmBlake3, InputAstBlake3)`, and `[T-MacroConfinement]` defaults `macro_emits_effects = []` in `Loom.toml`.

    [T-MacroConfinement]  d = PreResolvedDefId(ident) in expand(M, ast)   origin(d) = M
                          -------------------------------------------------------------------------
                          caps(d) <= macro_emits_effects(M) /\ capability_ceiling(P_caller)

## 7.5 Parse an extension in two steps

The first parse records a phrase skeleton, including its delimiters and argument positions. After imports resolve, an enforester groups those pieces using the selected sugar declaration. The expansion must honor passing modes and report its effects. If the caller lends a value for reading, the expansion cannot consume it.

<!-- technical-detail -->

Stage 1 builds import-free `PhraseSkeleton` CST nodes and Stage 2 enforests them with rank `< 64` (`E0140` when `Assoc::None` dotted operators mix); `vesper run` already prints `42` on the combined `macro` + `sugar` program below (expression macros resolve a closed function set).

    `.ves` Source ──▶ [Stage 1 Parser (`FIRST_2` + `TagIdent` trivia; zero import lookup)]
                           │
                           ├──▶ `PhraseSkeleton` CST ──▶ `Stylus` (`vesper fmt`) & `Chroma` Tier 0
                           ▼
                      [Stage 2 Enforester (`rank < 64`) & WASI `wasm32-wasip2` Sandbox (0 I/O, fuel)]
                           │ stamps `ScopeMark(k)` + checks `[WF-SugarPass]` & `[T-MacroConfinement]`
                           ▼
                      Checked Core HIR ──▶ `Pact` SLG Solver (`impl` vs `model` `ModelBrand`)

~~~vesper
macro keep($value: expr) -> expr { quote { $value } }
sugar second: infix(`.+`, additive, left) {
    rule ($left:expr .+ $right:expr) => { keep!($right) }
}
fn main() -> i64 { 10 .+ 42 }
~~~

`[WF-SugarPass]` evaluates rvalue `$e:expr` holes at most once per path and bars hidden `inout`/`out`/`sink` place escalations, while `[T-SugarEffectHonesty]` confines template-introduced effects to `emits eff [eps_s]` (`@lean_notation` is proposed for `Notations.lean`). Statement-tail `$..rest:stmts` runs on fallthrough only (`guard`), whereas implemented `$..rest:scope` wraps exits in `Exit::resume()` across fallthrough, `return`, `break`, and concrete unlabelled `Fail` (diagnosing open rows or labelled `Fail`).

    [WF-SugarPass]          forall j, mu_pat >=_vis mu_j  /\  ((exists j != in) ==> LinearSinglePath(p, t))
    [T-SugarEffectHonesty]  d in PreResolvedDefs(T) ==> caps(d) <= eps_s /\ macro_emits_effects(M) /\ caller_ceiling

~~~vesper
pub sugar defer : stmt {
    rule (defer $cleanup:block $..rest:scope) => {
        { let __exit = $..rest; $cleanup; __exit.resume() }
    }
}
~~~

## 7.6 Recognize the proposed standard sugar

`errdefer` attaches cleanup to an error path. `ensure` checks a condition and raises the supplied error if the check fails. `guard let` binds a matching value and sends the unmatched case down an exit path. `emplace` initializes a destination, `swap` exchanges owners, and `scratch` scopes temporary storage. `par` groups concurrent work, `within` supplies a timeout and a recovery arm, and `holding` brackets acquisition and release of a guard.

`bits{…}` describes a bit layout, `ghost calc` chains proof steps, and elementwise operators apply a scalar operation across lanes. The proposed library forms all expand into operations in the checked core.

<!-- technical-detail -->

The `std/*` catalog below is an unshipped design proposal; in `errdefer`, placing `match __res` outside local `handle Fail` lets `tx: WriteTxn` cross `?` while `[T-Match]` rejects forgetting `tx` (`E0302`).

    Proposed Sugar          Call-Site Shape                       Slot & Pass Floor                  Core Boilerplate Replaced
    ----------------------- ------------------------------------- ---------------------------------- ---------------------------------------------------------
    `std/control::errdefer` `errdefer(e) { tx.rollback(); }`      `TailSugarStmt` (`$..rest:stmts`)  Local `handle Fail` + `match` (`E0302` if `tx` forgotten)
    `std/control::ensure`   `ensure n <= 4096 else Err::Big;`     `TailSugarStmt` (`eff [Fail]`)     `if !(...) { Fail::raise(...) }` (`@lean_notation`)
    `std/control::guard`    `guard let Some(x) = e else { ... };` `TailSugarStmt` (`Never` diverge)  Rightward `match` drift while keeping path facts in `Phi`
    `std/mem::emplace`      `emplace out pkt { ver: 1, ... };`    `TailSugarStmt` (`out $p:place`)   Direct field-by-field `OutSlot` init with zero stack temp
    `std/strata::scratch`   `scratch arena[4096] { ... }`         `ClauseSugarExpr`                  `Region::scoped_exact[B](|cap arena| ...)`
    `std/hw::holding`       `holding irq = Irq::mask() { ... }`   `ClauseSugarExpr`                  `@ lifo` guard acquire + reverse release on `Ok`/`Fail`
    `std/pulse::par`        `par { a(...) } and { b(...) }`       `ClauseSugarExpr` (`eff [Scope]`)  `Pulse::scope` fork-join checked by `[T-OutSplitSlice]`
    `std/pulse::within`     `within 5.ms() { ... } on_timeout {}` `ClauseSugarExpr` (`eff [Clock]`)  Local `Cancel` handler with mandatory timeout arm
    `std/wire::bits`        `bits{ ver: u4 = 4, len: u12 = n }`   `TaggedDelimExpr` (`@lean_notation`) Bitfield shifts/masks checked in `QF_BV`
    `std/verify::calc`      `ghost calc { a == b; <= c; }`        `TailSugarStmt` (`@0` ghost)       Zero-cost intermediate lemmas mirrored to Lean `calc`

~~~vesper
pub sugar errdefer : stmt emits eff [Fail] {
    rule (errdefer($err:pat) $cleanup:block $..rest:stmts) => {
        {
            let __res = handle Fail in { Ok({ $..rest }) } with { return(v) => v, raise(e, _) => Err(e) };
            match __res { Ok(v) => v, Err($err) => { $cleanup; Fail::raise($err) } }
        }
    }
}
pub sugar emplace : stmt {
    rule (emplace out $dst:place { $( $field:ident : $val:expr ),* } ;) => { { $( $dst.$field = $val; )* } }
}
pub sugar par : expr emits eff [Scope] {
    rule (par $left:block and $right:block) => {
        @vesper-pulse/scope::Pulse::scope(|inout __s| {
            let __f1 = __s.spawn(|| $left);
            let __f2 = __s.spawn(|| $right);
            (__f1.join(), __f2.join())
        })
    }
}
~~~

## 7.7 Read domain-specific sugar by its expansion

`send` and `recv` advance a session endpoint to its next protocol state, and `close` consumes the final endpoint. `transact` proposes rollback for opted-in mutations. `morph` rewrites a uniquely owned tree node in place. `ct_if` expresses a secret-dependent selection subject to timing checks. `modify_reg` groups appropriate device register updates into a write.

A loop invariant states what each iteration preserves. A decreases clause names a quantity that makes progress toward termination. A `zset` comprehension builds weighted stream records. Expanding the shorthand leaves those permission checks and proof obligations in place.

<!-- technical-detail -->

The domain catalog and its helper targets (`Chan::send`, `fip::unpack_reuse`, `RmwReg::read`, `ct_select`) are likewise design proposals expanding under `[WF-SugarPass]`.

    Proposed Domain Sugar        Call-Site Shape                               Slot & Pass Floor                 Subsystem Contract Preserved After Expansion
    ---------------------------- --------------------------------------------- --------------------------------- ---------------------------------------------------------
    `std/txn::transact`          `transact inout bbr { ... }`                  `ClauseSugarExpr` (`inout`)       Opt-in snapshot rollback over keep-writes when `$p: copy`
    `std/chorus::send` / `recv`  `send m on sink ch; recv a from sink ch;`     `TailSugarStmt` (`sink $ch`)      Linear session typestate rebinding (`E0302` if unclosed)
    `std/vault::ct_if`           `ct_if mask { a } else { b }`                 `ClauseSugarExpr` (`ConstTime`)   Evaluates both arms unconditionally into `ct_select`
    `std/fip::morph`             `morph sink node { Black(l,r) => Red(l,r) }`  `ClauseSugarExpr` (`sink`)        Zero-alloc `@fip` `ReuseSlot` enum rewrite
    `std/silicon::modify_reg`    `modify_reg inout uart.cr { .txe = 1 };`      `TailSugarStmt` (`inout`)         Single MMIO read + masks + write; rejects `W1C` registers
    `std/aura::focus`            `focus inout app.{ nav => ..., pane => ... }` `ClauseSugarExpr` (`inout`)       Opens disjoint `FieldTree` sub-borrows under `[T-CallProj]`
    `std/flux::zset`             `zset{ (r.k, r.v) => +1 | r in b, r.ok }`     `TaggedDelimExpr`                 Pre-sized `TiledSeq` AoSoA DBSP comprehension
    `std/verify::while_verified` `while c invariant phi decreases d { ... }`   `ClauseSugarExpr` (`@0` ghost)    Expands to `loop` + `@0` entry/step/variant assertions
    `std/pact::using`            `using model ReverseOrd { map.insert(k,v) }`  `ClauseSugarExpr`                 Scopes a non-canonical Pact `model` to a single expression
    `std/beacon::trace_span`     `trace_span "wal.sync" [lsn = s] { ... }`     `ClauseSugarExpr` (`eff [Trace]`) Stack W3C span checking `@pii` via `Telemetrable`

~~~vesper
pub sugar send : stmt emits eff [Net] {
    rule (send $msg:expr on sink $ch:place ; $..rest:stmts) => {
        { let $ch = @vesper-chorus/chan::Chan::send(sink $ch, $msg); $..rest }
    }
}
pub sugar recv : stmt emits eff [Net] {
    rule (recv $pat:pat from sink $ch:place ; $..rest:stmts) => {
        { let ($pat, $ch) = @vesper-chorus/chan::Chan::recv(sink $ch); $..rest }
    }
}
pub sugar transact : expr emits eff [Fail] {
    rule (transact inout $p:place $body:block) => { { let __snap = $p; errdefer(_) { $p = __snap; } $body } }
}
pub sugar ct_if : expr emits eff [ConstTime] {
    rule (ct_if $mask:expr $t:block else $f:block) => { { let __t = $t; let __f = $f; @vesper-vault/secret::ct_select($mask, __t, __f) } }
}
pub sugar modify_reg : stmt {
    rule (modify_reg inout $reg:place { $( . $f:ident = $v:expr ),* } ;) => {
        { let mut __w = @vesper-hw/mmio::RmwReg::read(in $reg); $( __w = __w.$f($v); )* @vesper-hw/mmio::RmwReg::write(inout $reg, __w); }
    }
}
pub sugar while_verified : expr {
    rule (while $c:expr invariant $inv:expr decreases $v:expr $body:block) => {
        {
            let ghost _ = @std/verify::assert_ghost($inv);
            loop { if !($c) { break; } let ghost __v0: usize = $v; $body; let ghost _ = @std/verify::assert_ghost($inv); let ghost _ = @std/verify::assert_lt_ghost($v, __v0); }
        }
    }
}
~~~

## 7.8 Decode shorthand for common tasks

The readability proposals include tagged literals for bytes, durations, addresses, and regular expressions. Formatting and logging tags describe where values go. Pipeline placeholders show where the previous result becomes an argument. `race` chooses a completion, and `retry` carries a clock effect when it waits between attempts.

Collection folds such as `sum` and `any` express accumulation. `lanes` handles a vector tail. `update` describes changes to a copyable record, and `transition` names a typestate change. An assertion can name its supporting lemma. Test and property declarations register examples or generators. The language guide shows the test syntax that runs in this checkout.

<!-- technical-detail -->

The readability catalog is also proposed; infix word operators (`r matches Ok(v)`) remain banned by Rule 7 (§2.2) so parsing stays independent of imports.

| Proposed Readability Sugar | Call-Site Shape | Skeleton Slot | Expanded Core Lowering |
| --- | --- | --- | --- |
| `std/control::holding` | `holding f = File::open(p)? { ... }` | `ClauseSugarExpr` (`:scope`) | Releases any `Release` resource via `Exit::resume` |
| `std/fmt::fmt`, `std/beacon::log` | `fmt"lsn={lsn}"`, `log"pages={n}"` | `TaggedDelimExpr` | Writer chains; `log` checks `@pii` `Telemetrable` |
| `std/lit::hex`,`bytes`,`dur`,`ip` | `hex"dead beef"`, `dur"5ms"`, `re"[a-z]+"` | `TaggedDelimExpr` | Compile-time validated constant literals |
| `std/control::\|>` placeholder | `frame \|> parse(_, in cfg) \|?> check(_)` | `PipeExpr` | Substitutes piped value into bare `_` argument |
| `std/pulse::race` | `race { r = recv(in ch) => ..., ... }` | `ClauseSugarExpr` (`Scope`) | `Pulse::race` keeping losing-arm cancel visible |
| `std/pulse::retry` | `retry 3 backoff 10.ms() { ... }` | `ClauseSugarExpr` (`Clock`) | Bounded backoff loop keeping `Clock` at call site |
| `std/iter::sum`, `max`, `any` | `sum{ x * w \| (x, w) in zip(a, b) }` | `TaggedDelimExpr` | Single counted accumulator loop with zero alloc |
| `std/vector::lanes` | `lanes v in xs { ... }` | `ClauseSugarExpr` | `VVec` SIMD strip-mining plus scalar tail |
| `std/control::update` | `update cfg { port = 80, tls = true }` | `ClauseSugarExpr` | Copy-and-assign update for `copy` structs |
| `std/typestate::transition` | `transition inout c from Init to Open { }` | `ClauseSugarExpr` (`inout`) | Typestate rebinding for protocol state machines |
| `std/verify::assert` | `assert n <= cap because lemma_bound(buf);` | `TailSugarStmt` | Passes named lemma to solver / Lean `by exact` |
| `std/test::test`, `prop` | `test "wal" { ... }`, `prop "rt" for x: T` | `ItemSugar` | Test and Crucible/Codex property registration |

~~~vesper
pub sugar holding : expr {
    rule (holding $r:ident = $acquire:expr $body:scope) => {
        { let $r = $acquire; let __exit = $body; @std/control::Release::release(sink $r); __exit.resume() }
    }
}
pub sugar retry : expr emits eff [Clock] {
    rule (retry $n:expr backoff $delay:expr $body:block) => { @std/pulse::retry_with_backoff($n, $delay, || $body) }
}
pub sugar sum : tag {
    rule (sum{ $e:expr | $p:pat in $src:expr }) => { { let mut __acc = 0; for $p in $src { __acc += $e; } __acc } }
}
~~~

## 7.9 From a phrase to a resolved call

The trait solver records goals so repeated or recursive lookups can share work. The enforester selects the syntax rule for a phrase. The hygiene engine preserves identifier identity through expansion. When debugging an unexpected call, inspect which rule expanded, which names it bound, and which implementation the resulting call selected.

<!-- technical-detail -->

`crates/vesper-forge` expands `PhraseSkeleton` nodes, checks `HolePassFloor` (`[WF-SugarPass]`), verifies Flatt subset hygiene (`HygienicIdent::can_bind_local`), and audits pre-resolved macro definitions against `macro_emits_effects` and `CapabilityCeiling` (`verify_macro_anti_smuggling`), before `crates/vesper-pact` memoizes canonicalized trait goals (`ModelBrand`, `CanonicalTraitGoal`) and enforces orphan/overlap coherence (`PACT0101`, `PACT0102`).

    // crates/vesper-pact: SLG tabled goal canonicalization and orphan/brand checking
    pub enum ModelBrand { Canonical { trait_id: TraitId }, Named { model_id: ModelId } }
    pub struct CanonicalTraitGoal {
        pub trait_id: TraitId, pub brand: ModelBrand, pub canonical_type_args: Vec<u32>,
        pub max_mode_bound: Mode, pub effect_row: EffectRow,
    }
    pub fn check_canonical_orphan_rule(
        defining_pkg: u32, trait_pkg: u32, ordered_type_constructors: &[Option<u32>],
    ) -> Result<(), &'static str>;
    ----
    // crates/vesper-forge: scope-set subset hygiene, anti-smuggling, and `[WF-SugarPass]`
    pub struct HygienicIdent { pub text: SmolStr, pub marks: Vec<ScopeMark>, pub pre_resolved: Option<DefId> }
    impl HygienicIdent { pub fn can_bind_local(&self, use_site: &Self) -> bool; } // `self.marks <= use_site.marks`
    pub fn verify_macro_anti_smuggling(
        macro_pkg_name: &SmolStr, macro_pkg_id: u32, macro_declared_emits: &FxHashSet<SmolStr>,
        caller_ceiling: &CapabilityCeiling, emitted_idents: &[HygienicIdent],
        lookup_def_effects: impl Fn(DefId) -> Vec<SmolStr>,
    ) -> Result<(), MacroSecurityError>;
    pub enum HolePassFloor { SharedIn, MutInout, InitOut, MoveSink }
    impl HolePassFloor { pub fn permits_template_use(self, demanded: Self) -> bool; }
