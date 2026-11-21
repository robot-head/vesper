# 1 Why Vesper tracks obligations

An open transaction needs an owner on the error path as well as the success path. If a call can fail before the transaction commits, Vesper checks who will finish it.

## 1.1 Four research ideas behind a call

Substructural types count uses of a value. Mutable value semantics lends a caller’s storage to a function for the duration of a call. One-shot effects let a handler decide how a suspended operation finishes. Liquid refinements attach facts such as an index bound to a type. In a buffer write, ownership identifies who must keep the storage live. The loan identifies the writable bytes, an index bound constrains the write, and an effect tells the caller how it can fail.

## 1.2 When automatic cleanup leaves a choice open

An affine owner can drop a resource when it goes out of scope. That works for releasing memory, but committing and rolling back a transaction have different meanings. Making the transaction linear forces every path to choose an outcome. Projected loans let two helpers work on disjoint fields of the same record.

Async code raises the same ownership question at suspension. A pinned stack or future may still have pointers into its storage. Dropping a pending operation can skip its ordinary cleanup path. The manuscript explores lexical effects to expose these decisions at calls. At build time, a dependency or macro could inherit the compiler’s access to files and network services. The design lets a package limit that access.

## 1.3 Read a contract at a function boundary

Read the parameter modes to find who can read, change, initialize, or consume the arguments. Read the effect row to see which operations need an enclosing handler. A refinement can state what must be true on entry or return. Proof extraction translates supported operations into a mathematical model where such claims can be checked.

## 1.4 Follow a transaction through a broker

Imagine a broker with an index, counters, and configuration. A helper can update the index and counters while reading configuration through a shared loan. The caller still owns the transaction until it passes it to a consuming method. Follow both the normal branch and the failure branch and locate the last owner on each.

In the repository’s broker example, compare the function’s parameter list with the arguments at its call site.

    fn append(inout broker.{index, metrics}: Broker, in config: u64) {
        broker.index = wrapping_add(broker.index, config);
        broker.metrics = wrapping_add(broker.metrics, 1);
    }

    append(inout broker, in broker.config);

The callee can update exactly two fields through its first parameter. Its second parameter supplies a readable configuration value. Starting with index 1, metrics 0, and config 41 yields index 42, metrics 1, and config 41.

# 2 Reading and parsing Vesper source

Read a function signature from left to right. Square brackets hold type parameters, parentheses hold the arguments, and an effect row after the return type lists operations the caller must handle or pass on. These delimiters also help an editor recognize an incomplete declaration.

## 2.1 Recognize syntax before resolving names

The proposed parser uses short lookahead so an editor can recognize the shape of a file before loading its imports. Uppercase and lowercase names give clues to type and value positions. Square brackets distinguish generic arguments from comparisons. An `@` introduces an index or annotation where the surrounding grammar expects one. The draft still has ambiguous cases.

## 2.2 Read the grammar and its everyday consequences

In the grammar, alternatives describe choices and repeated elements describe lists. Read a production such as a function declaration from its leading keyword through parameters, result, effects, and body. Keywords depend on their position. For example, `state` can name a local even though a stateful handler uses it as syntax. Braces can start a block or a record. Parenthesizing a record literal in a loop or condition makes the boundary clear.

A `loop` can yield its `break` value. A range `a..b` excludes the upper bound and `a..=b` includes it. A `for` can also visit collection elements. A `return` exits the enclosing function or closure, so an early exit must account for its live owners. In an optional result, `?` can return `None` early. On a `Result` it can perform a failure effect. In `match`, alternatives joined by `|` share an arm and must bind the same names at the same types.

Integer kinds have distinct widths and signs. Choose an explicit widening, narrowing, saturating, or wrapping operation to say what an out-of-range value means. A fixed array `[T; N]` stores its elements inline. A module constant names a compile-time value. A scalar-valued enum groups constants, while a data-carrying variant selects a shape to match.

Floating-point operations use their floating-point results for infinities and NaNs. Explicit conversions select rounding behavior. The deterministic floating-point contract specifies canonical NaNs for selected operations. Bit reinterpretation lets a program inspect a representation directly. The grammar sketch and the compiler cover different sets of programs. For runnable syntax, start with the language guide.

