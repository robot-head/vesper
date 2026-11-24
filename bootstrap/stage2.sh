#!/usr/bin/env bash
# Official Stage 2 bootstrap runner powered by prebuilt `wasmtime` CLI (SH6 lane 2, #1397).
#
# Resolves the pinned `wasmtime` (v50.0.1) binary from:
#   1. $WASMTIME_BIN / $WASMTIME
#   2. `wasmtime` on $PATH (including Bazel test runfiles via @wasmtime//:wasmtime)
#   3. `bazel` / `bazelisk` building @wasmtime//:wasmtime
#
# Usage:
#   bootstrap/stage2.sh which
#   bootstrap/stage2.sh run <stage2.wasm> <pkg_dir> <out_wasm> [entry]
#   bootstrap/stage2.sh verify <stage2.wasm> [work_dir]
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PINNED_VERSION="50.0.1"

host_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -m "$1"
  else
    printf '%s\n' "$1"
  fi
}

resolve_wasmtime() {
  if [[ -n "${WASMTIME_BIN:-}" ]]; then
    printf '%s\n' "$WASMTIME_BIN"
    return 0
  fi
  if [[ -n "${WASMTIME:-}" ]]; then
    printf '%s\n' "$WASMTIME"
    return 0
  fi
  if command -v wasmtime >/dev/null 2>&1; then
    command -v wasmtime
    return 0
  fi
  local bazel_cmd=""
  if command -v bazel >/dev/null 2>&1; then
    bazel_cmd=bazel
  elif command -v bazelisk >/dev/null 2>&1; then
    bazel_cmd=bazelisk
  fi
  if [[ -n "$bazel_cmd" ]]; then
    (cd "$ROOT" && "$bazel_cmd" build @wasmtime//:wasmtime >&2) || return 1
    local rel
    rel=$(cd "$ROOT" && "$bazel_cmd" cquery --output=files @wasmtime//:wasmtime 2>/dev/null | tr -d '\r' | head -n 1)
    if [[ -n "$rel" && -x "$ROOT/$rel" ]]; then
      printf '%s\n' "$ROOT/$rel"
      return 0
    fi
  fi
  for cand in "$ROOT"/bazel-bin/external/+wasmtime+wasmtime_*/wasmtime.exe "$ROOT"/bazel-bin/external/+wasmtime+wasmtime_*/wasmtime; do
    if [[ -x "$cand" ]]; then
      printf '%s\n' "$cand"
      return 0
    fi
  done
  echo "bootstrap/stage2.sh: could not find wasmtime CLI (set WASMTIME_BIN, put wasmtime on PATH, or run with Bazel)" >&2
  return 1
}

check_wasmtime_version() {
  local wt="$1"
  local ver
  ver=$("$wt" --version)
  if [[ "${VESPER_ALLOW_UNPINNED_WASMTIME:-0}" != "1" && "$ver" != *"$PINNED_VERSION"* ]]; then
    echo "bootstrap/stage2.sh: expected wasmtime $PINNED_VERSION, got: $ver (set VESPER_ALLOW_UNPINNED_WASMTIME=1 to override)" >&2
    return 1
  fi
  printf '%s\n' "$ver"
}

run_stage2() {
  if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo "usage: bootstrap/stage2.sh run <stage2.wasm> <pkg_dir> <out_wasm> [entry]" >&2
    return 2
  fi
  local stage2_wasm="$1"
  local pkg_dir="$2"
  local out_wasm="$3"
  local entry="${4:-main}"

  local out_dir out_name host_root host_out
  out_dir=$(cd "$(dirname "$out_wasm")" 2>/dev/null || { mkdir -p "$(dirname "$out_wasm")" && cd "$(dirname "$out_wasm")"; } && pwd)
  out_name=$(basename "$out_wasm")
  local host_out_file="$out_dir/$out_name"

  rm -f "$host_out_file" "${host_out_file}.err.txt" "${host_out_file}.stage" "${host_out_file}.syms"

  local wt
  wt=$(resolve_wasmtime)
  host_root=$(host_path "$ROOT")
  host_out=$(host_path "$out_dir")

  local env_args=()
  if [[ -n "${VESPER_STAGE2_SYMS:-}" ]]; then
    env_args=(--env "VESPER_STAGE2_SYMS=$VESPER_STAGE2_SYMS")
  fi

  "$wt" run \
    --dir "${host_root}::." \
    --dir "${host_out}::/out" \
    "${env_args[@]}" \
    --argv0 stage2 \
    "$stage2_wasm" \
    "$pkg_dir" \
    "/out/$out_name" \
    "$entry"

  if [[ -s "${host_out_file}.err.txt" ]]; then
    echo "stage2 error diagnostic (${host_out_file}.err.txt):" >&2
    cat "${host_out_file}.err.txt" >&2
    return 1
  fi
  if [[ ! -s "$host_out_file" ]]; then
    echo "stage2 produced no output component at $host_out_file" >&2
    if [[ -f "${host_out_file}.stage" ]]; then
      echo "stage log:" >&2
      cat "${host_out_file}.stage" >&2
    fi
    return 1
  fi
}

verify_fixed_point() {
  if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "usage: bootstrap/stage2.sh verify <stage2.wasm> [work_dir]" >&2
    return 2
  fi
  local stage2_wasm="$1"
  local work_dir="${2:-$ROOT/target/bootstrap-stage2}"
  mkdir -p "$work_dir"
  local stage3_wasm="$work_dir/stage3.wasm"

  local wt
  wt=$(resolve_wasmtime)
  echo "== Using $(check_wasmtime_version "$wt") ($wt)"
  echo "== Self-rebuilding @vesper/stage2 with $stage2_wasm -> $stage3_wasm"
  VESPER_STAGE2_SYMS=${VESPER_STAGE2_SYMS:-1} run_stage2 "$stage2_wasm" "packages/vesper-stage2" "$stage3_wasm" "main"

  if ! cmp -s "$stage2_wasm" "$stage3_wasm"; then
    echo "stage2 fixed-point mismatch: $stage2_wasm differs from $stage3_wasm" >&2
    ls -l "$stage2_wasm" "$stage3_wasm" >&2
    return 1
  fi
  echo "== Stage 2 fixed-point verified under wasmtime: stage2 == stage3 ($(wc -c < "$stage3_wasm" | tr -d ' ') bytes)"
}

cmd="${1:-}"
shift || true
case "$cmd" in
  which)
    wt=$(resolve_wasmtime)
    check_wasmtime_version "$wt" >/dev/null
    printf '%s\n' "$wt"
    ;;
  run)
    run_stage2 "$@"
    ;;
  verify|fixed-point)
    verify_fixed_point "$@"
    ;;
  *)
    echo "usage: bootstrap/stage2.sh <which|run|verify> ..." >&2
    exit 2
    ;;
esac
