# 30 Wire formats and JSON serialization

A decoder can lend a view of its input, allocate a decoded object, or write directly into a column. The choice determines who owns the output, how long it stays valid, and whether the decoder has to copy the bytes.

## 30.1 Choose what the decoder owns

An unescaped JSON string can sometimes refer directly to input bytes. Escaped text needs decoding and suitable storage. A region can keep decoded values alive for one batch. If a composite decode fails after constructing a linear field, the error path still has to discharge that field.

## 30.2 Express storage and effects in a codec

An `Encode` operation writes a representation to an output. A `Decode[r]` operation can tie an output view or allocation to region `r`. The caller must keep its backing storage live. Generic effect rows allow a codec to express the operations its input, output, or allocator requires.

## 30.3 Parse JSON in two stages

The first pass locates structural characters while tracking quoted strings and escapes. A second pass interprets those positions as objects, arrays, and values. A comma inside a quoted string is data. The vectorized scan must track whether it is inside a string even across a group of bytes processed together.

## 30.4 Decode compressed integers and relative pointers

StreamVByte separates compact length information from integer payload bytes so a vector decoder can process groups. A `RelPtr32` locates data by an offset within an archive. Before constructing a view, validate the offset, length, and extent against the mapped archive.

## 30.5 Ingest a batch into columns

Read newline-delimited JSON into a bounded region, decode each row, and append selected fields to an AoSoA tile. Decide what a malformed row does to the unfinished batch. A round-trip proof compares a value with its decoded encoding. The ingestion path also needs bounds and cleanup checks.

## 30.6 Select a decoder for the host

The host implementation connects parsing and encoding kernels to storage and instruction-set profiles. Check an accelerated decoder against a simple reference on escaped quotes, truncated inputs, and partial final groups. These cases exercise the transition between scanning and interpreting.

# 31 Grammar and canonical binary formats

When bytes are hashed or signed, two byte strings that decode to the same value can cause disagreement. A canonical codec gives each accepted value one representation.

## 31.1 Specify which byte strings belong to a format

A parser may accept several representations that an encoder would never produce. A non-malleable format requires each accepted representation to be canonical. Secret-bearing formats add a separate requirement on what parsing reveals through timing, errors, or memory access.

## 31.2 Keep the unconsumed tail

Parsing a field returns a value and a remainder. An `UnconsumedTail` lease makes the caller account for the remaining bytes. In an Arrow column, lengths, validity bits, and offsets must describe compatible ranges. A typed view of values depends on those checks succeeding first.

## 31.3 Validate before exposing an Arrow view

Read the message metadata, check every referenced buffer, and only then project the selected column. A canonical integer decoder or a Distinguished Encoding Rules (DER) decoder can reject redundant encodings as the bytes arrive. A zero-copy view saves materialization but still needs validation over the borrowed bytes.

## 31.4 Test the two round-trip directions

Decoding an encoded value should recover that value. Encoding an accepted byte sequence’s decoded value should recover the same bytes. The second direction exposes alternate encodings. Together these laws describe a bijection on the explicitly supported domain.

## 31.5 Check a variable-length integer

LEB128 encodes an integer in groups of seven bits. A continuation bit tells the decoder whether to read another group. It must reject truncation, out-of-range results, and redundant groups when canonical encoding is required. For an Arrow buffer verifier the analogous checks relate recorded offsets to actual buffer lengths.

# 32 Beacon distributed tracing

A trace follows a request across services. Beacon treats each running span as a resource with a parent, a set of exportable fields, and an owner who must record how the span ended.

## 32.1 Decide what crosses the service boundary

A trace identifier groups work across services. A span identifies one unit of work within it. Context propagation connects the next service to the current span. Define which attributes may be exported before adding tracing to a path that handles personal or secret data.

## 32.2 Finish a span on each exit

An `ActiveSpan` represents an open observation. Consume it by recording completion or an error outcome. A lexical `Trace` capability carries context with the computation across fiber switches. A W3C `traceparent` header needs validation before it supplies a remote parent identity.

## 32.3 Batch telemetry by column

Per-shard ring buffers collect attributes in tiles. An exporter can take columns of IDs, timestamps, and values and send them using an Arrow-based telemetry representation. Keep the backing tile live until export completes. Define what happens when a producer fills the ring faster than an exporter drains it.

## 32.4 Prove what a redacted observation can reveal

