# Wave six lane 18: eBPF program types, tail calls, BTF CO-RE and verifier precision

Design coverage: §27.1 items 1 and 4 (`@btf_core`), §27.5 (pre-flight
verifier) for more program types. Claim issue #260.

- Base: `64c0be3` (origin/main, descends from `cda092f`).
- Branch: `claude/project-thread-jm0y7g`. It starts from the paused
  work-in-progress commit `5e440e8` of `claude/project-thread-hhftt1`
  (BTF reader, CO-RE resolver, program type enum, prog-array map kind),
  cherry-picked onto main. That thread was stopped while rewriting the ELF
  writer; this lane wrote the ELF part as a separate module
  (`btf_ext.rs`) with small additions to `elf.rs`.

## What was built

| Area | What |
| --- | --- |
| Program types | `ProgramType::{Xdp, SchedCls, Tracepoint, Kprobe}` in the verifier, interpreter (XDP only, see gaps), loader and ELF. |
| Contexts | tc: `struct __sk_buff` u32 fields (`len` … `tc_classid`), `data`/`data_end` as packet pointers with direct packet read/write, writes to `mark`, `priority`, `tc_index`, `cb[0..5]`, `tc_classid` (scalars, `STX` only). Tracepoint: read-only record bytes `[8, 2048)`, aligned, any size. Kprobe: read-only `pt_regs` (168 bytes on x86-64, 272 on AArch64; other architectures reject kprobe programs with `prog.unsupported_type`). |
| Helper sets | `helpers::allowed(type, id)`. Common: map lookup/update/delete, `ktime_get_ns`, `get_prandom_u32`, `get_smp_processor_id`, ring buffer, `tail_call`, `loop`. XDP adds `xdp_adjust_head`, `sk_lookup_tcp`, `sk_release`; tc adds the two socket helpers; tracing adds `get_current_pid_tgid`, `get_current_uid_gid`, `get_current_comm` (an `ARG_PTR_TO_UNINIT_MEM` buffer: stack bytes become initialised). |
| Tail calls | `MapKind::ProgArray` (`MapDef::prog_array`), `bpf_tail_call(ctx, prog_array, index)`; map helpers on a program array and tail calls through other maps are rejected; no tail call while a reference is held or preemption is disabled; r0 is unreadable after the call (`RET_VOID`). |
| `bpf_loop` | `Asm::ld_func` (`LD_IMM64 BPF_PSEUDO_FUNC`), `RegType::Func`, callbacks become subprograms and call-graph edges. The verifier explores "no iteration" and "enter the callback" at the call; each callback exit forks "another iteration" and "return to the caller". A re-entry whose state is included in the callback's own in-progress entry state is pruned (the kernel bounds the count at run time). The callback must return 0 or 1; its context argument is a stack pointer or NULL; flags must be 0. |
| kfuncs | `Asm::call_kfunc(name)`, `Program::kfuncs` (slot, name). Modelled: `bpf_preempt_disable`/`bpf_preempt_enable` (balanced on every path, no exit, tail call, `bpf_loop` or subprogram return inside a region). GPL-compatible license required. `btf::resolve_kfuncs` writes the vmlinux BTF id into each call; `kernel::prepare` does that (and CO-RE) before `BPF_PROG_LOAD`. |
| CO-RE | `core::{LocalType, CoreSpec, CoreRelo, apply}` (from the WIP) plus `Asm::core_ldx` (relocated load offset) and `Asm::core` (relocated immediate). Kinds: field byte offset/size/exists/signed, type exists/size, enumerator exists/value. |
| `.BTF.ext` | `btf_ext::build` writes the object `.BTF` (maps, one `FUNC` per subprogram, extern `FUNC`s in `.ksyms` for kfuncs, CO-RE local types) and `.BTF.ext` v1 (func info, line info, CO-RE records); `btf_ext::parse_ext`, `local_type`, `maps_from_btf` read them back. `Asm::line` records line info. |
| ELF | Sections `xdp`, `classifier`, `tracepoint/<cat>/<event>`, `kprobe/<function>` (`Program::attach`); `elf::parse_section`. kfunc calls get `R_BPF_64_32` against an undefined symbol of the kfunc's name. Objects without subprograms, kfuncs, CO-RE or line info are written exactly as before. |
| Loader | `kernel::load` passes program BTF and func info when the program has callbacks (the kernel requires it); `kernel::load_with_ext` also passes line info and the CO-RE records, so the kernel applies them itself. |
| Precision | Scalar id linking (`Frame::links`): `mov64 rX, rY` of a scalar links the two; a comparison narrows every linked register; any write unlinks; pruning requires the explored state's links to hold in the current one (`check_ids`). |

Item 5 (the linear `BpfRingSlot`/`BpfSocketRef` builder API) was not
touched, as the prompt and the owner require.

## Kernel evidence (this cloud host: Linux 6.18.44, x86-64, root, `CAP_BPF`, JIT on)

All measured here; on hosts without `bpf()` or `/sys/kernel/btf/vmlinux`
the kernel tests print that the evidence is unavailable and pass.

- Rule corpus (`kernel_agrees_on_the_rule_corpus`): every case, old and
  new (tc, tracepoint, kprobe, tail calls, `bpf_loop`, kfuncs, linking),
  gets the verdict its corpus entry states. Two new cases the kernel
  accepts and this crate rejects on purpose: `BPF_ST` into the tc context
  (6.18 accepts it) and `bpf_get_current_pid_tgid` from XDP (6.18's base
  helper set includes it).
