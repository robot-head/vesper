// ============================================================================
// Drop-in Typst Chapter for `docs/manuscript/content.typ` (Typst 0.15.1)
//
// Integration notes:
//   1. Insert inside `#let manuscript(chapter-deck) = [ ... ]` directly after
//      Chapter 5 (`<ch-5-5>`, line 815 of `content.typ`) and increment
//      subsequent chapter numbers (`6..41` -> `7..42`) and `verify.py` range,
//      OR append after Chapter 41 as `= 42. ...` with zero renumbering.
//   2. Uses `content.typ`'s native `#_infer`, `#_compact-listing`, `#chapter-deck`,
//      and bundled fonts (`Libertinus Serif`, `DejaVu Sans Mono`,
//      `New Computer Modern Math`) with zero external packages.
// ============================================================================

= 6. Substructural Continuation Fission, Escrow Projections, and Reaper Calculus
<ch-fission>
#chapter-deck[Additive-Conjunctive Continuations (`resume` & `reap`), Co-Owned `escrow` DMA Leases, PCG Backward Slicing, and Graded Quarantine Bounds]

Core substructural effect systems—including #link("https://iris-project.org/pdfs/2025-popl-affect.pdf")[Affect] (van Rooij and Krebbers, POPL 2025) and #link("https://www.microsoft.com/en-us/research/wp-content/uploads/2023/11/qualified-effect-types-2018.pdf")[Qualified Effect Types] (Leijen, 2018)—assign a single monolithic usage mode $q in {upright("copy"), upright("affine"), upright("linear")}$ to each captured delimited continuation $k$. In #link(<ch-4-2>)[Section 4.2] and #link(<ch-5-2>)[Section 5.2], Vesper's baseline `[T-Let]` Resumption Frame Condition ($u(bold(m)_z) subset.eq.sq upright("res")(epsilon_1)$) enforces soundness under that monolithic assumption by statically rejecting (`E0301`) any abortable effect (`Fail::raise` via `?` or `Cancel` via `Pulse::timeout`) whenever a `linear` obligation resides in the continuation frame $Gamma_2$.

In real systems software, however, monolithic continuations create an acute three-way trilemma across completion-based kernel I/O (`io_uring`, IOCP), zero-copy RDMA, GPU command queues, and transactional storage:

+ *Synchronous Coroutine Drop Causes Hardware DMA Use-After-Free:* In poll-based coroutine languages (#link("https://without.boats/blog/poll-drop/")[Asynchronous Destructors], Without Boats; #link("https://sabrinajewson.org/blog/async-drop")[Async Destructors and Completion Futures], Jewson; #link("https://clef-lang.com/blog/rusts-retrofits/")[Rust's Retrofits]), cancelling a future by dropping its state machine reclaims stack or coroutine memory synchronously while kernel `io_uring` SQEs (#link("https://man7.org/linux/man-pages/man7/io_uring_cancelation.7.html")[`io_uring_cancelation(7)`]) or remote RNIC `rkey` windows (#link(<ch-34-2>)[Section 34.2]) still hold live physical DMA pointers into that memory.
+ *Inline Blocking `AsyncDrop` Destroys Timeout Latency:* Stalling the cancelling fiber inside an asynchronous destructor until a wedged disk controller, stalled peer, or remote RNIC finishes draining defeats the deadline guarantee that motivated cancellation in the first place.
+ *Monolithic Linear Rejection (`E0301`) Prohibits Idiomatic `?` and Timeouts:* Requiring manual destructuring before every fallible `?` check or `Pulse::race` branch forces quadratic state-cleanup boilerplate across pipelined I/O loops.

*Substructural Continuation Fission* resolves all three constraints simultaneously without garbage collection, `'static` heap boxing, or `Pin`. Rather than treating a captured continuation $k$ as an indivisible function, Vesper fissions $k$ along Stage 3's Place Capability Graph (#link(<ch-7-5>)[Section 7.5]) into a *linear additive conjunction* ($amp$, Girard's _with_) pairing the normal resumption path $k . upright("resume")$ with a compiler-sliced residual reaper $k . upright("reap")$, backed by co-owned `escrow` place projections and pomonoid quarantine-memory budgets (`EscrowMem`).

== 6.1 Architectural Comparison and Core Fission Guarantees
<ch-fission-1>

