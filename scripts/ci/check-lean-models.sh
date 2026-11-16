#!/usr/bin/env bash
# Kernel-checks the Lean models and their axiom reports with the pinned Lean.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.elan/bin:$PATH"
lean --version | grep -F "version $(sed 's/.*:v//' lean-toolchain)"
for model in \
  Foundations RuntimeFrames SpecificationCounterexample EffectPlaces \
  MetatheoryFull Metatheory/Closures Metatheory/Polymorphism MetatheoryPlaces CheckedArithmetic \
  FluxDbsp AetherMemory WalSafety MetatheoryAppendix VesperUnifiedTypeTheory \
  StrongUpdate ClockQuotientSafety ResourceGrades MetronomeArithmetic StylusAndLintSafety \
  RewriteWitness CodexSearchAndLayout GrammarNonMalleability PrismAuthSafety IgnisTriBackendEquiv \
  BrandGeometrySafety InspectorUiSpecs; do
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

# Ghost erasure extends the same operational core; build its import from source.
lean -o "$oracle_model_dir/MetatheoryAppendix.olean" lean/MetatheoryAppendix.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/GhostErasure.olean" lean/GhostErasure.lean

# Compose closed ghost proofs with the polymorphic operational semantics.
mkdir -p "$oracle_model_dir/Metatheory"
lean -o "$oracle_model_dir/Metatheory/Polymorphism.olean" lean/Metatheory/Polymorphism.lean
LEAN_PATH="$oracle_import_dir" lean lean/OperationalGhostBridge.lean

# Check the refinement contract against the effect extractor's place lenses.
lean -o "$oracle_model_dir/EffectPlaces.olean" lean/EffectPlaces.lean
LEAN_PATH="$oracle_import_dir" lean lean/HandlerGrades.lean
lean -o "$oracle_model_dir/StrongUpdate.olean" lean/StrongUpdate.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/RefinementPlaces.olean" lean/RefinementPlaces.lean
lean -o "$oracle_model_dir/MetatheoryPlaces.olean" lean/MetatheoryPlaces.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/TypedPlaceRefinements.olean" lean/TypedPlaceRefinements.lean
LEAN_PATH="$oracle_import_dir" lean lean/PlacePolymorphismBridge.lean

# Ordered guard traces refine the same permission and LIFO primitives.
lean -o "$oracle_model_dir/VesperUnifiedTypeTheory.olean" lean/VesperUnifiedTypeTheory.lean
LEAN_PATH="$oracle_import_dir" lean lean/OrderedContexts.lean

# Compose the existing instruction-selection table over full data-row streams.
lean -o "$oracle_model_dir/VerifiedCodegen.olean" lean/VerifiedCodegen.lean
LEAN_PATH="$oracle_import_dir" lean lean/CodegenRv64Encoding.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/CodegenComposition.olean" lean/CodegenComposition.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/CodegenControlFlow.olean" lean/CodegenControlFlow.lean
LEAN_PATH="$oracle_import_dir" lean -o "$oracle_model_dir/CodegenValidation.olean" lean/CodegenValidation.lean
LEAN_PATH="$oracle_import_dir" lean lean/CodegenFlowValidation.lean