# 3 Ownership, movement, and erased evidence

A value’s usage mode determines whether you may copy it, drop it, or must consume it exactly once. Vesper tracks other restrictions independently, including whether you may move the value to another thread, whether a worker can change it, and whether you must release it in a particular order.

## 3.1 Read the four axes and presence

Usage tracks whether a value can be copied, dropped, or must be consumed. Portability distinguishes transferable and pinned values. Contention records whether another participant can interfere. Exchange order distinguishes freely ordered values from last-in, first-out obligations. A separate presence index distinguishes runtime values from erased proof material.

## 3.2 Derive a record’s mode from its fields

A record containing a linear ticket must protect the ticket even if its other fields are copyable. A field annotation can impose a stricter floor. The record combines the field requirements with its declared mode. Conversely, after consuming the record, a plain integer field can be used at its own intrinsic kind. This recovery of a known type’s permissions is called mode crossing.

## 3.3 Seal a capsule or release a guard

A `Capsule[T]` packages an isolated owner for transfer. Sealing needs evidence that no outside alias, borrowed capability, or thread-affine handle ties the contents to the sender. Last-in, first-out guards restrict release order. If you acquire A then B, release B before A. Two branches must agree on the guard stack when they rejoin.

## 3.4 Erase a proof without changing the run

A ghost value marked `@0` exists during checking. A proof can consume a ghost permission to justify an operation. Runtime control flow cannot depend on that permission. The erasure argument checks termination, freedom from runtime effects, and discharge of ghost obligations. The operational model proves erasure for ghost computations that satisfy these conditions and capture no outside resources.

# 4 Borrowing parts of a value

You can lend one field of a record to a writer while another operation reads a different field. Vesper tracks the storage each loan covers, down to a field path or a range of array elements.

## 4.1 Read a typing judgment as an inventory

The context to the left of a judgment lists the bindings available at that point. Splitting the context gives each subexpression its permissions. Shared views can be lent to both sides. An exclusive loan or a linear owner has a single destination. Ordered guards have their own stack. At a scope exit the checker asks which entries can be discarded and which still need an action.

## 4.2 Carry ownership through closures and branches

A closure that captures a linear ticket by ownership inherits an obligation to use it. A borrowed parameter is valid only within its loan. A closure may itself be lent for a call. A sequence must preserve the resources needed after its first expression. Each branch has to leave compatible permissions for the code after the branch.

## 4.3 Choose a parameter mode and a footprint

Use field loans to lend selected parts of a record. `in` lends shared access, `inout` lends initialized storage for mutation, `out` lends a destination for initialization, and `sink` transfers ownership. Two writers can receive the left and right halves of an array if their intervals are disjoint. A two-phase loan reserves the receiver while other arguments are evaluated, then activates exclusive access for the call. Matching a borrowed noncopy record can lend its fields without consuming it.

## 4.4 Initialize a destination exactly once

An `out` slot starts without a readable value. A callee must fill it on normal return before the caller reads it. Splitting a destination into slices requires both disjointness and complete coverage. Tail-modulo-cons construction uses the unfinished tail of a result as the destination for the next step, so recursive construction can fill the final structure directly.

For eight output slots, `[0..3]` and `[3..8]` cover the destination exactly. Moving the second start to four leaves slot three uninitialized. Moving it to two gives both writers access to slot two. Checking only the sum of the lengths would miss a gap paired with an overlap.

## 4.5 Update facts when their values change

A refinement adds a predicate such as “length is within capacity.” A strong update replaces the old facts about the location a program writes. Snapshot its old value first if a postcondition relates the new and old lengths. Other names for the same location need the update too. Across interference, keep only facts that every allowed change preserves. The manuscript’s proofs cover particular scalar stores and typed place operations.

## 4.6 Stage two DMA transfers

For direct memory access (DMA), split a staging buffer into nonoverlapping ranges, give each producer a destination, and use proof-only permissions to account for the ranges. Reading the whole buffer requires both destinations to be initialized. Keep the buffer live through device completion because the device may still use memory after the submitting function gives up control.

