# 25 Silicon device registers and DMA

A driver shares memory with a device that can keep running after a CPU function returns. Silicon uses permissions and explicit state transitions to describe when the CPU may read, write, or release a device buffer. This matters for direct memory access (DMA), where the device reads or writes memory itself.

<!-- technical-detail -->

A driver shares memory with a peripheral that keeps running after a CPU function returns, while hardware interrupts can preempt an active stack frame at any instruction boundary. Vesper Silicon uses modal ownership, ordered interrupt guards, and explicit capability rows to govern when bare-metal software may read, clear, or transfer a device register or direct memory access (DMA) buffer. Static call-graph stack budgets and page-table shootdown tokens pair those source obligations with target-level machine-frame checks.

Silicon spans `@vesper-hw/silicon` and host verifier crate `crates/vesper-silicon` alongside its Vesper port `@vesper/silicon` in `crates/vesper-silicon/vesper/`, audited via `vesper build --verify-stack` and `vesper doc --wcsd-stack`. Mechanized protocol and interrupt-delivery models live in `lean/SiliconFrames.lean`, `lean/SiliconAia.lean`, `lean/OrderedContexts.lean`, and `lean/ResourceGrades.lean`. Per `REVIEW-STATUS.md` (`TYPE-09` and `SYS-01–06`), source-level `Order::Lifo` event emission into diagnostic `E0306` remains partially wired (`E0306` currently diagnoses moving a field out of a linear aggregate), external DMA cancellation and fiber stack-relocation lifetimes across native pointers remain open obligations, and the Lean theorems cover discrete register and frame models rather than physical silicon or bus caches.

## 25.1 Specify the hardware available to a driver

A freestanding program may have only a fixed amount of static RAM (SRAM), interrupt handlers, and device registers. It may have no heap or operating system. Function effects describe which facilities it needs. Static stack budgeting matters because an interrupt can add a frame while another frame is still active.

<!-- technical-detail -->

Freestanding compilation in Vesper follows directly from lexical capability rows rather than a separate `std` versus `no_std` ecosystem split. Because heap allocation (`Alloc`), network sockets (`Net`), and filesystem access (`Storage`) are explicit effects, any module omitting `Alloc`, `Net`, and `Storage` from `eff [...]` compiles zero-heap bare-metal code natively across ARM Cortex-M, Cortex-A, RISC-V, and `x86_64` while sharing generic collection, serialization, and bitfield libraries unchanged.

Static SRAM fibers declare `@stack_limit(bytes = N)`, while hardware interrupt service routines declare `@isr(irq = N, stack_budget_bytes = B)` under an `AtomicCtx` frame restriction that statically bans suspending effects (`Async`, `DmaWait`, `Storage`). Beyond CMSIS-SVD microcontroller descriptions, `crates/vesper-silicon` (and its Vesper port in `crates/vesper-silicon/vesper/`) parses flattened devicetrees (FDT versions 16 and 17) with 128-bit four-cell `ranges`, hierarchical `dma-ranges` bus windows, PCI `msi-map` requester-ID routing, RISC-V PLIC 1.0, SiFive CLINT and ACLINT timers, RISC-V AIA 1.0 (APLIC `riscv,aplic` direct IDC and MSI forwarding, IMSIC `riscv,imsics` interrupt files, and hypervisor guest interrupt files controlled by `hstatus.VGEIN`, `hgeie`, `hgeip`, and `vstopei`), Arm GICv2 (`arm,gic-400`, `arm,cortex-a15-gic`) and GICv3 (`arm,gic-v3` plus GICv3 ITS `arm,gic-v3-its` `msi-controller` translation), and SiFive L2 `FLUSH64` non-coherent DMA cache maintenance.

    $ vesper build firmware/ --target riscv64gc-unknown-none-elf --verify-stack -o app.elf
    $ vesper doc firmware/ --bundle target/doc --wcsd-stack --target aarch64-unknown-none
    ----
    Interrupt / Bus Model      Devicetree / Hardware Binding                     Verifier & Lean Coverage
    RISC-V AIA 1.0 (APLIC/IMSIC) riscv,aplic + riscv,imsics + hstatus.VGEIN      crates/vesper-silicon, lean/SiliconAia.lean
    Arm GICv2 / GICv3 (+ ITS)  arm,gic-400 / arm,gic-v3 + arm,gic-v3-its         crates/vesper-silicon/src/dtb.rs (SPI/PPI/MSI)
    RISC-V PLIC & CLINT/ACLINT riscv,plic0 + sifive,clint0 + riscv,aclint-mtimer crates/vesper-silicon/src/intc.rs (port parity)
    DMA Windows & Cache Ops    dma-ranges + Zicbom CBO.INVAL + SiFive FLUSH64    crates/vesper-silicon/src/dma.rs, ccache.rs

## 25.2 Respect register semantics and device ownership

A write-one-to-clear (W1C) register clears a flag when software writes a one to the corresponding bit. An ordinary read-modify-write sequence can acknowledge unrelated flags. Generated register types distinguish permitted operations. Interrupt guards record release order. A DMA transfer changes who may access the buffer and may require cache maintenance.

Consider a two-bit status register containing only W1C flags. Both flags are pending. To acknowledge the low bit, write the low-bit mask.

    Pending before      11
    Acknowledgment      01
    Pending afterward   10

Writing back the entire value read from the register would clear both flags. The generated interface uses the register’s access semantics to select the write operation. Its address alone cannot tell the driver which bits to write.

<!-- technical-detail -->

Running inside Vesper Forge's fuel-metered `wasm32-wasip2` sandbox (`macro_emits_effects = []`), `@svd_pac` compiles CMSIS-SVD (exercised against the vendor RP2040 SVD) and devicetree register banks into linear peripheral typestate singletons (`Spi1[Disabled] -> Spi1[MasterReady]`) and access-indexed registers `MmioReg32[Reg, Access]`. Phantom policies `ReadOnly`, `ReadWrite`, and `Write1ToClear` select which volatile operations exist: `MmioReg32[Reg, Write1ToClear]` exposes `write_clear_mask` and omits read-modify-write helpers so clearing one interrupt status bit cannot acknowledge a sibling interrupt that arrived between the read and write.

    Register bit [1:0] (both W1C)    Naive RMW: reg |= (1 << 0)    Safe W1C: write_clear_mask(1 << 0)
    Pending in hardware before       11 (IRQ1=1, IRQ0=1)           11 (IRQ1=1, IRQ0=1)
    Value written to MMIO address    11 (echoes read 11 | 01)      01 (one-hot mask 1 << 0 only)
    Pending in hardware afterward    00 (BUG: IRQ1 silently lost!) 10 (IRQ0 cleared, IRQ1 preserved)
    ----
    @svd_pac Peripheral Typestate:   Spi1[Disabled] ──enable_clock()──► Spi1[Clocked] ──configure_master()──► Spi1[MasterReady]
    LIFO IrqGuard Stack (Order::Lifo):
      push g1 = disable_local_save()  ──►  push g2 = lock_nested()  ──►  pop g2 (must precede g1!)  ──►  restore_local(sink g1)

Spinlock acquisition in driver and kernel paths requires an `in` borrow of an `IrqGuard` token (`linear + lifo`), saving and restoring `DAIF` on Arm, `RFLAGS.IF` on `x86_64`, or `mstatus.MIE` on RISC-V so an ISR on the local core cannot preempt the lock holder and deadlock. The LIFO stack discipline is modeled in `lean/OrderedContexts.lean` (`LifoState`), though per `REVIEW-STATUS.md` (`TYPE-09`) full source-level guard-event emission into diagnostic `E0306` remains an open compiler integration obligation. Similarly, `DmaEngine::submit_rx` consumes `sink buf` into `DmaTransfer[Buf]` and emits architecture cache-maintenance barriers (`DC CVAC` and `DSB SY` on Arm, `FENCE IORW, IORW` or Zicbom `CBO.INVAL` under `xenvcfg.CBIE` on RISC-V) before peripheral bus access and upon completion.

~~~vesper
wire struct SgDmaDescWire {
    phys_addr: u64le,
    byte_len: u32le,
    flags: u16le,
    next_idx: u16le,
}
// Phantom register access markers omitting RMW on Write1ToClear registers:
copy struct ReadOnly {}
copy struct ReadWrite {}
copy struct Write1ToClear {}
// Ordered interrupt-mask guard and conserved 4 KiB physical page token:
linear struct IrqGuard { saved_psr: usize }
linear struct PhysFrame4K { pfn: u64 }
linear struct DmaTransfer[Buf] {
    channel: u8,
    desc_slot: u16,
    owned_buf: Buf,
}
linear effect CpuIrq {
    fn disable_local_save() -> IrqGuard;
    fn restore_local(sink guard: IrqGuard) -> Unit;
}
linear effect DmaEngine {
    fn submit_rx[Buf: DmaBuffer](channel: u8, sink buf: Buf) -> DmaTransfer[Buf];
    fn await_complete[Buf: DmaBuffer](sink xfer: DmaTransfer[Buf]) -> (Result[u32, DmaError], Buf);
}
~~~

## 25.3 Follow a frame through a page mapping

