# Fifth parallel implementation wave: every unstarted section and the Warp gaps

Status: **ready to assign**. One thread per lane; do not launch duplicate writers.
Each linked file is a complete standalone prompt. All fourteen lanes can run
concurrently under the [wave-five contracts](../../rfcs/wave-five-contracts.md):
every lane owns a new crate (or, for lanes 13 and 14, one existing crate that no
other lane writes) and depends only on crates already on `main`. Start from a
fresh `origin/main` containing **`af03122ea74d403f203170926945f9d35786b289`** or later.

Source: the design snapshot in [specification.txt](../../specification.txt)
(Google Doc revision of 2026-11-12 09:18 UTC, 41 numbered sections plus
Appendix A). At the baseline ten sections had no code at all: §23, §25, §26,
§27, §29, §30, §31, §32, §33 and §36. The Warp integration (PR #101) left three
recorded gaps. The [section status review](../../rfcs/wave-five-review.md)
lists what remains in the sections that already have code.

| Lane | Copyable prompt | Claim | Exclusive boundary | Coverage |
| --- | --- | --- | --- | --- |
| 1 | [Vesper Vault: constant-time verifier, secrets and nonce-safe AEAD](01-vault.md) | #121 | New `crates/vesper-vault/**`, `lean/VaultConstantTime.lean` | 23.1–23.5 |
| 2 | [Vesper Chorus: choreographies, session channels and quorum replication](02-chorus.md) | #122 | New `crates/vesper-chorus/**`, `lean/ChorusQuorum.lean` | 25.1–25.5 |
| 3 | [Vesper Silicon: stack bounds, typestate MMIO, IrqGuard and DMA conservation](03-silicon.md) | #123 | New `crates/vesper-silicon/**`, `lean/SiliconFrames.lean` | 26.1–26.5 |
| 4 | [Vesper eBPF: pre-flight verifier and XDP programs](04-ebpf.md) | #124 | New `crates/vesper-ebpf/**`, `lean/EbpfFirewall.lean` | 27.1–27.5 |
| 5 | [Vesper Crucible: ChoiceTape fuzzing and obligation-safe shrinking](05-crucible.md) | #125 | New `crates/vesper-crucible/**`, `lean/CrucibleShrink.lean` | 29.1, 29.2, 29.4, 29.5 shrinker |
| 6 | [Vesper Nexus: WIT resources, fuel and shared plugin rings](06-nexus.md) | #126 | New `crates/vesper-nexus/**`, `lean/NexusHandles.lean` | 30.1–30.5 |
| 7 | [Vesper Chrono: flight recording, reverse watchpoints and obligation lineage](07-chrono.md) | #127 | New `crates/vesper-chrono/**`, `lean/ChronoReplay.lean` | 31.1–31.5 |
| 8 | [Vesper Resonance: realtime confinement, PDC scheduling and SPSC retirement](08-resonance.md) | #128 | New `crates/vesper-resonance/**`, `lean/ResonanceSpsc.lean` | 32.1–32.5 |
| 9 | [Vesper Aegis: attestation policy, page acceptance and single-fetch host values](09-aegis.md) | #129 | New `crates/vesper-aegis/**`, `lean/AegisToctou.lean` | 33.1–33.5 |
| 10 | [Vesper Kinetic: DetFloat auditing and deterministic XPBD](10-kinetic.md) | #130 | New `crates/vesper-kinetic/**`, `lean/KineticColoring.lean` | 36.1–36.5 |
| 11 | [vesper migrate: C/Rust signature lifting and differential shadowing](11-migrate.md) | #131 | New `crates/vesper-migrate/**` | 29.3, 29.5 lifter |
| 12 | [Warp QUIC TLS 1.3 handshake](12-warp-tls.md) | #132 | New `crates/vesper-warp-tls/**` | 35 handshake gap |
| 13 | [Warp QUIC streams, retransmission and flow control](13-warp-streams.md) | #133 | `crates/vesper-warp/**` (exclusive during wave five) | 35 stream gap |
| 14 | [Source XdpNic simulation on the Warp software NIC](14-sim-softnic.md) | #134 | `crates/vesper-sim-session/**` | 35 simulation gap |

Each lane commits its own root registration patch (members, default-members,
new workspace dependencies, lockfile) onto its own PR branch, as the user
approved on 2026-11-12. Every new-crate lane touches the root member lists and
`Cargo.lock`, so later PRs rebase and regenerate the lockfile after earlier
ones merge; that is expected, not a conflict between lanes' code.

Use a separate checkout and build target per lane. Lean files are checked from
each crate's own tests under `VESPER_LEAN=lean`, so no CI workflow edits are
needed.

## Not in this wave

Work other agents hold: §16.4 source macros and Forge authority (#103–#110) and
§39 formatter work (#111–#120). Lanes implement host libraries where their
section calls for a Forge macro and leave the macro itself to a later wave.

## After this wave: integration (not parallel lanes)

- **A. Warp handshake (#135)**, once lanes 12 and 13 merge: `WarpEndpoint`
  sends and accepts Initial/Handshake long-header packets, runs the lane-12
  handshake over the lane-13 CRYPTO buffers, installs and discards keys per
  space, and an end-to-end test opens a connection with no caller-supplied
  secrets and transfers data over a lossy link.
- **B. CLI and compiler hooks (#136)**, once lanes 1–11 merge: root wiring
  for `vesper fuzz`, `vesper verify --refute`, `vesper migrate`,
  `vesper replay` and `vesper ebpf check`, and running the Vault, Silicon,
  Resonance and Kinetic verifiers on the Cranelift functions `src/native.rs`
  emits, behind explicit flags. It also merges the lane-3 and lane-4 stack
  analyzers.

## Dependencies between lanes

None during the wave. Integration A depends on lanes 12 and 13; integration B
depends on lanes 1–11. Lane 14 is independent of lanes 12 and 13.
