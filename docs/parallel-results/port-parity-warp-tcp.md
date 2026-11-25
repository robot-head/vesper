# Port parity lane 4: warp-tcp

Claim [#952](https://github.com/robot-head/vesper/issues/952). Prompt:
`docs/agent-prompts/port-parity/04-warp-tcp.md`. Baseline: `main` at
`cee8b1a` (descendant of the required `69c79b9`).

Rust stays the default build of `vesper-warp-tcp`, and no Rust code is
deleted. `vesper-port` stays a non-default feature.

## Result

| Workload | Instructions, `main` | Instructions, this lane | Wall clock, this lane |
|---|---|---|---|
| codec (64 segments encoded, checksummed, parsed, verified) | 7.63x | **0.90x** | 0.73x to 1.50x across runs (median of four runs about 1.05x) |
| transfer (64 KiB over the lossy harness) | 5.36x | **1.91x** | 2.59x (95% interval 2.47x to 2.66x) |

The codec now runs about as fast as Rust. In instructions it is 10% below
Rust. The wall clock on this shared host moves between 0.73x and 1.50x from
run to run, so it does not settle the parity bar either way. The transfer
path is still 1.9x in instructions and 2.6x in wall clock. What is left there
is native code generation in the connection state machine, which no port
change could remove (see "What is left"). **The port does not meet the
parity bar**, because transfer is above 1.05x.

## What changed in the port

Every change is in `crates/vesper-warp-tcp/`. One commit per lever:

1. **Wide header reads and writes, block payload copies** (`options.ves`,
   `conn_input.ves`). `rd16`/`rd32` use `try_read_u16_be`/`try_read_u32_be`,
   and `wr16`/`wr32` use `try_write_u16_be`/`try_write_u32_be`. The old byte
   path is kept as the fallback when the interval does not fit, so the
   partial read and write at the end of a buffer behave as before.
   `copy_into` (payload into the segment, segment into the receive ring) does
   one `try_copy_from` when the source interval is in range, and falls back
   to the byte loop otherwise.
2. **`sum_be16` checksum** (`segment.ves`). `sum_range` is one vectorized
   `sum_be16` scan over `bytes[from..min(to, len)]`. The handler and the
   stride-2 loop of `sum_range_raw` are gone. `compute_checksum`,
   `fill_checksum` and `verify_checksum` all go through `sum_range`.
3. **Wide reads in segment and option parsing** (`segment.ves`,
   `options.ves`). After its length check, `segment_parse` reads the fixed
   header as three `u64` words (bytes 0..8, 8..16 and 12..20) and takes the
   fields out with shifts. The option parser reads the kind and length bytes
   in place, and reads the timestamp pair and each SACK block with one
   `try_read_u64_be`. The `rd*` helpers stay as the fallback.

Other changes:

- **The `wire` divergence edit** (`build.rs` `DIVERGENT_EDITS` and
  `BUILD.bazel`) used to drop the odd trailing checksum byte. That line is
  now inside `sum_be16`, so the edit now changes the IPv4 pseudo-header
  protocol number from 6 to 0. The divergent build still fails
  `fixture_checksums_verify_and_recompute` and still passes
  `rfc1071_numeric_example`, as `scripts/test-port.sh` requires.
- **`examples/abba.rs`** also prints the 95% median interval of the
  Vesper/Rust ratio, from `vesper-bench`'s signed-rank analysis.

Not done, and why:

- **`fill` for buffer clears.** The port has no byte-clearing loop. The
  receive ring and the segment buffers belong to the Rust side, and
  `write_header` writes every header byte.
- **`// @export abi 3`.** It would not change the hot calls. The TCB already
  crosses as a handle, with its words in place and no copy. `Ips` is an
  enum, and enums keep the v2 conversion. `Hdr` and `Opts` go to
  `write_header` by value, and v3 changes only `in`/`inout` aggregates. The
  whole binding layer (`ported::conn` wrappers minus the Vesper functions
  they call) is about 0.4 M instructions of the 16.7 M in transfer.
- **Splitting the 17 files back into modules.** It does not affect speed, so
  it was left out of this lane.
- **Port-side fixes to the sent-queue scans.** A rewrite of `pipe` that walks
  the ring as two plain runs, with no `sq_slot` and no per-element modulo,
  measured the same (1,252,438 against 1,248,201 Ir), so it was dropped. Out-of-line
  fallbacks for `rd16`/`rd32` did not get them inlined either (see gap 1),
  so that was dropped too.

## Measurements

Setup: Linux x86-64 cloud container (Intel Xeon @ 2.30 GHz, 4 vCPU, kernel
6.18), Rust 1.97.0, release builds, the Vesper library built by the port
harness with `optimize = true`. Every Vesper arm printed the same digests as
Rust.

### Instructions (callgrind `Ir`, whole worker process)

`abba worker codec` with 64 requests and `abba worker transfer` with 4
requests, as in the Stage 1 reports.

| Step | Codec Ir | Codec ratio | Transfer Ir | Transfer ratio |
|---|---|---|---|---|
| Rust | 33.94 M | 1.00x | 8.74 M | 1.00x |
| Vesper, `main` | 259.02 M | 7.63x | 46.84 M | 5.36x |
| + wide reads/writes and block copies | 251.25 M | 7.40x | 27.53 M | 3.15x |
| + `sum_be16` checksum | 32.59 M | 0.96x | 17.05 M | 1.95x |
| + wide header and option parsing | 30.39 M | **0.90x** | 16.66 M | **1.91x** |

Marginal cost, with the fixed process start-up taken out (128 minus 64 codec
requests, 8 minus 4 transfers): codec Rust 32.70 M, `main` 257.77 M (7.88x),
this lane 29.15 M (**0.89x**). Transfer Rust 8.15 M, `main` 44.48 M (5.46x),
this lane 15.36 M (**1.89x**).

The Stage 1 reports measured 1.19x codec with only the `sum_be16` rewrite.
This lane is below that because it also includes the wide header writes, and
`main` now also has the register-status `Fail` ABI (#946).

### Wall clock (`abba drive`, 12 measured epochs, A/B/B/A)

One run per step, in one session, on the same machine:

| Step | Codec median (95% interval) | Transfer median (95% interval) |
|---|---|---|
| Vesper, `main` | 4.27x (4.12x to 5.10x) | 5.32x (5.10x to 5.51x) |
| + wide reads/writes and block copies | 3.86x (3.80x to 3.94x) | 3.55x (3.36x to 3.64x) |
| + `sum_be16` checksum | 1.08x (1.04x to 1.10x) | 1.77x (1.69x to 1.81x) |
| + wide header and option parsing | 1.05x (1.04x to 1.11x) | 2.59x (2.47x to 2.66x) |

Two more runs of the final build:

| Run | Codec | Transfer |
|---|---|---|
| 2 | 1.50x (1.06x to 1.56x) | 2.59x (2.50x to 2.70x) |
| 3 | 0.73x (0.72x to 0.87x) | 2.61x (2.49x to 2.68x) |

The codec's Rust median moved between 736 and 1,061 ns per segment across
these runs, so the codec's wall clock is noise around parity. Instruction
counts are the measure to trust for it. The transfer's wall clock is stable
at 2.6x. The 1.77x for `sum_be16` came from a run whose Rust median was an
outlier (190 µs against 122 to 132 µs in every other run). The earlier run
of the same binary measured 2.80x, so the parsing lever did not make
transfer slower.

## What is left

Self cost by function, transfer, final build (16.66 M Ir in all). Rust's
total for the same work is 8.74 M.

| Vesper cost | Ir | Share | Port or compiler |
|---|---|---|---|
| libc `memcpy` + `memset` (mostly the Rust harness's `Vec` work, which Rust pays too: 3.95 M there) | 4.47 M | 27% | neither (the harness is shared Rust) |
| `pipe` (in-flight bytes over the sent queue) | 1.25 M | 7.5% | compiler, gap 2 |
| `try_retransmit` (scan for a lost entry) | 1.21 M | 7.3% | compiler, gap 2 |
| `tcb_poll_transmit`, `try_new_data`, `on_synchronized`, `tcb_on_segment`, `handle_ack`, `emit_entry` (state machine, straight-line code) | 2.75 M | 16.5% | compiler, gap 2 |
| `opts_parse_at` + `segment_parse` | 1.12 M | 6.7% | compiler, gaps 1 and 2 (the port already does wide reads) |
| `sum_view` (the `sum_be16` scans) | 0.56 M | 3.4% | at parity: Rust's `compute_checksum` is 0.73 M |
| ported-glue wrappers (`ported::conn`, `with_slice`) | about 0.4 M | 2.5% | port glue, small |

The sent-queue scans run the same algorithm as Rust: the reference
`Connection::pipe` and `try_retransmit` loop over the same ring. Rust inlines
them into `poll_transmit`, and its whole `poll_transmit` is 1.53 M inclusive,
against 5.39 M here. The gap is the cost of each iteration and each
statement, not the work done.

## Compiler gaps

Both repros compile with this branch's compiler (`vesper build --lib
--target x86_64-unknown-linux-gnu`, optimizer on).

### 1. Functions with a slice parameter are never inlined

`src/ir_inline.rs` refuses any callee "whose types mention ... a slice
parameter". So every byte helper of the port stays a call: `rd8`, `rd16`,
`rd32`, `wr8`, `wr16`, `wr32`, `sum_view`, `copy_into` and `sack_blk_at`.
This is why the port reads the header in place in `segment_parse` and
`opts_parse_at` instead of calling `rd*`. Measured share: the remaining
`rd*`/`wr*` calls and the `sum_view` call wrappers are 1 to 2% of transfer.
The workaround of writing the reads in place costs readability in every
port that parses bytes.

```vesper
fn rd8(in b: Slice[u8], i: u64) -> u8 {
    match b.try_get(i) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}

pub fn header_sum(in b: Slice[u8]) -> u64 {
    let mut a: u64 = 0;
    let n: u64 = 20;
    for i in 0..n {
        a = wrapping_add(a, widen_u64(rd8(in b, i)));
    }
    a
}
```

`header_sum`'s loop still holds a `call` to `rd8`.

### 2. Per-statement cost of native code (stack slots, prologue zeroing, masks)

This is lane 3's finding from `stage1-perf-byte-loops.md` ("the register
model"), measured here on a port. In `pipe` (frame 0x180 bytes), the
prologue stores zero to every stack slot, and stores each one twice. Every
local is reloaded from its slot. Each `u32` operation is masked with
`and <constant>(%rip)`, a load from the constant pool, rather than done as a
32-bit operation. Each queue element is copied out whole as an
`Option[SentSeg]` (ten fields) before two `bool` fields are tested. The repro
below gives 62 zero stores and 10 constant-pool operands in one 312-line
function. Fixing it would help every hot function in the table above,
together about 40% of the Vesper transfer run, and every other port.

```vesper
copy struct Ent {
    start: u64,
    end: u64,
    syn: bool,
    fin: bool,
    pn: u64,
    sent_ns: u64,
    retransmits: u32,
    sacked: bool,
    lost: bool,
    rexmit: bool
}

fn sat_add32(a: u32, b: u32) -> u32 {
    let s = wrapping_add(a, b);
    if s < a {
        4294967295
    } else {
        s
    }
}

pub fn pipe(in q: ChunkSeq[Ent], n: u64) -> u32 {
    let mut p: u32 = 0;
    for i in 0..n {
        match q.try_get(i) {
            Option::Some(s) => {
                if !s.sacked && !s.lost {
                    p = sat_add32(p, wrapping_u32(wrapping_sub(s.end, s.start)));
                }
            },
            Option::None => (),
        }
    }
    p
}
```

What would close it, in order of payoff for this port: promote scalar locals
to SSA values instead of stack slots (`src/native.rs`, `ir_graph_at`); zero
only the slots that are read before they are written, and zero each once;
lower `u32` arithmetic to 32-bit operations or use immediate masks; and let
`try_get` on a sequence of copy structs project the fields that are used,
without copying the whole element.

## Tests

- `aspect crate warp-tcp test-port`: all 67 tests pass against
  the Vesper build, and the divergent build fails the six covered tests and
  passes the three unchanged ones.
- `cargo test -p vesper-warp-tcp` (Rust default build): 67 tests pass.
- macOS, Windows and Bazel (Linux) run in CI on the PR.

## Parity

Not met. The codec is at parity in instructions (0.89x to 0.90x), but its
wall clock is too noisy on this host to show the 95% interval below 1.05x
reliably. Transfer is 1.9x in instructions and 2.6x in wall clock, and gap 2
is the compiler work that would close it.