A physical frame token represents the right to manage a page. Mapping the page transfers that responsibility into a mapping, and unmapping it must recover the appropriate frame. In a driver, the analogous handoff is from a CPU-owned descriptor to the device and back on completion. Track the identity of the transferred page or descriptor until the corresponding unmap or completion.

<!-- technical-detail -->

Combining `@isr`, `@stack_limit`, `DmaTransfer[Buf]`, and `PhysFrame4K` keeps top-half interrupt acknowledgment, bottom-half DMA buffer ownership, and OS virtual-memory unmapping under one linear accounting discipline. When `PageTableRoot::unmap_leaf_4k` removes a user mapping, it withholds the underlying `PhysFrame4K` inside a linear `PendingTlbShootdown` obligation until `PendingTlbShootdown::flush_ipi_and_reclaim` broadcasts cross-core `SmpIpi::broadcast_tlb_invalidate` (`invlpg` or `tlbi`), preventing premature frame recycling while remote TLBs still cache stale translations.

    DMA Buffer Handoff:
      ┌───────────────────┐  submit_rx(sink buf)   ┌──────────────────────┐  await_complete(sink xfer)  ┌───────────────────┐
      │ CPU Exclusive Buf │ ─────────────────────► │ Bus-Master DMA Ring  │ ──────────────────────────► │ CPU Reclaimed Buf │
      │ (StaticDmaBuf)    │  DC CVAC / FENCE IORW  │ DmaTransfer[Buf]     │  ISR wake + CBO.INVAL       │ (valid_bytes)     │
      └───────────────────┘                        └──────────────────────┘                             └───────────────────┘
    Page-Frame Shootdown Gate (lean/SiliconFrames.lean):
      [Free] ──alloc──► [Held] ──map──► [Mapped] ──unmap_leaf_4k──► [PendingTlbShootdown] ──flush_ipi_and_reclaim──► [Free]

Per `REVIEW-STATUS.md` (`SYS-01–06`), linear buffer handoff alone does not solve stack relocation or asynchronous hardware abort races: Chapter 5's fiber stack-relocation argument applies only to pure value frames without external aliases, so DMA buffers (`StaticDmaBuf`) must reside in non-relocatable static SRAM or pinned `DmaSlab` pages, and dropping or unwinding an execution scope while bus-master DMA remains active requires hardware channel quiescence before memory reclamation.

~~~vesper
@isr(irq = 42, stack_budget_bytes = 256)
fn eth_mac_rx_isr(inout mac_regs: EthMacMmio, inout wake_ring: IsrWakeQueue) eff [Mmio] {
    mac_regs.irq_status_w1c.write_clear_mask(1 << 0);
    wake_ring.notify_dma_channel(0);
}
// SMP TLB shootdown obligation gating PhysFrame4K reclamation:
linear struct PendingTlbShootdown {
    vaddr: u64,
    asid: u16,
    reclaimed_frame: PhysFrame4K,
}
impl PendingTlbShootdown {
    fn flush_ipi_and_reclaim(sink self) -> PhysFrame4K eff [SmpIpi] {
        let PendingTlbShootdown { vaddr, asid, reclaimed_frame } = self;
        SmpIpi::broadcast_tlb_invalidate(vaddr, asid);
        reclaimed_frame
    }
}
fn unmap_and_recycle_user_page(
    inout pt: PageTableRoot, inout pmm: PhysBuddyAllocator, vaddr: u64,
) -> Result[Unit, VmError] eff [SmpIpi, Fail[VmError]] {
    let shootdown: PendingTlbShootdown = pt.unmap_leaf_4k(vaddr)?;
    let clean_frame: PhysFrame4K = shootdown.flush_ipi_and_reclaim();
    pmm.free_frame_4k(sink clean_frame);
    Ok(())
}
~~~

## 25.4 Prove a register or frame property

For a W1C model, check that acknowledging one flag leaves unrelated flags alone. For frame management, count identities before and after each transition to detect duplication and loss. The peripheral’s real register semantics and cache-coherence behavior still need to match the model.

<!-- technical-detail -->

Four Lean 4 modules mechanize Silicon's discrete models. In `lean/SiliconFrames.lean`, `w1c_single_ack` proves that writing a one-hot mask clears only bit `k`, `rmw_clears_all_pending` exhibits the read-modify-write hazard rejected by `silicon.mmio.rmw_of_write_clear_register`, and `step_preserves_pfns`, `count_partition`, and `no_reuse_without_shootdown` prove multiset conservation across `free`, `held`, `mapped`, `pending`, and `dma` frame states. In `lean/SiliconAia.lean`, `top_spec`, `claim_clears_top`, `msiPage_injective` (plus `qemu_s_level_drops_hart` documenting QEMU 8.2.2's `smsiaddrcfgh` `LHXW` collision), `vsclaim_isolated`, `toCpu_sound`, `compose_sound`, `mapId_sound`, and `hypTrace_matches_qemu` formalize RISC-V AIA delivery, `dma-ranges` composition, and PCI `msi-map` routing. Finally, `lean/OrderedContexts.lean` (`typed_checks`, `checked_is_typed`) and `lean/ResourceGrades.lean` (`stackBound`, distinguishing sequential `max` reuse from nested `frame` addition) cover LIFO guard traces and call-tree stack bounds. All four modules prove properties of their Lean definitions; silicon errata, bus write buffers, and physical DMA coherence still require hardware testing.

    -- lean/SiliconFrames.lean & lean/SiliconAia.lean (selected model theorems)
    theorem w1c_single_ack {n : Nat} (hw : Fin n → Bool) (k i : Fin n) (h : i ≠ k) :
        w1cWrite hw (oneHot k) i = hw i := by
      apply w1c_preserves_unwritten; simp [oneHot, h]
    theorem rmw_clears_all_pending {n : Nat} (hw extra : Fin n → Bool) (i : Fin n) :
        w1cWrite hw (fun j => hw j || extra j) i = false := by
      cases h : hw i <;> simp [w1cWrite, h]
    -- Frame & AIA isolation: SiliconFrames.no_reuse_without_shootdown, SiliconAia.vsclaim_isolated

## 25.5 Bound stack use along call paths

A stack-depth analysis combines the frames that can be active together. Interrupt nesting and indirect calls need bounds too. A register verifier can separately check a requested operation against the peripheral description. A small stack bound says nothing about whether a register write has the intended hardware effect.

<!-- technical-detail -->

At Stage 5 (`vesper build --verify-stack` and `vesper doc --wcsd-stack`), `crates/vesper-silicon` decodes real Cranelift 0.125 machine-code prologues across `x86_64`, `AArch64`, and `riscv64` into `MachineFrameInfo` nodes and runs `verify_freestanding_stack_and_isr`. A memoized depth-first search rejects call-graph cycles (`UnboundedRecursionInFreestanding`), unresolved function pointers (`IndirectCallWithoutStackBound`), suspending callees reachable from an `@isr` root (`SuspensionForbiddenInIsr`), and worst-case call paths exceeding `@stack_limit` (`StackBudgetExceeded`), while sibling passes in `atomic.rs` and `hazard.rs` audit spinlock ordering and W1C or read-side-effect MMIO loads and stores.

    // crates/vesper-silicon: Stage 5 WCSD & @isr call-graph verifier
    pub enum SiliconCheckError {
        UnboundedRecursionInFreestanding { cycle_at: SmolStr },
        IndirectCallWithoutStackBound { func: SmolStr },
        SuspensionForbiddenInIsr { isr_name: SmolStr, offending_fn: SmolStr },
        StackBudgetExceeded { entry: SmolStr, worst_case_bytes: u32, budget_bytes: u32 },
    }
    pub fn verify_freestanding_stack_and_isr(
        entry: FnSym, budget_bytes: u32, is_isr_context: bool,
        cg: &FxHashMap<FnSym, MachineFrameInfo>,
    ) -> Result<u32, SiliconCheckError> { /* DFS over frame_bytes + max(callees) */ }

# 26 eBPF packet programs and verifier checks

An extended Berkeley Packet Filter (eBPF) program runs under the kernel’s verifier. Vesper’s proposed pre-flight checks examine packet bounds and resource lifetimes before submission. The target kernel decides whether to admit the program.

<!-- technical-detail -->

An extended Berkeley Packet Filter (eBPF) program runs inside the operating system kernel only after passing the kernel verifier's static safety analysis. Vesper eBPF lowers wire-header views, linear map reservations, and bounded loops into bytecode shaped for verifier admission while running a pre-flight abstract interpreter before emitting an object file. Even when every Vesper check passes, the target Linux kernel verifier at load time remains the sole authority that admits or rejects the program.

The subsystem pairs `@vesper-kernel/ebpf` with `crates/vesper-ebpf` (and its Vesper port in `crates/vesper-ebpf/vesper/`), compiled via `vesper build --target bpfel-unknown-none` and inspected via `vesper ebpf check <obj|bin|hex> [--load --stack-budget 512 --disassemble]`. Packet-filter and straight-line bounds models reside in `lean/EbpfFirewall.lean`. Per `REVIEW-STATUS.md` (`SYS-01–06` and `TYPE-01`), Theorem 2 covers only its core linear calculus rather than full kernel helper semantics, `lean/EbpfFirewall.lean` models per-source counters as unbounded `Nat` values (ignoring `2^64` wraparound), and pre-flight verification never substitutes for `BPF_PROG_LOAD` on the target kernel.