Define which fields count as public and compare exports from runs that differ only in protected fields. Redaction has to cover derived fields and error paths too. Omitting a secret column is insufficient if a public label encodes the same information.

## 32.5 Follow one context through the host

The host parses or creates trace IDs, records events into buffers, and hands batches to the exporter. Use a known parent and child to check the encoded relationship. A parse or export failure should leave neither a dangling span nor a borrowed tile reused too early.

# 33 Warp packet ownership and network pacing

A network server can avoid copying a packet by transferring ownership of a frame between the driver and userspace. That saving makes it essential to know precisely when each frame can be reused.

## 33.1 Follow a frame between rings

An AF_XDP socket can exchange frames from a userspace memory area (UMEM) through queues. Receive, application processing, transmit, and completion each imply different owners. A retransmission may still need payload bytes after an individual send completes.

## 33.2 Separate the headers from the payload lease

Wire views describe offsets and byte order in packet headers. A linear ring capability represents the allowed next transition for the backing frame. A valid header does not release the underlying memory. Keep a lease on the backing bytes until the transport can retire them after acknowledgement or retransmission.

## 33.3 Schedule packet transmission

QUIC ingress reconstructs and validates packet information before using the payload. Egress must respect both available congestion window and pacing time. BBRv3 models delivery conditions to guide sending. A timing wheel groups future send times so ready packets can be found without scanning every connection.

The transport owns a different unit in each protocol. TCP exposes an ordered byte stream. QUIC carries separately ordered streams over packets and protects packets with keys and nonces. Splitting a header from a payload can avoid copying bytes, but a frame retained for retransmission still needs a live owner. Sharding connection state keeps a connection’s updates on its assigned worker.

## 33.4 Check conservation and pacing as separate properties

Frame conservation asks whether every frame is in exactly one allowed state. A pacing theorem asks whether modeled sends obey the computed bounds. Neither property guarantees a particular throughput on a live network. Delayed completions, loss, and reordering are useful simulation cases.

## 33.5 Reconstruct a packet number near a window edge

QUIC transmits a truncated packet number and the receiver reconstructs the intended full number relative to its expected value. Test around the half-window boundary, where the nearest candidate changes. The pacer and UMEM verifier track distinct state alongside this decoder.

# 34 Prism web service contracts

An HTTP handler can start streaming a response or hand its connection to another protocol. Prism tracks the response state so the caller knows which action is still valid if the client disconnects.

## 34.1 Model a request from route to completion

A router chooses a handler. Middleware supplies scoped behavior such as logging. A request arena owns short-lived decoded data. A response obligation remains live until the application finishes, aborts, or transfers it through an allowed upgrade.

## 34.2 Finish the right kind of response

A responder with pending headers can commit headers and enter a body-writing state. After the response finishes, sending more bytes would violate the protocol. Disconnect introduces another exit path. Account for live response and application resources before unwinding that path.

## 34.3 Scope middleware around an operation

A lexical handler can provide a request-specific policy to downstream code. Giving the handler a narrow projection of shard state limits the fields it may change. This lets a request update its counters while another operation reads unrelated configuration.

## 34.4 Transfer ownership on a WebSocket upgrade

A successful upgrade hands the connection from HTTP response handling to a WebSocket session. Treat it as a transition with one owner at the end. The ordinary response path must finish without an upgrade, and the upgraded path must eventually close the session.

## 34.5 Exercise failure at a protocol boundary

A simulated client can disconnect before headers, during a body, or after an upgrade. Check which obligations remain after each event. Policy proofs reason about modeled routes and states. Running the scenarios also tests whether the implementation emits the transitions the proof assumes.

## 34.6 Match a route in a radix tree

A radix tree shares common prefixes between route paths. Matching consumes a literal prefix or binds a parameter until it reaches a handler. Define precedence and validation for ambiguous paths, and keep extracted route values within the request lifetime.

# 35 Chorus protocols and distributed ownership

A choreography describes a conversation among services in one place. Chorus derives each participant’s send, receive, and close steps from that description so the endpoints agree on who acts next.

## 35.1 Read a protocol from one role’s viewpoint

A session type specifies an ordered conversation. A role can send, receive, select a branch, or close at the appropriate step. Following the protocol constrains local ordering. Delivery, progress during partitions, and agreement among replicas need explicit network assumptions.

## 35.2 Keep remote memory registered until access ends

