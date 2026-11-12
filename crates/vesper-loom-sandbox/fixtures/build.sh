#!/usr/bin/env bash
# Rebuilds the committed sandbox fixtures from fixtures/guest.
#
# Requires: rustup targets wasm32-wasip2 and wasm32-wasip1.
#   rustup target add wasm32-wasip2 wasm32-wasip1
#
# Outputs (committed so tests need no wasm toolchain):
#   bin/step.component.wasm  wasm32-wasip2 component (vesper:sandbox@0.1.0)
#   bin/step.preview1.wasm   wasm32-wasip1 core module, same source
#   bin/SHA256SUMS           checked by the test suite
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
guest="$here/guest"
export CARGO_TARGET_DIR="$guest/target"
# Keep host paths out of the binaries so rebuilds are comparable.
export RUSTFLAGS="--remap-path-prefix=$guest=/fixture --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
for target in wasm32-wasip2 wasm32-wasip1; do
  cargo build --locked --release --manifest-path "$guest/Cargo.toml" --target "$target"
done
mkdir -p "$here/bin"
cp "$CARGO_TARGET_DIR/wasm32-wasip2/release/vesper_sandbox_fixture.wasm" "$here/bin/step.component.wasm"
cp "$CARGO_TARGET_DIR/wasm32-wasip1/release/vesper_sandbox_fixture.wasm" "$here/bin/step.preview1.wasm"
(cd "$here/bin" && sha256sum step.component.wasm step.preview1.wasm > SHA256SUMS)
cat "$here/bin/SHA256SUMS"
