#!/usr/bin/env bash
# Stage 0 seed reproducibility and Path B cross-check (§42.2, §42.5).
#
#   bootstrap/run.sh            # release seed, writes bootstrap/stage0_seed.lock
#   PROFILE=dev bootstrap/run.sh
#
# 1. Builds the Rust seed (`vesper`) twice, each time from a fresh target
#    directory, with SOURCE_DATE_EPOCH pinned and source/registry paths remapped.
# 2. Compiles every bootstrap/corpus/*.ves program to an object with each seed
#    and requires bit-identical objects (the gate).
# 3. Path A results: links and runs each program with seed A (exit status).
# 4. Path B: compiles lean/BootstrapDdc/Reference.lean with Lean's C emitter,
#    compiles and links the C with gcc (no rustc, no Cranelift; CompCert is not
#    packaged here), runs the corpus and requires the same statuses.
# 5. Records everything in the seed lock with `vesper-bootstrap seed-lock`.
#
# Environment: VESPER_LEAN (lean binary; default `lean` on PATH), CC_B (Path B
# C compiler, default gcc), WORK (scratch, default target/bootstrap), OUT (lock
# path), SOURCE_DATE_EPOCH (default: HEAD's commit time).
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PROFILE=${PROFILE:-release}
WORK=${WORK:-$ROOT/target/bootstrap}
OUT=${OUT:-$ROOT/bootstrap/stage0_seed.lock}
LEAN=${VESPER_LEAN:-lean}
CC_B=${CC_B:-gcc}
case "$PROFILE" in
  release) PROFILE_DIR=release ;;
  dev) PROFILE_DIR=debug ;;
  *) echo "PROFILE must be release or dev" >&2; exit 2 ;;
esac
command -v "$LEAN" >/dev/null || { echo "Lean not found (set VESPER_LEAN)" >&2; exit 2; }
command -v "$CC_B" >/dev/null || { echo "$CC_B not found (set CC_B)" >&2; exit 2; }
LEAN_HOME=$(cd "$(dirname "$(command -v "$LEAN")")/.." && pwd)

cd "$ROOT"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}
export CARGO_INCREMENTAL=0
CARGO_HOME_DIR=${CARGO_HOME:-$HOME/.cargo}
SEED_FLAGS="--remap-path-prefix=$ROOT=/vesper --remap-path-prefix=$CARGO_HOME_DIR=/cargo"
BASE=$(git rev-parse HEAD)
mkdir -p "$WORK"
rm -rf "$WORK/seed-a" "$WORK/seed-b" "$WORK/objects-a" "$WORK/objects-b" "$WORK/exe" "$WORK/path-b"
mkdir -p "$WORK/objects-a" "$WORK/objects-b" "$WORK/exe" "$WORK/path-b"

build_seed() {
  echo "== building seed $1 ($PROFILE) from a clean target directory"
  rm -rf "$WORK/target"
  RUSTFLAGS="$SEED_FLAGS" cargo build --locked --profile "$PROFILE" --bin vesper \
    --target-dir "$WORK/target"
  cp "$WORK/target/$PROFILE_DIR/vesper" "$WORK/seed-$1"
}
build_seed a
build_seed b
rm -rf "$WORK/target"

mapfile -t CORPUS < <(cd "$ROOT" && ls bootstrap/corpus/*.ves | LC_ALL=C sort)
: >"$WORK/results-a.txt"
for src in "${CORPUS[@]}"; do
  stem=$(basename "$src" .ves)
  for seed in a b; do
    "$WORK/seed-$seed" emit "$src" --emit object -o "$WORK/objects-$seed/$stem.o" >/dev/null
  done
  "$WORK/seed-a" build "$src" -o "$WORK/exe/$stem" >/dev/null
  set +e
  "$WORK/exe/$stem"
  status=$?
  set -e
  echo "$stem $status" >>"$WORK/results-a.txt"
done

echo "== Path B: Lean C emitter + $CC_B"
(cd "$ROOT/lean" && "$LEAN" -c "$WORK/path-b/reference.c" BootstrapDdc/Reference.lean)
"$CC_B" -O2 -I"$LEAN_HOME/include" -c "$WORK/path-b/reference.c" -o "$WORK/path-b/reference.o"
"$CC_B" "$WORK/path-b/reference.o" -L"$LEAN_HOME/lib/lean" -lleanshared \
  -Wl,-rpath,"$LEAN_HOME/lib/lean" -o "$WORK/path-b/vesperc-lean-ddc"
"$WORK/path-b/vesperc-lean-ddc" "${CORPUS[@]}" >"$WORK/results-b.txt"

echo "== recording $OUT"
cargo run --quiet --locked -p vesper-codegen-verified --bin vesper-bootstrap \
  --target-dir "$WORK/tools" -- seed-lock \
  --base-commit "$BASE" \
  --rustc "$(rustc --version | cut -d' ' -f1-2)" \
  --profile "$PROFILE" \
  --epoch "$SOURCE_DATE_EPOCH" \
  --seed-a "$WORK/seed-a" --seed-b "$WORK/seed-b" \
  --objects-a "$WORK/objects-a" --objects-b "$WORK/objects-b" \
  --results-a "$WORK/results-a.txt" --results-b "$WORK/results-b.txt" \
  --lean "$("$LEAN" --version | cut -d' ' -f1-3 | tr -d ',')" \
  --cc "$("$CC_B" --version | head -1)" \
  --out "$OUT"
