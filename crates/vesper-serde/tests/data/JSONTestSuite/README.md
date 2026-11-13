# JSONTestSuite parsing corpus

Copied from https://github.com/nst/JSONTestSuite, commit
`1ef36fa01286573e846ac449e8683f8833c5b26a`, directory `test_parsing/`, under
the MIT license in `LICENSE`. File names start with `y_` (must accept), `n_`
(must reject) or `i_` (implementation-defined).

Two large generated files are not copied; `tests/json_test_suite.rs`
regenerates them byte for byte:

- `n_structure_100000_opening_arrays.json`: `[` repeated 100000 times.
- `n_structure_open_array_object.json`: `[{"":` repeated 50000 times, then `\n`.
