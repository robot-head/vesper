# Wave seven lane 17: eBPF library — every program type runs, tail calls, more kfuncs, CO-RE

Design coverage: §27.1 items 1 and 4 (`@btf_core`), §27.4 (Lean model),
§27.5 (pre-flight verifier). Claim issue #340.

- Base: `283c811` (origin/main, descends from `c025b9a`).
- Branch: `claude/project-thread-8wl3r7`.
- Overlap check: no open issue or PR other than #340 covers this work.
  PR #165 (the linear ringbuf/socket builder) is excluded by the owner and
  was not touched.

## What was built

| Item | What |
| --- | --- |
| 1. Program types in the interpreter | `interp::Input::{Xdp, Skb, Tracepoint, Kprobe}` and `interp::run`; `run_skb(prog, env, packet, &SkbMeta)`. tc gets a 192-byte `struct __sk_buff` built from `SkbMeta` (`SkbMeta::test_run(packet)` reproduces what `BPF_PROG_TEST_RUN` derives for a packet on `lo`: `pkt_type` from the destination MAC, `protocol` from the EtherType, `ifindex`/`ingress_ifindex` 1, `len`, `hash` 0), direct packet access through `data`/`data_end` at offsets 76/80, writes only to the kernel's writable u32 fields, and `RunResult::ctx` (the kernel's `ctx_out`). Tracepoint: the record padded to 2048 bytes, the 8 common bytes unreadable. Kprobe: `pt_regs` of exactly `PT_REGS_SIZE` bytes. Tracing helpers `get_current_pid_tgid`, `get_current_uid_gid` and `get_current_comm` (strscpy-pad semantics) read `Env::task`. Per-type helper availability is enforced at run time too. `XdpEnv` is now `Env` (the old name is an alias). |
| 2a. Tail calls | `Env::install_tail_call(map, index, &Program)` / `remove_tail_call`: the target must be of the same program type and use maps identical to the caller's (as the kernel's prog-array compatibility check); errors are `TailCallError`. `bpf_tail_call` follows the kernel order: index out of range, then the 33-call counter (`MAX_TAIL_CALL_CNT`, bumped before the empty-slot check), then the empty slot (falls through). `RunResult::tail_calls` counts the taken ones. |
| 2b. kfuncs | `bpf_rcu_read_lock`/`unlock` (no nesting for non-sleepable programs, balanced on every path), `bpf_local_irq_save`/`restore` (flags in a dedicated stack slot, restored in LIFO order, the slot unreadable as data), `bpf_iter_num_new`/`next`/`destroy` (a 16-byte iterator in the stack; `next` returns `NULL` or a read-only 4-byte pointer, `RegType::RdonlyMem`; destroy required before exit; new on a live slot rejected). The interpreter runs them with the kernel's semantics (`struct bpf_iter_num_kern { int cur; int end; }`, empty range when `start > end`). New rules: `kfunc.rcu_nested`, `kfunc.unbalanced`, `kfunc.irq_order`, `kfunc.arg_type`, `kfunc.slot_in_use`, `kfunc.region`, `mem.kfunc_object`, `mem.rdonly_write`, `mem.rdonly_out_of_bounds`. |
| 2c. Widening | At a bounded back edge (an active iterator's `next`, or a `bpf_loop` callback entry), a state that differs from its in-progress ancestor only in scalar values widens those scalars to unknown, as the kernel's `widen_imprecise_scalars`. Widened values are tainted; if one later decides a branch, becomes a pointer offset, a constant helper argument or a callback return value, the pass stops (`loop.widened_precise`, internal) and the program is explored again without widening — the result the kernel reaches by marking such scalars precise. `VerifyReport::widenings` counts widenings; `VerifierPolicy::widen_loops` (default on) disables it. |
| 3. CO-RE | Bitfield members in local types (`LocalType::structure_bits`, `Bitfield`), written to object BTF with `kind_flag` and read back. Kinds `FIELD_LSHIFT_U64`, `FIELD_RSHIFT_U64` and bitfield `FIELD_BYTE_OFFSET`/`FIELD_BYTE_SIZE`/`FIELD_SIGNED` with libbpf's load-size rule (from the member type's size, doubling to at most 8 bytes until the field fits an aligned load); `TYPE_ID_LOCAL` and `TYPE_ID_TARGET` (`Asm::finish` writes the local id into the instruction, `btf_ext::local_root_ids`); `TYPE_MATCHES` (libbpf's `__bpf_core_types_match`). `Btf::parse_split` reads split BTF (ids and string offsets continue after the base), `btf::host_modules` reads `/sys/kernel/btf/<module>`, `core::apply_with_modules` searches modules only when vmlinux has no candidate (as libbpf), and `kernel::prepare` uses it. |
| 4. Lean | `lean/EbpfFirewall.lean`, namespace `Bounds`: a model of the verifier's packet-bounds tracking for straight-line XDP programs, with soundness proofs (below). |

## Kernel evidence (this cloud host: Linux 6.18.44, x86-64, root, `CAP_BPF`, JIT on)

Measured here; on hosts without `bpf()` or `/sys/kernel/btf/vmlinux` the
kernel tests print that the evidence is unavailable and pass.

- tc context: 18 `__sk_buff` fields read by a tc program over 7 packets
  (IPv4, IPv6, ARP, unknown EtherType, unicast/broadcast/multicast
  destinations): all 126 values identical between the interpreter and
  `BPF_PROG_TEST_RUN`. Context writes and `ctx_out` identical.
- Random differential: 444 accepted random tc programs test-run in both
  with identical return values (CPU pinned to 0, since
  `bpf_get_smp_processor_id` is visible).
- Tail calls: XDP and tc chains installed in a real prog array return the
  same values as the interpreter, including a self-tail-call that stops at
  the 33-call limit and an empty slot that falls through.
- kfunc corpus (26 cases) and widening cases: every kernel verdict equals
  the corpus verdict. `iter_sum(10)`, `iter_sum(-3)`, `iter_sum(1000)`,
  `iter_counter(1000)` (and its map value), `loop_counter(5)` and
  `loop_counter(100)` return identical values in the kernel and the
  interpreter. A loop indexing a map value with its counter
  (`precise_counter`) is rejected by both ("BPF program is too large" /
  `complexity.limit`).
- CO-RE: for 13 relocations (tcphdr `doff`/`syn` offsets, sizes, shifts
  and signedness, two `TYPE_MATCHES` probes, `task_struct` type ids and a
  member offset) the kernel's own relocation of our `.BTF.ext` records
  (`load_with_ext`) and our user-space relocation give the same values
  (each program returns its relocated immediate). A packet reader that
  extracts `tcphdr.doff` with relocated offset and shifts returns the same
  `doff` for 3 packets in the kernel (both relocation paths) and in the
  interpreter. One finding: the kernel and libbpf do not compare bitfield
  widths in `TYPE_MATCHES`; our first version did, the differential test
  caught it, and it now follows them.
- Lean fixture programs: the kernel verifier's verdicts on the 7 programs
  equal the Lean model's.
- Not exercised on the kernel: module BTF. This host exposes only
  `/sys/kernel/btf/vmlinux` (no modules with BTF), so module search is
  tested with synthetic split BTF only.

## Lean (`lean/EbpfFirewall.lean`, Lean 4.24.0)

Proved, about the model only (not about the Rust verifier, the kernel or
the bytecode):

- `step_sound`: if the model accepts an instruction, then for every packet
  and every register file inside the abstract state, the concrete step
  either leaves through a bounds check or stays inside the new abstract
  state.
- `run_sound`, `accepted_in_bounds`: a program the model accepts never
  reads outside `[data, data_end)` — for every packet, every packet
  contents and every initial register file.

The model: registers are unknown, a scalar with an upper bound,
`pkt_end`, or `data + c + off` with a hidden `c ≤ vmax` and a range;
instructions cover loads of `data`/`data_end`, immediate moves, `add`,
`and`, `mul`, pointer-plus-scalar (resets the range, as in the kernel),
the bounds-check idiom `tmp = p + n; if tmp > end goto exit`, and byte
loads. It leaves out range propagation to other registers with the same
id (so it accepts fewer programs than the kernel) and the 64-bit wrap
(arithmetic is on naturals). The 7 fixture verdicts are proved with
`decide`; the Rust test rebuilds those programs as BPF and checks that our
verifier and the kernel give the same verdicts and that the accepted ones
run without faults in the interpreter on packets of 8 lengths. Axioms
used: `propext`, `Classical.choice`, `Quot.sound`. The previous seven SYN
filter theorems are unchanged; `AUDITED_THEOREMS` is now 10.

## Decisions

- Widening is two-pass (widened, then exact on `loop.widened_precise`)
  rather than a backward precision pass: verdicts can only get stricter
  than the widened pass, and the exact pass is the previous verifier. The
  error reported is the exact pass's, except that a `complexity.limit`
  from the exact pass after a widened failure for another reason reports
  the widened failure.
- Iterator depth (the kernel's `iter_active_depths_differ`) is only used
  to stop `loop.unbounded` firing between nested iterator states, not for
  pruning.
- The tail-call target check compares whole `MapDef`s and program types;
  the kernel also compares JIT/offload properties, which do not exist
  here.
- `TYPE_ID_*` immediates are written by `Asm::finish` from the object BTF
  the ELF writer and loader emit, so the program and its BTF cannot
  disagree.
- Split BTF on split BTF is rejected (`parse_split` needs a standalone
  base), as the kernel's module BTF is always split on vmlinux.
- Bitfield `TYPE_MATCHES` ignores widths, following the kernel (see the
  evidence above).
- Review fixes (Codex review of the first push): a `next()` pointer
  carries its iterator's id and dies when that iterator is destroyed (the
  kernel turns it into a scalar; both reject a load through it, checked on
  the host); an iterator revisit bounds a loop only when it is the same
  iterator instance as the earlier visit. The kernel accepts
  `new; next; destroy; new; goto next`, which never terminates (checked
  on the host); this verifier rejects it (`loop.unbounded`), a deliberate
  stricter point. Union bitfields keep their widths in emitted BTF, an
  imported object's `TYPE_ID_*` immediates are renumbered for the BTF this
  crate emits, and `bpf_get_current_comm` may fill a reserved ring-buffer
  record in the interpreter.

## Tests

New: `tests/program_types.rs` (11), `tests/kfuncs_widening.rs` (4),
`tests/core_depth.rs` (9), `tests/common/kfuncs.rs` (26 corpus cases);
extended: `tests/kernel_diff.rs` (tc test runs in the random
differential), `tests/soundness.rs` (random programs of every type never
fault when accepted), `tests/lean_model.rs` (bounds model).

Commands and results on this host:

```text
cargo fmt --all -- --check                                   # clean
cargo test -p vesper-ebpf --locked                           # all pass
VESPER_LEAN=…/lean-4.24.0-linux/bin/lean cargo test -p vesper-ebpf --test lean_model   # 4 pass
CARGO_BUILD_JOBS=2 cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings   # clean
cargo test --workspace --locked                              # see PR
```

## Root patch

None. No dependency, lockfile or root source change.

## Remaining gaps

- Module BTF was never read from a real kernel (none on this host).
- The interpreter models one CPU, one task (`Env::task`) and one
  `SkbMeta`; it does not model `bpf_skb_*` helpers, tc ingress/egress
  differences, sleepable programs or `bpf_iter_num` on other iterator
  kinds.
- kfuncs beyond the nine listed in `helpers::KFUNCS` are
  `call.kfunc_unsupported`.
- Widening applies to iterator and `bpf_loop` back edges, not to plain
  bounded loops (which the kernel does not widen either), and precision is
  approximated by the exact fallback rather than tracked backwards.
- The Lean model covers straight-line XDP packet bounds without id
  propagation, not branches, loops, stack or maps.
- The Vesper source language does not emit these programs; this is the
  library layer.