Remote direct memory access (RDMA) lets a network interface operate on registered memory. A lease tracks registration and outstanding work. A peer timeout alone leaves open the possibility of device access. Revocation and completion evidence are needed before quarantined storage returns to the allocator.

## 35.3 Project a quorum write to its participants

A coordinator sends a proposal, replicas acknowledge it, and a quorum determines whether the operation can advance. Project the global description to each endpoint. If an endpoint must choose between two behaviors, it needs enough information locally to distinguish the branches.

## 35.4 Separate quorum intersection from election safety

Two majorities of a fixed replica set overlap. In a set of five, any two groups of three share at least one replica. Turning that fact into election or log safety also requires rules for how a replica votes, records its decision, and recovers after a crash.

## 35.5 Check that local branches can merge

A role uninvolved in a global branch may see identical local behavior on each side. The projection can merge those paths. If one path requires receiving a different next message without an observable choice, the protocol needs an explicit signal or a revised ordering.

# 36 Quarry SQL and buffer pools

Quarry checks SQL against a known schema before execution. When a query runs, a page pin keeps the database from evicting storage that the query still needs. The query engine can then scan its fields in batches.

## 36.1 Check a query against a known catalog

Replay schema migrations into a local catalog and check names and types against that snapshot. A buffer pool separately tracks which pages are resident and pinned. Columnar execution works on batches of fields. For example, a valid column name passes the schema check even if the page holding that column has since been evicted.

## 36.2 Hold a pin while a page view is live

A slotted page stores a header, slot information, and row data. A pin prevents a buffer frame from being evicted while the caller uses it. Releasing a pin exactly once protects frame ownership. Concurrent page mutation also needs latches and a consistent acquisition order.

## 36.3 Execute a checked query in batches

A proposed `@sql` expansion reads the catalog during the build and produces typed query access. At execution, a filter can examine a tile of values and pass selected rows to the next operator. Push a filter earlier only when doing so preserves the query’s semantics.

## 36.4 Account for pages, tenants, and recovery

Pin conservation checks that acquisitions and releases balance. Tenant isolation checks that a query can reach only authorized pages. A write-ahead log and recovery protocol address the order in which changes become durable. A balanced pin count can coexist with a read from the wrong tenant. Check the page’s tenant as well as its pin.

## 36.5 Trace a plan through the optimizer

The catalog supplies types and schema facts. A relational optimizer compares candidate plans using valid equivalences and estimated costs. A useful test compares the results on empty inputs, duplicates, and joins with multiple matches, where a mistaken rewrite can quietly change multiplicity.

# 37 Flux incremental streams

When one record changes, a materialized view can update its answer without rerunning the entire query. Flux represents both insertion and removal as weighted changes and tracks how far processing has progressed.

## 37.1 Interpret a signed multiplicity

A Z-set assigns an integer weight to a record. Adding a row contributes +1 and retracting it contributes −1. Consolidation sums equal records and removes zero weights. If the same key arrives twice and is retracted once, one occurrence remains.

## 37.2 Track changes and frontiers

A change-data-capture (CDC) record describes an update. AoSoA tiles batch the changes, and a frontier ticket represents progress in logical time. A watermark states how far a source has advanced under its lateness contract. A checkpoint barrier coordinates a consistent snapshot across operators.

## 37.3 Update a join and close a window

To update a join, pair new rows from each input with matching old rows on the other. If both inputs change, also count matches between the two changes. A tumbling window needs a frontier and a late-data policy before it can discard old state.

For a single matching key, multiply the multiplicities to count joined pairs. Suppose the left count grows from two to three and the right count grows from three to five.

    Old joined pairs             2 × 3 = 6
    Added left × old right       1 × 3 = 3
    Old left × added right       2 × 2 = 4
    Added left × added right     1 × 2 = 2
    New joined pairs             6 + 3 + 4 + 2 = 15

A fresh batch join gives the same 15 pairs. Omitting the interaction between the two changes would lose two pairs.

## 37.4 Compare incremental and batch answers

Apply all input changes up to a logical time, then compare the maintained view with a fresh batch computation. A second property requires the watermark to move monotonically. General joins may retain unbounded history. A memory bound requires a window, eviction rule, or input bound.

## 37.5 Differentiate an operator circuit

A circuit describes how operators such as joins, sums, delays, and feedback connect. Differentiation constructs operators that propagate changes. Consolidation combines accumulated weights for equal records. Feedback needs a clear time boundary so a change returning around the circuit belongs to the correct step.
