# @vesper/lsp-proof-run

The proof-lens runner of `vesper lsp` (`src/lsp/proof_lens.rs`) in Vesper,
part 8 of #1828. Rust stays the default: the language server still runs
`src/lsp/proof_lens.rs`, and this package is checked against it by
`tests/self_host_proof_lens.rs`.

`proof_run(job_path, out_path)` reads a prepared job (as
`proof_lens::job_words` lays it out) and runs it the way `proof_lens::run`
does:

- it finds Lean through the launcher (`--print-prefix`) and requires
  Lean 4.35.0;
- it compiles the extraction module and each semantic model into a private
  staging directory, reusing `.olean` files from the content-keyed cache;
- when `omega` fails on a model it runs the `bv_decide` rendering, which
  either finds no counterexample (cached as `checked_native`) or renders
  one as source values (cached as `refuted`);
- it replays every compiled theorem through one `vesper proof-audit` run
  and reads its JSON report.

Children run in their own process group with both streams captured
(8 MiB each) and are killed with their tree at the deadline. The outcome
rows (`proof_lens::outcome_rows`) go to `out_path`.

Modules:

- `lib.ves`: the job, files, children, cache, stages and the entry.
- `json.ves`: serde_json's `from_slice` into a value, with its error
  messages and positions, and its string writer.
- `text.ves`: lossy UTF-8, `str::trim`, `first_error`, `io::Error` and
  `ExitStatus` displays.

serde_json's "number out of range" check comes from `@vesper/lsp`.

Cancellation (the server's `CancelToken`) is not part of this package yet;
the hybrid wiring adds it.