## 26.1 Track the kernel’s constraints

A kernel program has limits on stack use, loops, helper calls, and the references those helpers return. A packet view must stay inside the current packet. Kernel type metadata and relocations let a program refer to fields across supported kernel layouts. A pre-flight check can flag an unbounded loop before the program reaches the target kernel.

<!-- technical-detail -->

Linux eBPF programs face a hard 512-byte total call-stack ceiling (`BPF_MAX_STACK_BYTES`), zero heap allocation (`Alloc` omitted from `eff [...]`), bounded loop iteration, strict tracking of acquired helper references, and explicit packet-pointer range proofs. Vesper eBPF enforces `@stack_limit(bytes = 512)` over the call graph, compiles `decreases` loop measures to `bpf_loop` or bounded open-coded iterators, and runs `@btf_core` inside Vesper Forge's hermetic `wasm32-wasip2` sandbox (`macro_emits_effects = []`) to emit Compile-Once-Run-Everywhere (CO-RE) `.BTF.ext` field relocations without host `clang` or `bindgen` dependencies.

In C and Rust eBPF toolchains, LLVM middle-end passes frequently hoist or algebraically rewrite pointer comparisons so `kernel/bpf/verifier.c` loses `PTR_TO_PACKET` range provenance. On entry to an XDP hook, `XdpPacketCtx` wraps the kernel `xdp_md*` context pointer in `R1` and lowers `view_wire_prefix[EthIpv4TcpWire]()` (and `view_wire_at[W](offset)`) directly into verifier-canonical `r_data + sizeof(W) <= r_data_end` guard sequences shielded from range-destroying transformations.

    $ vesper build xdp_syn.ves --target bpfel-unknown-none -o mitigator.o
    $ vesper ebpf check mitigator.o --load --stack-budget 512 --disassemble
    ----
    XDP Packet Buffer & Verifier-Canonical Bounds Guard (R1 = ContextPtr -> xdp_md*):
      ┌────────────────┬───────────────────────┬───────────────────────┬──────────────────────────┐
      │ Eth (14 B)     │ IPv4 (20 B, IHL >= 5) │ TCP (20 B)            │ Unvalidated Payload      │
      └────────────────┴───────────────────────┴───────────────────────┴──────────────────────────┘
      ▲ r_data (offset 0)                                              ▲ r_data + 54              ▲ r_data_end
      └─────────── validated_bytes = 54 after guard: if (r_data + 54 > r_data_end) goto pass ─────┘

## 26.2 Reserve and finish a ring slot

A ring-buffer reservation represents exclusive space for an event. Every path after a successful reservation must submit or discard the slot. Before decoding a header with a wire view, establish that the packet contains the entire header. Even when the packet is too short to decode, any slot already reserved still needs a submit or discard.

<!-- technical-detail -->

Kernel helpers such as `bpf_ringbuf_reserve`, `bpf_sk_lookup_tcp`, and `bpf_kptr_xchg` return live kernel references that must be released on every exit path. Vesper eBPF types a ring-buffer reservation as `linear struct BpfRingSlot[T]` under `linear effect BpfMaps`, pairing `reserve_ringbuf[T]` with terminal consumers `submit_ringbuf` and `discard_ringbuf` (`sink slot: BpfRingSlot[T]`). Sharing `EthIpv4TcpWire` and `SynDropAlertWire` as padding-free `wire struct` definitions guarantees identical byte layouts across the XDP producer and user-space consumer while making an unsubmitted or undiscarded slot a compile-time linearity error.

    BpfRingSlot[T] Reference State Machine (live_refs tracked per path):
                             ┌── None ───────────────────────────────────────────────► live_refs = ∅ ──► Exit (OK)
      reserve_ringbuf[T](1) ─┤                                      ┌─ submit_ringbuf(sink slot, in val) ─┐
                             └── Some(slot): live_refs = {ref_id} ──┼─ discard_ringbuf(sink slot) ────────┼─► live_refs = ∅ ──► Exit (OK)
                                                                    └─ early return without sink! ────────┴─► REJECT (E0302 / UnreleasedKernelReferenceAtExit)

~~~vesper
wire struct EthIpv4TcpWire {
    dst_mac: ByteArray[6],
    src_mac: ByteArray[6],
    ether_type: u16be,
    ver_ihl: u8,
    tos: u8,
    total_len: u16be,
    ident: u16be,
    frag_off: u16be,
    ttl: u8,
    proto: u8,
    ip_checksum: u16be,
    src_ipv4: u32be,
    dst_ipv4: u32be,
    src_port: u16be,
    dst_port: u16be,
    seq_num: u32be,
    ack_num: u32be,
    data_offset_flags: u16be,
    window_size: u16be,
    tcp_checksum: u16be,
    urgent_ptr: u16be,
}
wire struct SynDropAlertWire {
    src_ipv4: u32be,
    dst_port: u16be,
    tcp_flags: u16be,
    observed_pps: u64le,
}
copy enum XdpAction { Aborted = 0, Drop = 1, Pass = 2, Tx = 3, Redirect = 4 }
linear struct BpfRingSlot[T] { ring_ptr_id: u64 }
linear effect BpfMaps {
    fn percpu_increment_u64(map_id: u32, key: u32be) -> u64;
    fn reserve_ringbuf[T: WireFrame](ring_id: u32) -> Option[BpfRingSlot[T]];
    fn submit_ringbuf[T: WireFrame](sink slot: BpfRingSlot[T], in val: T) -> Unit;
    fn discard_ringbuf[T: WireFrame](sink slot: BpfRingSlot[T]) -> Unit;
}
~~~

## 26.3 Pass a decision to userspace

In the packet-filter example, an Express Data Path (XDP) hook examines traffic near the start of the receive path. It can record a compact event for a userspace consumer through the ring. Producer and consumer agree on the wire layout. A short packet needs a defined outcome even when the requested header is absent.

<!-- technical-detail -->

In `mitigate_syn_flood`, the XDP filter validates the 54-byte `EthIpv4TcpWire` prefix (and in `crates/vesper-ebpf/src/mitigator.rs` additionally checks `IHL >= 5` and `14 + 4 * IHL + 20 <= data_end` so IPv4 options cannot obscure the TCP header), passes non-TCP traffic and any segment carrying `TCP_FLAG_ACK`, increments a per-CPU counter on SYN-only segments, and streams `SynDropAlertWire` telemetry to a zero-allocation user-space `UserBpfRingReader[SynDropAlertWire]`. Whether the filter submits an alert or suppresses noisy telemetry under load, `sink slot` must be discharged through `submit_ringbuf` or `discard_ringbuf` before returning `XdpAction::Drop`.

~~~vesper
const ETH_P_IPV4: u16 = 0x0800;
const IPPROTO_TCP: u8 = 6;
const TCP_FLAG_SYN: u16 = 0x0002;
const TCP_FLAG_ACK: u16 = 0x0010;
@xdp_prog(section = "xdp", stack_budget_bytes = 512)
fn mitigate_syn_flood(in pkt: XdpPacketCtx, pps_threshold: u64, emit_alerts: bool) -> XdpAction eff [BpfMaps] {
    let Some(in hdr) = pkt.view_wire_prefix[EthIpv4TcpWire]() else { return XdpAction::Pass; };
    if hdr.ether_type.to_native() != ETH_P_IPV4 || hdr.proto != IPPROTO_TCP { return XdpAction::Pass; }
    let flags = hdr.data_offset_flags.to_native() & 0x003F;
    if (flags & TCP_FLAG_SYN) == 0 || (flags & TCP_FLAG_ACK) != 0 { return XdpAction::Pass; }
    let src_count = BpfMaps::percpu_increment_u64(0, hdr.src_ipv4);
    if src_count <= pps_threshold { return XdpAction::Pass; }
    match BpfMaps::reserve_ringbuf[SynDropAlertWire](1) {
        Some(slot) => {
            if emit_alerts {
                let alert = SynDropAlertWire {
                    src_ipv4: hdr.src_ipv4, dst_port: hdr.dst_port,
                    tcp_flags: flags.to_be(), observed_pps: src_count.to_le(),
                };
                BpfMaps::submit_ringbuf(sink slot, in alert);
            } else {
                BpfMaps::discard_ringbuf(sink slot);
            }
            XdpAction::Drop
        }
        None => XdpAction::Drop,
    }
}
~~~

## 26.4 Scope a filter proof

One modeled property is that a selected policy classifies packets according to its rule. A separate check establishes bounds for the inspected fields. Both depend on the packet representation and the policy assumptions. The kernel decides whether it can safely run the program. To verify the filtering decision, check it against the intended policy.

<!-- technical-detail -->