# 5 Effects, budgets, and equality

An effect names an operation a function asks its caller to provide. A grade can bound the operation’s resource cost. Refinements and proofs describe which values a caller may supply and what it can expect back.

## 5.1 Handle an operation and thread its state

Performing an effect transfers control to its lexical handler. The handler receives a continuation representing the rest of the computation. A linear continuation must be resumed exactly once. An affine continuation may be abandoned. For a counter, pass the updated counter as handler state on each resumption so later operations see the new value.

## 5.2 Trace the error edge of a question mark

Consider opening a transaction and then applying `?` to validation. On success execution reaches the commit. On failure the handler can abandon the continuation, leaving nobody to consume the transaction. Validate first, or explicitly consume the transaction on both branches. The resumption frame condition rejects the first version because the abandoned continuation would still own the transaction.

## 5.3 Compose resource grades

A grade describes how much of a resource an operation needs. An ordered monoid supplies a zero, a way to combine costs, and an order for comparing them with a budget. Two allocations may add their byte counts. Two sequential calls can reuse stack space so their peak is the larger requirement. Nested calls need space for both active frames. An exact timing contract also needs both branches to agree on cost.

For example, if two children need 24 and 40 bytes of stack and finish one after the other, the children’s peak is 40 bytes. If the parent keeps its own 16-byte frame throughout, the combined peak is 56 bytes. A bounded loop can reuse that stack on each iteration even when allocations retained in a region accumulate.

## 5.4 Delay recursion and hide representations

A clock-indexed `Later` delays a recursive observation by one logical step. A clock controls when the observation can be forced and what a recursive definition may capture. A quotient treats multiple representations as one abstract value. For example, two representations of the same key must give the same observable lookup result. Each exported operation needs evidence that it respects the equivalence.

## 5.5 Read the scope of a soundness claim

Progress says a well-typed program in the model is finished or can step. Preservation says a step maintains its typing. Conservation accounts for resource identities across steps. To use one of these results, identify the modeled operations and the assumptions on effects, recursion, and stores. Appendix A identifies which features have been proved together.

# 6 What happens to an outstanding operation on cancellation?

A service can cancel a read while the device is still writing to its buffer. Continuation fission is a proposal for giving that buffer an owner on either exit, whether the read finishes normally or the service cancels it.

## 6.1 Separate normal completion from cancellation

On the normal path a linear write transaction still needs a terminal operation. At an abort edge, the proposal would let a dedicated cleanup action take responsibility. Some actions finish locally. Others must retain storage until an external completion arrives. Giving a value a cleanup rule would preserve the original obligation on ordinary returns.

## 6.2 Express custody with escrow and reap

`escrow` marks the place temporarily entrusted to an external operation. A ticket records which storage it protects. A `reap` declaration describes how to discharge that type’s obligation during unwind. For a read into a buffer, the ticket protects the bytes even after the caller loses interest in the result.

## 6.3 Check both exits at a suspension

A well-formed reaper must account for the fields it consumes and declare its cleanup effects. While an escrow ticket is live, conflicting access to its place stays suspended. At a possible abort, fissioned sequencing computes which owners the cleanup branch must receive. The handler chooses either the resume continuation or the reap continuation. Executing both would give two consumers the same resource.

## 6.4 Budget for detached storage

`EscrowMem` counts memory that may remain quarantined. `IoReap` accounts for external cleanup work. A service that admits many concurrent reads needs a bound on the sum of buffers that might remain live after cancellations. If another cancellation would exceed the byte budget, the service must wait for storage to return before admitting more work.

## 6.5 Distinguish rollback from external completion

A write-ahead log transaction may roll back its local state immediately. A submitted kernel read needs the original request’s terminal completion. A remote memory transfer may need confirmation from the network interface before the registration can be released.

## 6.6 Model a choice of continuations

The proposed extraction gives Lean a normal branch and a cleanup branch that share a resource budget. A proof would need to account for every owner on whichever branch actually executes. The chapter’s Lean listing illustrates the intended theorem. External device behavior and completion still need an operational model.

