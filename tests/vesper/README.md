# The Vesper test suite

Tests written in Vesper, run by Anvil (`vesper test`). This is where the
compiler's Rust integration tests move as the Rust leaves (Rust-exit wave
SH6 lane 19, #1414). The recipe is in
[porting-a-crate.md](../../docs/maintenance/porting-a-crate.md#moving-the-tests-to-vesper).

Run it from the repository root:

```sh
vesper test tests/vesper --engines all --no-cache --fuel 200000000
```

`tests/vesper_suite.rs` runs the same command under `cargo test` and
`bazel test`, so every CI platform runs it.

## Layout

One directory per family: the Rust test file it came from, named after it
(`tests/continue.rs` became `continue/`). Each `.ves` file is one standalone
program with its own `main` and a `test { }` block, so cases never share
names. A family converted by `scripts/convert-rust-tests.py` has a
`converted.json` that records the Rust file, the number of cases, the
`#[test]` functions the cases cover completely, and the ones kept in Rust.
`tests/vesper_suite.rs` checks the case count, so a lost file fails CI.

## The two shapes

A program that computes a value:

```vesper
fn main() -> i64 { ... }

test {
    @test
    fn loop_odd() -> bool {
        main() == 100
    }
}
```

With `--engines all`, `vesper test` runs each passing unit test again on the
evaluator and the JIT, optimized and not, and requires `true` from each; the
JIT must leave nothing for teardown and free every `Box` it allocates; the
test and `main` must compile to objects for `x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu`; and where a C driver links executables, the
executable's exit status must be the low byte of the evaluator's `main()`.
That is what the Rust `differential` helpers checked.

A program the checker rejects:

```vesper
linear struct Token { n: i64 }

test {
    @test(expect_error = E0302, message = "must be consumed exactly once")
    fn main() -> i64 {
        let t = Token { n: 1 };
        0
    }
}
```

The function holding the error goes in the test block; the rest of the
program stays outside it and must check. `message` (a substring of the
diagnostic's message) joins `primary` and `related`.
