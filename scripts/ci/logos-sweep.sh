#!/usr/bin/env bash
# Independent Logos replay of every Lean module under lean/ (tools/logos-sweep).
# Reuses the sealed root that prepare-logos42.sh published.
#
#   VESPER_LEAN=actual-lean [LOGOS42_DRIVER=path] [VESPER_BIN=path] \
#     logos-sweep.sh SEALED_ROOT REPORT
set -euo pipefail
cd "$(dirname "$0")/../.."

if [[ $# != 2 || -z "${VESPER_LEAN:-}" ]]; then
  echo 'usage: VESPER_LEAN=actual-lean logos-sweep.sh SEALED_ROOT REPORT' >&2
  exit 2
fi
. scripts/bazel-lib.sh
if [[ -z "${LOGOS42_DRIVER:-}" ]]; then
  LOGOS42_DRIVER="$(bazel_executable //:bin/logos42)"
fi
if [[ -z "${VESPER_BIN:-}" ]]; then
  VESPER_BIN="$(bazel_executable //:bin/vesper)"
fi
status=0
python3 tools/logos-sweep/sweep.py \
  --root "$1" --driver "$LOGOS42_DRIVER" --vesper "$VESPER_BIN" --lean "$VESPER_LEAN" \
  --batch-jobs "${LOGOS_SWEEP_BATCH_JOBS:-3}" \
  --report "$2" || status=$?
if [[ -f "$2" ]]; then
  python3 - "$2" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
print(f"logos-sweep: {r['status']}: {r['modules']} modules, "
      f"{sum(b['logos_checked'] for b in r['batches'])} declarations checked by Logos "
      f"in {len(r['batches'])} batches, {r['total_seconds']} s")
for d in r["declined"]:
    print(f"  declined (Lean kernel only): {d['declaration']}: {d['reason'] or d['detail']}")
for p in r["problems"]:
    print(f"  PROBLEM: {p}")
PY
fi
exit "$status"
