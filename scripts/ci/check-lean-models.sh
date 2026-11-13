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