## 6.7 Lower the landing pads and preserve compatibility

The proposed compiler walks backward from an abort edge to identify required reapers. An `EscrowCell` separates storage that may outlive the suspended stack. Separate landing pads transfer control to resume or cleanup. Removing cleanup support or raising a public quarantine requirement can break callers that already rely on it. Constant-time detachment and bounded reclamation remain design requirements.

# 7 Traits, models, macros, and syntax extensions

Pact lets a library define an operation for many types or choose between implementations for one type. Forge lets a library introduce new notation that expands into operations the compiler already checks.

## 7.1 Extend a program without hiding its contracts

An abstraction must preserve who consumes a value, which places change, and what effects occur. A library fold can abstract over a source’s effect row. A macro introducing a write still has to expose the corresponding permission. The caller can then read the access requirements from the signature or macro invocation.

## 7.2 Abstract over a source and its effects

A source trait can name the item it yields through an associated type. Reading a memory sequence might be pure while reading a socket can suspend. An effect variable stands for the operations needed by each source. A fold also accounts for the effects of its combining function, so the same loop can work with both sources.

## 7.3 Pick a canonical implementation or a named model

Coherence gives an implicit trait call a unique implementation. The orphan and overlap rules limit where canonical implementations can be introduced. A named model lets a caller choose explicitly, such as ordering points by horizontal position. A type brand can remember that choice in a container so code cannot accidentally mix incompatible orderings.

## 7.4 Keep macro names and authority local

Hygiene protects bindings in two directions. A temporary introduced by a macro must keep its own identity. A reference the macro author intended to resolve at definition time must keep that meaning at the call site. The proposed WebAssembly System Interface (WASI) macro boundary limits access to declared inputs, allowing expansion results to be cached by macro and input content.

## 7.5 Parse an extension in two steps

The first parse records a phrase skeleton, including its delimiters and argument positions. After imports resolve, an enforester groups those pieces using the selected sugar declaration. The expansion must honor passing modes and report its effects. If the caller lends a value for reading, the expansion cannot consume it.

## 7.6 Recognize the proposed standard sugar

`errdefer` attaches cleanup to an error path. `ensure` checks a condition and raises the supplied error if the check fails. `guard let` binds a matching value and sends the unmatched case down an exit path. `emplace` initializes a destination, `swap` exchanges owners, and `scratch` scopes temporary storage. `par` groups concurrent work, `within` supplies a timeout and a recovery arm, and `holding` brackets acquisition and release of a guard.

`bits{…}` describes a bit layout, `ghost calc` chains proof steps, and elementwise operators apply a scalar operation across lanes. The proposed library forms all expand into operations in the checked core.

## 7.7 Read domain-specific sugar by its expansion

`send` and `recv` advance a session endpoint to its next protocol state, and `close` consumes the final endpoint. `transact` proposes rollback for opted-in mutations. `morph` rewrites a uniquely owned tree node in place. `ct_if` expresses a secret-dependent selection subject to timing checks. `modify_reg` groups appropriate device register updates into a write.

A loop invariant states what each iteration preserves. A decreases clause names a quantity that makes progress toward termination. A `zset` comprehension builds weighted stream records. Expanding the shorthand leaves those permission checks and proof obligations in place.

## 7.8 Decode shorthand for common tasks

The readability proposals include tagged literals for bytes, durations, addresses, and regular expressions. Formatting and logging tags describe where values go. Pipeline placeholders show where the previous result becomes an argument. `race` chooses a completion, and `retry` carries a clock effect when it waits between attempts.

Collection folds such as `sum` and `any` express accumulation. `lanes` handles a vector tail. `update` describes changes to a copyable record, and `transition` names a typestate change. An assertion can name its supporting lemma. Test and property declarations register examples or generators. The language guide shows the test syntax that runs in this checkout.

## 7.9 From a phrase to a resolved call

The trait solver records goals so repeated or recursive lookups can share work. The enforester selects the syntax rule for a phrase. The hygiene engine preserves identifier identity through expansion. When debugging an unexpected call, inspect which rule expanded, which names it bound, and which implementation the resulting call selected.
