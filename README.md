# Vesper

A Rust implementation of the Vesper language described in [the supplied specification](https://docs.google.com/document/d/1zO7boUVwNTHRA-1ZNL4bDYi68OjQZBCimLWkETH6bso/edit). The expanded design is preserved in [docs/specification.txt](docs/specification.txt). The core design used for this implementation checkpoint is preserved separately in [docs/core-specification-baseline.txt](docs/core-specification-baseline.txt).

This repository now contains an executable language implementation rather than the original Hello World program. It is a research compiler, **not yet a complete implementation of every backend, runtime, and proof obligation in the document**. See [implementation coverage](docs/implementation.md) for the exact boundaries.

## Run

Install stable Rust and a C linker, then:

```sh
cargo build --locked
cargo run -- check examples/transaction.ves
cargo run -- run examples/transaction.ves       # 42
cargo run -- run examples/projections.ves       # 42
cargo run -- run examples/fail.ves              # 42
cargo run -- run examples/counter.ves           # 3
cargo run -- jit examples/counter.ves           # 3
cargo run -- jit examples/fibonacci.ves         # 55
cargo run -- jit examples/resumptions.ves       # 42
cargo run -- jit examples/checked_math.ves       # 42
cargo run -- build examples/fibonacci.ves -o /tmp/vesper-fib
/tmp/vesper-fib
echo $?                                        # 55 (the program's exit status)
```

`run` uses the explicit frame machine and supports algebraic data, methods, closures, place mutation, aborting handlers, and general one-shot resumptions. `jit` and `build` use Cranelift for the supported native subset; unsupported constructs produce an error. There is no silent interpreter fallback.

```sh
cargo run -- emit examples/fibonacci.ves --emit cst
cargo run -- emit examples/fibonacci.ves --emit hir
cargo run -- emit examples/fibonacci.ves --emit ir
cargo run -- emit examples/fibonacci.ves --emit clif
cargo run -- emit examples/fibonacci.ves --emit object -o /tmp/fib.o
cargo run -- emit examples/fibonacci.ves --emit object --target aarch64-unknown-linux-gnu -o /tmp/fib-arm.o
cargo run -- emit examples/fibonacci.ves --emit lean -o /tmp/Fib.lean
cargo run -- check examples/rejected/error_unwind.ves --json
cargo run -- lsp
```

`--entry` selects an entry function, `--fuel` bounds evaluator steps, and `--no-opt` disables both the pure and effectful equality-saturation passes. `--target` selects x86_64/AArch64 object generation; `build --linker` supplies a cross linker. Entry functions must take no arguments. Executable builds currently use `main`; they return its integer result as the process exit status.

## Ownership and effects

```vesper
linear struct Tx { id: u64 }

fn finish(sink tx: Tx) -> u64 {
    let Tx { id } = tx;
    id
}

fn main() -> u64 {
    let tx = Tx { id: 42 };
    finish(sink tx)
}
```

Linear values must be consumed on every path. `copy enum Option[T]` and `Result[T, E]` inherit their payload modes. `in` and `inout` parameters borrow second-class places; closures cannot capture them. The checker rejects overlapping mutable arguments, use after move, wildcard disposal of linear values, and abortable effects crossing live linear obligations or mutable argument loans.

```vesper
linear effect Counter { fn next() -> i64; }

fn main() -> i64 {
    handle Counter state count = 0 in {
        Counter::next() + Counter::next()
    } with {
        return (value, count) => wrapping_add(value, count),
        next(count, resume) => resume(count, wrapping_add(count, 1)),
    }
}
```

Plain `effect` declarations permit aborting a continuation. `linear effect` declarations require consuming it once. Stateful handlers explicitly thread unique state, and ambient clause captures must be owned copy values. Lexical labels distinguish nested handlers of the same effect. Native handlers support non-tail resumptions and linear state through stack-resident return dispatch.

Integer `+`, `-`, `*` and negation use checked semantics and require `Fail[ArithOverflow]` unless constant facts prove them safe. Use explicit `wrapping_add`, `wrapping_sub` and `wrapping_mul` for modular arithmetic. Signed `MIN / -1` also raises overflow. Division/remainder by zero and dynamic tuple indexing use the separate `Fail[RuntimeError]`. Primitive failures cannot bypass linear obligations. See [the arithmetic migration](docs/parallel-results/checked-arithmetic.md) and `examples/checked_math.ves`.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

Install the Lean version pinned in `lean-toolchain` to check extracted code and the foundation lemmas:

```sh
lean lean/Foundations.lean
lean lean/RuntimeFrames.lean
lean lean/SpecificationCounterexample.lean
lean lean/CheckedArithmetic.lean
VESPER_LEAN=lean cargo test --test lean
```

Lean integration tests use `VESPER_LEAN` or `lean` on `PATH`; they print a skip notice if neither is installed. CI installs the pinned toolchain and requires these checks. The Lean foundation file proves mode-lattice properties and the one-shot transition invariant. It does not claim compiler preservation, progress, or total linear exhaustion for the entire implementation.

With QEMU and AArch64 binutils installed, `scripts/test-aarch64.sh` links and executes all eight examples without a target libc. `AARCH64_AS`, `AARCH64_LD`, and `QEMU_AARCH64` can select custom tool paths.

The test suite includes positive/negative ownership cases, nested pattern coverage, projection overlap, effect row instantiation, capability escape, parser recovery/losslessness, interpreter/JIT comparisons, executable linking, UTF-16 LSP updates, state-token verification, effectful read/write optimization, AArch64 object generation, and Lean compilation plus scalar execution comparisons.

The Linux embedding API connects `Runtime::begin_external` and `io_session::Session` to owned io_uring requests, a completion scheduler, and nursery joins/cancellation. Segmented data frames use checked relative addresses so their storage can move. Native parked execution still needs to be connected to these runtime components.

Parallel implementation ownership and ready-to-send external agent prompts are listed in [docs/parallel-work.md](docs/parallel-work.md).

`vesper simd scan input.bin --delimiter 10 --json` runs the independent native
Baseline128 delimiter/XOR scanner. See [SIMD support and limits](docs/simd-scanner.md).

`vesper cas` ingests, verifies and materializes local domain-separated content.
See [CAS workflows](docs/cas-workflows.md).

`vesper emit --emit interface --package @org/name` and `vesper semver` now compare experimental checked source contracts. `vesper resolve --catalog ...` resolves local Loom metadata into an exact solution snapshot. See [package workflows and runnable examples](docs/package-workflows.md).