The repository model `lean/EbpfFirewall.lean` formalizes `crates/vesper-ebpf/src/mitigator.rs` over explicit packet byte lists where every read requires an index-bounds proof derived from the 54-byte prefix guard and the variable `14 + 4 * IHL` TCP offset check. It proves `reads_within_checked_bounds`, `short_packets_pass`, `drop_iff_spec`, `only_syn_only_packets_are_counted`, `syn_mitigator_never_drops_ack_packets` (any TCP packet with `ACK` set yields `.pass` regardless of counters or threshold), and a `Bounds` soundness theorem for straight-line XDP packet-range tracking, differentially checked against the bytecode interpreter in `crates/vesper-ebpf/tests/lean_model.rs`. These proofs establish policy accuracy for the Lean model only; at deployment time, Linux's `BPF_PROG_LOAD` verifier remains authoritative over actual instructions, BTF relocations, map FDs, and helper contracts.

    -- lean/EbpfFirewall.lean: zero false-positive drops on ACK packets & checked packet reads
    theorem syn_mitigator_never_drops_ack_packets
        (pkt : List UInt8) (counts : SrcCounts) (thresh : Nat)
        (h_ack : hasCheckedTcpAck pkt = true) :
        (mitigateSynFlood pkt counts thresh).action = Action.pass := by
      exact Vesper.EbpfFirewall.syn_mitigator_never_drops_ack_packets pkt counts thresh h_ack

## 26.5 Approximate possible machine states

An abstract verifier tracks facts such as “this register points into a packet” or “this offset is in this range.” It checks both successors of a branch and joins their facts. If one path lacks a sufficient bound, a safe pre-flight analysis must decline to certify the access.

<!-- technical-detail -->

Before ELF emission or `vesper ebpf check --load`, `crates/vesper-ebpf` runs an abstract verifier modeling `R0..R10` with `BpfRegState` (`Uninitialized`, `Scalar { min_u64, max_u64 }`, `ContextPtr` initialized in `R1`, `PacketData { validated_bytes }`, `PacketEnd`, and `RingBufReservation { ref_obj_id }`) plus the active reference set `live_refs`. In `verify_ebpf_block_preflight` (and its control-flow join engine in `crates/vesper-ebpf/src/verifier.rs`), packet reads beyond `validated_bytes` raise `UncheckedPacketAccess`, frames exceeding `BPF_MAX_STACK_BYTES` (`512`) raise `StackLimitExceeded`, and any `Exit` reached with non-empty `live_refs` raises `UnreleasedKernelReferenceAtExit`.

    // crates/vesper-ebpf: abstract register lattice and pre-flight verifier
    pub const BPF_MAX_STACK_BYTES: u32 = 512;
    pub enum BpfRegState {
        Uninitialized,
        Scalar { min_u64: u64, max_u64: u64 },
        ContextPtr,
        PacketData { validated_bytes: u32 },
        PacketEnd,
        RingBufReservation { ref_obj_id: u32 },
    }
    pub fn verify_ebpf_block_preflight(
        stack_frame_bytes: u32, insts: &[BpfAbstractInst],
    ) -> Result<(), EbpfPreFlightError> { /* R1 = ContextPtr; tracks validated_bytes & live_refs */ }

# 27 Aether virtual machines and snapshots

Cloning a paused machine also clones its memory and unfinished state. Two copies can therefore begin with the same random generator, device state, and pending identifiers. Aether treats resuming a clone as an explicit transition.

<!-- technical-detail -->

Cloning a paused virtual machine duplicates its guest RAM, entropy pool, open virtual-device queues, and in-flight network state across every resumed instance. Vesper Aether models snapshot quiescing and post-fork wake-up as linear state transitions so each clone consumes a fresh hardware generation ticket before reseeding local encryption state. Safe one-to-many copy-on-write cloning still requires closing active transport sessions and draining virtual queues before capture.

Aether spans `@vesper-cloud/aether` and host monitor crate `crates/vesper-aether` (with Vesper port modules in `crates/vesper-aether/vesper/` covering PVH boot structures, packed virtqueues, slot planning, and hotsets), while `vesper build --unikernel` remains a proposed CLI compilation target rather than a shipped subcommand flag. Checked guest-interval theorems reside in `lean/AetherMemory.lean`. Per `REVIEW-STATUS.md` (`SYS-01–06` and `REPO-04`), image-size and latency figures (1.6 MiB image, sub-1.2 ms cold boot, 400-microsecond fork) are design targets awaiting reproducible benchmarks, and reseeding a local `AeadSealer` via `VMGENID` does not rotate peer-negotiated QUIC or TLS session keys.

## 27.1 Decide what a snapshot can share

A unikernel packages an application with the small set of services it needs. A guest may share immutable backing pages across clones while keeping private writes separate. The snapshot contract must account for network sessions, device work, and entropy as well as RAM.

<!-- technical-detail -->

Under the proposed `vesper build --unikernel` pipeline, services expressing I/O via `eff [Net, Storage, Clock, ConstTime]` swap hosted POSIX handlers for Silicon (`@vesper-hw/silicon`) and Warp user-space networking over VirtIO 1.2 MMIO, booting directly via `x86_64` PVH (`crates/vesper-aether/src/pvh.rs`) or AArch64 direct kernel entry without BIOS, ACPI, PCI enumeration, or an `initramfs`. Rather than shelling out to an external JSON-controlled VMM, `crates/vesper-aether` embeds a seccomp-bpf-locked KVM monitor (with proposed macOS `Virtualization.framework` and Windows WHPX backends) directly inside worker shards.

To avoid duplicating read-only memory across thousands of concurrent sandboxes, Aether backs immutable Cranelift `.text`/`.rodata` segments and quantized model weights with Loom's BLAKE3 content-addressed store (`~/.vesper/store/`), mapping shared read-only `memfd` pages across guests via `UFFDIO_CONTINUE` while isolating writable guest heaps via private copy-on-write (`UFFDIO_COPY`) and proactive 2 MiB `io_uring` hotset batches.

    Aether PVH Unikernel GPA Layout & Lazy userfaultfd (UFFD) Snapshot Restore:
      GPA 0x0000_0000 ┌────────────────────────────────────┐
                      │ PVH Boot Info + AetherVmGenIdWire  │ ──► Fresh 128-bit VMGENID injected per clone
                      ├────────────────────────────────────┤
                      │ .text / .rodata / Quantized Tensor │ ──► UFFDIO_CONTINUE (ContinueSharedCasMemfd, 0-copy RO)
      cas_ro_end_gpa  ├────────────────────────────────────┤
                      │ Dirty Snapshot Heap & Stack Pages  │ ──► UFFDIO_COPY     (CopyPrivateCowPage, dirty bitmap=1)
                      ├────────────────────────────────────┤
                      │ Untouched VirtualArena Pages       │ ──► UFFDIO_ZEROPAGE (ZeroPage4K, dirty bitmap=0)
                      └────────────────────────────────────┘

## 27.2 Own the descriptor and the resume ticket

PVH describes a boot interface. VirtIO queues describe buffers passed between guest and virtual device. A descriptor lease records which side may reuse a queue entry. A separate resume ticket records the steps a restored clone must complete before performing identity-sensitive work.

<!-- technical-detail -->

Guest-to-host I/O over VirtIO 1.2 (`virtio-net`, `virtio-blk`, `virtio-vsock`) pairs 16-byte padding-free `VirtqPackedDescWire` ring entries with linear `VirtqDescLease` tokens so a driver cannot reuse an in-flight descriptor slot or leak ring capacity on malformed packets. Across snapshot forks, hypervisor wake supplies a 24-byte `AetherVmGenIdWire` block and mints a linear `SnapshotResumeTicket` carrying the clone's 128-bit hardware `VMGENID`; `AetherVm::quiesce_and_fork_snapshot` consumes `sink old_sealer` so pre-fork `AeadSealer` nonces cannot be minted after quiesce, and `AetherVm::reseed_after_fork` consumes `sink ticket` to derive a fresh `ReseededVaultEpoch[Alg]`.

~~~vesper
wire struct VirtqPackedDescWire {
    buf_gpa: u64le,
    byte_len: u32le,
    desc_id: u16le,
    flags_avail_used: u16le,
}
wire struct AetherVmGenIdWire {
    vm_gen_lo: u64le,
    vm_gen_hi: u64le,
    host_tsc_khz: u32le,
    virtio_mmio_count: u32le,
}
linear struct VirtqDescLease { queue_idx: u16, desc_id: u16 }
linear struct SnapshotResumeTicket { gen_lo: u64, gen_hi: u64 }
linear struct ReseededVaultEpoch[Alg] {
    fresh_sealer: AeadSealer[Alg],
    fork_epoch_id: u64,
}
linear effect AetherVm {
    fn quiesce_and_fork_snapshot[Alg: AeadCipher](sink old_sealer: AeadSealer[Alg]) -> SnapshotResumeTicket;
    fn reseed_after_fork[Alg: AeadCipher](
        sink ticket: SnapshotResumeTicket,
        in master_key: AeadKeySlot[Alg],
        inout slab: SecretSlab,
    ) -> ReseededVaultEpoch[Alg];
}
~~~

## 27.3 Quiesce, clone, and reseed

Quiescing brings outstanding operations to a defined boundary. Restoring a clone requires fresh entropy and a policy for old transport state. A lazy page resolver may supply missing pages from a content-addressed snapshot on demand. Restore or retire the saved transport state before letting either clone send on that session.

<!-- technical-detail -->