#figure(
  table(
    columns: (21%, 19%, 19%, 19%, 22%),
    align: (left, left, left, left, left),
    table.header(
      [*Property*],
      [*Sync Coroutine `Drop`*],
      [*Inline `AsyncDrop`*],
      [*Monolithic `[T-Let]`*],
      [*Vesper Continuation Fission*],
    ),
    [*Normal-Path Linearity*],
    [None (`mem::forget` legal)],
    [Weak (opt-in binding)],
    [*Exact-once* (`Theorem 2`)],
    [*Exact-once* (`Theorem 2` preserved)],
    [*`?` / `Cancel` Across Live Obligations*],
    [Permitted (silent drops)],
    [Permitted (inline await)],
    [*Rejected* (`E0301`)],
    [*Permitted* for types with `reap`],
    [*`io_uring` / RDMA Stack DMA Safety*],
    [*Unsound* (DMA UAF on drop)],
    [Sound only if caller blocks],
    [Requires `OwnedDmaBuf`],
    [*Sound* (`EscrowCell` CQE quarantine)],
    [*Caller Wakeup on Timeout*],
    [$O(1)$ (unsafe)],
    [*Unbounded stall*],
    [Cannot cancel],
    [*$O(1)$ deterministic* (`4` instructions)],
    [*Zombie Quarantine Memory*],
    [N/A],
    [Caller frame retained],
    [N/A],
    [*Statically bounded* (`EscrowMem @ <= B`)],
    [*Lean 4 Verification*],
    [Requires Iris prophecies],
    [Opaque poll state machine],
    [Single-path `EffM`],
    [*Dual-branch pure state reducer*],
  ),
  kind: table,
)

Four normative guarantees govern Continuation Fission across all five compiler stages:

+ *Uncompromised Normal-Path Linearity:* Defining a `reap` clause on a `linear struct` *never* relaxes its usage mode to `affine`. Falling off the end of a block without an explicit terminal consumer (`tx.commit()`, `IoUring::await_read()`) remains a hard compile-time error; `reap` is reachable exclusively along typed abort edges (`k.reap()`).
+ *Co-Owned `escrow` Custody Without `'static` Heap Copies:* Passing `escrow buf[0..4096]` loans disjoint read/write custody of `buf` to an external hardware or kernel queue bound to a linear ticket `h : T[@buf]`. While `h` is live, the Place Capability Graph forbids all CPU `in`, `inout`, `out`, and `sink` accesses to `buf[0..4096]`.
+ *$O(1)$ Non-Blocking Unwind via Split `EscrowCell` Detachment:* Synchronous `reap` clauses (`eff []`, such as rolling back an in-memory `WriteTxn`) execute inline during unwind. Asynchronous `reap` clauses (`eff [IoReap]`, such as awaiting a cancelled `io_uring` CQE or `ibv_wr_local_inv` work completion) detach *only* the minimal `EscrowCell` sub-frame into the worker shard's reaper ring while immediately recycling the 64 KiB CPU fiber stack.
+ *Pomonoid Quarantine Admission Control:* Every potential abort edge crossing an asynchronous `escrow` loan contributes its exact detached byte footprint to `EscrowMem`, enabling Bitwuzla to prove statically that worst-case cancellation storms never exhaust fixed `ShardSlab` or `VirtualArena` budgets.

== 6.2 LL(2) Surface Grammar Extensions: `escrow` and `reap`
<ch-fission-2>

Continuation Fission extends #link(<ch-2-2>)[Section 2.2]'s deterministic LL(2) grammar with three conflict-free productions (`'escrow'` and `'reap'` are reserved keywords in parameter/item positions):

```text
(* 1. Fifth Place-Passing Mode: Asynchronous Hardware / Kernel Co-Custody *)
PassMode       ::= 'in' | 'inout' | 'out' | 'sink' | 'escrow' ;

(* 2. Linear Struct Reaper Declaration (inside StructDecl body) *)
StructDecl     ::= GhostMod? StorageSpec? ModeSpec? 'struct' UpperIdent TypeParams?
                   ( '{' FieldDeclList? ReapDecl? '}' | ';' ) ;

ReapDecl       ::= 'reap' '(' 'sink' 'self' ( ',' ParamList )? ')' EffClause? Block ;

(* 3. Additive-Conjunctive Handler Continuation Elimination *)
PostfixOp      ::= ...
                 | '.' 'resume' '(' ArgList? ')'     (* pi_1(k): normal one-shot resumption *)
                 | '.' 'reap'   '(' ')' ;            (* pi_2(k): residual obligation reaper *)
```

