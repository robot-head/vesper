#!/usr/bin/env bash
# Official Rust wasm32-wasip2 target, locked guest dependencies. No wasm-tools
# or cargo-component is needed. Override CARGO_TARGET_DIR for private builds.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
guest="$here/guest"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$guest/target}"
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export RUSTFLAGS="--remap-path-prefix=$guest=/quotation-fixture --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
cargo build --locked --release --manifest-path "$guest/Cargo.toml" --target wasm32-wasip2
mkdir -p "$here/bin"
cp "$CARGO_TARGET_DIR/wasm32-wasip2/release/vesper_quotation_fixture.wasm" "$here/bin/quotation.component.wasm"
(cd "$here/bin" && sha256sum quotation.component.wasm > SHA256SUMS)
cat "$here/bin/SHA256SUMS"