A critical security boundary documented in manuscript §27.3 and `REVIEW-STATUS.md` (`SYS-01–06`) governs snapshot cloning: consuming `sink active_sealer` and reseeding `AeadSealer` from `VMGENID` rotates only the guest's local epoch sealer and CSPRNG—it does NOT rotate negotiated QUIC or TLS peer traffic keys, session tickets, or packet-number counters held inside a live `conn_table`! If a worker forks one-to-many while `conn_table` retains established sessions, sibling clones wake with identical peer traffic keys and sequence numbers, causing catastrophic AEAD `(Key, Nonce)` reuse on the first egress packet. Every one-to-many CoW snapshot must close all TCP and QUIC sessions, drain all in-flight `VirtqDescLease` VirtIO queues to idle, and assert an empty `conn_table` before calling `AetherVm::quiesce_and_fork_snapshot`, establishing fresh handshakes only after `AetherVm::reseed_after_fork`.

    Safe One-to-Many CoW Snapshot-Fork Lifecycle (§27.3 / SYS-01–06):
      [Active Guest] ──► close_all_sessions (conn_table = ∅) & drain VirtqDescLease rings
                     ──► quiesce_and_fork_snapshot(sink active_sealer)
                     ──► [Golden Snapshot] ──┬──► Clone A (VMGENID_A) ──► reseed_after_fork ──► fresh QUIC/TLS handshake
                                             └──► Clone B (VMGENID_B) ──► reseed_after_fork ──► fresh QUIC/TLS handshake

~~~vesper
fn unikernel_quiesce_and_fork_loop[Alg: AeadCipher, P: IsaProfile](
    in root_key: AeadKeySlot[Alg],
    inout secret_slab: SecretSlab,
    sink initial_sealer: AeadSealer[Alg],
    inout conn_table: QuicConnTable,
    inout virtio_rx_tx: VirtioQueuePair,
) -> Never eff [AetherVm, XdpNic, ConstTime, Clock, Isa[P]] {
    let mut active_sealer = initial_sealer;
    loop {
        // Mandatory SYS-01-06 teardown: close negotiated peer sessions & drain VirtIO rings BEFORE fork!
        conn_table.close_all_sessions_for_snapshot();
        virtio_rx_tx.drain_all_leases_to_idle();
        let fork_ticket: SnapshotResumeTicket = AetherVm::quiesce_and_fork_snapshot(sink active_sealer);
        let ReseededVaultEpoch { fresh_sealer, fork_epoch_id } =
            AetherVm::reseed_after_fork(sink fork_ticket, in root_key, inout secret_slab);
        active_sealer = fresh_sealer;
        Region::scoped(256.kib(), |cap _req_arena| {
            accept_fresh_post_fork_sessions[Alg, P](
                fork_epoch_id, inout conn_table, in secret_slab, inout active_sealer,
            );
        });
    }
}
~~~

## 27.4 State the isolation and uniqueness assumptions

A model of extended page tables (EPT) can show that two guest mappings obey the stated separation policy. A nonce uniqueness argument needs independent post-fork identities and correct reseeding. Duplicating a snapshot without changing its entropy state violates that premise.

<!-- technical-detail -->

The checked repository module `lean/AetherMemory.lean` proves `checked_interval_sound`, `checked_endpoint_exact` (exact `UInt64` endpoint addition below `maxU64`), `separated_no_common_address`, and `fault_page_contained` (every in-range fault has its entire 4096-byte page inside the aligned slot), while explicitly noting that it is a host numeric model rather than a hardware EPT proof and that no axiom asserting unconditional HKDF injectivity is valid. In contrast, the manuscript's illustrative theorems `ept_slots_disjoint_host_phys` and `snapshot_fork_vmgenid_separates_epochs` summarize the intended security reduction: disjoint host physical slots share no frame, and distinct 128-bit `VMGENID` inputs yield distinct fork epochs only under standard cryptographic collision-resistance assumptions for HKDF combined with hypervisor enforcement of unique `VMGENID` injection and transport teardown.

    -- lean/AetherMemory.lean (checked numeric interval model) & §27.4 illustrative spec
    theorem separated_no_common_address (aStart aEnd bStart bEnd p : Nat)
        (sep : aEnd ≤ bStart ∨ bEnd ≤ aStart) (inA : aStart ≤ p ∧ p < aEnd) :
        ¬ (bStart ≤ p ∧ p < bEnd) := by omega
    -- Illustrative reductions (§27.4): ept_slots_disjoint_host_phys, snapshot_fork_vmgenid_separates_epochs

## 27.5 Validate mappings and resolve missing pages

The host checks which guest ranges may back each mapping. A userfaultfd (UFFD) resolver handles a missing-page request by finding the expected content and installing it under the proper sharing policy. Page identity, guest identity, and write permissions all matter at installation.

<!-- technical-detail -->

On the host side, `crates/vesper-aether` runs `verify_kvm_ept_layout` before registering `KVM_SET_USER_MEMORY_REGION` slots, rejecting zero-length regions (`ZeroSizedMemorySlot`), non-4-KiB-aligned guest or host bounds (`UnalignedMemorySlot`), and overlapping guest physical ranges (`OverlappingGuestPhysRange`). During lazy restore, `classify_uffd_snapshot_fault` maps faults below `cas_ro_end_gpa` to shared Loom CAS `memfd` pages (`ContinueSharedCasMemfd`), dirty bitmap hits to private CoW pages (`CopyPrivateCowPage`), and untouched heap pages to kernel zero pages (`ZeroPage4K`).

    // crates/vesper-aether: KVM EPT slot layout audit & UFFD fault classifier
    pub enum UffdPageAction {
        ZeroPage4K,
        ContinueSharedCasMemfd { cas_offset: u64 },
        CopyPrivateCowPage { snapshot_offset: u64 },
    }
    pub fn verify_kvm_ept_layout(slots: &[KvmMemSlotSpec]) -> Result<(), AetherVmmError> { /* 4K align & disjoint GPA */ }
    pub fn classify_uffd_snapshot_fault(
        fault_gpa: u64, cas_ro_end_gpa: u64, dirty_cow_bitmap: &[u64],
    ) -> UffdPageAction { /* ContinueSharedCasMemfd | CopyPrivateCowPage | ZeroPage4K */ }

# 28 Vault secrets, timing, and nonces

A key must stay out of public outputs, including logs. Encryption also needs a fresh nonce for each use under the same key, and secret-dependent operations can reveal information through timing. Vault treats each as a distinct obligation.

<!-- technical-detail -->

Cryptographic implementations fail not only when ciphertexts are computed incorrectly, but whenever secret keys leak into telemetry, nonces repeat under the same key, or execution timing varies with secret bits. Vesper Vault separates opaque secret scalars, linear single-use nonce tickets, locked key slabs, and backend static single assignment (SSA) taint checks into distinct compile-time obligations. Because register allocation, stack spilling, and silicon microarchitecture sit below SSA, these checks bound specific bug classes rather than proving physical side-channel immunity.

Vault comprises `@vesper-crypto/vault` and `crates/vesper-vault`, invoked at Stage 5 via `vesper build --verify-const-time` (or `--verify=const-time`) and paired with mechanized models in `lean/VaultConstantTime.lean`. Per `REVIEW-STATUS.md` (`SYS-01–06`) and §28.1, three concrete limitations apply: x86 `IA32_UARCH_MISC_CTL` (`DOITM`) is a privileged ring-0 MSR unavailable directly to user space, `verify_constant_time_ssa` `CtAuditedInst::Load` from a public address must propagate taint when loading secret memory, and SSA taint checking executes before register allocation, instruction selection, and stack-slot spills.

## 28.1 Track a secret beyond its type

Secret-marked data should stay out of public outputs, including logs. Temporary copies, allocator behavior, and generated instructions also influence exposure. The chapter presents constant-time execution and zeroization as design goals whose end-to-end behavior requires target-specific evidence.

<!-- technical-detail -->

Vault wraps secret values in opaque scalars `SecU8`, `SecU32`, `SecU64`, and comparison masks `SecMask64` that intentionally omit `Eq`, `Ord`, `/`, `%`, `Telemetrable`, and `Encode`, statically preventing accidental `if`/`match` branching, variable-latency hardware division, wire serialization, or structured log emission. Long-lived keys reside in `SecretSlab` pages pinned with `mlock` and excluded from core dumps via `MADV_DONTDUMP`, referenced by linear `AeadKeySlot[Alg]` handles that must terminate in `AeadKeySlot::shred` (`volatile_zero_and_fence`), while hybrid post-quantum key establishment pairs FIPS 203 ML-KEM-768 with X25519 in zero-allocation `X25519MLKEM768`.

Three §28.1 / `SYS-01–06` review limitations bound what `eff [ConstTime]` and `SecretSlab` establish today. First, while Armv8.4+ exposes `PSTATE.DIT` (`MSR DIT, #1`) to user space, `x86_64` `IA32_UARCH_MISC_CTL` (`DOITM`) is a ring-0 MSR that user-space processes cannot write directly without OS kernel mediation, and hardware DIT modes do not close all microarchitectural channels. Second, in `verify_constant_time_ssa`, `CtAuditedInst::Load { dst, addr }` rejects tainted addresses (`SecretDependentMemoryAddress`) yet leaves `dst` untainted when `addr` is public—secret-bearing memory loads from public base pointers must propagate taint into `dst` so loaded key words cannot feed unflagged branches. Third, SSA verification runs prior to register allocation, target instruction selection, and stack-frame spilling, meaning compiler-introduced `cmov`-to-branch rewrites or unzeroed stack spills require post-lowering machine-code auditing.

