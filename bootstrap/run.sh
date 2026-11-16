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
# Stage 1 needs neither Lean nor a second Rust seed build. Its historical
# Stage 0 lock is a lineage anchor; the actual seed binary is bound separately.
#
#   bootstrap/run.sh stage1 [--reproduce] [--record | --outcome] [--require-cross-backend]
#
# --record      write a new baseline to OUT (default bootstrap/stage1.lock).
# (default)     rebuild and require OUT to match bit for bit.
# --outcome     ratchet against OUT without requiring identical artifacts:
#               fail on another corpus or a lost verified build, report
#               improvements and stale fields, write WORK/stage1.fresh.lock.
# --require-cross-backend  also require every program to build with the
#               verified backend (the strict DDC-style gate).
# --reproduce   first build the runner twice from clean target directories at
#               two different paths, record with each, and require the two
#               locks (seed digest, objects, verified executables) to be
#               byte-identical; then run the requested mode with build B.
if [[ ${1:-seed} == stage1 ]]; then
  shift
  cd "$ROOT"
  stage1_reproduce=0
  stage1_args=()
  for arg in "$@"; do
    case "$arg" in
      --reproduce) stage1_reproduce=1 ;;
      --record|--outcome|--require-cross-backend) stage1_args+=("$arg") ;;
      *) echo "usage: bootstrap/run.sh stage1 [--reproduce] [--record | --outcome] [--require-cross-backend]" >&2; exit 2 ;;
    esac
  done
  stage1_work=${WORK:-$ROOT/target/bootstrap-stage1}
  stage1_profile=${PROFILE:-release}
  case "$stage1_profile" in
    release) stage1_profile_dir=release ;;
    dev) stage1_profile_dir=debug ;;
    *) echo "PROFILE must be release or dev" >&2; exit 2 ;;
  esac
  export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} CARGO_INCREMENTAL=0
  export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
  mkdir -p "$stage1_work"
  stage1_work=$(cd "$stage1_work" && pwd)
  export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-1794650795}
  stage1_target=$(rustc -vV | sed -n 's/^host: //p')
  stage1_out=${OUT:-$ROOT/bootstrap/stage1.lock}
  # build_runner TARGET_DIR EXE: the target directory is remapped too, so
  # the runner's digest does not depend on where it was built. rustc applies
  # the last matching prefix, so the target mapping comes last (WORK may lie
  # inside the checkout).
  build_runner() {
    RUSTFLAGS="--remap-path-prefix=$ROOT=/vesper --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$1=/target" \
      cargo build --locked --profile "$stage1_profile" --target-dir "$1" \
      --lib -p vesper -p vesper-codegen-verified
    # Cargo 1.100 may place dependencies in build/<crate>/<hash>/out rather
    # than deps/. Discover artifact directories instead of assuming its layout.
    python3 "$ROOT/bootstrap/compile-stage1.py" "$1/$stage1_profile_dir" \
      "$ROOT/bootstrap/stage1.rs" "$2"
  }
  if [[ $stage1_reproduce == 1 ]]; then
    for side in a b; do
      echo "== Stage 1 runner $side ($stage1_profile) from a clean target directory"
      rm -rf "$stage1_work/repro-$side"
      mkdir -p "$stage1_work/repro-$side"
      build_runner "$stage1_work/repro-$side/target" "$stage1_work/repro-$side/stage1-runner"
      "$stage1_work/repro-$side/stage1-runner" "$ROOT" "$stage1_work/repro-$side/work" \
        "$stage1_target" "$stage1_work/repro-$side/stage1.lock" --record
      rm -rf "$stage1_work/repro-$side/target"
    done
    if ! cmp "$stage1_work/repro-a/stage1.lock" "$stage1_work/repro-b/stage1.lock"; then
      diff "$stage1_work/repro-a/stage1.lock" "$stage1_work/repro-b/stage1.lock" >&2 || true
      echo "stage1: ReproducibilityMismatch: two clean builds recorded different locks" >&2
      exit 1
    fi
    echo "== Stage 1 reproducible: two clean builds recorded identical locks"
    cp "$stage1_work/repro-b/stage1-runner" "$stage1_work/stage1-runner"
  else
    build_runner "${CARGO_TARGET_DIR:-$stage1_work/target}" "$stage1_work/stage1-runner"
  fi
  exec "$stage1_work/stage1-runner" "$ROOT" "$stage1_work" \
    "$stage1_target" "$stage1_out" ${stage1_args[@]+"${stage1_args[@]}"}
fi
if [[ $# -gt 0 ]]; then
  echo "usage: bootstrap/run.sh [stage1 [--reproduce] [--record | --outcome] [--require-cross-backend]]" >&2
  exit 2
fi
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
# Ask Lean for its toolchain prefix: under elan, `lean` on PATH is a launcher.
LEAN_HOME=$("$LEAN" --print-prefix)

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
  --lean "Lean $("$LEAN" --version | sed -n 's/.*version \([0-9][0-9.]*\).*/\1/p')" \
  --cc "$("$CC_B" --version | head -1)" \
  --out "$OUT"
