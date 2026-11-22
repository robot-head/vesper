# 25 Silicon device registers and DMA

A driver shares memory with a device that can keep running after a CPU function returns. Silicon uses permissions and explicit state transitions to describe when the CPU may read, write, or release a device buffer. This matters for direct memory access (DMA), where the device reads or writes memory itself.

## 25.1 Specify the hardware available to a driver

A freestanding program may have only a fixed amount of static RAM (SRAM), interrupt handlers, and device registers. It may have no heap or operating system. Function effects describe which facilities it needs. Static stack budgeting matters because an interrupt can add a frame while another frame is still active.

## 25.2 Respect register semantics and device ownership

A write-one-to-clear (W1C) register clears a flag when software writes a one to the corresponding bit. An ordinary read-modify-write sequence can acknowledge unrelated flags. Generated register types distinguish permitted operations. Interrupt guards record release order. A DMA transfer changes who may access the buffer and may require cache maintenance.

Consider a two-bit status register containing only W1C flags. Both flags are pending. To acknowledge the low bit, write the low-bit mask.

    Pending before      11
    Acknowledgment      01
    Pending afterward   10

Writing back the entire value read from the register would clear both flags. The generated interface uses the register’s access semantics to select the write operation. Its address alone cannot tell the driver which bits to write.

## 25.3 Follow a frame through a page mapping

A physical frame token represents the right to manage a page. Mapping the page transfers that responsibility into a mapping, and unmapping it must recover the appropriate frame. In a driver, the analogous handoff is from a CPU-owned descriptor to the device and back on completion. Track the identity of the transferred page or descriptor until the corresponding unmap or completion.

## 25.4 Prove a register or frame property

For a W1C model, check that acknowledging one flag leaves unrelated flags alone. For frame management, count identities before and after each transition to detect duplication and loss. The peripheral’s real register semantics and cache-coherence behavior still need to match the model.

## 25.5 Bound stack use along call paths

A stack-depth analysis combines the frames that can be active together. Interrupt nesting and indirect calls need bounds too. A register verifier can separately check a requested operation against the peripheral description. A small stack bound says nothing about whether a register write has the intended hardware effect.

# 26 eBPF packet programs and verifier checks

An extended Berkeley Packet Filter (eBPF) program runs under the kernel’s verifier. Vesper’s proposed pre-flight checks examine packet bounds and resource lifetimes before submission. The target kernel decides whether to admit the program.

## 26.1 Track the kernel’s constraints

A kernel program has limits on stack use, loops, helper calls, and the references those helpers return. A packet view must stay inside the current packet. Kernel type metadata and relocations let a program refer to fields across supported kernel layouts. A pre-flight check can flag an unbounded loop before the program reaches the target kernel.

## 26.2 Reserve and finish a ring slot

A ring-buffer reservation represents exclusive space for an event. Every path after a successful reservation must submit or discard the slot. Before decoding a header with a wire view, establish that the packet contains the entire header. Even when the packet is too short to decode, any slot already reserved still needs a submit or discard.

## 26.3 Pass a decision to userspace

In the packet-filter example, an Express Data Path (XDP) hook examines traffic near the start of the receive path. It can record a compact event for a userspace consumer through the ring. Producer and consumer agree on the wire layout. A short packet needs a defined outcome even when the requested header is absent.

## 26.4 Scope a filter proof

One modeled property is that a selected policy classifies packets according to its rule. A separate check establishes bounds for the inspected fields. Both depend on the packet representation and the policy assumptions. The kernel decides whether it can safely run the program. To verify the filtering decision, check it against the intended policy.

## 26.5 Approximate possible machine states

An abstract verifier tracks facts such as “this register points into a packet” or “this offset is in this range.” It checks both successors of a branch and joins their facts. If one path lacks a sufficient bound, a safe pre-flight analysis must decline to certify the access.

# 27 Aether virtual machines and snapshots

Cloning a paused machine also clones its memory and unfinished state. Two copies can therefore begin with the same random generator, device state, and pending identifiers. Aether treats resuming a clone as an explicit transition.

## 27.1 Decide what a snapshot can share

A unikernel packages an application with the small set of services it needs. A guest may share immutable backing pages across clones while keeping private writes separate. The snapshot contract must account for network sessions, device work, and entropy as well as RAM.

