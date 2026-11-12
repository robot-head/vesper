#!/usr/bin/env bash
# Rebuilds the committed macro fixture component from fixtures/guest.
#
# Requires: rustup target add wasm32-wasip2
#
# The guest is compiled against the (read-only) protocol WIT in
# ../../vesper-loom-sandbox/wit, so it links against exactly the
# `vesper:sandbox/step@0.1.0` interface the sandbox host provides.
#
# Outputs (committed so tests need no wasm toolchain):
#   bin/macro.component.wasm  wasm32-wasip2 component
#   bin/SHA256SUMS            checked by the test suite
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
guest="$here/guest"
export CARGO_TARGET_DIR="$guest/target"
# Keep host paths out of the binary so rebuilds are comparable.
export RUSTFLAGS="--remap-path-prefix=$guest=/fixture --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
cargo build --locked --release --manifest-path "$guest/Cargo.toml" --target wasm32-wasip2
mkdir -p "$here/bin"
cp "$CARGO_TARGET_DIR/wasm32-wasip2/release/vesper_forge_runner_fixture.wasm" "$here/bin/macro.component.wasm"
(cd "$here/bin" && sha256sum macro.component.wasm > SHA256SUMS)
cat "$here/bin/SHA256SUMS"
