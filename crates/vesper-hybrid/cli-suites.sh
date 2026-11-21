#!/usr/bin/env bash
# The root package's CLI suites (tests/*cli*.rs) on the hybrid compiler,
# whose `main` is the Vesper driver (self-hosting SH5 lane 12, #1335).
#
# The suites run `env!("CARGO_BIN_EXE_vesper")`, a path fixed when they are
# compiled, so this builds them, puts the hybrid binary at that path for the
# run (the default binary is restored afterwards, also on failure) and runs
# every suite. Extra arguments go to each test binary (e.g. `--skip name`).
#
#   crates/vesper-hybrid/cli-suites.sh [test binary args...]
#
# CARGO_TARGET_DIR is honoured; CLI_SUITES (names such as `cli audit_cli`)
# runs only those suites. The `vesper-<name>` tools the suites reach are
# built too, beside the binary. Exit status: 0 when every suite passed.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
target=${CARGO_TARGET_DIR:-$repo/target}
exe=$(rustc -vV | sed -n 's/^host: .*windows.*/.exe/p')

suites=()
for f in tests/*cli*.rs; do
  name=$(basename "$f" .rs)
  if [ -z "${CLI_SUITES:-}" ] || [[ " $CLI_SUITES " == *" $name "* ]]; then
    suites+=(--test "$name")
  fi
done

cargo build --locked -p vesper --bin vesper
cargo build --locked -p vesper-tools
cargo build --locked -p vesper-hybrid
executables=$(cargo test --locked -p vesper --no-run "${suites[@]}" 2>&1 |
  sed -n 's/^ *Executable .* (\(.*\))$/\1/p')

default="$target/debug/vesper$exe"
saved="$target/debug/vesper.default$exe"
cp "$default" "$saved"
restore() { cp "$saved" "$default"; rm -f "$saved"; }
trap restore EXIT
cp "$target/debug/vesper-hybrid$exe" "$default"

failed=()
count=0
for t in $executables; do
  count=$((count + 1))
  if ! (cd "$repo" && "$t" "$@"); then
    failed+=("$t")
  fi
done
echo "cli-suites: $count suites on the hybrid, ${#failed[@]} failed"
for t in "${failed[@]}"; do
  echo "  failed: $t"
done
[ ${#failed[@]} -eq 0 ]
