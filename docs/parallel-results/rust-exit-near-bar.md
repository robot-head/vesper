# Rust exit: near-bar library flips

Claim #1873 (part of #1426, #1405, #1399). Re-measures the libraries that
sat just above the 1.50x flip bar after the Wave 23 codegen work, and flips
each one whose Vesper port passes: full function, 0 differential diffs, and
the upper end of the 95% interval of the median Vesper/Rust ratio at or
below 1.50x.

Host: this cloud container (4 cores, shared). Release builds with
`RUSTFLAGS="-C target-cpu=native"`, 30 ABBA epochs per workload, the
distribution-free interval x(10)..x(21) of the median.

## Part 1: ebpf `.BTF.ext` readers and beacon spans

| Crate | Workload | Compiler | Median | 95% interval | Verdict |
|---|---|---|---:|---|---|
| `vesper-ebpf` | `ext` | #1866 (Wave 24) | 1.23x | 1.17..1.24 | pass |
| `vesper-ebpf` | `ext` | `main` `b6611c8` | 1.22x | 1.17..1.22 | pass |
| `vesper-ebpf` | `ext` (repeat) | `main` `b6611c8` | 1.19x | 1.16..1.22 | pass |
| `vesper-beacon` | `spans` | #1866 (Wave 24) | 1.35x | (not printed then) | |
| `vesper-beacon` | `spans` | `main` `b6611c8` | 1.35x | 1.33..1.38 | pass |
| `vesper-ebpf` | `decode` | #1866 (Wave 24) | 1.65x | 1.62..1.70 | fail |
| `vesper-warp-wheel` | insert/advance/drain | #1866 (Wave 24) | 1.54x | 1.52..1.55 | fail |

The census's older `ext` 1.68x and `spans` 1.61x figures were from slower
hosts or older compilers; on this host both have been under the bar since
the part 1 speed-ups of #1441 and the #1426 waves.

Differentials: `tests/ext_port.rs` (`parse_ext`, `local_type` and
`maps_from_btf` against the frozen seed) and every `vesper-ebpf` test file
pass in both builds; `tests/behavior.rs` and every `vesper-beacon` test
file pass in both builds. 0 diffs.

What moved:

- `vesper-ebpf`: `btf_ext::parse_ext`, `local_type` and `maps_from_btf` run
  the Vesper port in every build. Their Rust bodies and helpers (271 lines)
  leave `src/btf_ext.rs`; the frozen copy is
  `seed/src/engine/btf_ext.rs`, which the compiler links until Stage 2.
  The `.BTF.ext` writer (`build`) stays Rust with the BTF builder.
- `vesper-beacon`: `SpanPool`, `TelemetryRing` and the drain iterator run
  the Vesper port in every build. The Rust bodies (227 lines) leave
  `src/spans.rs`; the frozen copy is `seed/src/engine/spans.rs`.
  `tests/abba.rs` now prints the 95% interval and the flip verdict, as the
  eBPF harness does.

Rust lines deleted from the crates' own sources: 498. The seed copies
(1,367 lines) go with the Stage 2 retirement (#1397).

## Still above the bar

- `vesper-ebpf` `decode` (1.65x): one export crossing per instruction and
  the enum match lowering (gaps 3 and 4 of rust-exit-silicon-ebpf.md).
- `vesper-warp-wheel` (1.54x, 1.52..1.55): bounds-checked loads from seven
  parallel sequences in `open_tick`, `place` and `next_work_tick`.