## 27.2 Own the descriptor and the resume ticket

PVH describes a boot interface. VirtIO queues describe buffers passed between guest and virtual device. A descriptor lease records which side may reuse a queue entry. A separate resume ticket records the steps a restored clone must complete before performing identity-sensitive work.

## 27.3 Quiesce, clone, and reseed

Quiescing brings outstanding operations to a defined boundary. Restoring a clone requires fresh entropy and a policy for old transport state. A lazy page resolver may supply missing pages from a content-addressed snapshot on demand. Restore or retire the saved transport state before letting either clone send on that session.

## 27.4 State the isolation and uniqueness assumptions

A model of extended page tables (EPT) can show that two guest mappings obey the stated separation policy. A nonce uniqueness argument needs independent post-fork identities and correct reseeding. Duplicating a snapshot without changing its entropy state violates that premise.

## 27.5 Validate mappings and resolve missing pages

The host checks which guest ranges may back each mapping. A userfaultfd (UFFD) resolver handles a missing-page request by finding the expected content and installing it under the proper sharing policy. Page identity, guest identity, and write permissions all matter at installation.

# 28 Vault secrets, timing, and nonces

A key must stay out of public outputs, including logs. Encryption also needs a fresh nonce for each use under the same key, and secret-dependent operations can reveal information through timing. Vault treats each as a distinct obligation.

## 28.1 Track a secret beyond its type

Secret-marked data should stay out of public outputs, including logs. Temporary copies, allocator behavior, and generated instructions also influence exposure. The chapter presents constant-time execution and zeroization as design goals whose end-to-end behavior requires target-specific evidence.

## 28.2 Bound observable behavior

A constant-time scope restricts operations whose control flow or memory addresses depend on a secret. Data-independent timing (DIT) features have a hardware-defined scope. Fenced zeroization is intended to keep a cleanup write from disappearing during optimization. Copies in other buffers, registers, or spill slots require separate accounting.

## 28.3 Consume a nonce ticket when sealing

Authenticated encryption with associated data (AEAD) authenticates a message and accompanying metadata. A linear nonce ticket represents one permitted sealing operation under a key. The proposed session combines classical and post-quantum key establishment. On decryption, quarantine plaintext until authentication succeeds.

## 28.4 Distinguish correctness from leakage

A functional proof can relate the output to the encryption specification. A leakage proof compares modeled observations from runs with different secrets. Two implementations can produce identical ciphertext while taking different observable branches. Read the leakage model to see which observations it covers.

## 28.5 Inspect lowered instructions for secret dependencies

A taint analysis marks secret-derived values and follows them through intermediate instructions. A marked branch condition or memory index calls for inspection under the timing policy. Validating intermediate representation leaves the final machine code and hardware as additional parts of the timing argument.

# 29 Aegis confidential computing

A confidential guest can protect its private memory and still receive false data from the host. Aegis describes how the guest validates that input and how a remote party checks evidence about the guest before releasing a secret.

## 29.1 Separate private pages from shared input

Private guest pages and shared transfer buffers have different trust assumptions. Copy host-provided input into private storage, validate that snapshot, and use the validated copy. Rereading shared input after checking it could observe a different value.

## 29.2 Interpret a grant and an attestation claim

A bounce-buffer grant bounds a host-visible memory transfer. Page acceptance records the transition of a page into guest use. Attestation claims describe the measured guest and its configuration in a common representation. A policy must still authenticate the evidence and verify the expected identity and freshness.

## 29.3 Admit an inference request

The chapter’s inference example stages input, validates it, checks the authorization evidence, then permits computation over private state. A model file’s digest can be included in the policy so that changing the model changes the admission decision. Keep host-provided lengths and offsets tied to the validated snapshot.

## 29.4 Bind the full claim to the checked request

An attestation binding connects evidence to the specific code, artifacts, or session under review. A display prefix can identify a record for a human, while policy verification needs the complete authenticated binding. A single-fetch proof addresses changes after copying. Validating the content of that first copy remains the guest’s responsibility.

## 29.5 Check policy and page transitions separately

The attestation policy verifier evaluates claims against expected measurements. The guest physical address tracker accounts for accepted pages and allowed state changes. A valid quote does not make a malformed memory range valid. Check the guest address range and its current state before accepting the pages.