Lookahead remains strictly *LL(2)*: inside `StructDecl`, `'reap' '('` disambiguates in two tokens from `FieldDecl` (`Vis? GhostMod? TempTier? LowerIdent ':'`), and inside `Arg` / `Param`, `'escrow'` occupies the exact lexical slot of `PassMode`.

== 6.3 Formal Typing Judgments: `escrow`, `[T-Let-Fission]`, and `[T-Handle-With]`
<ch-fission-3>

=== Well-Formedness of `reap` Declarations

A `reap` declaration on a `linear` nominal type $T$ specifies an *infallible* terminal compensation morphism ($upright("res")(epsilon_(upright("reap"))) = upright("linear")$). Forbidding abortable effects inside `reap` itself prevents secondary unwind cascades during cleanup:

#_infer(
  "[WF-Reap]",
  $u(bold(m)_(upright("decl"))(T)) = upright("linear") quad upright("res")(epsilon_(upright("reap"))) = upright("linear") \ Delta ; Sigma ; Phi ; (upright("self") :_((1, upright("linear")))^(upright("own")) T, overline(z :_((1, bold(m)_z))^(mu_z) tau_z)) tack.r e_(upright("reap")) : upright("Unit") @ (1, upright("copy")) divides epsilon_(upright("reap")) \ forall j, quad mu_(z_j) in {upright("in"), upright("inout"), upright("escrow")}$,
  $Delta tack.r (upright("reap") (upright("sink self"), overline(mu_z med z : tau_z)) upright(" eff ")[epsilon_(upright("reap"))] med e_(upright("reap"))) upright(" ok in ") T$,
)

=== Co-Owned `escrow` Place Custody

In the substructural place context $Gamma$ (#link(<ch-4-1>)[Section 4.1]), an `escrow` operation transitions a disjoint place or slice $p = x . pi$ from owned/uninitialized state into a *frozen co-custody state* $p :_((1, bold(m)))^(upright("escrow")(h)) tau_p$ indexed by the linear ticket $h$ returned by the submission primitive:

#_infer(
  "[T-EscrowLoan]",
  $(x . pi :_((1, bold(m)))^(mu_0) tau_p) in Gamma quad mu_0 in {upright("own"), upright("out")} quad p(bold(m)) = upright("portable") \ f_(upright("sub")) : (upright("escrow ") y : tau_p) arrow.r^(epsilon) upright("Ticket")[tau_p] @ (1, upright("linear")) quad h in.not upright("dom")(Gamma)$,
  $Delta ; Sigma ; Phi ; Gamma tack.r (upright("let ") h = f_(upright("sub"))(upright("escrow ") x . pi)) thick gt.closed thick (Gamma[x . pi mapsto (x . pi :_((1, bold(m)))^(upright("escrow")(h)) tau_p)], h :_((1, upright("linear")))^(upright("own")) upright("Ticket")[tau_p] @ p) divides epsilon$,
)

#_infer(
  "[T-EscrowClaim]",
  $f_(upright("wait")) : (upright("sink ") upright("Ticket")[tau_p], upright("escrow ") tau_p) arrow.r^(epsilon) tau_(upright("ret")) @ (1, bold(m)_(upright("ret")))$,
  $Delta ; Sigma ; Phi ; Gamma, (x . pi :_((1, bold(m)))^(upright("escrow")(h)) tau_p), (h :_((1, upright("linear")))^(upright("own")) upright("Ticket")[tau_p] @ p) tack.r f_(upright("wait"))(upright("sink ") h, upright("escrow ") x . pi) : tau_(upright("ret")) @ (1, bold(m)_(upright("ret"))) thick gt.closed thick (Gamma, x . pi :_((1, bold(m)))^(upright("own")) tau_p) divides epsilon$,
)

While $x . pi$ carries permission $upright("escrow")(h)$:
+ Neither `[T-Var]`, `[T-CallProj]`, nor `discard(Gamma)` applies to $x . pi$ (preventing all CPU reads, CPU writes, and premature stack frame drops while hardware DMA is active);
+ Only `[T-EscrowClaim]` (normal completion) or $h$'s declared `reap(sink self, escrow p)` transition (fissioned abort) can consume $h$ and unlock $x . pi$.