## 28.2 Bound observable behavior

A constant-time scope restricts operations whose control flow or memory addresses depend on a secret. Data-independent timing (DIT) features have a hardware-defined scope. Fenced zeroization is intended to keep a cleanup write from disappearing during optimization. Copies in other buffers, registers, or spill slots require separate accounting.

<!-- technical-detail -->

Inside `eff [ConstTime]`, comparisons on `SecU64` yield `SecMask64` (`0` or `0xFFFF_FFFF_FFFF_FFFF`) for branchless bitwise selection (`ct_select`), and crossing back to a public `bool` requires an explicit, auditable `declassify_bool` boundary—used only on the final single-bit AEAD authentication verdict in `verify_tag_16_ct` while `AeadKeySlot::shred` discharges key ownership via `volatile_zero_and_fence`.

    eff [ConstTime] SSA Taint Barrier (verify_constant_time_ssa):
      ┌───────────────────────┐   PureAlu / BitwiseSelect    ┌───────────────────────┐  declassify_bool  ┌─────────────┐
      │ SecU8 / SecU32 /      │ ───────────────────────────► │ Tainted SsaVal        │ ────────────────► │ Public bool │
      │ SecU64 / SecMask64    │                              │ (no Eq/Ord/Encode)    │  (tag verdict)    │             │
      └───────────────────────┘                              └───────────┬───────────┘                   └─────────────┘
                                                                         ├──► CondBranch         ──► REJECT: SecretDependentBranch
                                                                         ├──► Load/Store[addr]   ──► REJECT: SecretDependentMemoryAddress
                                                                         └──► VariableLatencyDiv ──► REJECT: SecretTaintedVariableLatencyDiv

~~~vesper
copy struct SecU8(u8);
copy struct SecU32(u32);
copy struct SecU64(u64);
copy struct SecMask64(u64);
copy effect ConstTime {}
linear struct AeadKeySlot[Alg] { slot_id: u32 }
impl[Alg] AeadKeySlot[Alg] {
    fn shred(sink self, inout slab: SecretSlab) -> Unit eff [ConstTime] {
        let AeadKeySlot { slot_id } = self;
        slab.volatile_zero_and_fence(slot_id);
    }
}
fn verify_tag_16_ct(in expected: ByteArray[16], in computed: ByteArray[16]) -> bool eff [ConstTime] {
    let diff_lo: SecU64 = expected.load_sec_u64le(0).bit_xor(computed.load_sec_u64le(0));
    let diff_hi: SecU64 = expected.load_sec_u64le(8).bit_xor(computed.load_sec_u64le(8));
    diff_lo.bit_or(diff_hi).ct_is_zero().declassify_bool()
}
~~~

## 28.3 Consume a nonce ticket when sealing

Authenticated encryption with associated data (AEAD) authenticates a message and accompanying metadata. A linear nonce ticket represents one permitted sealing operation under a key. The proposed session combines classical and post-quantum key establishment. On decryption, quarantine plaintext until authentication succeeds.

<!-- technical-detail -->

To eliminate `(Key, Nonce)` reuse across retries or concurrent fibers, `AeadSealer[Alg]` owns a monotonic sequence counter accessed through `inout self` in `mint_nonce`, refusing sequence wraparound at `0xFFFF_FFFF_FFFF_FFFE` (`EpochExhaustedRekeyRequired`) and minting a linear `NonceTicket[Alg]` consumed exactly once by `seal_in_place` over the 28-byte `EncryptedFrameWireHeader`. On ingress, `open_authenticated_frame` enforces the cryptographic Doom Principle: if `verify_tag_16_ct` rejects the 16-byte tag, `Vault::volatile_zero_slice(inout payload)` scrubs tentative plaintext in place before raising `AuthenticationTagMismatch` so unauthenticated plaintext never becomes addressable as `VerifiedPlaintextSlice`.

    Linear NonceTicket Seal & Doom-Principle Open Lifecycle:
      Egress: inout AeadSealer (next_seq < 0xFFFF_FFFF_FFFF_FFFE) ──mint_nonce()──► linear NonceTicket[Alg] ──seal_in_place(sink ticket)──► EncryptedFrameWireHeader
      Ingress: sink CiphertextSlice ──decrypt_tentative_ct──► verify_tag_16_ct ──┬─ true  ──► Ok(VerifiedPlaintextSlice)
                                                                                 └─ false ──► volatile_zero_slice(payload) ──► Err(AuthenticationTagMismatch)

