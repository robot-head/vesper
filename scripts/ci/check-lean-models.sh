#!/usr/bin/env bash
# Kernel-checks the Lean models and their axiom reports with the pinned Lean.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.elan/bin:$PATH"
lean --version | grep -F "version $(sed 's/.*:v//' lean-toolchain)"
for model in \
  Foundations RuntimeFrames SpecificationCounterexample EffectPlaces \
  CheckedArithmetic FluxDbsp AetherMemory MetatheoryAppendix MetronomeArithmetic \
  StylusAndLintSafety RewriteWitness; do
  echo "lean lean/$model.lean"
  lean "lean/$model.lean"
done

# The executable oracle imports the fixed rewrite model. Keep its compiled
# dependency private, and check definitions without starting the IO server.
oracle_model_dir="$(mktemp -d)"
trap 'rm -rf "$oracle_model_dir"' EXIT
oracle_import_dir="$oracle_model_dir"
if command -v cygpath >/dev/null 2>&1; then
  oracle_import_dir="$(cygpath -w "$oracle_model_dir")"
fi
lean -o "$oracle_model_dir/RewriteWitness.olean" lean/RewriteWitness.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/ArithmeticOracle.olean" lean/ArithmeticOracle.lean