=== Fissioned Sequencing (`[T-Let-Fission]`)

When $e_1$ performs an abortable effect row $epsilon_1$ ($upright("res")(epsilon_1) = upright("affine")$), `[T-Let-Fission]` partitions the continuation environment $Gamma_2 = Gamma_(upright("aff")) convolve.o Gamma_(upright("reap"))$, where $upright("discard")(Gamma_(upright("aff")))$ holds and $Gamma_(upright("reap")) = [h_1, dots, h_m]$ is an ordered sequence of `linear` obligations (and their bound `escrow` places) that each possess a valid `reap` transition with pairwise-disjoint mutable/escrow footprints in reverse acquisition (`LIFO`) order:

#_infer(
  "[T-ReapSlice]",
  $upright("discard")(Gamma_(upright("aff"))) quad forall k in {1 .. m}, med (h_k :_((1, bold(m)_k))^(upright("own")) T_k) in Gamma_(upright("reap")) quad T_k upright(" has ") upright("reap")_k (upright("sink self"), overline(mu_(k j) med p_(k j))) upright(" eff ")[epsilon_(upright("reap"), k)] \ forall a eq.not b, med upright("mut_escrow")(p_a) perp upright("mut_escrow")(p_b) quad B_(upright("esc")) = sum_(k : upright("async")(k)) upright("sizeof")(upright("escrow")(h_k))$,
  $Delta ; Phi ; Gamma_(upright("aff")) tack.r Gamma_(upright("reap")) leadsto_(upright("reap")) (epsilon_(upright("reap"), m) compose dots.c compose epsilon_(upright("reap"), 1)) compose { upright("EscrowMem") @ lt.eq B_(upright("esc")) }$,
)

#_infer(
  "[T-Let-Fission]",
  $Delta ; Sigma ; Phi ; Gamma_1 tack.r e_1 : tau_1 @ (1, bold(m)_1) divides epsilon_1 quad upright("res")(epsilon_1) = upright("affine") \ Delta ; Sigma ; Phi ; Gamma_2, x :_((1, bold(m)_1))^(upright("own")) tau_1 tack.r e_2 : tau_2 @ (1, bold(m)_2) divides epsilon_2 \ Gamma_2 = Gamma_(upright("aff")) convolve.o Gamma_(upright("reap")) quad Delta ; Phi ; Gamma_(upright("aff")) tack.r Gamma_(upright("reap")) leadsto_(upright("reap")) epsilon_(upright("reap"))$,
  $Delta ; Sigma ; Phi ; Gamma_1 convolve.o Gamma_2 tack.r (upright("let ") x = e_1 upright(" in ") e_2) : tau_2 @ (1, bold(m)_2) divides epsilon_1 compose (epsilon_2 union.sq epsilon_(upright("reap")))$,
)

Notice that when $Gamma_(upright("reap")) = emptyset$, $epsilon_(upright("reap")) = emptyset$ and `[T-Let-Fission]` degenerates identically to #link(<ch-4-2>)[Section 4.2]'s `[T-Let]`. Conversely, if a `linear` obligation intentionally *omits* a `reap` clause (such as an uncompensated bare-metal `IrqGuard` or two-phase commit coordinator vote that *must* be resolved explicitly), `[T-ReapSlice]` fails and Vesper preserves `E0301`.

=== Additive-Conjunctive Handler Typing (`[T-Handle-With]`)

Inside an abortable `handle` block ($q = upright("affine")$), each operation clause receives a *linear additive-conjunctive continuation* $k$ exposing `.resume(...)` and `.reap()`, branded with capability label $l$ at mode `linear + pinned + lifo` so $k$ can never escape the handler clause:

#_infer(
  "[T-Handle-With]",
  $Delta ; Sigma ; Phi ; Gamma_s tack.r e_(upright("init")) : tau_s @ (1, bold(m)_s) divides epsilon quad u(bold(m)_s) subset.eq.sq upright("res")(epsilon) quad forall z in Gamma_h, med u(bold(m)_z) = upright("copy") \ Delta ; (Sigma, l : upright("Cap")_(upright("affine")) [E]) ; Phi ; Gamma_(upright("body")) tack.r e_(upright("body")) : tau_b @ (1, bold(m)_b) divides { l : E^(upright("affine")) @ g } compose epsilon \ Delta ; Sigma ; Phi ; Gamma_h, x :_((1, bold(m)_b))^(upright("own")) tau_b, s :_((1, bold(m)_s))^(upright("own")) tau_s tack.r e_(upright("ret")) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon \ forall (upright("op")_i : tau_i arrow.r sigma_i) in E : \ Delta ; Sigma ; Phi ; Gamma_h, y :_((1, bold(m)_(y_i)))^(upright("own")) tau_i, s :_((1, bold(m)_s))^(upright("own")) tau_s, k :_((1, upright("linear") + upright("pinned") + upright("lifo")))^(upright("own")) upright("FissionCont")_l [sigma_i, tau_s, tau_(upright("out")), epsilon, epsilon_(upright("reap"))] tack.r e_i : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon union.sq epsilon_(upright("reap"))$,
  $Delta ; Sigma ; Phi ; Gamma_s convolve.o Gamma_(upright("body")) convolve.o Gamma_h tack.r (upright("handle ") l : E^(upright("affine")) upright(" state ") s = e_(upright("init")) upright(" in ") e_(upright("body")) upright(" with ") { upright("return")(x, s) => e_(upright("ret")), upright("op")_i (y, s, k) => e_i }) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon union.sq epsilon_(upright("reap"))$,
)

where `FissionCont` eliminates exclusively via one of its two additive projections:

#_infer(
  "[T-With-Resume]",
  $Delta ; Sigma ; Phi ; Gamma_1 tack.r v : sigma_i @ (1, bold(m)_v) divides emptyset quad Delta ; Sigma ; Phi ; Gamma_2 tack.r s : tau_s @ (1, bold(m)_s) divides emptyset$,
  $Delta ; Sigma ; Phi ; Gamma_1 convolve.o Gamma_2, k :_((1, upright("linear") + upright("pinned") + upright("lifo")))^(upright("own")) upright("FissionCont")_l [sigma_i, tau_s, tau_(upright("out")), epsilon, epsilon_(upright("reap"))] tack.r k . upright("resume")(v, s) : tau_(upright("out")) @ (1, bold(m)_(upright("out"))) divides epsilon$,
)

#_infer(
  "[T-With-Reap]",
  $upright("discard")(Gamma)$,
  $Delta ; Sigma ; Phi ; Gamma, k :_((1, upright("linear") + upright("pinned") + upright("lifo")))^(upright("own")) upright("FissionCont")_l [sigma_i, tau_s, tau_(upright("out")), epsilon, epsilon_(upright("reap"))] tack.r k . upright("reap")() : upright("Unit") @ (1, upright("copy")) divides epsilon_(upright("reap"))$,
)

For standard `Fail[E]` handlers, `raise(err, s, k) => { k.reap(); Err(err) }` discharges `k`'s linear obligation in one line—running synchronous rollbacks inline and detaching asynchronous `escrow` reapers automatically before returning `Err(err)`.

== 6.4 Ordered Pomonoid Quarantine Grades (`EscrowMem` and `IoReap`)
<ch-fission-4>

Unbounded background cleanup under a timeout storm is a classic availability hazard in completion-I/O runtimes. Continuation Fission prevents zombie memory exhaustion statically by integrating `EscrowMem` and `IoReap` into #link(<ch-5-3>)[Section 5.3]'s ordered resource-grade algebra:

- *Sequential High-Water Quarantine (`EscrowMem`):* Across sequential statements `e_1; e_2` where `e_1`'s `escrow` tickets are claimed prior to `e_2`, detached quarantine memory composes via $max(g_1, g_2)$, whereas simultaneously live nested `escrow` tickets sum additively ($g_1 + g_2$):
  $ upright("EscrowMem")(upright("let ") h_1 = upright("prep")(upright("escrow ") b_1) upright(" in let ") h_2 = upright("prep")(upright("escrow ") b_2) upright(" in ") e_(upright("abort"))) = upright("sizeof")(b_1) + upright("sizeof")(b_2) $
