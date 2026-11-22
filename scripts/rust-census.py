#!/usr/bin/env python3
"""Rust census for the Rust-exit program (docs/maintenance/rust-exit-plan.md).

Counts the non-test Rust lines of every workspace crate and of every module of
the root compiler, joins them with the hand-kept STATUS tables below, and
prints docs/maintenance/rust-census.md to stdout.

    python3 scripts/rust-census.py > docs/maintenance/rust-census.md

Non-test lines: non-blank, non-comment lines outside `#[cfg(test)]` items,
skipping `tests.rs`, `tests/`, `benches/` and `examples/`. Update STATUS when a
lane reports (the scoreboard lane, SH6 lane 1, owns this file).
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SKIP_DIRS = {"target", "tests", "benches", "examples"}

# Classes, in the order the census lists them.
CLASSES = {
    "gone": "Rust gone: the Vesper port is the only build (a frozen seed may remain until Stage 2)",
    "flip": "At the bar (full function, <= 1.5x Rust): flip to Vesper and delete the Rust now",
    "partial": "Port exists but is unfinished or above 1.5x: finish, speed up, then flip",
    "compiler": "In the compiler: ported module by module into the hybrid; Rust deleted at the Stage 2 retirement",
    "unported": "No Vesper port yet: port it",
    "c-api": "Rust wrapper over a C or OS API: call the C API or syscalls straight from Vesper `.vsys`, no Rust left",
    "rust-lib": "Wraps a Rust-only library that Vesper can replace: port the library's job to Vesper",
    "shim": "Irreducible: thin C-ABI shim over a vetted Rust-only library with no C equivalent",
    "diverse": "Kept Rust on purpose as a diverse implementation (owner call)",
    "infra": "Rust-vs-Vesper port infrastructure: deleted when the last Rust build it compares goes",
}

# crate -> (class, Vesper port, last measured speed, what is left, SH6 lane)
CRATES = {
    "vesper-aura": ("gone", "in crate", "0.29-0.53x", "", "-"),
    "vesper-pact": ("gone", "in crate (+ seed)", "1.10x / 0.33x", "seed until Stage 2", "12"),
    "vesper-simpulse": ("gone", "in crate (+ seed)", "0.29-1.00x", "seed until Stage 2", "-"),
    "vesper-loom-solver": ("gone", "in crate (+ seed)", "0.21-1.27x", "seed until Stage 2", "-"),
    "vesper-chorus": ("partial", "in crate", "0.70x parse+project", "front end Vesper-only; simulated network, Raft driver", "12"),
    "vesper-warp": ("partial", "in crate", "1.08-5.39x", "recovery 5.4x, frames 2.3x; crypto via warp-crypto shim", "4"),
    "vesper-warp-tcp": ("partial", "in crate", "codec 0.72x Ir, transfer 1.47x Ir", "harness/simulator still Rust", "4"),
    "vesper-warp-bbr": ("partial", "in crate", "2.2-2.9x", "on_event dispatch Rust", "4"),
    "vesper-warp-wheel": ("partial", "in crate", "1.73-1.77x", "speed only", "4"),
    "vesper-prism": ("gone", "in crate", "0.96-1.34x", "", "5"),
    "vesper-prism-h3": ("partial", "in crate", "0.99-1.36x", "protocol core Vesper-only; connection, client, session drivers and Warp adapter still Rust", "5"),
    "vesper-quarry": ("partial", "in crate", "1.80x", "buffer pool, executor, online DDL", "6"),
    "vesper-flux": ("partial", "in crate", "unmeasured", "CDC, Kafka wire, durable state, cluster driver", "6"),
    "vesper-ignis": ("partial", "in crate", "0.68-3.1x", "build 2.5x, lower 2.7-3.1x", "7"),
    "vesper-grammar": ("partial", "in crate", "4.7-6.3x", "speed", "7"),
    "vesper-bench": ("partial", "in crate", "2.2-3.2x", "timers via bench-pmu", "7"),
    "vesper-aegis": ("partial", "in crate", "unmeasured", "X.509, webpki, ECDSA stay behind the crypto shim; rest ports", "8"),
    "vesper-kinetic": ("partial", "in crate", "1.83x+", "detfloat, simd, threaded world", "9"),
    "vesper-resonance": ("partial", "in crate", "5.0-5.3x", "realtime rings, f32 kernels", "9"),
    "vesper-silicon": ("partial", "in crate", "unmeasured", "SVD model, stack/prologue/hazard analysis (Cranelift IR), mmio", "10"),
    "vesper-ebpf": ("partial", "in crate", "1.2-4.7x", "BTF writers, ELF, bpf() syscalls", "10"),
    "vesper-serde": ("partial", "in crate", "4.8-5.7x", "SIMD stage 1 needs source vectors", "11"),
    "vesper-tensor": ("partial", "in crate", "unmeasured", "safetensors, memmap, simd kernels", "11"),
    "vesper-alloc": ("partial", "in crate", "unmeasured", "cache B-tree, arena keys, OS pages", "11"),
    "vesper-crucible": ("partial", "in crate", "unmeasured", "fuzzer, schedule explorers, DPOR, arena", "12"),
    "vesper-semver": ("partial", "in crate", "2.2-3.0x", "speed: the Rust encoder hash-conses every node", "12"),
    "vesper-loom-calculus": ("gone", "in crate (+ seed)", "1.12x @8 / 0.80x @64 pkgs", "seed until Stage 2; TOML, JSON and SemVer text readers (#965)", "12"),
    "vesper-loom-audit": ("gone", "in crate (+ seed)", "1.37x @8 / 1.15x @64 / 0.96x @256 pkgs", "seed until Stage 2; size checks and BLAKE3 digests (vesper-blake3-sys)", "12"),
    "vesper-beacon": ("partial", "in crate", "2.1-7.1x", "speed", "12"),
    "vesper-sim-session": ("partial", "in crate", "unmeasured", "umem, warp, JSON, driver (SH5 lane 19)", "SH5-19"),
    # The compiler's own crates.
    "vesper-syntax": ("compiler", "packages/vesper-front", "parse swap 1.03x corpus", "editor mode, lossless CST users", "SH1/2"),
    "vesper-core": ("compiler", "packages/vesper-ast", "in parse swap", "", "SH1/2"),
    "vesper-span": ("compiler", "in crate", "pilot", "", "2"),
    "vesper-diagnostics": ("compiler", "in crate", "pilot", "", "2"),
    "vesper-lean-syntax": ("compiler", "in crate", "pilot", "", "2"),
    "vesper-fmt": ("compiler", "in crate", "unmeasured", "", "2"),
    "vesper-lint": ("compiler", "in crate", "unmeasured", "", "2"),
    "vesper-highlight": ("partial", "in crate (lexical)", "unmeasured", "out of the compiler's closure (#1398): no seed needed; rasterizer: tiny-skia, rustybuzz, png, ttf-parser", "15"),
    "vesper-forge": ("compiler", "in crate", "unmeasured", "macro expansion #1157", "SH1-14"),
    "vesper-doc": ("compiler", "packages (SH5-15)", "0.76-0.87x", "renderers part 2", "SH5-15"),
    "vesper-lsp": ("compiler", "SH5-16", "unmeasured", "", "SH5-16"),
    "vesper-test": ("compiler", "SH5-14", "unmeasured", "Anvil DTOs and reporters", "SH5-14"),
    "vesper-pact-binding": ("compiler", "none (goes away)", "", "DTOs vanish once the checker calls @vesper/pact (#1185)", "SH2-13"),
    "vesper-pact-bridge": ("compiler", "none (goes away)", "", "same as pact-binding", "SH2-13"),
    "vesper-tools": ("compiler", "SH5-13", "", "tool binaries outside the compiler's closure (SH0-2, SH6-3)", "SH5-13, 3"),
    "vesper-hybrid": ("infra", "", "", "becomes the default compiler, then is deleted at the retirement", "2"),
    "vesper-codegen-swap-test": ("infra", "", "", "", "2"),
    "vesper-codegen-numeric-test": ("infra", "", "", "", "2"),
    "vesper-port-abi": ("infra", "", "", "", "2"),
    "vesper-port-harness": ("infra", "", "", "", "2"),
    "vesper-port-build": ("infra", "", "", "", "2"),
    "vesper-port-pilot": ("infra", "", "", "", "2"),
    "vesper-std-driver": ("infra", "", "", "sample driver", "13"),
    # Was "stays Rust" in the port survey. Re-sorted for the Rust-exit goal.
    "vesper-std-host": ("c-api", "", "", "libc fs/process/env: import libc directly", "13"),
    "vesper-pulse": ("c-api", "", "", "io_uring/epoll/kqueue/IOCP syscalls; scheduler ports", "13"),
    "vesper-fiber-abi": ("c-api", "", "", "stack switching asm (assembly files or Vesper asm), unwinding; Cranelift JIT via the shim", "13"),
    "vesper-ffi": ("c-api", "", "", "Bridge runtime and layouts; BLAKE3 C", "13"),
    "vesper-chrono": ("c-api", "", "", "perf/ptrace syscalls; iced-x86 disassembler (port or Capstone C)", "14"),
    "vesper-bench-pmu": ("c-api", "", "", "perf_event_open", "14"),
    "vesper-warp-umem": ("c-api", "", "", "AF_XDP rings, mmap", "14"),
    "vesper-aether": ("c-api", "", "", "KVM ioctls, userfaultfd", "14"),
    "vesper-warp-shard": ("unported", "", "", "SIMD CID lookup: source vectors", "4"),
    "vesper-aura-gtk": ("c-api", "", "", "gtk4-rs: call the GTK 4 C API", "15"),
    "vesper-ignis-vulkan": ("c-api", "", "", "ash: call the Vulkan C API; naga via shim", "15"),
    "vesper-gpu-compute": ("c-api", "", "", "ash: Vulkan C API; naga via shim", "15"),
    "vesper-loom-cas": ("c-api", "codecs + store (non-default)", "1.0-3.5x", "ingest/materialize/verify_all, codec speed, fs calls in shim", "16"),
    "vesper-loom-admission": ("c-api", "", "", "out of the compiler's closure (#1398); glue over CAS + trust", "16"),
    "vesper-loom-build": ("unported", "", "", "sandbox driver", "16"),
    "vesper-loom-sandbox": ("c-api", "", "", "wasmtime: use the wasmtime C API", "16"),
    "vesper-nexus": ("c-api", "", "", "wasmtime C API; wit-parser ports (SH5-11 has a WIT reader)", "16"),
    "vesper-forge-runner": ("c-api", "", "", "wasmtime C API", "16"),
    "vesper-loom-tuf": ("rust-lib", "", "", "tough/tokio: TUF metadata on the Vesper JSON port + crypto shim", "16"),
    "vesper-fusion": ("rust-lib", "", "", "egglog: move rule sets onto @vesper/egraph", "18"),
    "vesper-beacon-arrow": ("rust-lib", "", "", "arrow-rs, tonic, prost: Arrow IPC and gRPC on Prism", "18"),
    "vesper-migrate": ("rust-lib", "", "", "out of the compiler's closure (#1398); lang-c, syn: C and Rust readers in Vesper", "18"),
    "vesper-loom-trust": ("shim", "", "", "out of the compiler's closure (#1398); Ed25519 quorum: logic ports, ed25519 behind the crypto shim", "17"),
    "vesper-warp-crypto": ("shim", "", "", "RustCrypto AEADs: one crypto C-ABI shim", "17"),
    "vesper-vault": ("shim", "", "", "libcrux ML-KEM, x25519, AEADs: crypto shim; Cranelift via its shim", "17"),
    "vesper-warp-tls": ("shim", "", "", "rustls: TLS shim (or the TLS 1.3 state machine ports over the crypto shim)", "17"),
    "vesper-simd": ("c-api", "", "", "out of the compiler's closure (#1398); Cranelift JIT host: through vesper-cranelift-sys", "13"),
    "vesper-crypto-sys": ("shim", "", "", "vetted crypto C-ABI shim (SH6 lane 17)", "17"),
    "vesper-blake3-sys": ("c-api", "", "", "BLAKE3 C library: import it straight from .vsys", "G1-12"),
    "vesper-cranelift-sys": ("shim", "", "", "Cranelift C-ABI shim (owner 2026-11-19)", "-"),
    "vesper-codegen-verified": ("diverse", "", "", "DDC auditor, diverse on purpose", "owner"),
}

# Root compiler module -> (owner lane(s), Vesper package, status note)
MODULES = {
    "bin": ("owner (`diverse`)", "", "test oracles (*42), diverse on purpose: exempt from the flip gate; moved out of `src/` before the retirement"),
    "verified_codegen": ("SH6b", "", "Stage 1 guard; port with the verified-backend breadth track"),
    "lean": ("SH5-18", "vesper-lean-extract", "part 3a 2.37x"),
    "lean_ir": ("SH5-18", "vesper-lean-extract", ""),
    "lean_nodes": ("SH5-18", "vesper-lean-extract", ""),
    "verify_commands": ("SH5-18", "", ""),
    "proof_commands": ("SH5-18", "", ""),
    "native": ("G1-11", "vesper-codegen-base", "family hooks swapped by SH4; the walk itself ports in gap wave 1 lane 11"),
    "seq": ("SH4-9", "", ""),
    "wasm_backend": ("SH4-15, SH5-11", "", "0 diffs (#1311)"),
    "check": ("SH2-7", "vesper-check-core-typing", ""),
    "tooling": ("SH5-15, SH5-17", "", ""),
    "lsp": ("SH5-16, SH5-17", "", ""),
    "tensor_types": ("SH2-14", "vesper-check-tensor-types", "0.28-0.49x"),
    "pact_frontend": ("SH2-13", "", "#1225 part 1"),
    "forge_frontend": ("SH1-14", "", "#1157"),
    "native_export": ("SH4-12", "vesper-codegen-export", ""),
    "ffi_lowering": ("SH4-13", "", ""),
    "optimize": ("G1-4", "vesper-optimize", "search 0.68x; checker 1.94x"),
    "fip": ("SH2-12", "vesper-check-fip", "0.92x"),
    "bounds_hoist": ("SH3-12", "vesper-bounds-hoist", ""),
    "tiled": ("SH4-11", "vesper-codegen-numeric", ""),
    "capability_effects": ("SH2-8", "vesper-check-effects", "0.28x"),
    "test_runner": ("SH5-14", "vesper-test-runner", "1.69x"),
    "ebpf_backend": ("SH4-16", "vesper-ebpf-backend", "0.93-1.10x with verifier"),
    "regions": ("SH2-9", "vesper-check-regions-seqs", "1.12-1.17x"),
    "trace_effects": ("SH2-8", "vesper-check-effects", ""),
    "freestanding": ("SH4-14", "", ""),
    "native_handlers": ("SH4-10", "", ""),
    "packages": ("SH2-6", "vesper-check-names-packages", "0.72-0.87x wall"),
    "vector": ("SH4-11", "vesper-codegen-numeric", ""),
    "native_aggregates": ("SH4-8", "", ""),
    "ir_inline": ("SH3-11", "vesper-ir-inline", "0.76x, swapped"),
    "anvil_jit": ("SH5-14", "vesper-test-runner", ""),
    "integer_widths": ("SH2-7", "vesper-check-core-typing", ""),
    "loops": ("SH2-10", "vesper-check-loops-atomics", ""),
    "ir": ("SH3-13", "vesper-place-ir-lower", "0.40x"),
    "doctest": ("SH5-14", "", ""),
    "native_session": ("SH4-10", "", ""),
    "runtime": ("SH4-17", "vesper-backend-checks", ""),
    "dyn_trait": ("SH2-11", "vesper-check-dyn", "0.28x"),
    "dyn_fn": ("SH2-11", "vesper-check-dyn", ""),
    "strings": ("SH2-9", "vesper-check-regions-seqs", ""),
    "native_verifiers": ("SH4-17, 3", "vesper-backend-checks", "links kinetic/silicon/resonance seeds"),
    "ir_check": ("SH2-5", "vesper-ir-check", "0.53x, swapped"),
    "native_coverage": ("SH4-17, 3", "", "links the crucible seed; only `vesper fuzz` (a tool) uses it"),
    "main": ("SH5-12", "vesper-driver", "#1367"),
    "arrays": ("SH2-9", "", ""),
    "anvil_hash": ("SH5-14", "", ""),
    "atomics": ("SH2-10", "vesper-check-loops-atomics", "2.31x"),
    "anvil_sim": ("SH5-14", "", ""),
    "native_freestanding": ("SH4-14", "", ""),
    "native_vector": ("SH4-11", "", ""),
    "ir_opt_policy": ("G1-12", "", ""),
    "ir_opt": ("SH3-9", "vesper-ir-opt", "0.0036x"),
    "native_windows": ("SH4-14", "", ""),
    "native_batch": ("SH4-17", "", ""),
    "database": ("G1-12", "", "salsa: memo tables"),
    "float_math": ("SH2-7", "", ""),
    "package_graph": ("SH2-6", "", ""),
    "layout": ("SH2-5", "vesper-check-layout", "swapped"),
    "interface": ("SH2-6", "", ""),
    "codegen_swap": ("2", "", "goes with the hybrid"),
    "ir_swap": ("2", "", "goes with the hybrid"),
    "stack_bound": ("SH2-10", "vesper-check-loops-atomics", "2.54x"),
    "io_session": ("SH2-10", "", ""),
    "native_arrays": ("SH4-8", "", ""),
    "lib": ("SH5-12", "", ""),
    "atomic_file": ("SH5-12", "", ""),
    "test_discovery": ("SH5-14", "", ""),
    "native_aggregate_decisions": ("SH4-8", "", ""),
    "compiler_modules": ("2", "", "goes with the hybrid"),
    "swap_input": ("2", "", "goes with the hybrid"),
    "test_engines": ("SH6-19", "", ""),
    "source_analysis": ("G1-12", "", "tour playground"),
    "browser_tour": ("G1-12", "", "tour playground"),
    "native_clif_opt": ("G1-11", "", "new in #1466 (#1426); ported with the walk"),
}


def nontest_lines(path):
    """Counts production lines, skipping every item marked `#[cfg(test)]`.

    A `#[cfg(test)]` item is skipped up to the end of its braces (or its `;`),
    so a test-only helper in the middle of a file does not hide the production
    code after it.
    """
    if path.endswith("tests.rs"):
        return 0
    n = 0
    skipping = False
    depth = 0
    opened = False
    with open(path, errors="replace") as f:
        for line in f:
            s = line.strip()
            if not skipping and re.match(r"#\[cfg\(test\)\]", s):
                skipping, depth, opened = True, 0, False
                continue
            if skipping:
                if s.startswith("#[") and not opened:
                    continue
                depth += line.count("{") - line.count("}")
                opened = opened or "{" in line
                if (opened and depth <= 0) or (not opened and s.endswith(";")):
                    skipping = False
                continue
            if s and not s.startswith("//"):
                n += 1
    return n


def tree_lines(d):
    total = 0
    for root, dirs, files in os.walk(d):
        dirs[:] = [x for x in dirs if x not in SKIP_DIRS]
        for f in files:
            if f.endswith(".rs"):
                total += nontest_lines(os.path.join(root, f))
    return total


def tracked_lines(pattern):
    out = subprocess.run(["git", "ls-files", pattern], cwd=ROOT, capture_output=True, text=True).stdout.split()
    total = 0
    for p in out:
        with open(os.path.join(ROOT, p), errors="replace") as f:
            total += sum(1 for _ in f)
    return total


def main():
    os.chdir(ROOT)
    head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    crates = {c: tree_lines(f"crates/{c}") for c in sorted(os.listdir("crates")) if os.path.isdir(f"crates/{c}")}
    modules = {}
    for root, _, files in os.walk("src"):
        for f in files:
            if f.endswith(".rs"):
                rel = os.path.relpath(os.path.join(root, f), "src")
                key = rel.split(os.sep)[0].removesuffix(".rs")
                modules[key] = modules.get(key, 0) + nontest_lines(os.path.join(root, f))
    missing = [c for c in crates if c not in CRATES and crates[c] > 0]
    missing += [m for m in modules if m not in MODULES and not m.endswith("_commands") and modules[m] > 0]

    w = sys.stdout.write
    w("# Rust census\n\n")
    w(f"> Generated by `scripts/rust-census.py` at `{head}`. Do not edit by hand: update the\n")
    w("> STATUS tables in the script and re-run it. Plan: [rust-exit-plan.md](rust-exit-plan.md).\n\n")
    w("Lines are non-test Rust (see the script). Speeds are Vesper time / Rust time from the\n")
    w("latest lane report; \"unmeasured\" means no ABBA number on file. The bar to flip a module\n")
    w("and delete its Rust (owner, 2026-11-21): full function and within 1.5x of Rust's speed.\n\n")

    by_class = {}
    for c, n in crates.items():
        cls = CRATES.get(c, ("unknown",))[0]
        by_class[cls] = by_class.get(cls, 0) + n
    # The `*42` oracles in src/bin are diverse on purpose: they stay Rust and
    # move out of `src/` before the retirement (SH6 lane 2).
    oracles = modules.get("bin", 0)
    by_class["compiler"] = by_class.get("compiler", 0) + sum(modules.values()) - oracles
    by_class["diverse"] = by_class.get("diverse", 0) + oracles
    w("## Totals\n\n| Class | Meaning | Non-test lines |\n|---|---|--:|\n")
    for cls, meaning in CLASSES.items():
        w(f"| `{cls}` | {meaning} | {by_class.get(cls, 0):,} |\n")
    w(f"| | **All non-test Rust** (root `src/` plus `crates/`) | **{sum(by_class.values()):,}** |\n\n")
    w("Test and harness Rust (all lines, tracked files): ")
    w(f"`tests/` {tracked_lines('tests/*.rs'):,}, ")
    w(f"`crates/*/tests` {tracked_lines('crates/*/tests/*.rs'):,}. ")
    w("These move to Vesper tests with each flip (SH6 lane 19).\n\n")

    w("## Crates\n\n| Crate | Lines | Class | Vesper port | Last speed | Left | SH6 lane |\n|---|--:|---|---|---|---|---|\n")
    order = list(CLASSES)
    for c in sorted(crates, key=lambda c: (order.index(CRATES[c][0]) if c in CRATES else 99, -crates[c])):
        cls, port, speed, left, lane = CRATES.get(c, ("unknown", "", "", "", ""))
        w(f"| `{c}` | {crates[c]:,} | `{cls}` | {port} | {speed} | {left} | {lane} |\n")

    w(f"\n## Root compiler (`src/`, {sum(modules.values()):,} lines)\n\n")
    w("Every module has an owner lane. SHn-m is wave n lane m; G1-m is gap wave 1 lane m; a bare number is an SH6 lane.\n\n")
    w("| Module | Lines | Owner lane | Vesper package | Status |\n|---|--:|---|---|---|\n")
    for m in sorted(modules, key=lambda m: -modules[m]):
        default = ("SH5-13", "vesper-driver-commands", "") if m.endswith("_commands") else ("**none**", "", "")
        lane, pkg, note = MODULES.get(m, default)
        w(f"| `{m}` | {modules[m]:,} | {lane} | {pkg} | {note} |\n")
    if missing:
        w("\n## Not in the STATUS tables\n\n" + "".join(f"- `{x}`\n" for x in missing))


if __name__ == "__main__":
    main()