~~~vesper
wire struct EncryptedFrameWireHeader {
    key_epoch: u32be,
    nonce_seq: u64be,
    tag: ByteArray[16],
}
linear struct NonceTicket[Alg] { epoch: u32, seq: u64 }
linear struct AeadSealer[Alg] { key_slot: AeadKeySlot[Alg], epoch: u32, next_seq: u64 }
impl[Alg: AeadCipher] AeadSealer[Alg] {
    fn mint_nonce(inout self) -> Result[NonceTicket[Alg], CryptoError] eff [Fail[CryptoError]] {
        if self.next_seq == 0xFFFF_FFFF_FFFF_FFFE {
            Fail::raise(CryptoError::EpochExhaustedRekeyRequired);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        Ok(NonceTicket { epoch: self.epoch, seq })
    }
    fn seal_in_place[P: IsaProfile](
        in self.{key_slot}, in slab: SecretSlab, sink ticket: NonceTicket[Alg],
        in aad: ByteSlice, inout frame_buf: ByteSlice,
    ) -> Result[Unit, CryptoError] eff [ConstTime, Isa[P], Fail[CryptoError]] {
        let NonceTicket { epoch, seq } = ticket;
        let (inout hdr, inout payload) = match frame_buf.split_wire_mut[EncryptedFrameWireHeader]() {
            Ok(parts) => parts,
            Err(_) => Fail::raise(CryptoError::BufferTooSmall),
        };
        hdr.key_epoch = epoch.to_be();
        hdr.nonce_seq = seq.to_be();
        hdr.tag = Alg::encrypt_detached_ct[P](in slab.borrow_key(in self.key_slot), epoch, seq, in aad, inout payload);
        Ok(())
    }
}
fn open_authenticated_frame[Alg: AeadCipher, P: IsaProfile](
    in key_slot: AeadKeySlot[Alg], in slab: SecretSlab, in aad: ByteSlice, sink raw_cipher: CiphertextSlice,
) -> Result[VerifiedPlaintextSlice, CryptoError] eff [ConstTime, Isa[P], Fail[CryptoError]] {
    let mut buf = raw_cipher.into_raw_mut();
    let (in hdr, inout payload) = buf.split_wire_mut[EncryptedFrameWireHeader]()
        .map_err(|_| CryptoError::MalformedHeader)?;
    let computed_tag = Alg::decrypt_tentative_ct[P](
        in slab.borrow_key(in key_slot), hdr.key_epoch.to_native(), hdr.nonce_seq.to_native(), in aad, inout payload,
    );
    if !verify_tag_16_ct(in hdr.tag, in computed_tag) {
        Vault::volatile_zero_slice(inout payload);
        Fail::raise(CryptoError::AuthenticationTagMismatch);
    }
    Ok(VerifiedPlaintextSlice::from_authenticated(payload))
}
~~~

## 28.4 Distinguish correctness from leakage

A functional proof can relate the output to the encryption specification. A leakage proof compares modeled observations from runs with different secrets. Two implementations can produce identical ciphertext while taking different observable branches. Read the leakage model to see which observations it covers.

<!-- technical-detail -->

Functional equivalence and side-channel non-interference are proved separately in `lean/VaultConstantTime.lean`. First, `VaultSpec.verify_tag_16_ct_sound` uses SAT bit-blasting (`bv_decide`) to prove across all `2^256` pairs of 128-bit tags that XOR-folding two 64-bit halves into `((c ||| (0 - c)) >>> 63) == 0` matches `decide (expected = computed)`. Second, `VaultCt.trace_independent` (instantiated by `tagKernel_ct`, alongside manuscript §28.4's `chacha20_seal_constant_time_trace`) models an abstract `LeakageTrace` emitting program counters, memory addresses, divide operands, and branch conditions, proving that `WellTyped` kernels produce identical traces across arbitrary secret inputs of equal public length. Both theorems hold over their Lean models excluding `declassify_bool` and native backend lowering.

    -- lean/VaultConstantTime.lean & §28.4 LeakageTrace non-interference
    theorem verify_tag_16_ct_sound (expected computed : BitVec 128) :
        verifyTag16Ct expected computed = decide (expected = computed) := by
      unfold verifyTag16Ct; bv_decide
    theorem chacha20_seal_constant_time_trace
        (k1 k2 : BitVec 256) (nonce : BitVec 96) (plain1 plain2 : ByteArray)
        (h_len : plain1.size = plain2.size) :
        LeakageTrace (VaultSpec.chacha20Blocks k1 nonce plain1) =
        LeakageTrace (VaultSpec.chacha20Blocks k2 nonce plain2) := by
      simp [VaultSpec.chacha20Blocks, LeakageTrace, h_len]

## 28.5 Inspect lowered instructions for secret dependencies

A taint analysis marks secret-derived values and follows them through intermediate instructions. A marked branch condition or memory index calls for inspection under the timing policy. Validating intermediate representation leaves the final machine code and hardware as additional parts of the timing argument.

<!-- technical-detail -->

When `vesper build --verify-const-time` compiles a `ConstTime` function, `crates/vesper-vault` runs `verify_constant_time_ssa` over `CtAuditedInst` nodes (`PureAlu`, `BitwiseSelect`, `ExplicitDeclassify`, `Load`, `Store`, `VariableLatencyDiv`, `CondBranch`). It propagates `FxHashSet<SsaVal>` taint through `PureAlu` and `BitwiseSelect`, clears taint only at `ExplicitDeclassify`, and returns `ConstantTimeViolation` on any tainted branch (`SecretDependentBranch`), tainted load/store pointer (`SecretDependentMemoryAddress`), or tainted `udiv`/`sdiv` operand (`SecretTaintedVariableLatencyDiv`)—subject to the §28.1 caveats that secret-memory `Load` results from public pointers must also be seeded as tainted and post-regalloc machine instructions still require inspection.

    $ vesper build crypto_service.ves --verify-const-time -o crypto_service
    ----
    // crates/vesper-vault: Stage 5 SSA constant-time taint violations
    pub enum ConstantTimeViolation {
        SecretDependentBranch { cond: SsaVal, inst_span_offset: u32 },
        SecretDependentMemoryAddress { addr: SsaVal },
        SecretTaintedVariableLatencyDiv { operand: SsaVal },
    }
    pub fn verify_constant_time_ssa(
        initial_secret_vals: &[SsaVal], insts: &[CtAuditedInst],
    ) -> Result<(), ConstantTimeViolation> { /* forward taint pass; see §28.1 Load & regalloc caveats */ }

# 29 Aegis confidential computing

A confidential guest can protect its private memory and still receive false data from the host. Aegis describes how the guest validates that input and how a remote party checks evidence about the guest before releasing a secret.

<!-- technical-detail -->

Hardware memory encryption shields a confidential virtual machine's private RAM from an untrusted host hypervisor, yet every shared bounce buffer, hypercall return register, and dynamic page mapping still originates outside the trust boundary. Vesper Aegis quarantines host inputs behind single-fetch region snapshots, tracks guest physical page acceptance linearly, and binds remote attestation evidence to supply-chain and proof digests. Full cryptographic quote verification and platform exception policies remain essential to enforce those boundaries against an active host adversary.

Aegis spans `@vesper-tee/aegis` and `crates/vesper-aegis`—whose quote, report, timestamp, CBOR/CoRIM, CCA token, and policy parsers are ported to Vesper in `crates/vesper-aegis/vesper/` under `crates/vesper-aegis/flip-gate.toml` (`SH6-8` differential and ABBA gates)—alongside `vesper build --tee` and `vesper attest`. Mechanized models live in `lean/AegisToctou.lean` and `lean/AegisCollateral.lean`. Per `REVIEW-STATUS.md` (`SYS-05` and `SYS-01–06`), the 128-byte `AegisAttestationClaimsWire` header carries only an 8-byte `chan_binding_prefix` whereas `verify_attestation_claims` extracts and compares the full authenticated 48-byte `SHA-384` `X25519MLKEM768` transcript binding from hardware quote evidence via `ct_eq`, while Intel TDX `#VE` private-memory policy caveats remain open.

## 29.1 Separate private pages from shared input

Private guest pages and shared transfer buffers have different trust assumptions. Copy host-provided input into private storage, validate that snapshot, and use the validated copy. Rereading shared input after checking it could observe a different value.

<!-- technical-detail -->

Aegis targets three confidential-VM architectures: Intel TDX 1.5+ (`TDG.MEM.PAGE.ACCEPT` dynamic page acceptance and `#VE` virtualization exceptions), AMD SEV-SNP (`PVALIDATE` RMP validation and `VMGEXIT` host exits), and Arm CCA Realms (`RSI_HOST_CALL` realm-service exits and `CCA_TOKEN` Realm Extensible Measurements). Guest fibers, `SecretSlab` keys, and `VirtualArena` scratchpads reside in hardware-encrypted `PrivateEnc` RAM, whereas host-visible VirtIO and SWIOTLB DMA transfers cross an explicit unencrypted aperture leased via `HostBounceGrant` and quarantined scalars `UntrustedHostVal[T]`.

At build time, `vesper build --tee` runs offline measurement (`vesper.tee-measurement/1`) to write `<exe>.tee.json`, hashing the executable with BLAKE3 and computing single-extend values `H(0^len(H) || digest || 0^(pad - 32))` for both 48-byte `SHA-384` TDX `RTMR` registers and 64-byte `SHA-512` Arm CCA `REM` registers without requiring TEE hardware on the build host, while `vesper attest verify` and `vesper attest import` check Ed25519 builder-quorum artifact bundles. Per `SYS-01–06`, TDX guests must still enforce strict `#VE` exception policies (`SEPT_VE_DISABLE` on private GPAs) so a malicious VMM cannot inject `#VE` faults on private memory to emulate MMIO or corrupt guest execution.

    $ vesper build enclave_gate.ves --tee -o enclave_gate   # writes enclave_gate.tee.json (SHA-384 RTMR / SHA-512 REM)
    $ vesper attest verify --expected identity.json --policy builders.json --bundle attestations.json --artifact enclave_gate
    ----
    Untrusted Host / VMM (Shared Unencrypted GPA)          Hardware Trust Boundary          Confidential Guest (Encrypted PrivateEnc GPA)
    ┌───────────────────────────────────────────┐          (#VE / VMGEXIT / RSI)           ┌─────────────────────────────────────────────────┐
    │ SWIOTLB Ring: HostBounceGrant             │ ──1. snapshot_from_host_bounce at t0───► │ Region::scoped (PrivateEnc) + open_auth_frame   │
    │ Hypercall Ret: UntrustedHostVal[u32]      │   (single-fetch blocks VMM TOCTOU!)      │ SecretSlab + NonceTicket + evaluate_policy_simd │
    │ Dynamic GPA:   UnacceptedGpaRange         │ ──TDG.MEM.PAGE.ACCEPT / PVALIDATE──────► │ GpaAcceptanceTracker -> PrivatePageSlab         │
    └───────────────────────────────────────────┘ ◄─2. flush_ciphertext_and_release────────┴─────────────────────────────────────────────────┘

## 29.2 Interpret a grant and an attestation claim

A bounce-buffer grant bounds a host-visible memory transfer. Page acceptance records the transition of a page into guest use. Attestation claims describe the measured guest and its configuration in a common representation. A policy must still authenticate the evidence and verify the expected identity and freshness.

<!-- technical-detail -->

Aegis normalizes TDX, SEV-SNP, and Arm CCA evidence into the 128-byte `AegisAttestationClaimsWire` header paired with linear obligations `HostBounceGrant` (an unencrypted SWIOTLB DMA slot), `UnacceptedGpaRange` (requiring `TDG.MEM.PAGE.ACCEPT` or `PVALIDATE` before private use), and `UntrustedHostVal[T]` (blocking Iago hypercall return spoofing).

Per `REVIEW-STATUS.md` (`SYS-05`), `AegisAttestationClaimsWire` stores only an 8-byte `chan_binding_prefix: ByteArray[8]`, which is a non-authoritative framing prefix too short to prevent collision forgery by itself. When `crates/vesper-aegis` evaluates a quote in `verify_attestation_claims`, it extracts the complete authenticated 48-byte `SHA-384` `X25519MLKEM768` handshake transcript hash (`channel_binding: [u8; 48]`) directly from verified hardware quote `REPORTDATA` or CCA challenge claims and compares all 48 bytes in constant time via `crate::claims::ct_eq`, treating `chan_binding_prefix` solely as an auxiliary header consistency check.

    AegisAttestationClaimsWire (128 B exact) vs Authenticated Quote Evidence (SYS-05):
      [0..2]    tee_arch_tag (u16le: 1=TDX 1.5, 2=SEV-SNP, 3=Arm CCA)   [2..4]   tcb_svn (u16le)   [4..8] enclave_flags (u32le)
      [8..56]   mr_enclave_sha384 (48 B: MRTD / SNP Launch Digest / CCA RIM)
      [56..88]  loom_lock_blake3  (32 B: RTMR[2] hermetic Loom.lock supply-chain digest)
      [88..120] olean_spec_mif    (32 B: RTMR[3] Lean 4 .olean theorem interface digest)
      [120..128] chan_binding_prefix (8 B non-authoritative hint) ──► Authority: 48 B SHA-384(X25519MLKEM768) in Quote REPORTDATA via ct_eq!

~~~vesper
wire struct AegisAttestationClaimsWire {
    tee_arch_tag: u16le,
    tcb_svn: u16le,
    enclave_flags: u32le,
    mr_enclave_sha384: ByteArray[48],
    loom_lock_blake3: ByteArray[32],
    olean_spec_mif: ByteArray[32],
    chan_binding_prefix: ByteArray[8],
}
copy struct UntrustedHostVal[T] { raw_bits: T }
linear struct HostBounceGrant { slot_idx: u32, byte_cap: u32 }
linear struct UnacceptedGpaRange { gpa_base: u64, page_count_4k: u32 }
linear effect TeeHardware {
    fn accept_private_pages(sink pending: UnacceptedGpaRange) -> Result[PrivatePageSlab, TeeError];
    fn snapshot_from_host_bounce[r: Region](
        in bounce: HostBounceGrant, untrusted_len: UntrustedHostVal[u32], in dest_arena: RegionCap[r],
    ) -> Result[RegionSlice[r, u8], TeeError];
    fn attest_channel_binding[r: Region](
        in dest_arena: RegionCap[r], in transcript_hash: ByteArray[48],
    ) -> Result[RegionSlice[r, u8], TeeError];
    fn flush_ciphertext_and_release(sink bounce: HostBounceGrant, in sealed_ciphertext: CiphertextSlice) -> Result[Unit, TeeError];
    fn abort_and_scrub_bounce(sink bounce: HostBounceGrant) -> Unit;
}
~~~

## 29.3 Admit an inference request

The chapter’s inference example stages input, validates it, checks the authorization evidence, then permits computation over private state. The relying-party policy binds `mr_enclave_sha384`, `loom_lock_blake3` in RTMR[2], and `olean_spec_mif` in RTMR[3]. The channel check uses the complete 48-byte SHA-384 binding. Keep host-provided lengths and offsets tied to the validated snapshot.

<!-- technical-detail -->

Remote relying parties admit an enclave session through `RelyingPartyPolicy` by verifying the allowed `TeeArch`, `ENCLAVE_FLAG_DEBUG_DISABLED`, `min_tcb_svn`, the 48-byte hardware boot measurement `mr_enclave_sha384` (`MRTD`, SNP Launch Digest, or CCA `RIM`), the 32-byte `Loom.lock` supply-chain Merkle digest `loom_lock_blake3` in `RTMR[2]`, and the 32-byte Lean 4 `.olean` formal theorem interface digest `olean_spec_mif` in `RTMR[3]`, together with the full 48-byte `X25519MLKEM768` channel binding. Once attested, `process_attested_enclave_req` copies untrusted host bytes once via `TeeHardware::snapshot_from_host_bounce` into a zero-allocation `PrivateEnc` `Region::scoped` arena, authenticates and decrypts the request with `open_authenticated_frame`, runs confidential scoring in encrypted RAM, seals the reply with a linear `NonceTicket`, and guarantees under `[T-Let]` that every error path scrubs and releases `sink bounce_slot` via `TeeHardware::abort_and_scrub_bounce`.

~~~vesper
fn process_attested_enclave_req[Alg: AeadCipher, P: IsaProfile](
    in rx_key: AeadKeySlot[Alg], inout tx_sealer: AeadSealer[Alg], in secret_slab: SecretSlab,
    in aad_transcript: ByteArray[48], sink bounce_slot: HostBounceGrant, host_reported_len: UntrustedHostVal[u32],
) -> Result[Unit, EnclaveError] eff [TeeHardware, ConstTime, Isa[P], Fail[EnclaveError]] {
    Region::scoped(512.kib(), |cap priv_arena| {
        let encrypted_copy = match TeeHardware::snapshot_from_host_bounce(in bounce_slot, host_reported_len, in priv_arena) {
            Ok(slice) => slice,
            Err(tee_err) => {
                TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
                Fail::raise(EnclaveError::HostIagoAttempt(tee_err));
            }
        };
        let verified_plain = match open_authenticated_frame[Alg, P](
            in rx_key, in secret_slab, in aad_transcript.as_slice(),
            sink CiphertextSlice::from_private_region(encrypted_copy),
        ) {
            Ok(pt) => pt,
            Err(crypto_err) => {
                TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
                Fail::raise(EnclaveError::UnauthenticatedPayload(crypto_err));
            }
        };
        let mut reply_buf = priv_arena.alloc_byte_slice(256);
        evaluate_private_policy_simd[P](in verified_plain, inout reply_buf);
        let nonce_ticket = match tx_sealer.mint_nonce() {
            Ok(t) => t,
            Err(e) => {
                TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
                Fail::raise(EnclaveError::RekeyRequired(e));
            }
        };
        if let Err(seal_err) = tx_sealer.seal_in_place[P](
            in secret_slab, sink nonce_ticket, in aad_transcript.as_slice(), inout reply_buf,
        ) {
            TeeHardware::abort_and_scrub_bounce(sink bounce_slot);
            Fail::raise(EnclaveError::SealFailed(seal_err));
        }
        TeeHardware::flush_ciphertext_and_release(
            sink bounce_slot, in CiphertextSlice::from_sealed(reply_buf),
        ).map_err(EnclaveError::HostEgress)
    })
}
~~~

## 29.4 Bind the full claim to the checked request

An attestation binding connects evidence to the specific code, artifacts, or session under review. A display prefix can identify a record for a human, while policy verification needs the complete authenticated binding. A single-fetch proof addresses changes after copying. Validating the content of that first copy remains the guest’s responsibility.

<!-- technical-detail -->

Two Lean 4 developments formalize Aegis's host-boundary and appraisal logic. First, `lean/AegisToctou.lean` models shared host RAM as `HostTimeline := Nat -> Nat -> UInt8`, proving that single-fetch `ingress` at instant `t0` is invariant under arbitrary hypervisor memory mutations at `t > t0` (with an explicit double-fetch counterexample) and proving soundness and completeness of `checkPolicy` across all 128 single-condition failure combinations tested differentially in `crates/vesper-aegis/tests/lean_model.rs`. Second, `lean/AegisCollateral.lean` formalizes Intel TDX's seven-status TCB partial order (`ConfigurationNeeded` incomparable with `OutOfDate`), proves convergence and evaluation-order monotonicity (`evaluate_monotone`, `evaluate_revoked_qe`, `evaluate_revoked_module`, `evaluate_module_first`), proves anti-rollback monotonicity for `TdxCollateralFloor` and `check_snp_crl_floor`, and verifies CoRIM reference-value appraisal. Single-fetch proofs address only post-snapshot TOCTOU races; bounds, AEAD tags, and payload semantics of the initial `t0` copy still require enclave validation.

    -- lean/AegisToctou.lean & lean/AegisCollateral.lean (single-fetch & TCB monotonicity models)
    theorem snapshot_blocks_hypervisor_toctou
        (hostMem1 hostMem2 : Nat -> ByteArray) (t0 : Nat) (len : UInt32)
        (h_agree_at_t0 : hostMem1 t0 = hostMem2 t0) :
        EnclaveSpec.evalAfterSnapshot (hostMem1 t0) len =
        EnclaveSpec.evalAfterSnapshot (hostMem2 t0) len := by rw [h_agree_at_t0]
    -- TDX module -> platform -> QE convergence monotonicity & anti-rollback floor:
    -- AegisCollateral.evaluate_monotone, AegisCollateral.evaluate_revoked_module

## 29.5 Check policy and page transitions separately

The attestation policy verifier evaluates claims against expected measurements. The guest physical address tracker accounts for accepted pages and allowed state changes. A valid quote does not make a malformed memory range valid. Check the guest address range and its current state before accepting the pages.

<!-- technical-detail -->

In `crates/vesper-aegis` (whose quote, CBOR/CoRIM, CCA token, and policy modules run in Vesper under `flip-gate.toml` lane `SH6-8`, differentially tested against 200,000 random claims/policy pairs and 20,000 CCA claim sets), `verify_attestation_claims` enforces `allowed_archs`, `ENCLAVE_FLAG_DEBUG_DISABLED`, `min_tcb_svn`, `mr_enclave_sha384`, `loom_lock_blake3`, `olean_spec_mif`, and constant-time equality `crate::claims::ct_eq(&claims.channel_binding, expected_chan_binding)` over the full 48-byte authenticated transcript binding. Independently, `GpaAcceptanceTracker::accept_gpa_range` tracks 4 KiB private page frame numbers (`gpa_base >> 12`), rejecting zero-sized or unaligned ranges (`UnalignedOrZeroGpaRange`) and hypervisor double-accept replay attempts (`HypervisorGpaDoubleAccept`).

    // crates/vesper-aegis: full 48-byte channel-binding policy check & GPA replay tracker
    pub fn verify_attestation_claims(
        claims: &AttestationClaims, policy: &RelyingPartyPolicy, expected_chan_binding: &[u8; 48],
    ) -> Result<(), AegisVerifyError> {
        // ... arch, ENCLAVE_FLAG_DEBUG_DISABLED, min_tcb_svn, mr_enclave_sha384,
        //     loom_lock_blake3 (RTMR[2]), and olean_spec_mif (RTMR[3]) checks ...
        if !crate::claims::ct_eq(&claims.channel_binding, expected_chan_binding)
            || claims.chan_binding_prefix != expected_chan_binding[..8] {
            return Err(AegisVerifyError::ChannelTranscriptBindingMismatch);
        }
        Ok(())
    }
    pub struct GpaAcceptanceTracker { accepted_4k_pfns: FxHashSet<u64> } // blocks HypervisorGpaDoubleAccept