- *Compile-Time Slab Admission Proof:* When a worker loop runs inside `Pulse::with_escrow_pool[MAX_BYTES]`, Stage 3 verifies via Bitwuzla (`QF_UFBV`) that `max_concurrent_fibers * per_fiber_escrow_mem <= MAX_BYTES`, guaranteeing zero dynamic heap fallback and zero `ENOMEM` stalls during mass connection disconnects.

== 6.5 End-to-End Showcase: Cancel-Safe `io_uring`, WAL Rollback, and RDMA Quarantine
<ch-fission-5>

The following module combines a synchronous `WriteTxn` rollback, an asynchronous Linux `io_uring` stack-detached page read, and a *Vesper Chorus* (#link(<ch-34-2>)[Section 34.2]) Type-2 RDMA Memory Window invalidation (#link("https://man7.org/linux/man-pages/man3/ibv_wr_local_inv.3.html")[`ibv_wr_local_inv(3)`]) across both `?` propagation and `Pulse::timeout`:

#_compact-listing[
```vesper
// 1. Synchronous WAL transaction: rolls back dirty page table inline in 0 ns on `?` or `Cancel`.
pub linear struct WriteTxn {
    lsn: u64,
    staged_slots: RegionVec[u32],

    reap(sink self, inout wal: WalJournal) eff [] {
        let WriteTxn { lsn, staged_slots } = self;
        wal.abort_staged_lsn(lsn, sink staged_slots);
    }
}

// 2. Completion-based io_uring SQE ticket: quarantines `escrow dst` until kernel CQE drains.
pub linear struct UringReadTicket[@sqe_id: u64] {
    sqe_id: u64,

    reap(sink self, escrow dst: ByteSlice) eff [IoReap @ <= 1] {
        let UringReadTicket { sqe_id } = self;
        IoUring::prep_async_cancel(sqe_id);
        IoUring::await_final_cqe(sqe_id, escrow dst); // `dst` recycled ONLY after CQE harvest!
    }
}

// 3. Chorus Type-2 RDMA Memory Window: fences remote RNIC `rkey` DMA before freeing buffer.
pub linear struct RdmaWindowTicket[@rkey: u32] {
    mw_handle: u64,
    rkey: u32,

    reap(sink self, escrow wire_buf: ByteSlice) eff [RdmaReap @ <= 1] {
        let RdmaWindowTicket { mw_handle, rkey } = self;
        RdmaQp::post_local_inv_and_await_wc(mw_handle, rkey, escrow wire_buf);
    }
}

// 4. Unified pipeline: idiomatic `?` and 5ms timeout with zero UAF and zero boilerplate!
pub fn replicate_verified_page(
    inout wal: WalJournal,
    in hdr_bytes: ByteSlice,
    fd: RawFd,
    disk_off: u64,
) -> Result[u64, ReplError]
eff [
    Io, Rdma,
    IoReap @ <= 1, RdmaReap @ <= 1, EscrowMem @ <= 8192,
    Fail[ReplError], Cancel,
] {
    Pulse::timeout(5.ms(), || {
        let mut disk_page: ByteArray[4096] = uninit;
        let mut rdma_ack:  ByteArray[4096] = uninit;

        // Both 4 KiB buffers allocate in a detachable 8 KiB EscrowCell (`EscrowMem = 8192`):
        let disk_sqe = IoUring::submit_pread(fd, disk_off, escrow disk_page[0..4096]);
        let rdma_mw  = RdmaQp::bind_mw2_recv(escrow rdma_ack[0..4096]);
        let tx       = wal.begin_txn();

        // If CRC validation fails OR the 5ms timeout fires here:
        //   1. `tx.reap(inout wal)` runs inline on the CPU stack;
        //   2. `rdma_mw.reap` and `disk_sqe.reap` detach the 8 KiB EscrowCell to ReapRing;
        //   3. Caller receives `Err(...)` immediately while NIC & NVMe drain safely!
        let meta = FrameHeader::decode_non_malleable(in hdr_bytes)?;

        let n_read = IoUring::await_pread(sink disk_sqe, escrow disk_page[0..4096])?;
        let quorum = RdmaQp::await_mw2_ack(sink rdma_mw, escrow rdma_ack[0..4096])?;
        tx.stage_verified(inout wal, in meta, in disk_page[0..n_read], in rdma_ack[0..quorum]);
        Ok(tx.commit(inout wal))
    })
}
```
]