- Random differential, XDP, 4,000 programs, default seed: both accept
  638, kernel only 551, neither 2,811, ours-only 0; 5,104 identical
  `BPF_PROG_TEST_RUN` vs interpreter runs. Our rules on the kernel-only
  programs: `mem.uninit_stack_read` 533, `mem.packet_out_of_range` 9,
  `alu.packet_var_too_large` 3, `mem.map_value_out_of_bounds` 2,
  `mem.null_deref` 2, `atomic.invalid_target` 1, `mem.stack_misaligned` 1.
  The same run on main gives the identical numbers: the random generator
  does not produce copy-then-bound patterns, so linking (proved on the
  corpus case `accept/linked_scalars`, which main rejects) does not move
  this column.
- Random differential over the new types (600 programs each, the XDP
  generator moved to each context layout): tc 80 / 81 / 439, tracepoint
  17 / 10 / 573, kprobe 11 / 9 / 580 (both / kernel only / neither),
  ours-only 0 everywhere.
- Accepted XDP corpus programs (including tail calls, `bpf_loop` and the
  preempt kfuncs) return the same value and packet under
  `BPF_PROG_TEST_RUN` and the interpreter; `accept/bpf_loop_count` returns
  3 in both.
- CO-RE: against `/sys/kernel/btf/vmlinux`, the probe's local
  `pt_regs.di`/`.si` at offsets 0/8 relocate to 112/104, `sizeof(struct
  pt_regs)` to 168 and `BPF_MAP_TYPE_PROG_ARRAY` to 3. aya-obj 0.3.0
  parses our object (`.BTF.ext` found, line and func info counts match),
  and its own `relocate_btf` against vmlinux produces instruction for
  instruction what `core::apply` produces. The kernel loads the probe with
  our CO-RE records and line info and relocates it itself; a record
  naming a type the kernel lacks makes the kernel log
  `relo #0: no matching targets found` and reject, so the records are
  really applied.

## Item 4: precision, and why the kernel-only column barely moves

533 of the 551 kernel-only programs read stack bytes that were never
written. A privileged kernel allows that (`CAP_PERFMON`); this verifier
rejects it on purpose (the interpreter treats it as a fault, and §27.5
wants a deterministic pre-flight). Closing that gap would mean accepting
reads of unspecified data, so it stays.

The kernel's "precision tracking" (`mark_chain_precision`) does not decide
acceptance: it lets the kernel prune more states by forgetting scalar
values that never reach a pointer or helper argument. None of the
kernel-only programs is rejected here for complexity (`complexity.limit`
is absent from the list above), so backtracking precision would change
nothing in this differential and was not built. Scalar id linking does
change acceptance and was built; the remaining 18 non-stack cases are
listed above and left as gaps.

## Decisions

- Every new kernel-permissive point is a rejection here, never the
  reverse: tc narrow context loads, `queue_mapping`/`tstamp` writes,
  `BPF_ST` into the context, tail calls in programs with subprograms or
  callbacks (the kernel allows them on x86-64 with frame limits), kfuncs
  other than the preempt pair, `bpf_loop` inside a preempt region, and
  tracing helpers from XDP.
- Program BTF for loading is generated from the instructions (func info
  for every subprogram), with `int main(void *ctx)` global and every other
  subprogram `static int f(u64, void *)`, so the kernel verifies callees
  in their caller's context as this verifier does.
- `.ksyms` keeps libbpf's layout (extern `FUNC`s); the kernel itself does
  not accept that BTF, so `load_with_ext` resolves kfunc ids first and
  loads BTF without the externs, as libbpf does.
- `section_name(SchedCls)` is `classifier` (aya-obj 0.3 does not know
  `tc`); the reader accepts `tc` and `tp` too.

## Tests

- `tests/verifier_rules.rs`: corpus verdicts and rule coverage; 34 new
  corpus cases in `tests/common/depth.rs` (every new rule covered).
- `tests/kernel_diff.rs`: corpus vs kernel, random XDP differential with
  test runs, random differential over tc/tracepoint/kprobe, interpreter vs
  kernel on the accepted corpus.
- `tests/btf_core.rs`: CO-RE against synthetic and host BTF, object round
  trip of kfuncs, CO-RE records, line info and callbacks, malformed and
  truncated `.BTF.ext`, aya-obj parse and relocation, kernel-side CO-RE.
- `tests/elf.rs`: round trip of every accepted corpus program (all four
  types now).

Commands, all passing on this host:

```text
cargo fmt --all -- --check
cargo test -p vesper-ebpf --locked
VESPER_EBPF_DIFF_PROGRAMS=4000 cargo test -p vesper-ebpf --test kernel_diff -- --nocapture
CARGO_BUILD_JOBS=2 cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings
```

## Root patch

`src/ebpf_commands.rs` builds a `Program` literal; the four new fields
(`attach`, `kfuncs`, `core_relos`, `lines`) are added there with empty
values. No dependency or lockfile change. Kept in its own commit.

## Remaining gaps

- The interpreter runs XDP programs only; tc, tracepoint and kprobe
  programs are checked against the kernel verifier but not executed.
- The interpreter installs no tail-call targets: every `bpf_tail_call`
  falls through, as the kernel's does for an empty slot.
- Two kfuncs are modelled. Others are `call.kfunc_unsupported`.
- `bpf_loop` callbacks that accumulate distinct values on the caller's
  stack never reach a repeated entry state and run into the
  1M-instruction limit; the kernel widens imprecise scalars there.
- CO-RE: no bitfields, `TYPE_ID_*` or `TYPE_MATCHES`; kernel BTF of
  modules is not searched.
- Lean: `lean/EbpfFirewall.lean` is unchanged; nothing in this lane is
  modelled in Lean.
- The Vesper source language does not yet emit these program types; this
  is the library layer (`Asm`, verifier, ELF, loader).