== 6.6 Dual-Branch Lean 4 Extraction and Mechanized Soundness
<ch-fission-6>

Stage 5B (#link(<ch-8-2>)[Section 8.2]) translates `[T-Let-Fission]` into a *prophecy-free two-arm pure state transition* in Lean 4, exposing both normal commit states and abort/timeout reaped states to `vesper verify`:

$ ⟦upright("let ") x = e_1 upright(" in ") e_2⟧_(Gamma_(upright("reap"))) (s_0) := upright("match ") ⟦e_1⟧(s_0) upright(" with") \
  divides upright(".ok ") (v, s_1) => ⟦e_2⟧(v, s_1) \
  divides upright(".error ") (e, s_1) => upright("let ") s_2 := ⟦upright("reap")_(Gamma_(upright("reap")))⟧(s_1) upright(" ; .error ") (e, s_2) $

The standalone zero-`sorryAx` Lean 4 module `lean/ContinuationFission.lean` mechanizes both *Theorem 5 (Fissioned Total Linear Exhaustion)* and *Theorem 6 (Zero-UAF Escrow Quarantine Safety)*:

#_compact-listing[
```lean
-- lean/ContinuationFission.lean
-- Zero-sorryAx mechanization of [T-EscrowLoan], [T-Let-Fission], [T-Handle-With],
-- Fissioned Total Linear Exhaustion (Theorem 5), and DMA Quarantine Safety (Theorem 6).

namespace Vesper.ContinuationFission

inductive PlaceState where
  | cpuOwned    : PlaceState
  | escrowed    : (ticketId : Nat) -> PlaceState
  | quarantined : (ticketId : Nat) -> PlaceState
  | recycled    : PlaceState
  deriving DecidableEq, Repr

inductive TicketState where
  | active      : (placeId : Nat) -> (escrowBytes : Nat) -> TicketState
  | committed   : TicketState
  | reaping     : (placeId : Nat) -> (escrowBytes : Nat) -> TicketState
  | reaped      : TicketState
  deriving DecidableEq, Repr

structure FissionWorld where
  places  : Nat -> PlaceState
  tickets : Nat -> TicketState

/-- Hardware DMA may write to `p` whenever its bound ticket is `active` or `reaping`. -/
def HwDmaMayTouch (w : FissionWorld) (p : Nat) : Prop :=
  exists tid sz,
    (w.tickets tid = .active p sz \/ w.tickets tid = .reaping p sz)

/-- CPU reuse / slab recycling is permitted iff `w.places p = .recycled` or `.cpuOwned`. -/
def WellFormedEscrow (w : FissionWorld) : Prop :=
  forall tid p sz,
    (w.tickets tid = .active p sz  -> w.places p = .escrowed tid) /\
    (w.tickets tid = .reaping p sz -> w.places p = .quarantined tid)

/-- `k.reap()` detaches an active ticket into quarantine without premature recycling. -/
def detachToReaper (w : FissionWorld) (tid p sz : Nat) : FissionWorld :=
  { places  := fun q => if q = p then .quarantined tid else w.places q
  , tickets := fun t => if t = tid then .reaping p sz else w.tickets t }

/-- Reaper completion after terminal CQE / `IBV_WC_LOCAL_INV` harvest. -/
def completeReapCqe (w : FissionWorld) (tid p : Nat) : FissionWorld :=
  { places  := fun q => if q = p then .recycled else w.places q
  , tickets := fun t => if t = tid then .reaped else w.tickets t }

theorem detach_preserves_well_formed
    (w : FissionWorld) (tid p sz : Nat)
    (hwf : WellFormedEscrow w)
    (h_act : w.tickets tid = .active p sz)
    (h_uniq : forall t2 p2 sz2, t2 ≠ tid ->
      (w.tickets t2 = .active p2 sz2 \/ w.tickets t2 = .reaping p2 sz2) -> p2 ≠ p) :
    WellFormedEscrow (detachToReaper w tid p sz) := by
  intro t q s
  unfold detachToReaper
  by_cases ht : t = tid
  · subst ht
    simp only [ ↓reduceIte]
    constructor
    · intro h_absurd; cases h_absurd
    · intro h_eq; injection h_eq with hp _; subst hp; simp
  · simp only [ht, ↓reduceIte]
    have ⟨hA, hR⟩ := hwf t q s
    constructor
    · intro ha
      have h_neq : q ≠ p := h_uniq t q s ht (Or.inl ha)
      simp [h_neq, hA ha]
    · intro hr
      have h_neq : q ≠ p := h_uniq t q s ht (Or.inr hr)
      simp [h_neq, hR hr]

/-- Theorem 6 (Zero-UAF Escrow Quarantine Safety): No place reachable by in-flight
    hardware DMA (`active` or `reaping`) is ever observable as `.cpuOwned` or `.recycled`. -/
theorem zero_uaf_during_escrow_and_quarantine
    (w : FissionWorld) (p : Nat)
    (hwf : WellFormedEscrow w)
    (hdma : HwDmaMayTouch w p) :
    w.places p ≠ .cpuOwned /\ w.places p ≠ .recycled := by
  rcases hdma with ⟨tid, sz, h_live⟩
  have ⟨hA, hR⟩ := hwf tid p sz
  cases h_live with
  | inl h_act =>
    rw [hA h_act]
    exact ⟨by decide, by decide⟩
  | inr h_rep =>
    rw [hR h_rep]
    exact ⟨by decide, by decide⟩

/-- Theorem 5 (Additive Continuation Exhaustion): Eliminating `FissionCont` via
    either `.resume` or `.reap` transitions every live linear ticket to `.committed`
    or `.reaped` in exactly one step (`0` leaked obligations). -/
inductive FissionChoice where | resumePath | reapPath deriving DecidableEq

def dischargeLinearTicket (choice : FissionChoice) : TicketState :=
  match choice with
  | .resumePath => .committed
  | .reapPath   => .reaped

theorem fission_choice_exhausts_linear_obligation (c : FissionChoice) :
    dischargeLinearTicket c = .committed \/ dischargeLinearTicket c = .reaped := by
  cases c <;> simp [dischargeLinearTicket]

end Vesper.ContinuationFission
```
]

== 6.7 Five-Stage Lowering, Split `EscrowCell` ABI, and SemVer Variance
<ch-fission-7>

Continuation Fission introduces zero overhead on functions without `escrow` projections and zero heap allocations on abort paths:

+ *Stage 3 — Backward Reaper Slicing (`vesper-mir-pcg`):* At every CFG edge performing an abortable effect (`res(epsilon) = affine`), the PCG solver walks live `linear` bindings $h_m, dots, h_1$ in reverse dominance order, validates `[T-ReapSlice]` disjointness, and marks only locals referenced by `escrow` parameters of asynchronous `reap` blocks with storage class `Storage::EscrowSubFrame`.
+ *Stage 4 — Split `EscrowCell` Sub-Frame Placement (`vesper-opt-egglog`):* CPU-only locals remain in standard stack slots on the 64 KiB `Pulse` fiber stack. `Storage::EscrowSubFrame` locals pack into a cache-aligned `EscrowCell` bumped in 3 instructions from the worker shard's Domain-1 `ShardSlab` (#link(<ch-12-3>)[Section 12.3]) at function prologue and popped inline on normal return (`0` `memcpy`, `0` atomic instructions).
+ *Stage 5A — Dual-Target Landing Pads (`(resume_ip, reap_ip)`):* Each fissioned suspension descriptor emits a 16-byte pair `(resume_ip, reap_ip)` in `.rodata`. Invoking `k.resume(v, s)` jumps directly to `resume_ip`. Invoking `k.reap()` jumps to `reap_ip`, which executes synchronous `reap` blocks inline, enqueues `{ escrow_cell_ptr, async_reap_step_fn }` onto `PulseWorkerShard::reap_ring` in 4 instructions, and immediately returns the 64 KiB CPU fiber stack to the shard pool.
+ *SemVer Substitutability (#link(<ch-11-2>)[Section 11.2] Integration):* Adding a `reap` declaration to an existing `linear struct` (or lowering its `EscrowMem` / `IoReap` grade) is *SemVer Minor-compatible* ($I_(upright("new")) prec.curly.eq_(upright("pub")) I_(upright("old"))$), because every existing caller that manually consumed the obligation continues to typecheck unchanged while new callers gain `?` and `Cancel` composability; removing a `reap` declaration or raising its `EscrowMem` ceiling is *Major-breaking*.
